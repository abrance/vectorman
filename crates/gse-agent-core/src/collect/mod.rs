//! Agent 采集：按 GSE 下发的采集项运行采集器，内存缓冲后直连 dataserver 上报。
//!
//! 采集项以 GSE 为准；无采集项则不采集。Agent 不向 dataserver 轮询采集项。

pub mod buffer;
pub mod clean;
pub mod config;
pub mod ebpf;
pub mod envelope;
pub mod glob;
pub mod k8s;
pub mod kubeconfig;
pub mod logfile;
pub mod metrics;
pub mod otlp;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use geminio::{Bytes, End};
use serde_json::Value;
use tokio::sync::{mpsc, RwLock};

use buffer::{Buffer, DEFAULT_MAX_RECORDS};
use config::CollectorConfig;
use envelope::{DataEnvelope, IngestReply};
use gse_agent_ebpf::attach::EbpfItemKind;
use gse_proto::{CollectItem, CollectItemsReply, DataplaneAddrReply, DataplaneAddrRequest};

/// 当前时间（微秒）。
pub fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

static BATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// 采集器与上报循环共用的运行时状态。
pub struct CollectShared {
    pub agent_id: String,
    host_id: RwLock<Option<String>>,
    ingest_url: RwLock<Option<String>>,
    buffer: Buffer,
    /// OTLP 接收器的进程级配置（来自 Agent 本地配置，非采集项下发）。
    pub otlp_enabled: bool,
    pub otlp_listen: String,
    pub otlp_max_body_bytes: usize,
    pub otlp_token: String,
    pub otlp_allowed_cidrs: Vec<String>,
}

impl CollectShared {
    pub fn new(agent_id: String) -> Self {
        Self {
            agent_id,
            host_id: RwLock::new(None),
            ingest_url: RwLock::new(None),
            buffer: Buffer::new(DEFAULT_MAX_RECORDS),
            otlp_enabled: false,
            otlp_listen: otlp::DEFAULT_LISTEN.to_string(),
            otlp_max_body_bytes: otlp::DEFAULT_MAX_BODY_BYTES,
            otlp_token: String::new(),
            otlp_allowed_cidrs: Vec::new(),
        }
    }

    /// 设置 OTLP 接收器参数（Agent 启动时从本地配置调用）。
    pub fn set_otlp_options(
        &mut self,
        enabled: bool,
        listen: String,
        max_body_bytes: usize,
        token: String,
        allowed_cidrs: Vec<String>,
    ) {
        self.otlp_enabled = enabled;
        self.otlp_listen = if listen.trim().is_empty() {
            otlp::DEFAULT_LISTEN.to_string()
        } else {
            listen
        };
        self.otlp_max_body_bytes = max_body_bytes.max(1_024);
        self.otlp_token = token;
        self.otlp_allowed_cidrs = allowed_cidrs;
    }

    pub async fn host_id(&self) -> Option<String> {
        self.host_id.read().await.clone()
    }

    pub async fn set_host_id(&self, value: Option<String>) {
        *self.host_id.write().await = value;
    }

    pub async fn ingest_url(&self) -> Option<String> {
        self.ingest_url.read().await.clone()
    }

    pub async fn set_ingest_url(&self, value: Option<String>) {
        *self.ingest_url.write().await = value.filter(|u| !u.is_empty());
    }

    /// 组批入队；`data_id = item_id`，空记录不入队。
    pub async fn push(&self, data_type: &str, item_id: &str, records: Vec<Value>) {
        if records.is_empty() {
            return;
        }
        let host_id = self.host_id().await.unwrap_or_default();
        let env = DataEnvelope {
            batch_id: format!(
                "{}-{}-{}",
                self.agent_id,
                now_micros(),
                BATCH_SEQ.fetch_add(1, Ordering::Relaxed)
            ),
            data_type: data_type.to_string(),
            data_id: item_id.to_string(),
            agent_id: self.agent_id.clone(),
            host_id,
            sent_at_micros: now_micros(),
            records,
        };
        self.buffer.push(env).await;
    }
}

/// 上报结果分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// 已受理（2xx 且 `ok`/`partial`，或 400 整批非法）。
    Confirm,
    /// 可重试（5xx、超时、连接失败）。
    Retry,
    /// 可重试且需要重新拉取地址（503 或 `code=unavailable`）。
    Repull,
}

/// `POST {ingest_url}/v1/ingest`，超时 10s。
fn post_ingest(agent: &ureq::Agent, ingest_url: &str, env: &DataEnvelope) -> Disposition {
    let url = format!("{}/v1/ingest", ingest_url.trim_end_matches('/'));
    match agent
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(env)
    {
        Ok(resp) => {
            let code = resp.status();
            let reply = resp.into_json::<IngestReply>().ok();
            if code == 503 {
                return Disposition::Repull;
            }
            match reply {
                Some(r) if r.status == "ok" || r.status == "partial" => Disposition::Confirm,
                Some(r) if r.code.as_deref() == Some("unavailable") => Disposition::Repull,
                _ => Disposition::Retry,
            }
        }
        Err(ureq::Error::Status(400, _)) => Disposition::Confirm,
        Err(ureq::Error::Status(503, _)) => Disposition::Repull,
        Err(_) => Disposition::Retry,
    }
}

/// 采集项对齐控制消息。
enum Control {
    /// GSE 下发/推送的采集项整表。
    Items(CollectItemsReply),
    /// 立即重新拉取数据面地址。
    PullAddr,
}

/// OTLP 接收器的进程级参数。
#[derive(Debug, Clone, Default)]
pub struct OtlpOptions {
    pub enabled: bool,
    pub listen: String,
    pub max_body_bytes: usize,
    pub token: String,
    pub allowed_cidrs: Vec<String>,
}

/// 采集运行时句柄：持有共享状态，向 supervisor 投递采集项与地址拉取请求。
#[derive(Clone)]
pub struct CollectorHandle {
    shared: Arc<CollectShared>,
    tx: mpsc::UnboundedSender<Control>,
}

impl CollectorHandle {
    /// OTLP 接收器的进程级参数（来自 Agent 本地配置）。
    pub fn new_with_otlp(agent_id: String, end: End, otlp: OtlpOptions) -> Self {
        let mut shared = CollectShared::new(agent_id);
        shared.set_otlp_options(
            otlp.enabled,
            otlp.listen,
            otlp.max_body_bytes,
            otlp.token,
            otlp.allowed_cidrs,
        );
        Self::from_shared(shared, end)
    }

    /// 创建句柄并启动 supervisor；上报循环随之启动。
    pub fn new(agent_id: String, end: End) -> Self {
        Self::from_shared(CollectShared::new(agent_id), end)
    }

    fn from_shared(shared: CollectShared, end: End) -> Self {
        let shared = Arc::new(shared);
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(supervise(shared.clone(), end, rx));
        Self { shared, tx }
    }

    pub fn shared(&self) -> Arc<CollectShared> {
        self.shared.clone()
    }

    /// 应用一批采集项（可空表，表示停止全部）。
    pub fn apply(&self, reply: CollectItemsReply) {
        let _ = self.tx.send(Control::Items(reply));
    }

    /// 认证成功后触发首次地址拉取。
    pub fn pull_addr_now(&self) {
        let _ = self.tx.send(Control::PullAddr);
    }
}

struct Runner {
    fingerprint: String,
    handle: tokio::task::JoinHandle<()>,
}

async fn supervise(shared: Arc<CollectShared>, end: End, mut rx: mpsc::UnboundedReceiver<Control>) {
    tokio::spawn(report_loop(shared.clone()));
    let mut runners: HashMap<String, Runner> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    tick.tick().await;
    let mut started = false;

    loop {
        tokio::select! {
            maybe = rx.recv() => match maybe {
                Some(Control::Items(reply)) => reconcile(&shared, &mut runners, reply.items),
                Some(Control::PullAddr) => {
                    started = true;
                    pull_addr(&shared, &end).await;
                }
                None => break,
            },
            _ = tick.tick(), if started => {
                if shared.ingest_url().await.is_none() {
                    pull_addr(&shared, &end).await;
                }
            }
        }
    }
    for (_, runner) in runners {
        runner.handle.abort();
    }
}

/// 按 `item_id` 对齐采集器：新增/变更重启，移除或停用则停止。
fn reconcile(
    shared: &Arc<CollectShared>,
    runners: &mut HashMap<String, Runner>,
    items: Vec<CollectItem>,
) {
    let mut keep: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in items {
        if !item.enabled {
            continue;
        }
        keep.insert(item.item_id.clone());
        let fp = fingerprint(&item);
        if runners
            .get(&item.item_id)
            .map(|r| r.fingerprint == fp)
            .unwrap_or(false)
        {
            continue;
        }
        if let Some(old) = runners.remove(&item.item_id) {
            old.handle.abort();
        }
        let handle = spawn_collector(shared.clone(), item.clone());
        runners.insert(
            item.item_id.clone(),
            Runner {
                fingerprint: fp,
                handle,
            },
        );
    }
    let stale: Vec<String> = runners
        .keys()
        .filter(|k| !keep.contains(*k))
        .cloned()
        .collect();
    for key in stale {
        if let Some(runner) = runners.remove(&key) {
            runner.handle.abort();
        }
    }
}

fn fingerprint(item: &CollectItem) -> String {
    serde_json::json!({
        "kind": item.kind,
        "enabled": item.enabled,
        "collector": item.collector,
        "storage": item.storage,
    })
    .to_string()
}

fn spawn_collector(shared: Arc<CollectShared>, item: CollectItem) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let cfg = CollectorConfig::from_value(&item.collector);
        match item.kind.as_str() {
            "metrics_host" => metrics::run(shared, item.item_id, cfg.interval_secs).await,
            "log_file" => logfile::run(shared, item.item_id, cfg).await,
            "log_k8s_stdout" => k8s::run(shared, item.item_id, cfg).await,
            "apm_otlp" => {
                if !shared.otlp_enabled {
                    eprintln!(
                        "gse-agent: collect item {} is apm_otlp but otlp_enabled=false; skipped",
                        item.item_id
                    );
                    return;
                }
                // OTLP 接收器的进程级参数来自 Agent 配置（listen/token/CIDR/上限），
                // 采集项级参数（名单、攒批）来自 collector JSON。
                let receiver = otlp::OtlpConfig::from_item(
                    &item.item_id,
                    &item.collector,
                    &shared.otlp_listen,
                    shared.otlp_max_body_bytes,
                    &shared.otlp_token,
                    &shared.otlp_allowed_cidrs,
                );
                otlp::run(shared, receiver).await
            }
            // eBPF 采集项：三个类型共用加载/挂载/差分层，行为差异由类型决定。
            kind if EbpfItemKind::parse(kind).is_some() => {
                let ebpf_kind = EbpfItemKind::parse(kind).expect("已判断");
                ebpf::run_item(shared, item.item_id, ebpf_kind, item.collector).await
            }
            other => eprintln!(
                "gse-agent: unknown collect kind {other} for item {}",
                item.item_id
            ),
        }
    })
}

/// 拉取本 Agent 的数据面地址与主机归属。
async fn pull_addr(shared: &CollectShared, end: &End) {
    let req = DataplaneAddrRequest {
        agent_id: shared.agent_id.clone(),
    };
    let body = match serde_json::to_vec(&req) {
        Ok(b) => Bytes::from(b),
        Err(e) => {
            eprintln!("gse-agent: encode dataplane_addr failed: {e}");
            return;
        }
    };
    match end.call("dataplane_addr", body).await {
        Ok(resp) => match serde_json::from_slice::<DataplaneAddrReply>(&resp) {
            Ok(reply) if reply.ok => {
                shared.set_host_id(reply.host_id).await;
                shared.set_ingest_url(reply.ingest_url).await;
            }
            Ok(reply) => {
                shared.set_ingest_url(None).await;
                if let Some(reason) = reply.reason {
                    eprintln!("gse-agent: dataplane_addr unavailable: {reason}");
                }
            }
            Err(e) => eprintln!("gse-agent: bad dataplane_addr reply: {e}"),
        },
        Err(e) => eprintln!("gse-agent: dataplane_addr rpc failed: {e}"),
    }
}

/// 上报循环：确认出队，失败退回队头并指数退避；连续失败或 `unavailable` 触发重新拉地址。
async fn report_loop(shared: Arc<CollectShared>) {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(10))
        .build();
    let mut failures = 0u32;
    let mut backoff = 1u64;
    loop {
        let Some(env) = shared.buffer.pop_front().await else {
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        };
        let Some(base) = shared.ingest_url().await else {
            shared.buffer.push_front(env).await;
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };
        match post_ingest(&agent, &base, &env) {
            Disposition::Confirm => {
                failures = 0;
                backoff = 1;
            }
            Disposition::Repull => {
                shared.set_ingest_url(None).await;
                shared.buffer.push_front(env).await;
                failures = 0;
                backoff = 1;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Disposition::Retry => {
                shared.buffer.push_front(env).await;
                failures += 1;
                if failures > 3 {
                    shared.set_ingest_url(None).await;
                }
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(60);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn envelope() -> DataEnvelope {
        DataEnvelope {
            batch_id: "b-1".to_string(),
            data_type: "logs".to_string(),
            data_id: "item-1".to_string(),
            agent_id: "a-1".to_string(),
            host_id: "h-1".to_string(),
            sent_at_micros: 1,
            records: vec![serde_json::json!({"message": "hi"})],
        }
    }

    fn start_mock(status: &str, body: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let status = status.to_string();
        let body = body.to_string();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    fn http_agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .build()
    }

    #[test]
    fn post_ingest_confirms_on_ok_and_partial() {
        for status in ["ok", "partial"] {
            let base = start_mock("200 OK", &format!(r#"{{"status":"{status}"}}"#));
            assert_eq!(
                post_ingest(&http_agent(), &base, &envelope()),
                Disposition::Confirm
            );
        }
    }

    #[test]
    fn post_ingest_repulls_on_unavailable_and_503() {
        let base = start_mock("200 OK", r#"{"status":"error","code":"unavailable"}"#);
        assert_eq!(
            post_ingest(&http_agent(), &base, &envelope()),
            Disposition::Repull
        );
        let base = start_mock("503 Service Unavailable", r#"{"status":"error"}"#);
        assert_eq!(
            post_ingest(&http_agent(), &base, &envelope()),
            Disposition::Repull
        );
    }

    #[test]
    fn post_ingest_confirms_on_400_and_retries_on_500() {
        let base = start_mock("400 Bad Request", r#"{"status":"error"}"#);
        assert_eq!(
            post_ingest(&http_agent(), &base, &envelope()),
            Disposition::Confirm
        );
        let base = start_mock("500 Internal Server Error", r#"{"status":"error"}"#);
        assert_eq!(
            post_ingest(&http_agent(), &base, &envelope()),
            Disposition::Retry
        );
    }

    #[test]
    fn post_ingest_retries_on_connection_failure() {
        // 未监听的端口：连接被拒。
        assert_eq!(
            post_ingest(&http_agent(), "http://127.0.0.1:1", &envelope()),
            Disposition::Retry
        );
    }

    #[test]
    fn fingerprint_changes_with_collector_config() {
        let base = CollectItem {
            item_id: "i-1".to_string(),
            agent_ids: vec!["a-1".to_string()],
            name: "n".to_string(),
            kind: "metrics_host".to_string(),
            enabled: true,
            collector: serde_json::json!({"interval_secs": 15}),
            storage: serde_json::json!({"retention_days": 1}),
        };
        let mut changed = base.clone();
        changed.collector = serde_json::json!({"interval_secs": 30});
        assert_ne!(fingerprint(&base), fingerprint(&changed));

        let mut disabled = base.clone();
        disabled.enabled = false;
        assert_ne!(fingerprint(&base), fingerprint(&disabled));
    }

    #[tokio::test]
    async fn push_builds_envelope_and_enqueues() {
        let shared = CollectShared::new("a-1".to_string());
        shared.set_host_id(Some("h-1".to_string())).await;
        shared
            .push("logs", "item-7", vec![serde_json::json!({"m": 1})])
            .await;
        let env = shared.buffer.pop_front().await.expect("batch");
        assert_eq!(env.data_type, "logs");
        assert_eq!(env.data_id, "item-7");
        assert_eq!(env.agent_id, "a-1");
        assert_eq!(env.host_id, "h-1");
        assert_eq!(env.records.len(), 1);

        // 空记录不入队。
        shared.push("logs", "item-7", Vec::new()).await;
        assert!(shared.buffer.is_empty().await);
    }
}
