//! eBPF 采集的用户态部分：前置校验、快照差分、边记录与 `ebpf_*` 指标。
//!
//! 分工（`/.monkeycode/specs/ebpf-observability/design.md`）：
//!
//! - 内核态程序只做 per-CPU 计数；用户态每 `flush_interval_secs` 读一次快照、差分、
//!   按 `bucket_secs` 出边记录，按分钟出指标。
//! - 本 crate **不做服务名反查**（Agent 不知道全局服务表）：边记录的 `src_service` /
//!   `dst_service` 留空，由 `dataserver` 反查回填。因此 `apm_edge_*{source="ebpf"}`
//!   指标也由服务端在反查后产生，本 crate 只产 `ebpf_*`。
//! - 真实内核态程序由 `MapSource` 的实现提供（aya 加载器在其后接入）；本 crate 的全部
//!   逻辑都能用假快照测试，不需要特权环境。

pub mod aggregate;
pub mod config;
pub mod preflight;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub use aggregate::{
    bucket_start, conn_view, diff, edge_record, is_empty, reason_str, sum_per_cpu, ConnAgg,
    ConnKey, ConnView, EbpfEdgeRecord, MinuteAccumulator, ProcessContext,
};
pub use config::{EbpfConfig, FilterInput};
pub use preflight::{PreflightEnv, PreflightReport};

/// 一个周期内的 per-CPU 连接快照。
pub type ConnSnapshot = Vec<(ConnKey, Vec<ConnAgg>)>;

/// 内核态 map 的读取接口。
///
/// 真实实现由 aya 加载器提供（读取 per-CPU hashmap 的全部 CPU 副本并把内核侧计数写零）；
/// 测试用假实现直接给快照，因此差分与聚合逻辑无需特权环境即可验证。
pub trait MapSource: Send {
    /// 读取本周期快照；实现方负责在读取后把内核侧计数复位。
    fn drain(&mut self) -> Result<ConnSnapshot, String>;

    /// 进程/容器上下文（由 `cgroup_id` 反查；取不到时返回 `None`）。
    fn process_context(&self, _key: &ConnKey) -> Option<ProcessContext> {
        None
    }

    /// 当前生效的内核态程序数量（自监控用）。
    fn attached_programs(&self) -> u64 {
        0
    }
}

/// 上行出口：把边记录与指标交给既有采集通道（`CollectShared::push`）。
pub trait EbpfSink: Send + Sync {
    /// `data_type=ebpf_edges` 的边记录。
    fn edges(&self, item_id: &str, records: Vec<serde_json::Value>);

    /// `data_type=metrics` 的指标记录。
    fn metrics(&self, item_id: &str, records: Vec<serde_json::Value>);
}

/// 运行统计。
#[derive(Debug, Default)]
pub struct EbpfStats {
    pub flushes: AtomicU64,
    pub edges: AtomicU64,
    pub metrics: AtomicU64,
    /// 被过滤器丢弃的键数。
    pub filtered: AtomicU64,
    /// 差分为空而跳过的键数。
    pub idle_keys: AtomicU64,
    /// 读取 map 失败次数。
    pub read_errors: AtomicU64,
    /// map 满丢弃计数（内核态 `OVERFLOW_SLOT` 汇总）。
    pub map_overflow_dropped: AtomicU64,
}

/// 统计快照。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct EbpfSnapshot {
    pub flushes: u64,
    pub edges: u64,
    pub metrics: u64,
    pub filtered: u64,
    pub idle_keys: u64,
    pub read_errors: u64,
    pub map_overflow_dropped: u64,
}

impl EbpfStats {
    #[must_use]
    pub fn snapshot(&self) -> EbpfSnapshot {
        EbpfSnapshot {
            flushes: self.flushes.load(Ordering::Relaxed),
            edges: self.edges.load(Ordering::Relaxed),
            metrics: self.metrics.load(Ordering::Relaxed),
            filtered: self.filtered.load(Ordering::Relaxed),
            idle_keys: self.idle_keys.load(Ordering::Relaxed),
            read_errors: self.read_errors.load(Ordering::Relaxed),
            map_overflow_dropped: self.map_overflow_dropped.load(Ordering::Relaxed),
        }
    }
}

/// 采集项运行入口：前置校验 → 周期差分 → 出边记录与指标。
///
/// `source` 由调用方提供（aya 加载器或测试假实现）。任务被 abort 时随之结束。
pub async fn run(
    source: Box<dyn MapSource>,
    sink: Arc<dyn EbpfSink>,
    cfg: EbpfConfig,
    item_id: String,
    agent_id: String,
    report: PreflightReport,
) {
    if !report.ok() {
        // 前置校验失败：只降级本项能力，Agent 日志给 warn + 建议动作。
        eprintln!(
            "gse-agent: ebpf item {item_id} disabled: {}; {}",
            report.reason.clone().unwrap_or_default(),
            report.advice.clone().unwrap_or_default()
        );
        sink.metrics(
            &item_id,
            vec![capability_metric(&agent_id, &report, item_id.as_str())],
        );
        return;
    }

    let stats = Arc::new(EbpfStats::default());
    run_loop(source, sink, cfg, item_id, agent_id, report, stats).await;
}

/// `run` 的主循环（拆出来便于测试直接驱动，不经过前置校验）。
pub async fn run_loop(
    mut source: Box<dyn MapSource>,
    sink: Arc<dyn EbpfSink>,
    cfg: EbpfConfig,
    item_id: String,
    agent_id: String,
    report: PreflightReport,
    stats: Arc<EbpfStats>,
) {
    // 上一周期快照：键 → 绝对值（差分基准）。
    let mut previous: std::collections::BTreeMap<ConnKey, ConnAgg> =
        std::collections::BTreeMap::new();
    let mut minute = MinuteAccumulator::default();
    let interval = Duration::from_secs(cfg.flush_interval_secs.max(1));

    // 首次上报能力状态，便于链路页显示不可用/可用。
    sink.metrics(
        &item_id,
        vec![capability_metric(&agent_id, &report, &item_id)],
    );

    loop {
        tokio::time::sleep(interval).await;
        let snapshot = match source.drain() {
            Ok(snapshot) => snapshot,
            Err(reason) => {
                stats.read_errors.fetch_add(1, Ordering::Relaxed);
                eprintln!("gse-agent: ebpf item {item_id} drain failed: {reason}");
                continue;
            }
        };
        stats.flushes.fetch_add(1, Ordering::Relaxed);
        let bucket_ts = bucket_start(now_micros(), cfg.bucket_secs);

        let mut edges = Vec::new();
        for (key, per_cpu) in snapshot {
            let current = sum_per_cpu(&per_cpu);
            let delta = diff(previous.get(&key), &current);
            previous.insert(key, current);
            if is_empty(&delta) {
                stats.idle_keys.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let view = conn_view(&key);
            let ctx = source.process_context(&key);
            let filter = FilterInput {
                src_ip: view.src_ip.clone(),
                dst_ip: view.dst_ip.clone(),
                src_port: view.sport,
                dst_port: view.dport,
                cgroup_id: key.cgroup_id,
                process_name: ctx
                    .as_ref()
                    .map(|c| c.process_name.clone())
                    .unwrap_or_default(),
                pod_name: ctx.as_ref().map(|c| c.pod_name.clone()).unwrap_or_default(),
            };
            if !config::keep(&filter, &cfg) {
                stats.filtered.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if let Some(record) = edge_record(
                &agent_id,
                &key,
                &delta,
                bucket_ts,
                cfg.bucket_secs,
                ctx.as_ref(),
            ) {
                minute.observe(&record);
                match serde_json::to_value(&record) {
                    Ok(value) => edges.push(value),
                    Err(e) => eprintln!("gse-agent: ebpf item {item_id} encode edge failed: {e}"),
                }
            }
        }

        // 清理本周期未再出现的键，避免快照无限增长（内核侧已复位）。
        let seen: std::collections::BTreeSet<ConnKey> = previous
            .iter()
            .filter(|(_, agg)| !is_empty(agg))
            .map(|(key, _)| *key)
            .collect();
        previous.retain(|key, _| seen.contains(key));

        stats.edges.fetch_add(edges.len() as u64, Ordering::Relaxed);
        if !edges.is_empty() {
            sink.edges(&item_id, edges);
        }

        let metrics = minute.drain_closed(now_micros(), &agent_id);
        stats
            .metrics
            .fetch_add(metrics.len() as u64, Ordering::Relaxed);
        if !metrics.is_empty() {
            sink.metrics(&item_id, metrics);
        }
    }
}

/// 能力状态指标：链路页据此显示 eBPF 可用/不可用与原因。
#[must_use]
pub fn capability_metric(
    agent_id: &str,
    report: &PreflightReport,
    item_id: &str,
) -> serde_json::Value {
    let available = report.ok();
    serde_json::json!({
        "record_id": format!(
            "{agent_id}:{item_id}:agent_ebpf_capability:{}",
            now_micros()
        ),
        "timestamp": now_micros(),
        "measurement": "agent_ebpf_capability",
        "tags": {
            "agent_id": agent_id,
            "item_id": item_id,
            "kernel_release": report.kernel_release,
            "kernel_ok": report.kernel_ok.to_string(),
            "btf_ok": report.btf_ok.to_string(),
            "capability_ok": report.capability_ok.to_string(),
            "reason": report.reason.clone().unwrap_or_default(),
        },
        "field_name": "available",
        "field_value": if available { 1.0 } else { 0.0 },
    })
}

/// 当前 Unix 微秒。
#[must_use]
pub fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 假 map 源：按预设顺序返回快照。
    struct FakeSource {
        snapshots: Mutex<Vec<ConnSnapshot>>,
        contexts: Vec<Option<ProcessContext>>,
    }

    impl MapSource for FakeSource {
        fn drain(&mut self) -> Result<ConnSnapshot, String> {
            let mut guard = self.snapshots.lock().unwrap();
            if guard.is_empty() {
                return Ok(Vec::new());
            }
            Ok(guard.remove(0))
        }

        fn process_context(&self, _key: &ConnKey) -> Option<ProcessContext> {
            self.contexts.first().cloned().flatten()
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        edges: Mutex<Vec<serde_json::Value>>,
        metrics: Mutex<Vec<serde_json::Value>>,
    }

    impl EbpfSink for RecordingSink {
        fn edges(&self, _item_id: &str, records: Vec<serde_json::Value>) {
            self.edges.lock().unwrap().extend(records);
        }

        fn metrics(&self, _item_id: &str, records: Vec<serde_json::Value>) {
            self.metrics.lock().unwrap().extend(records);
        }
    }

    fn key() -> ConnKey {
        ConnKey {
            pid: 1,
            cgroup_id: 9,
            saddr: u32::from_be_bytes([10, 0, 0, 5]),
            daddr: u32::from_be_bytes([10, 0, 0, 9]),
            sport: 40_000,
            dport: 8_080,
            protocol: 6,
        }
    }

    fn ok_report() -> PreflightReport {
        PreflightReport {
            kernel_release: "6.1.0".into(),
            kernel_ok: true,
            btf_ok: true,
            capability_ok: true,
            reason: None,
            advice: None,
        }
    }

    /// 用真实时钟驱动一轮（把间隔压到 1 秒，测试里只等一轮）。
    #[tokio::test]
    async fn loop_emits_edges_from_fake_snapshots() {
        let mut first = ConnAgg {
            connections: 2,
            bytes_sent: 100,
            ..ConnAgg::default()
        };
        first.latency_hist = vec![1];
        let mut second = first.clone();
        second.connections = 5;
        second.bytes_sent = 400;

        let source = Box::new(FakeSource {
            snapshots: Mutex::new(vec![
                vec![(key(), vec![first.clone(), ConnAgg::default()])],
                vec![(key(), vec![second, ConnAgg::default()])],
            ]),
            contexts: vec![Some(ProcessContext {
                process_name: "java".into(),
                container_id: "c1".into(),
                pod_name: "order-api-1".into(),
            })],
        });
        let sink = Arc::new(RecordingSink::default());
        let cfg = EbpfConfig {
            flush_interval_secs: 1,
            ..EbpfConfig::default()
        };
        let handle = tokio::spawn(run_loop(
            source,
            sink.clone(),
            cfg,
            "item-ebpf".to_string(),
            "agent-1".to_string(),
            ok_report(),
            Arc::new(EbpfStats::default()),
        ));
        tokio::time::sleep(Duration::from_millis(2_400)).await;
        handle.abort();

        let edges = sink.edges.lock().unwrap().clone();
        assert_eq!(edges.len(), 2, "两轮各出一条边: {edges:?}");
        assert_eq!(edges[0]["connections"], 2.0, "首轮按绝对值计");
        assert_eq!(edges[1]["connections"], 3.0, "次轮为增量");
        assert_eq!(edges[1]["src_process"], "java");
        assert_eq!(edges[1]["source"], "ebpf");
        assert!(edges[1]["src_service"].as_str().unwrap().is_empty());

        let metrics = sink.metrics.lock().unwrap().clone();
        assert!(
            metrics
                .iter()
                .any(|m| m["measurement"] == "agent_ebpf_capability"),
            "能力状态必须先上报"
        );
    }

    #[tokio::test]
    async fn preflight_failure_only_reports_capability() {
        let sink = Arc::new(RecordingSink::default());
        let report = PreflightReport {
            kernel_release: "5.4.0".into(),
            kernel_ok: false,
            btf_ok: true,
            capability_ok: true,
            reason: Some("eBPF preflight failed: kernel >= 5.8".into()),
            advice: Some("run as root".into()),
        };
        run(
            Box::new(FakeSource {
                snapshots: Mutex::new(Vec::new()),
                contexts: Vec::new(),
            }),
            sink.clone(),
            EbpfConfig::default(),
            "item-ebpf".to_string(),
            "agent-1".to_string(),
            report,
        )
        .await;

        let metrics = sink.metrics.lock().unwrap().clone();
        assert_eq!(metrics.len(), 1, "只上报能力状态，不采集");
        assert_eq!(metrics[0]["field_name"], "available");
        assert_eq!(metrics[0]["field_value"], 0.0);
        assert!(metrics[0]["tags"]["reason"]
            .as_str()
            .unwrap()
            .contains("kernel >= 5.8"));
        assert!(sink.edges.lock().unwrap().is_empty());
    }
}
