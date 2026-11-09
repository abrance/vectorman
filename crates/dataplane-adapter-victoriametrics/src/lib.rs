//! VictoriaMetrics `TimeSeriesStore` 适配器（集群版 / 单机版通用）。
//!
//! # 写入模型：有界队列 + 批量 flush
//!
//! 数据面是「一个点一次 [`TimeSeriesStore::write`]」，VM 的写入接口是
//! Prometheus 导入格式的 HTTP 批量请求。逐点发 HTTP 会把上一轮刚修好的回放
//! （4h52m → 48.6s）打回原形，所以这里不是直写：
//!
//! - `write()` 把点放进有界队列就返回。**`Ok` 表示「已入队」，不表示「已落库」。**
//! - 后台任务按 `batch_size` 或 `flush_interval` 凑批，一次 HTTP 提交一批。
//! - 队列满时 `write()` 立刻返回 `unavailable`（背压），调用方（接入路径）应回 5xx
//!   让 Agent 缓冲重试，而不是无限堆内存。
//! - 提交失败按 `max_retries` 指数退避重试；仍失败则丢弃该批并计入
//!   `background_errors_total` / `degraded`（见 [`VictoriaMetricsStore::storage_stats`]），
//!   也就是**丢数据有明确计数，不静默**。4xx（除 429）视为数据本身非法，不重试。
//! - 进程退出前调用 [`VictoriaMetricsStore::flush`]，否则最多丢 `flush_interval`
//!   之内的点。
//!
//! # 时间精度
//!
//! 数据面时间戳是 Unix **微秒**，VM 只存**毫秒**。写入时 `µs / 1000` 截断，
//! 同一毫秒内的多个点会互相覆盖（后写胜出）；查询返回的毫秒再 `× 1000` 还原成微秒，
//! 因此跨后端比对的期望是「毫秒对齐后一致」，不是逐微秒一致。
//!
//! # 路径与租户
//!
//! 集群版 vminsert / vmselect 是不同服务，必须分别配置：
//! `/insert/{tenant}/prometheus/api/v1/...`、`/select/{tenant}/prometheus/api/v1/...`。
//! dataserver 没有多租户概念，`tenant` 固定 `"0"`（单机版没有这段前缀，用
//! `single_node = true` 关掉）。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dataplane_core::{DataplaneError, ErrorCode};
use dataplane_ts::{
    PromResult, PromResultType, PromSeries, TimeSeriesStore, TsDeleteReport, TsMatcher,
    TsMatcherOp, TsPoint, TsSeriesSelection, TsStorageStats,
};
use tokio::sync::{mpsc, oneshot};

const DEFAULT_BATCH_SIZE: usize = 5_000;
const DEFAULT_FLUSH_INTERVAL_MS: u64 = 200;

/// VM 连接与批量参数。
#[derive(Debug, Clone)]
pub struct VmConfig {
    /// vminsert 基址，如 `http://vminsert:8480`（单机版与 select 相同）。
    pub insert_url: String,
    /// vmselect 基址，如 `http://vmselect:8481`。
    pub select_url: String,
    /// 租户号；dataserver 单租户，固定 `"0"`。
    pub tenant: String,
    /// 集群版为 `false`（带 `/insert|/select` 前缀），单机版为 `true`。
    pub single_node: bool,
    /// 单批点数上限。
    pub batch_size: usize,
    /// 攒批的最长等待。
    pub flush_interval: Duration,
    /// 队列容量（点数）；满即背压。
    pub queue_capacity: usize,
    /// 单次 HTTP 超时。
    pub timeout: Duration,
    /// 失败重试次数（首次之外的额外尝试）。
    pub max_retries: usize,
    /// 保留窗口（天），仅用于 `storage_stats` 上报；VM 侧保留由 vmstorage 决定。
    pub retention_days: u32,
    /// 删除鉴权 key（VM v1.155 只有 `-deleteAuthKey`，没有 `-deleteData`）。
    pub delete_auth_key: Option<String>,
    /// 是否接受「按时间窗删除」退化成**整序列删除**。
    ///
    /// VM 的删除接口不支持 `start`/`end`（实测 400：`delete API does not support
    /// specific time ranges ... the series can only be deleted completely`），
    /// 因此数据面按时间窗的删除无法精确映射。`false`（缺省）时明确拒绝；
    /// `true` 时整序列删掉并打 warn —— 只适合「采集项已删除，整条历史都不要了」的场景。
    pub whole_series_delete: bool,
}

impl Default for VmConfig {
    fn default() -> Self {
        Self {
            insert_url: "http://127.0.0.1:8428".to_string(),
            select_url: "http://127.0.0.1:8428".to_string(),
            tenant: "0".to_string(),
            single_node: true,
            batch_size: DEFAULT_BATCH_SIZE,
            flush_interval: Duration::from_millis(DEFAULT_FLUSH_INTERVAL_MS),
            queue_capacity: 200_000,
            timeout: Duration::from_secs(10),
            max_retries: 3,
            retention_days: 7,
            delete_auth_key: None,
            whole_series_delete: false,
        }
    }
}

#[derive(Default)]
struct FlushStats {
    enqueued_points: AtomicU64,
    flushed_points: AtomicU64,
    failed_batches: AtomicU64,
    dropped_points: AtomicU64,
    degraded: AtomicBool,
    last_error: Mutex<Option<String>>,
}

impl FlushStats {
    fn record_error(&self, points: usize, message: String) {
        self.failed_batches.fetch_add(1, Ordering::Relaxed);
        self.dropped_points
            .fetch_add(points as u64, Ordering::Relaxed);
        self.degraded.store(true, Ordering::Relaxed);
        if let Ok(mut slot) = self.last_error.lock() {
            *slot = Some(message);
        }
    }
}

enum Cmd {
    Point(TsPoint),
    Flush(oneshot::Sender<()>),
}

/// VM 远程时序存储。
pub struct VictoriaMetricsStore {
    cfg: VmConfig,
    select_agent: ureq::Agent,
    tx: mpsc::Sender<Cmd>,
    stats: Arc<FlushStats>,
}

impl std::fmt::Debug for VictoriaMetricsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VictoriaMetricsStore")
            .field("insert_url", &self.cfg.insert_url)
            .field("select_url", &self.cfg.select_url)
            .field("tenant", &self.cfg.tenant)
            .field("batch_size", &self.cfg.batch_size)
            .finish_non_exhaustive()
    }
}

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

fn micros_to_millis(ts_micros: i64) -> i64 {
    ts_micros.div_euclid(1_000)
}

/// VM / Prometheus API 的时间是秒（浮点）；还原成微秒。
fn seconds_to_micros(secs: f64) -> i64 {
    (secs * 1_000_000.0).round() as i64
}

fn valid_metric_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == ':' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

fn valid_label_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn escape_label_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// 一个点编码成 Prometheus 导入格式的一行。
fn encode_point(point: &TsPoint, out: &mut String) -> Result<(), DataplaneError> {
    if !valid_metric_name(&point.measurement) {
        return Err(DataplaneError::new(
            ErrorCode::InvalidArgument,
            format!(
                "invalid metric name for victoriametrics: {:?}",
                point.measurement
            ),
        ));
    }
    out.push_str(&point.measurement);
    if !point.tags.is_empty() {
        out.push('{');
        for (i, (k, v)) in point.tags.iter().enumerate() {
            if !valid_label_name(k) {
                return Err(DataplaneError::new(
                    ErrorCode::InvalidArgument,
                    format!("invalid label name for victoriametrics: {k:?}"),
                ));
            }
            if i > 0 {
                out.push(',');
            }
            out.push_str(k);
            out.push_str("=\"");
            out.push_str(&escape_label_value(v));
            out.push('"');
        }
        out.push('}');
    }
    out.push(' ');
    out.push_str(&point.field_value.to_string());
    out.push(' ');
    out.push_str(&micros_to_millis(point.timestamp).to_string());
    out.push('\n');
    Ok(())
}

fn encode_batch(points: &[TsPoint]) -> Result<String, DataplaneError> {
    let mut body = String::with_capacity(points.len() * 64);
    for p in points {
        encode_point(p, &mut body)?;
    }
    Ok(body)
}

/// 表单参数编码（PromQL 里有 `{ } " =` 等字符，必须编码）。
fn form_encode(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        for c in k.chars() {
            push_encoded(c, &mut out);
        }
        out.push('=');
        for c in v.chars() {
            push_encoded(c, &mut out);
        }
    }
    out
}

fn push_encoded(c: char, out: &mut String) {
    if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | ':') {
        out.push(c);
    } else {
        let mut buf = [0u8; 4];
        for b in c.encode_utf8(&mut buf).as_bytes() {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
}

/// ureq 错误 → 数据面错误 + 是否值得重试。
fn map_http_error(op: &str, e: ureq::Error) -> (DataplaneError, bool) {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let retryable = code >= 500 || code == 429;
            let code_err = if retryable {
                ErrorCode::Unavailable
            } else {
                ErrorCode::QueryFailed
            };
            (
                DataplaneError::new(
                    code_err,
                    format!("victoriametrics {op}: HTTP {code}: {body}"),
                ),
                retryable,
            )
        }
        ureq::Error::Transport(t) => (
            DataplaneError::new(ErrorCode::Unavailable, format!("victoriametrics {op}: {t}")),
            true,
        ),
    }
}

fn http_status(e: &ureq::Error) -> Option<u16> {
    match e {
        ureq::Error::Status(code, _) => Some(*code),
        ureq::Error::Transport(_) => None,
    }
}

impl VictoriaMetricsStore {
    /// 建 store 并启动后台攒批任务。需要 tokio 运行时。
    pub fn new(cfg: VmConfig) -> Result<Self, DataplaneError> {
        if cfg.batch_size == 0 || cfg.queue_capacity == 0 {
            return Err(DataplaneError::config_invalid(
                "victoriametrics batch_size and queue_capacity must be > 0",
            ));
        }
        let (tx, rx) = mpsc::channel(cfg.queue_capacity.max(cfg.batch_size));
        let stats = Arc::new(FlushStats::default());
        let insert_agent = ureq::AgentBuilder::new().timeout(cfg.timeout).build();
        let select_agent = ureq::AgentBuilder::new().timeout(cfg.timeout).build();
        tokio::spawn(flusher_loop(
            cfg.clone(),
            insert_agent.clone(),
            rx,
            Arc::clone(&stats),
        ));
        Ok(Self {
            cfg,
            select_agent,
            tx,
            stats,
        })
    }

    /// 删除接口在 vmselect 的 `/delete/{tenant}/...` 下（不是 `/select/...`）。
    fn delete_path(&self) -> String {
        let base = self.cfg.select_url.trim_end_matches('/');
        let key = self.cfg.delete_auth_key.as_deref().unwrap_or_default();
        let sep = if key.is_empty() { "" } else { "?" };
        let auth = if key.is_empty() {
            String::new()
        } else {
            form_encode(&[("authKey", key)])
        };
        if self.cfg.single_node {
            format!("{base}/prometheus/api/v1/admin/tsdb/delete_series{sep}{auth}")
        } else {
            format!(
                "{base}/delete/{}/prometheus/api/v1/admin/tsdb/delete_series{sep}{auth}",
                self.cfg.tenant
            )
        }
    }

    fn select_path(&self, api: &str) -> String {
        let base = self.cfg.select_url.trim_end_matches('/');
        if self.cfg.single_node {
            format!("{base}/prometheus/api/v1/{api}")
        } else {
            format!("{base}/select/{}/prometheus/api/v1/{api}", self.cfg.tenant)
        }
    }

    /// 把队列里已入队的点全部提交。返回时队列已排空（或提交失败）。
    pub async fn flush(&self) -> Result<(), DataplaneError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        match tokio::time::timeout(self.cfg.timeout, self.tx.send(Cmd::Flush(ack_tx))).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                return Err(DataplaneError::new(
                    ErrorCode::Unavailable,
                    "victoriametrics flusher is gone",
                ))
            }
            Err(_) => {
                return Err(DataplaneError::new(
                    ErrorCode::Unavailable,
                    "victoriametrics flush request timed out",
                ))
            }
        }
        // 排空一个满队列是「队列容量 / 批大小」次 HTTP，每次最坏用到 `timeout`：
        // 用单次请求的超时去等整队列排空，会在回放/迁移这种大批量场景假超时。
        let drain_bounds = (self.cfg.queue_capacity / self.cfg.batch_size).max(1) as u32 + 1;
        let ack_timeout = self.cfg.timeout.saturating_mul(drain_bounds);
        if tokio::time::timeout(ack_timeout, ack_rx).await.is_err() {
            return Err(DataplaneError::new(
                ErrorCode::Unavailable,
                format!(
                    "victoriametrics flush ack timed out after {}s",
                    ack_timeout.as_secs()
                ),
            ));
        }
        if self.stats.degraded.load(Ordering::Relaxed) {
            let msg = self
                .stats
                .last_error
                .lock()
                .ok()
                .and_then(|slot| slot.clone())
                .unwrap_or_else(|| "unknown flush failure".to_string());
            return Err(DataplaneError::new(ErrorCode::Unavailable, msg));
        }
        Ok(())
    }

    /// 后台提交用的同步 HTTP 提交（跑在 `spawn_blocking` 里）。
    /// POST 表单并解析 JSON（`status=error` 转成错误）。
    #[allow(clippy::result_large_err)]
    async fn post_form_json(
        &self,
        url: String,
        body: String,
        op: &'static str,
    ) -> Result<serde_json::Value, DataplaneError> {
        let agent = self.select_agent.clone();
        let text = tokio::task::spawn_blocking(move || {
            agent
                .post(&url)
                .set("Content-Type", "application/x-www-form-urlencoded")
                .send_string(&body)
                .map_err(|e| map_http_error(op, e).0)?
                .into_string()
                .map_err(|e| DataplaneError::new(ErrorCode::QueryFailed, format!("read body: {e}")))
        })
        .await
        .map_err(|e| DataplaneError::new(ErrorCode::QueryFailed, format!("{op} task: {e}")))??;
        let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            DataplaneError::new(
                ErrorCode::QueryFailed,
                format!("victoriametrics: invalid JSON from {op}: {e}"),
            )
        })?;
        if json.get("status").and_then(|s| s.as_str()) == Some("error") {
            let kind = json
                .get("errorType")
                .and_then(|s| s.as_str())
                .unwrap_or("error");
            let msg = json
                .get("error")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown");
            return Err(DataplaneError::new(
                ErrorCode::QueryFailed,
                format!("victoriametrics {op}: {kind}: {msg}"),
            ));
        }
        Ok(json)
    }

    /// POST 表单，不关心响应体。
    // `ureq::Error` 是 272 字节的大 enum，盒一层只为消一条 clippy 警告不值当。
    #[allow(clippy::result_large_err)]
    async fn post_form_empty(
        &self,
        url: String,
        body: String,
        op: &'static str,
    ) -> Result<(), DataplaneError> {
        let agent = self.select_agent.clone();
        let result = tokio::task::spawn_blocking(move || {
            agent
                .post(&url)
                .set("Content-Type", "application/x-www-form-urlencoded")
                .send_string(&body)
                .map(|_| ())
        })
        .await
        .map_err(|e| DataplaneError::new(ErrorCode::QueryFailed, format!("{op} task: {e}")))?;
        result.map_err(|e| {
            let code = http_status(&e);
            let (mut mapped, _) = map_http_error(op, e);
            if matches!(
                code,
                Some(400) | Some(401) | Some(403) | Some(404) | Some(501)
            ) {
                mapped.message = format!(
                    "{}（检查 DATASERVER_VM_DELETE_AUTH_KEY 是否等于 vmselect 的 -deleteAuthKey；\
                     VM 删除只支持整序列，不支持 start/end。见 docs/external-storage.md）",
                    mapped.message
                );
            }
            mapped
        })
    }
}

async fn flusher_loop(
    cfg: VmConfig,
    agent: ureq::Agent,
    mut rx: mpsc::Receiver<Cmd>,
    stats: Arc<FlushStats>,
) {
    let base = cfg.insert_url.trim_end_matches('/');
    let url = if cfg.single_node {
        format!("{base}/api/v1/import/prometheus")
    } else {
        format!(
            "{base}/insert/{}/prometheus/api/v1/import/prometheus",
            cfg.tenant
        )
    };
    let mut buf: Vec<TsPoint> = Vec::with_capacity(cfg.batch_size);
    loop {
        let received = if buf.is_empty() {
            rx.recv().await
        } else {
            match tokio::time::timeout(cfg.flush_interval, rx.recv()).await {
                Ok(cmd) => cmd,
                Err(_) => {
                    submit_batch(&agent, &url, &mut buf, &cfg, &stats).await;
                    continue;
                }
            }
        };
        match received {
            Some(Cmd::Point(p)) => {
                buf.push(p);
                if buf.len() >= cfg.batch_size {
                    submit_batch(&agent, &url, &mut buf, &cfg, &stats).await;
                }
            }
            Some(Cmd::Flush(ack)) => {
                submit_batch(&agent, &url, &mut buf, &cfg, &stats).await;
                let _ = ack.send(());
            }
            None => {
                // 所有 store 句柄都掉了：把剩下的点送出去再退出。
                submit_batch(&agent, &url, &mut buf, &cfg, &stats).await;
                break;
            }
        }
    }
}

async fn submit_batch(
    agent: &ureq::Agent,
    url: &str,
    buf: &mut Vec<TsPoint>,
    cfg: &VmConfig,
    stats: &FlushStats,
) {
    if buf.is_empty() {
        return;
    }
    let points = std::mem::take(buf);
    let body = match encode_batch(&points) {
        Ok(body) => body,
        Err(e) => {
            // 单点命名非法会在 `write()` 就拒掉；走到这里说明标签名非法，整批丢并计数。
            stats.record_error(
                points.len(),
                format!("encode: {}: {}", e.code.as_str(), e.message),
            );
            buf.reserve(cfg.batch_size);
            return;
        }
    };
    let mut attempt = 0usize;
    loop {
        let agent = agent.clone();
        let url = url.to_string();
        let body = body.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            agent
                .post(&url)
                .set("Content-Type", "text/plain")
                .send_string(&body)
                .map(|_| ())
                .map_err(|e| map_http_error("import", e))
        })
        .await
        .unwrap_or_else(|join| {
            Err((
                DataplaneError::new(
                    ErrorCode::QueryFailed,
                    format!("flush task panicked: {join}"),
                ),
                false,
            ))
        });
        match outcome {
            Ok(()) => {
                stats
                    .flushed_points
                    .fetch_add(points.len() as u64, Ordering::Relaxed);
                buf.reserve(cfg.batch_size);
                return;
            }
            Err((mapped, retryable)) => {
                if !retryable || attempt >= cfg.max_retries {
                    eprintln!(
                        "victoriametrics: dropped {} points after {} attempt(s): {}",
                        points.len(),
                        attempt + 1,
                        mapped.message
                    );
                    stats.record_error(points.len(), mapped.message);
                    buf.reserve(cfg.batch_size);
                    return;
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(50 * (1 << attempt.min(5)))).await;
            }
        }
    }
}

fn json_number(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.parse::<f64>().ok())
}

fn parse_prom_result(json: &serde_json::Value) -> Result<PromResult, DataplaneError> {
    let data = json.get("data").ok_or_else(|| {
        DataplaneError::new(ErrorCode::QueryFailed, "victoriametrics: missing data")
    })?;
    let result_type = data
        .get("resultType")
        .and_then(|v| v.as_str())
        .unwrap_or("vector");
    match result_type {
        "vector" => {
            let mut series = Vec::new();
            for item in data
                .get("result")
                .and_then(|v| v.as_array())
                .map(|v| v.as_slice())
                .unwrap_or_default()
            {
                let metric = parse_metric(item.get("metric"));
                let value = item.get("value").and_then(parse_sample);
                series.push(PromSeries {
                    metric,
                    value,
                    values: None,
                });
            }
            Ok(PromResult {
                result_type: PromResultType::Vector,
                result: series,
            })
        }
        "matrix" => {
            let mut series = Vec::new();
            for item in data
                .get("result")
                .and_then(|v| v.as_array())
                .map(|v| v.as_slice())
                .unwrap_or_default()
            {
                let metric = parse_metric(item.get("metric"));
                let values = item
                    .get("values")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(parse_sample).collect())
                    .unwrap_or_default();
                series.push(PromSeries {
                    metric,
                    value: None,
                    values: Some(values),
                });
            }
            Ok(PromResult {
                result_type: PromResultType::Matrix,
                result: series,
            })
        }
        // scalar/string 按 vector 一条返回，与 tsink 侧的投影口径一致。
        "scalar" | "string" => {
            let sample = data.get("result").and_then(parse_sample);
            Ok(PromResult {
                result_type: PromResultType::Vector,
                result: vec![PromSeries {
                    metric: BTreeMap::new(),
                    value: sample,
                    values: None,
                }],
            })
        }
        other => Err(DataplaneError::new(
            ErrorCode::QueryFailed,
            format!("victoriametrics: unsupported resultType {other}"),
        )),
    }
}

fn parse_metric(metric: Option<&serde_json::Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(obj) = metric.and_then(|m| m.as_object()) {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                out.insert(k.clone(), s.to_string());
            }
        }
    }
    out
}

/// `[秒, "值"]` → `(微秒, 值)`。
fn parse_sample(sample: &serde_json::Value) -> Option<(i64, f64)> {
    let arr = sample.as_array()?;
    let ts = json_number(arr.first()?)?;
    let value = json_number(arr.get(1)?)?;
    Some((seconds_to_micros(ts), value))
}

fn matcher_to_prom(m: &TsMatcher) -> String {
    let op = match m.op {
        TsMatcherOp::Equal => "=",
        TsMatcherOp::NotEqual => "!=",
        TsMatcherOp::RegexMatch => "=~",
        TsMatcherOp::RegexNoMatch => "!~",
    };
    format!("{}{}\"{}\"", m.name, op, escape_label_value(&m.value))
}

/// 把选择条件拼成 PromQL 的 `match[]`。
fn selection_to_match(selection: &TsSeriesSelection) -> String {
    let mut inner = Vec::new();
    if let Some(measurement) = &selection.measurement {
        inner.push(format!("__name__=\"{}\"", escape_label_value(measurement)));
    }
    for m in &selection.matchers {
        inner.push(matcher_to_prom(m));
    }
    if inner.is_empty() {
        return "{__name__=~\".+\"}".to_string();
    }
    format!("{{{}}}", inner.join(","))
}

#[async_trait]
impl TimeSeriesStore for VictoriaMetricsStore {
    async fn write(&self, point: TsPoint) -> Result<(), DataplaneError> {
        // 命名非法当场拒绝：入队后再失败只能整批丢，错误也定位不到点。
        if !valid_metric_name(&point.measurement) {
            return Err(DataplaneError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "invalid metric name for victoriametrics: {:?}",
                    point.measurement
                ),
            ));
        }
        match self.tx.try_send(Cmd::Point(point)) {
            Ok(()) => {
                self.stats.enqueued_points.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => Err(DataplaneError::new(
                ErrorCode::Unavailable,
                format!(
                    "victoriametrics write queue full ({} points, backpressure)",
                    self.cfg.queue_capacity
                ),
            )),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(DataplaneError::new(
                ErrorCode::Unavailable,
                "victoriametrics flusher is gone",
            )),
        }
    }

    async fn query_instant(
        &self,
        expr: &str,
        eval_time: Option<i64>,
    ) -> Result<PromResult, DataplaneError> {
        let mut pairs = vec![("query", expr)];
        let time;
        if let Some(ts) = eval_time {
            time = format!("{:.3}", ts as f64 / 1_000_000.0);
            pairs.push(("time", time.as_str()));
        }
        let body = form_encode(&pairs);
        let json = self
            .post_form_json(self.select_path("query"), body, "instant query")
            .await?;
        parse_prom_result(&json)
    }

    async fn query_range(
        &self,
        expr: &str,
        start: i64,
        end: i64,
        step: i64,
    ) -> Result<PromResult, DataplaneError> {
        let start_s = format!("{:.3}", start as f64 / 1_000_000.0);
        let end_s = format!("{:.3}", end as f64 / 1_000_000.0);
        let step_s = step.to_string();
        let body = form_encode(&[
            ("query", expr),
            ("start", &start_s),
            ("end", &end_s),
            ("step", &step_s),
        ]);
        let json = self
            .post_form_json(self.select_path("query_range"), body, "range query")
            .await?;
        parse_prom_result(&json)
    }

    async fn delete_series(
        &self,
        selection: TsSeriesSelection,
    ) -> Result<TsDeleteReport, DataplaneError> {
        if selection.from_ts >= selection.to_ts {
            return Err(DataplaneError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "from_ts ({}) must be earlier than to_ts ({})",
                    selection.from_ts, selection.to_ts
                ),
            ));
        }
        if self
            .cfg
            .delete_auth_key
            .as_deref()
            .unwrap_or_default()
            .is_empty()
        {
            return Err(DataplaneError::new(
                ErrorCode::ConfigInvalid,
                "victoriametrics delete_series requires delete_auth_key \
                 (DATASERVER_VM_DELETE_AUTH_KEY, VM 的 -deleteAuthKey)",
            ));
        }
        let matched = selection_to_match(&selection);
        let start_s = format!("{:.3}", selection.from_ts as f64 / 1_000_000.0);
        let end_s = format!("{:.3}", selection.to_ts as f64 / 1_000_000.0);
        // 先数命中序列：VM 的删除接口不回数量，`matched_series` 只能自己数。
        let count_body =
            form_encode(&[("match[]", &matched), ("start", &start_s), ("end", &end_s)]);
        let json = self
            .post_form_json(self.select_path("series"), count_body, "series lookup")
            .await?;
        let matched_series = json
            .get("data")
            .and_then(|d| d.as_array())
            .map(|a| a.len() as u64)
            .unwrap_or(0);

        // 整序列删除必须是「有 matcher 的选择」：没有 matcher 就等于删掉整个指标的
        // 全部序列（APM 保留期就是这种入参），一旦开了 whole_series_delete 会把共享
        // 聚合指标的历史全删光。
        if self.cfg.whole_series_delete && selection.matchers.is_empty() {
            return Err(DataplaneError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "victoriametrics 拒绝整序列删除无 label matcher 的选择（会删掉 {matched} 的全部序列）；\
                     请带上 item_id 等 matcher"
                ),
            ));
        }
        if !self.cfg.whole_series_delete {
            return Err(DataplaneError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "victoriametrics 只能整序列删除（VM 删除接口不接受 start/end）；\
                     命中 {matched_series} 条序列，本次按时间窗 [{}, {}) 的删除被拒绝。\
                     采集项整条历史都要删时设 whole_series_delete=true",
                    selection.from_ts, selection.to_ts
                ),
            ));
        }

        // VM 只吃 match[]：删的是整条序列。调用方必须知道「窗口外的点也会一起没」。
        eprintln!(
            "victoriametrics: whole-series delete for {matched}; requested window [{}, {}) is ignored by VM",
            selection.from_ts, selection.to_ts
        );
        let delete_body = form_encode(&[("match[]", &matched)]);
        self.post_form_empty(self.delete_path(), delete_body, "delete_series")
            .await?;
        Ok(TsDeleteReport {
            matched_series,
            tombstones_applied: matched_series,
        })
    }

    async fn storage_stats(&self) -> Result<TsStorageStats, DataplaneError> {
        let agent = self.select_agent.clone();
        let url = self.select_path("status/tsdb");
        let json = tokio::task::spawn_blocking(move || {
            let text = agent
                .get(&url)
                .call()
                .map_err(|e| map_http_error("status/tsdb", e).0)?
                .into_string()
                .map_err(|e| {
                    DataplaneError::new(ErrorCode::QueryFailed, format!("read body: {e}"))
                })?;
            serde_json::from_str::<serde_json::Value>(&text).map_err(|e| {
                DataplaneError::new(
                    ErrorCode::QueryFailed,
                    format!("victoriametrics: invalid JSON from status/tsdb: {e}"),
                )
            })
        })
        .await
        .map_err(|e| DataplaneError::new(ErrorCode::QueryFailed, format!("stats task: {e}")))??;
        let data = json.get("data").cloned().unwrap_or_default();
        let series_count = data
            .get("totalSeries")
            .and_then(json_number)
            .map(|v| v as u64)
            .or_else(|| {
                data.get("seriesCountByMetricName")
                    .and_then(|a| a.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|e| e.get("value").and_then(json_number))
                            .sum::<f64>() as u64
                    })
            })
            .unwrap_or(0);
        Ok(TsStorageStats {
            series_count,
            memory_used_bytes: 0,
            memory_budget_bytes: 0,
            wal_size_bytes: 0,
            retention_days: self.cfg.retention_days,
            retention_enforced: self.cfg.retention_days > 0,
            expired_segments_total: 0,
            future_skew_points_total: 0,
            background_errors_total: self.stats.failed_batches.load(Ordering::Relaxed),
            degraded: self.stats.degraded.load(Ordering::Relaxed),
            last_background_error: self
                .stats
                .last_error
                .lock()
                .ok()
                .and_then(|slot| slot.clone()),
            sampled_at_ts: now_micros(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(name: &str, ts: i64, v: f64) -> TsPoint {
        let mut tags = BTreeMap::new();
        tags.insert("item_id".to_string(), "item-1".to_string());
        TsPoint {
            measurement: name.to_string(),
            tags,
            field_name: "value".to_string(),
            field_value: v,
            timestamp: ts,
        }
    }

    #[test]
    fn encode_line_is_prometheus_import_format() {
        let mut out = String::new();
        encode_point(&point("cpu_usage", 1_700_000_000_123_456, 1.5), &mut out).unwrap();
        assert_eq!(out, "cpu_usage{item_id=\"item-1\"} 1.5 1700000000123\n");
    }

    #[test]
    fn encode_escapes_label_values() {
        let mut p = point("m", 1_000, 1.0);
        p.tags.insert("path".to_string(), "a\"b\\c\nd".to_string());
        let mut out = String::new();
        encode_point(&p, &mut out).unwrap();
        assert!(out.contains("path=\"a\\\"b\\\\c\\nd\""), "{out}");
    }

    #[test]
    fn micros_to_millis_truncates_and_collides() {
        assert_eq!(micros_to_millis(1_700_000_000_123_456), 1_700_000_000_123);
        assert_eq!(micros_to_millis(1_700_000_000_123_999), 1_700_000_000_123);
    }

    #[test]
    fn seconds_roundtrip_to_micros() {
        assert_eq!(seconds_to_micros(1_700_000_000.123), 1_700_000_000_123_000);
    }

    #[test]
    fn invalid_names_rejected() {
        assert!(!valid_metric_name("1abc"));
        assert!(!valid_metric_name("a b"));
        assert!(valid_metric_name("a:b_c1"));
        assert!(!valid_label_name("a-b"));
        assert!(valid_label_name("item_id"));
    }

    #[test]
    fn form_encode_escapes_promql() {
        let s = form_encode(&[("query", "up{job=\"a b\"}")]);
        assert_eq!(s, "query=up%7Bjob%3D%22a%20b%22%7D");
    }

    #[test]
    fn selection_match_builds_promql() {
        let sel = TsSeriesSelection {
            measurement: Some("m".to_string()),
            matchers: vec![TsMatcher {
                name: "item_id".to_string(),
                op: TsMatcherOp::Equal,
                value: "x".to_string(),
            }],
            from_ts: 0,
            to_ts: 1,
        };
        assert_eq!(selection_to_match(&sel), "{__name__=\"m\",item_id=\"x\"}");
    }

    #[test]
    fn write_rejects_bad_name_before_enqueue() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let store = VictoriaMetricsStore::new(VmConfig {
                insert_url: "http://127.0.0.1:1".to_string(),
                select_url: "http://127.0.0.1:1".to_string(),
                queue_capacity: 4,
                batch_size: 2,
                ..VmConfig::default()
            })
            .unwrap();
            let err = store.write(point("bad name", 1, 1.0)).await.unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument);
        });
    }

    /// 假 VM：只收 `/api/v1/import/prometheus`，请求体进共享列表。
    async fn fake_vm() -> (String, Arc<Mutex<Vec<String>>>) {
        use axum::extract::State;
        use axum::routing::post;

        type Bodies = Arc<Mutex<Vec<String>>>;
        async fn capture(State(bodies): State<Bodies>, body: String) -> axum::http::StatusCode {
            bodies.lock().unwrap().push(body);
            axum::http::StatusCode::NO_CONTENT
        }

        let bodies: Bodies = Arc::new(Mutex::new(Vec::new()));
        let app = axum::Router::new()
            .route("/api/v1/import/prometheus", post(capture))
            .with_state(Arc::clone(&bodies));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), bodies)
    }

    fn test_store(base: &str, batch_size: usize, flush_ms: u64) -> VictoriaMetricsStore {
        VictoriaMetricsStore::new(VmConfig {
            insert_url: base.to_string(),
            select_url: base.to_string(),
            single_node: true,
            batch_size,
            flush_interval: Duration::from_millis(flush_ms),
            queue_capacity: 10_000,
            ..VmConfig::default()
        })
        .unwrap()
    }

    #[tokio::test]
    async fn explicit_flush_posts_remaining_points_in_one_request() {
        let (base, bodies) = fake_vm().await;
        let store = test_store(&base, 5_000, 5_000);
        for i in 0..1_000 {
            // 不同毫秒，避免毫秒碰撞掩盖事实。
            store
                .write(point(
                    "probe_metric",
                    1_700_000_000_000_000 + i * 1_000,
                    i as f64,
                ))
                .await
                .unwrap();
        }
        store.flush().await.unwrap();
        let captured = bodies.lock().unwrap().clone();
        assert_eq!(captured.len(), 1, "攒批应合并成一次 HTTP：{captured:?}");
        assert_eq!(captured[0].lines().count(), 1_000);
        assert!(captured[0].starts_with("probe_metric{item_id=\"item-1\"} 0 1700000000000\n"));
    }

    #[tokio::test]
    async fn batch_size_triggers_flush_without_waiting() {
        let (base, bodies) = fake_vm().await;
        let store = test_store(&base, 2, 60_000);
        for i in 0..2 {
            store
                .write(point("m", 1_000_000 + i * 1_000, 1.0))
                .await
                .unwrap();
        }
        // 不等 flush_interval：整批满就该走。
        for _ in 0..100 {
            if !bodies.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn flush_interval_covers_partial_batch() {
        let (base, bodies) = fake_vm().await;
        let store = test_store(&base, 5_000, 50);
        store.write(point("m", 1_000_000, 1.0)).await.unwrap();
        for _ in 0..100 {
            if !bodies.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let captured = bodies.lock().unwrap().clone();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].lines().count(), 1);
    }

    fn sel(from: i64, to: i64) -> TsSeriesSelection {
        TsSeriesSelection {
            measurement: Some("m".to_string()),
            matchers: vec![],
            from_ts: from,
            to_ts: to,
        }
    }

    #[tokio::test]
    async fn delete_path_uses_delete_prefix_and_auth_key_as_query_arg() {
        let cluster = VictoriaMetricsStore::new(VmConfig {
            insert_url: "http://127.0.0.1:1".to_string(),
            select_url: "http://vmselect:8481/".to_string(),
            single_node: false,
            delete_auth_key: Some("k e y".to_string()),
            ..VmConfig::default()
        })
        .unwrap();
        assert_eq!(
            cluster.delete_path(),
            "http://vmselect:8481/delete/0/prometheus/api/v1/admin/tsdb/delete_series?authKey=k%20e%20y"
        );
        let single = VictoriaMetricsStore::new(VmConfig {
            insert_url: "http://127.0.0.1:1".to_string(),
            select_url: "http://vm:8428".to_string(),
            single_node: true,
            delete_auth_key: Some("k".to_string()),
            ..VmConfig::default()
        })
        .unwrap();
        assert_eq!(
            single.delete_path(),
            "http://vm:8428/prometheus/api/v1/admin/tsdb/delete_series?authKey=k"
        );
    }

    #[tokio::test]
    async fn delete_without_auth_key_fails_before_any_http() {
        let store = test_store("http://127.0.0.1:1", 10, 10);
        let err = store.delete_series(sel(1, 2)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid, "{}", err.message);
        assert!(err.message.contains("delete_auth_key"), "{}", err.message);
    }

    #[tokio::test]
    async fn ranged_delete_is_rejected_when_whole_series_disabled() {
        let (base, _bodies) = fake_vm_with_series().await;
        let store = VictoriaMetricsStore::new(VmConfig {
            insert_url: base.clone(),
            select_url: base,
            single_node: true,
            delete_auth_key: Some("k".to_string()),
            whole_series_delete: false,
            ..VmConfig::default()
        })
        .unwrap();
        let err = store.delete_series(sel(1, 2)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{}", err.message);
        assert!(err.message.contains("整序列删除"), "{}", err.message);
    }

    #[tokio::test]
    async fn whole_series_delete_refuses_selector_without_matchers() {
        let (base, _seen) = fake_vm_with_series().await;
        let store = VictoriaMetricsStore::new(VmConfig {
            insert_url: base.clone(),
            select_url: base,
            single_node: true,
            delete_auth_key: Some("k".to_string()),
            whole_series_delete: true,
            ..VmConfig::default()
        })
        .unwrap();
        let err = store.delete_series(sel(0, 1)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{}", err.message);
        assert!(err.message.contains("matcher"), "{}", err.message);
    }

    #[tokio::test]
    async fn whole_series_delete_posts_only_match() {
        let (base, seen) = fake_vm_with_series().await;
        let store = VictoriaMetricsStore::new(VmConfig {
            insert_url: base.clone(),
            select_url: base,
            single_node: true,
            delete_auth_key: Some("k".to_string()),
            whole_series_delete: true,
            ..VmConfig::default()
        })
        .unwrap();
        let mut selection = sel(1, 2);
        selection.matchers = vec![TsMatcher {
            name: "item_id".to_string(),
            op: TsMatcherOp::Equal,
            value: "x".to_string(),
        }];
        let report = store.delete_series(selection).await.expect("delete");
        assert_eq!(report.matched_series, 1);
        let deletes = seen.lock().unwrap().clone();
        assert_eq!(deletes.len(), 1, "{deletes:?}");
        assert!(deletes[0].contains("match%5B%5D="), "{}", deletes[0]);
        assert!(!deletes[0].contains("start="), "{}", deletes[0]);
    }

    /// 假 VM：`/series` 返回一条序列，`/delete...` 回 204 并记录请求体。
    async fn fake_vm_with_series() -> (String, Arc<Mutex<Vec<String>>>) {
        use axum::extract::State;
        use axum::routing::post;

        type Bodies = Arc<Mutex<Vec<String>>>;
        async fn series() -> axum::response::Response {
            use axum::response::IntoResponse;
            axum::Json(serde_json::json!({
                "status": "success",
                "data": [{"__name__": "m"}]
            }))
            .into_response()
        }
        async fn delete(State(bodies): State<Bodies>, body: String) -> axum::http::StatusCode {
            bodies.lock().unwrap().push(body);
            axum::http::StatusCode::NO_CONTENT
        }

        let bodies: Bodies = Arc::new(Mutex::new(Vec::new()));
        let app = axum::Router::new()
            .route("/prometheus/api/v1/series", post(series))
            .route("/prometheus/api/v1/admin/tsdb/delete_series", post(delete))
            .with_state(Arc::clone(&bodies));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), bodies)
    }

    #[tokio::test]
    async fn transport_failure_is_counted_not_silent() {
        // 端口 1 必拒绝：重试 1 次后丢弃并计数。
        let store = VictoriaMetricsStore::new(VmConfig {
            insert_url: "http://127.0.0.1:1".to_string(),
            select_url: "http://127.0.0.1:1".to_string(),
            single_node: true,
            batch_size: 2,
            flush_interval: Duration::from_millis(10),
            queue_capacity: 100,
            max_retries: 1,
            timeout: Duration::from_secs(2),
            ..VmConfig::default()
        })
        .unwrap();
        store.write(point("m", 1_000_000, 1.0)).await.unwrap();
        // flush 本身会把失败上报成 unavailable。
        let err = store.flush().await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Unavailable, "{}", err.message);
    }
}
