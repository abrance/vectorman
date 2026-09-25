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
pub mod attach;
pub mod backoff;
pub mod btf;
pub mod cfg;
pub mod cgroup;
pub mod config;
pub mod loader;
pub mod preflight;
pub mod tracepoint_format;

use aggregate::ipv4_of;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub use aggregate::{
    bucket_start, conn_view, diff, edge_record, is_empty, reason_str, sum_per_cpu, view_per_cpu,
    ConnAgg, ConnKey, ConnView, EbpfEdgeRecord, ProcessContext,
};
pub use attach::{AttachPlan, AttachPoint, EbpfItemKind};
pub use backoff::Backoff;
pub use cfg::CfgValues;
pub use config::{EbpfConfig, FilterInput};
pub use loader::{
    object_bytes, objects_embedded, sum_process, AyaMapSource, ConnSource, LoadedItem,
    ProcessSource,
};
pub use preflight::{PreflightEnv, PreflightReport};

/// 一个周期内的 per-CPU 连接快照（原始内核态布局，求和见 [`aggregate::view_per_cpu`]）。
pub type ConnSnapshot = Vec<(ConnKey, Vec<ebpf_abi::ConnAggWire>)>;

/// 一个周期内的 per-CPU 进程快照。
pub type ProcSnapshot = Vec<(ebpf_abi::ProcKey, Vec<ebpf_abi::ProcAggWire>)>;

/// 进程计数三元组（exec/exit/fork）。
pub type ProcCounts = (u64, u64, u64);

/// 进程差分基准键：`(pid, cgroup_id, comm)`。
type ProcSeenKey = (u32, u64, [u8; ebpf_abi::TASK_COMM_LEN]);

/// 内核态 map 的读取接口。
///
/// 真实实现由 aya 加载器提供（读取 per-CPU hashmap 的全部 CPU 副本并把内核侧计数写零）；
/// 测试用假实现直接给快照，因此差分与聚合逻辑无需特权环境即可验证。
pub trait MapSource: Send {
    /// 读取本周期快照；实现方负责在读取后把内核侧计数复位。
    fn drain(&mut self) -> Result<ConnSnapshot, String>;

    /// 按 `pid` 反查进程/容器上下文（取不到时返回 `None`）。
    ///
    /// 需要 `&mut self`：反查结果要写缓存（每个 pid 只读一次 `/proc`）。
    /// 连接键与进程键都带 `pid`，所以这一条同时服务两种采集项。
    fn process_context_by_pid(&mut self, _pid: u32) -> Option<ProcessContext> {
        None
    }

    /// 连接键的进程/容器上下文（默认转调 [`MapSource::process_context_by_pid`]）。
    fn process_context(&mut self, key: &ConnKey) -> Option<ProcessContext> {
        self.process_context_by_pid(key.pid)
    }

    /// 当前生效的内核态程序数量（自监控用）。
    fn attached_programs(&self) -> u64 {
        0
    }

    /// 读走内核态因**限流**丢弃的事件数（跨 CPU 求和并复位）。
    ///
    /// 与聚合值同样的口径：读走就复位，否则下一周期会重复计入。
    fn take_rate_limit_drops(&mut self) -> u64 {
        0
    }
}

/// 上行出口：把边记录与指标交给既有采集通道（`CollectShared::push`）。
pub trait EbpfSink: Send + Sync {
    /// `data_type=ebpf_edges` 的边记录。
    fn edges(&self, item_id: &str, records: Vec<serde_json::Value>);

    /// `data_type=metrics` 的指标记录。
    fn metrics(&self, item_id: &str, records: Vec<serde_json::Value>);

    /// `data_type=ebpf` 的原始事件（默认关闭，按比例抽样）。
    fn raw_events(&self, item_id: &str, records: Vec<serde_json::Value>);
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
    /// 被内核态令牌桶限流丢弃的事件数（需求 12.3 要求可观测）。
    pub rate_limited: AtomicU64,
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
    pub rate_limited: u64,
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
            rate_limited: self.rate_limited.load(Ordering::Relaxed),
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
        let limited = source.take_rate_limit_drops();
        if limited > 0 {
            stats.rate_limited.fetch_add(limited, Ordering::Relaxed);
        }
        // 自监控：限流丢弃/map 满/读取失败这些「没报错但出事了」的情况要能在 Prom 上看见。
        sink.metrics(
            &item_id,
            stats_metrics(&agent_id, &item_id, &stats.snapshot()),
        );
        let bucket_ts = bucket_start(now_micros(), cfg.bucket_secs);

        let mut edges = Vec::new();
        for (key, per_cpu) in snapshot {
            let current = view_per_cpu(&per_cpu);
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

        // 边指标不由 Agent 产出（理由见 `aggregate` 末尾）：这里只产出边记录。
    }
}

/// 进程采集项的主循环：按分钟出 `ebpf_process_*` 指标 + 按比例上行原始事件。
///
/// 与连接型采集项不同，进程项没有「边」的概念，只出指标（需求 4.6）。
pub async fn run_process_loop(
    mut source: AyaMapSource,
    sink: Arc<dyn EbpfSink>,
    cfg: EbpfConfig,
    item_id: String,
    agent_id: String,
    stats: Arc<EbpfStats>,
) {
    let mut seen: std::collections::BTreeMap<ProcSeenKey, ProcCounts> =
        std::collections::BTreeMap::new();
    let mut minute = ProcessMinuteAccumulator::default();
    let interval = Duration::from_secs(cfg.flush_interval_secs.max(1));
    let mut sample_cursor = 0u64;

    loop {
        tokio::time::sleep(interval).await;
        let snapshot = match source.drain_process() {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => Vec::new(),
            Err(reason) => {
                stats.read_errors.fetch_add(1, Ordering::Relaxed);
                eprintln!("gse-agent: ebpf item {item_id} drain failed: {reason}");
                continue;
            }
        };
        stats.flushes.fetch_add(1, Ordering::Relaxed);
        let limited = source.take_rate_limit_drops();
        if limited > 0 {
            stats.rate_limited.fetch_add(limited, Ordering::Relaxed);
        }
        let bucket_ts = bucket_start(now_micros(), cfg.bucket_secs);

        for (key, per_cpu) in snapshot {
            // 容器/Pod 由 pid 反查（需求 4.6 的 `container_id` 维度）。
            let context = source.process_context_by_pid(key.pid);
            let (exec, exit, fork) = sum_process(&per_cpu);
            let current = (exec, exit, fork);
            let delta = match seen.get(&(key.pid, key.cgroup_id, key.comm)) {
                Some(prev) => (
                    exec.saturating_sub(prev.0),
                    exit.saturating_sub(prev.1),
                    fork.saturating_sub(prev.2),
                ),
                None => current,
            };
            seen.insert((key.pid, key.cgroup_id, key.comm), current);
            if delta == (0, 0, 0) {
                stats.idle_keys.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            minute.observe(&key, delta, bucket_ts, context.as_ref());
        }
        seen.retain(|_, v| *v != (0, 0, 0));

        // 原始事件：默认关闭；开启时按 `raw_events_sample_ratio` 抽样（需求 4.5、10.3）。
        if cfg.raw_events_enabled {
            // 单调时钟 → 墙上时钟的偏移每轮算一次即可（同一轮内的事件误差可忽略）。
            let offset = monotonic_to_unix_offset_micros().unwrap_or_else(|| {
                eprintln!("gse-agent: /proc/uptime 不可读，原始事件时间戳将退化为读取时刻");
                now_micros()
            });
            let events = source.drain_raw_events();
            let mut records = Vec::new();
            for event in events {
                sample_cursor = sample_cursor.wrapping_add(1);
                if !sample(sample_cursor, cfg.raw_events_sample_ratio) {
                    continue;
                }
                records.push(raw_event_record(&agent_id, &event, offset));
            }
            if !records.is_empty() {
                stats
                    .metrics
                    .fetch_add(records.len() as u64, Ordering::Relaxed);
                sink.raw_events(&item_id, records);
            }
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

/// 抽样判定：按 1/ratio 的整数间隔抽样，保证长期比例接近配置值且不引入随机数依赖。
#[must_use]
pub fn sample(cursor: u64, ratio: f64) -> bool {
    if ratio >= 1.0 {
        return true;
    }
    if ratio <= 0.0 {
        return false;
    }
    let step = (1.0 / ratio).round().max(1.0) as u64;
    cursor.is_multiple_of(step)
}

/// 单调时钟（内核 `bpf_ktime_get_ns`）与墙上时钟的偏移（微秒）。
///
/// 内核只有单调时钟，原始事件的时间戳必须换算成 Unix 时间才对前端有意义。
/// `/proc/uptime` 的第一列就是单调秒数，两者同源，因此 `偏移 = now_unix − uptime`。
/// 读不到 `/proc/uptime` 时返回 `None`，调用方退回「按读取时刻打时间戳」。
#[must_use]
pub fn monotonic_to_unix_offset_micros() -> Option<i64> {
    let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
    let seconds: f64 = uptime.split_whitespace().next()?.parse().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some(now_micros().saturating_sub((seconds * 1_000_000.0) as i64))
}

/// 原始事件 → `data_type=ebpf` 记录。
///
/// **字段必须与 dataserver 的 `EbpfRecord` 对齐**（`record_id`/`timestamp`/`event_type`/`pid`/
/// `process_name`/`message`/`labels`）。踩过的坑：早期版本发的是 `kind`/`comm`/`saddr`…，
/// 两边各自单测都过，但拼起来整批 `invalid_record: missing field event_type` —— 这类
/// 「契约不一致」只有真跑一次端到端才会暴露。
#[must_use]
pub fn raw_event_record(
    agent_id: &str,
    event: &ebpf_abi::RawEvent,
    monotonic_offset_micros: i64,
) -> serde_json::Value {
    let kind = match event.kind {
        ebpf_abi::EVENT_KIND_CONNECT => "connect",
        ebpf_abi::EVENT_KIND_ACCEPT => "accept",
        ebpf_abi::EVENT_KIND_CLOSE => "close",
        ebpf_abi::EVENT_KIND_PROCESS_EXEC => "process_exec",
        ebpf_abi::EVENT_KIND_PROCESS_EXIT => "process_exit",
        ebpf_abi::EVENT_KIND_PROCESS_FORK => "process_fork",
        // 未知类型不丢：新内核加事件类型时能直接看到原始编号。
        other => return unknown_event_record(agent_id, event, other, monotonic_offset_micros),
    };
    let comm = comm_str(&event.comm);
    let saddr = ipv4_of(event.saddr);
    let daddr = ipv4_of(event.daddr);
    let message = if event.kind >= ebpf_abi::EVENT_KIND_PROCESS_EXEC {
        format!("{kind} {comm}")
    } else {
        format!(
            "{kind} {comm} {saddr}:{} -> {daddr}:{}",
            event.sport, event.dport
        )
    };
    serde_json::json!({
        "record_id": format!(
            "{agent_id}:{}:{kind}:{}:{}",
            event.timestamp_ns, event.pid, event.saddr
        ),
        "timestamp": event_timestamp_micros(event, monotonic_offset_micros),
        "event_type": kind,
        "pid": event.pid as i64,
        "process_name": comm,
        "message": message,
        "labels": {
            "cgroup_id": event.cgroup_id.to_string(),
            "protocol": event.protocol.to_string(),
            "saddr": saddr,
            "daddr": daddr,
            "sport": event.sport.to_string(),
            "dport": event.dport.to_string(),
        },
    })
}

/// 事件时间戳（Unix 微秒）：单调时钟 + 偏移；内核没填（0）时退回当前时刻。
#[must_use]
pub fn event_timestamp_micros(event: &ebpf_abi::RawEvent, monotonic_offset_micros: i64) -> i64 {
    if event.timestamp_ns == 0 {
        return now_micros();
    }
    (event.timestamp_ns / 1_000) as i64 + monotonic_offset_micros
}

/// 未知事件类型：仍然按契约发，`event_type` 保留 `unknown_<n>` 便于排障。
fn unknown_event_record(
    agent_id: &str,
    event: &ebpf_abi::RawEvent,
    kind: u32,
    monotonic_offset_micros: i64,
) -> serde_json::Value {
    let event_type = format!("unknown_{kind}");
    serde_json::json!({
        "record_id": format!("{agent_id}:{}:{event_type}:{}", event.timestamp_ns, event.pid),
        "timestamp": event_timestamp_micros(event, monotonic_offset_micros),
        "event_type": event_type,
        "pid": event.pid as i64,
        "process_name": comm_str(&event.comm),
        "message": format!("未知事件类型 {kind}"),
        "labels": {"cgroup_id": event.cgroup_id.to_string()},
    })
}

/// 进程名（去掉结尾 NUL）。
#[must_use]
pub fn comm_str(comm: &[u8; ebpf_abi::TASK_COMM_LEN]) -> String {
    let end = comm.iter().position(|b| *b == 0).unwrap_or(comm.len());
    String::from_utf8_lossy(&comm[..end]).into_owned()
}

/// 每分钟汇总的进程计数（只输出已关闭的分钟桶）。
#[derive(Default)]
pub struct ProcessMinuteAccumulator {
    buckets: std::collections::BTreeMap<(i64, ProcSeenKey), (ProcCounts, ProcessLabels)>,
}

/// 进程指标的额外维度（容器/Pod，由 pid 反查得到）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessLabels {
    pub container_id: String,
    pub pod_uid: String,
}

impl ProcessMinuteAccumulator {
    /// 记录一个周期桶；`context` 是 pid 反查到的容器/Pod（可能为空）。
    pub fn observe(
        &mut self,
        key: &ebpf_abi::ProcKey,
        delta: ProcCounts,
        bucket_ts: i64,
        context: Option<&ProcessContext>,
    ) {
        let labels = ProcessLabels {
            container_id: context.map(|c| c.container_id.clone()).unwrap_or_default(),
            pod_uid: context.map(|c| c.pod_name.clone()).unwrap_or_default(),
        };
        let entry = self
            .buckets
            .entry((bucket_ts, (key.pid, key.cgroup_id, key.comm)))
            .or_insert((ProcCounts::default(), labels.clone()));
        // 先到的周期可能拿不到上下文（进程刚退出），后续周期补上时以非空值覆盖。
        if entry.1.container_id.is_empty() && !labels.container_id.is_empty() {
            entry.1 = labels;
        }
        entry.0 .0 = entry.0 .0.saturating_add(delta.0);
        entry.0 .1 = entry.0 .1.saturating_add(delta.1);
        entry.0 .2 = entry.0 .2.saturating_add(delta.2);
    }

    /// 取出已关闭分钟桶（`now` 之前的整分钟），产出 `ebpf_process_*` 指标。
    pub fn drain_closed(&mut self, now: i64, agent_id: &str) -> Vec<serde_json::Value> {
        let current_minute = now - now.rem_euclid(60_000_000);
        let closed: Vec<_> = self
            .buckets
            .keys()
            .filter(|(ts, _)| *ts < current_minute)
            .cloned()
            .collect();
        let mut out = Vec::new();
        for key in closed {
            let Some(((exec, exit, fork), labels)) = self.buckets.remove(&key) else {
                continue;
            };
            let (ts, (pid, cgroup_id, comm)) = key;
            let comm = comm_str(&comm);
            for (measurement, value) in [
                ("ebpf_process_exec_total", exec),
                ("ebpf_process_exit_total", exit),
                ("ebpf_process_fork_total", fork),
            ] {
                if value == 0 {
                    continue;
                }
                out.push(serde_json::json!({
                    "record_id": format!("{agent_id}:{pid}:{ts}:{measurement}"),
                    "timestamp": ts,
                    "measurement": measurement,
                    "tags": {
                        "agent_id": agent_id,
                        "pid": pid.to_string(),
                        "cgroup_id": cgroup_id.to_string(),
                        "process_name": comm,
                        // 容器 ID 由 pid 反查得到；Pod 只到 uid（名字需要 k8s 侧数据）。
                        "container_id": labels.container_id,
                        "pod_uid": labels.pod_uid,
                    },
                    "field_name": "value",
                    "field_value": value as f64,
                }));
            }
        }
        out
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

/// 约束统计快照 → `agent_ebpf_*` 指标点（需求 12.3：统计算并输出每项限制的触发次数与被丢弃的数据量）。
///
/// 为什么要有：内核态限流、map 满丢弃、map 读取失败这些「出事了但没报错」的情况，
/// 如果只留在内存里的计数器，运维在 Prom 上什么都看不到 —— 采集看起来一直「正常」。
/// 每个字段一条点（`field_name` 固定 `value`，指标名区分口径），标签带 `agent_id`/`item_id`。
#[must_use]
pub fn stats_metrics(
    agent_id: &str,
    item_id: &str,
    snapshot: &EbpfSnapshot,
) -> Vec<serde_json::Value> {
    let ts = now_micros();
    let rows: [(&str, u64); 8] = [
        ("agent_ebpf_flushes_total", snapshot.flushes),
        ("agent_ebpf_edges_total", snapshot.edges),
        ("agent_ebpf_metric_points_total", snapshot.metrics),
        ("agent_ebpf_filtered_total", snapshot.filtered),
        ("agent_ebpf_idle_keys_total", snapshot.idle_keys),
        ("agent_ebpf_read_errors_total", snapshot.read_errors),
        (
            "agent_ebpf_map_overflow_dropped_total",
            snapshot.map_overflow_dropped,
        ),
        ("agent_ebpf_rate_limited_total", snapshot.rate_limited),
    ];
    rows.iter()
        .map(|(measurement, value)| {
            serde_json::json!({
                "record_id": format!("{agent_id}:{item_id}:{measurement}:{ts}"),
                "timestamp": ts,
                "measurement": measurement,
                "tags": {"agent_id": agent_id, "item_id": item_id},
                "field_name": "value",
                "field_value": *value as f64,
            })
        })
        .collect()
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

        fn process_context_by_pid(&mut self, _pid: u32) -> Option<ProcessContext> {
            self.contexts.first().cloned().flatten()
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        edges: Mutex<Vec<serde_json::Value>>,
        metrics: Mutex<Vec<serde_json::Value>>,
        raw_events: Mutex<Vec<serde_json::Value>>,
    }

    impl EbpfSink for RecordingSink {
        fn edges(&self, _item_id: &str, records: Vec<serde_json::Value>) {
            self.edges.lock().unwrap().extend(records);
        }

        fn metrics(&self, _item_id: &str, records: Vec<serde_json::Value>) {
            self.metrics.lock().unwrap().extend(records);
        }

        fn raw_events(&self, _item_id: &str, records: Vec<serde_json::Value>) {
            self.raw_events.lock().unwrap().extend(records);
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
        let mut first = ebpf_abi::ConnAggWire {
            connections: 2,
            bytes_sent: 100,
            ..Default::default()
        };
        first.latency_hist[3] = 1;
        let mut second = first;
        second.connections = 5;
        second.bytes_sent = 400;

        let source = Box::new(FakeSource {
            snapshots: Mutex::new(vec![
                vec![(key(), vec![first, ebpf_abi::ConnAggWire::default()])],
                vec![(key(), vec![second, ebpf_abi::ConnAggWire::default()])],
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

    #[test]
    fn sampling_ratio_behaves() {
        assert!(sample(1, 1.0) && sample(999, 1.0), "ratio=1 全采");
        assert!(!sample(1, 0.0) && !sample(10, 0.0), "ratio=0 不采");
        // 0.1 → 每 10 个取 1 个（cursor 从 1 开始计数）。
        let hits = (1..=1000).filter(|c| sample(*c, 0.1)).count();
        assert_eq!(hits, 100);
        // 极端小比例不会除以 0：step 至少是 1，命中间隔等于 1/ratio。
        assert!(sample(1_000_000_000, 1e-9));
    }

    #[test]
    fn raw_event_records_map_kinds_and_keep_unknown() {
        let mut comm = [0u8; ebpf_abi::TASK_COMM_LEN];
        comm[..4].copy_from_slice(b"java");
        let event = ebpf_abi::RawEvent {
            kind: ebpf_abi::EVENT_KIND_PROCESS_EXEC,
            pid: 42,
            timestamp_ns: 1_700_000_000_123_456_789,
            cgroup_id: 7,
            saddr: u32::from_be_bytes([10, 0, 0, 5]),
            daddr: u32::from_be_bytes([10, 0, 0, 9]),
            sport: 40_000,
            dport: 8_080,
            protocol: 6,
            comm,
            ..Default::default()
        };
        let record = raw_event_record("agent-1", &event, -1_700_000_000_000_000);
        // 字段名必须与 dataserver 的 `EbpfRecord` 一致（`event_type`/`process_name`/`message`）：
        // 早期版本发的是 `kind`/`comm`，两边各自单测都过，拼起来整批 `missing field event_type`。
        assert_eq!(record["event_type"], "process_exec");
        assert_eq!(record["process_name"], "java");
        assert_eq!(record["pid"], 42);
        assert!(record["message"]
            .as_str()
            .unwrap()
            .contains("process_exec java"));
        assert_eq!(record["labels"]["saddr"], "10.0.0.5");
        assert_eq!(record["labels"]["dport"], "8080");

        // 内核没填时间戳（0）时退回当前时刻，不出现 1970 年。
        let untimed = ebpf_abi::RawEvent {
            timestamp_ns: 0,
            ..event
        };
        assert!(
            raw_event_record("agent-1", &untimed, 0)["timestamp"]
                .as_i64()
                .unwrap()
                > 0
        );
        // 时间戳 = 单调时钟 + 偏移（这里用负偏移模拟「boot 时间与 Unix 起点之差」）。
        assert_eq!(record["timestamp"].as_i64().unwrap(), 123_456);

        // 连接类事件的消息里带五元组，便于日志页直接读。
        let connect = ebpf_abi::RawEvent {
            kind: ebpf_abi::EVENT_KIND_CONNECT,
            ..event
        };
        let message = raw_event_record("agent-1", &connect, 0)["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            message.contains("connect java 10.0.0.5:40000 -> 10.0.0.9:8080"),
            "{message}"
        );

        let unknown = ebpf_abi::RawEvent { kind: 99, ..event };
        assert_eq!(
            raw_event_record("agent-1", &unknown, 0)["event_type"],
            "unknown_99"
        );
    }

    /// 自监控指标点：每个计数字段一条，名字与需求 12.3 的口径对齐。
    #[test]
    fn stats_metrics_cover_every_counter() {
        let snapshot = EbpfSnapshot {
            flushes: 4,
            edges: 9,
            metrics: 2,
            filtered: 3,
            idle_keys: 7,
            read_errors: 1,
            map_overflow_dropped: 5,
            rate_limited: 6,
        };
        let points = stats_metrics("agent-1", "item-1", &snapshot);
        assert_eq!(points.len(), 8, "每个计数字段一条点");
        let value_of = |name: &str| -> f64 {
            points
                .iter()
                .find(|p| p["measurement"] == name)
                .unwrap_or_else(|| panic!("缺 {name}"))["field_value"]
                .as_f64()
                .unwrap()
        };
        assert_eq!(value_of("agent_ebpf_rate_limited_total"), 6.0);
        assert_eq!(value_of("agent_ebpf_map_overflow_dropped_total"), 5.0);
        assert_eq!(value_of("agent_ebpf_read_errors_total"), 1.0);
        assert_eq!(value_of("agent_ebpf_edges_total"), 9.0);
        // 记录 ID 唯一（接入侧按 record_id 去重，重复 ID 会让后续点被吃掉）。
        let ids: std::collections::HashSet<&str> = points
            .iter()
            .filter_map(|p| p["record_id"].as_str())
            .collect();
        assert_eq!(ids.len(), 8);
        assert_eq!(points[0]["tags"]["agent_id"], "agent-1");
        assert_eq!(points[0]["tags"]["item_id"], "item-1");
        assert_eq!(points[0]["field_name"], "value");
    }

    #[test]
    fn process_minute_accumulator_closes_by_minute() {
        let base = 1_020_000_000i64; // 整分钟
        let key = |pid: u32, name: &[u8]| {
            let mut comm = [0u8; ebpf_abi::TASK_COMM_LEN];
            comm[..name.len()].copy_from_slice(name);
            ebpf_abi::ProcKey {
                pid,
                cgroup_id: 5,
                comm,
                ..Default::default()
            }
        };
        let mut acc = ProcessMinuteAccumulator::default();
        let context = ProcessContext {
            process_name: "java".into(),
            container_id: "c1".into(),
            pod_name: "9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210".into(),
        };
        acc.observe(&key(1, b"java"), (2, 0, 1), base, Some(&context));
        acc.observe(&key(1, b"java"), (1, 3, 0), base + 10_000_000, None);

        assert!(acc.drain_closed(base + 30_000_000, "a1").is_empty());
        let metrics = acc.drain_closed(base + 60_000_000, "a1");
        // 两个 10 秒桶各自是一条指标点，这里按 measurement 求和再比对。
        let value = |measurement: &str| -> f64 {
            metrics
                .iter()
                .filter(|m| m["measurement"] == measurement)
                .map(|m| m["field_value"].as_f64().unwrap())
                .sum()
        };
        assert_eq!(value("ebpf_process_exec_total"), 3.0, "同一分钟累加");
        assert_eq!(value("ebpf_process_exit_total"), 3.0);
        assert_eq!(value("ebpf_process_fork_total"), 1.0);
        let exec = metrics
            .iter()
            .find(|m| m["measurement"] == "ebpf_process_exec_total")
            .unwrap();
        assert_eq!(exec["tags"]["process_name"], "java");
        assert_eq!(exec["tags"]["pid"], "1");
        assert_eq!(exec["tags"]["container_id"], "c1");
        assert_eq!(
            exec["tags"]["pod_uid"],
            "9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210"
        );
        let ts = exec["timestamp"].as_i64().unwrap();
        assert!(
            ts == base || ts == base + 10_000_000,
            "时间戳应是两个桶起点之一，实际 {ts}"
        );
        assert!(
            acc.drain_closed(base + 60_000_000, "a1").is_empty(),
            "取出不重复"
        );
    }

    #[test]
    fn objects_are_absent_until_built() {
        // 仓库里没有 .o 时必须是空切片（保证可编译），而不是「假装可用」。
        let expected = match std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packaging/ebpf/network.o"
        ))
        .exists()
        {
            true => !object_bytes(EbpfItemKind::Network).is_empty(),
            false => object_bytes(EbpfItemKind::Network).is_empty(),
        };
        assert!(expected);
        assert_eq!(
            objects_embedded(),
            !object_bytes(EbpfItemKind::Tcp).is_empty()
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
