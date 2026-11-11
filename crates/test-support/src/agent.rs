//! Agent 侧指标生成器：**直接调产品代码的产出函数**，不手搓 JSON。
//!
//! 与 `data::*` 的分工：`data` 造的是「接入侧契约」（手写 envelope，验网关解析）；这里造的是
//! 「Agent 真会发什么」——`metrics_record` / `drain_closed` / `duration_metrics` / `interval_records`
//! 都是产品入口，产品改了标签名、值口径、`field_name`，这些用例会跟着红，而不是继续绿着撒谎。
//!
//! 为什么不能起真 eBPF：`ebpf_process_*` / `ebpf_syscall_*` / `agent_ebpf_*` 需要在 Agent 侧
//! 先用 `aya` 加载内核态程序（root + `CAP_BPF` + BTF），TP 环境没有这些特权。这里的口径是
//! **信「Agent 产出 → dataserver `/v1/ingest` → VM」这段链路**（Agent 之外的每一跳都是真进程），
//! 内核态采集本身由 `gse-agent-ebpf` 的单元测试覆盖。
//!
//! 所有生成器都不带唯一前缀：调用方必须用 `util::unique_prefix()` 拼 `agent_id` / `item_id` /
//! `process_name`，否则并行跑或 CI 复用同一台 VM 时会看到别人的序列。

use std::sync::atomic::Ordering;

use ebpf_abi::{ProcKey, SyscallKey, TASK_COMM_LEN};
use gse_agent_ebpf::{
    duration_metrics, failure_metrics, interval_records, EbpfStats, ProcessMinuteAccumulator,
};
use serde_json::Value;

pub use ebpf_abi::{SYSCALL_OP_FSYNC, SYSCALL_OP_OPENAT, SYSCALL_OP_READ, SYSCALL_OP_WRITE};
pub use gse_agent_ebpf::PreflightReport;

/// 一轮主机指标采集的两条记录：`cpu_usage` + `mem_usage`。
///
/// 走 `metrics::metrics_record`，所以 `record_id` 指纹、`field_name="value"`、
/// 标签集合（`agent_id` / `item_id` / 可选 `host_id`）都是产品口径。
pub fn host_metric_records(
    agent_id: &str,
    item_id: &str,
    host_id: Option<&str>,
    cpu_percent: f64,
    mem_percent: f64,
    timestamp: i64,
) -> Vec<Value> {
    [("cpu_usage", cpu_percent), ("mem_usage", mem_percent)]
        .into_iter()
        .map(|(measurement, value)| {
            serde_json::to_value(gse_agent_core::collect::metrics::metrics_record(
                agent_id,
                item_id,
                host_id,
                measurement,
                value,
                timestamp,
            ))
            .expect("MetricsRecord 必须能序列化")
        })
        .collect()
}

/// 一小时内已关闭分钟桶的 `ebpf_process_{exec,exit,fork}_total`。
///
/// `samples`：`(pid, cgroup_id, process_name, (exec, exit, fork))`。`process_name` 最多 15 字节
/// （内核 `TASK_COMM_LEN`），超长会被截断成**另一个** `process_name` 标签值——调用方给短名字。
///
/// 桶起点取 `now` 之前那个整分钟（`drain_closed` 只吐已关闭的桶），三条指标的 `value` 为 0 时
/// 产品侧不产出点，所以每条计数都要给非零值才会落库。
/// 单条 eBPF 进程采样：`(pid, cgroup_id, process_name, (exec, exit, fork))`。
type EbpfProcessSample<'a> = (u32, u64, &'a str, (u64, u64, u64));

pub fn ebpf_process_records(
    agent_id: &str,
    now_micros: i64,
    samples: &[EbpfProcessSample<'_>],
) -> Vec<Value> {
    let bucket = crate::util::floor_minute(now_micros) - 60_000_000;
    let mut acc = ProcessMinuteAccumulator::default();
    for &(pid, cgroup_id, process_name, counts) in samples {
        let mut comm = [0u8; TASK_COMM_LEN];
        let bytes = process_name.as_bytes();
        let len = bytes.len().min(TASK_COMM_LEN);
        comm[..len].copy_from_slice(&bytes[..len]);
        let key = ProcKey {
            pid,
            _pad: 0,
            cgroup_id,
            comm,
        };
        // `context` 留空：TP 环境没有 pid→容器/Pod 反查表，空值在 VM 里等于没有该标签。
        acc.observe(&key, counts, bucket, None);
    }
    acc.drain_closed(now_micros, agent_id)
}

/// 一个周期的 `ebpf_syscall_duration_micros`：`field="avg"` 与 `field="p95"` 各一条。
///
/// `hist_slot` 是全部 `calls` 落在的直方图槽（槽 `i` 覆盖 `[2^i, 2^(i+1))` 微秒），
/// 因此 `p95` 等于该槽的上界 `2^(slot+1)`。
#[allow(clippy::too_many_arguments)]
pub fn ebpf_syscall_duration_records(
    agent_id: &str,
    item_id: &str,
    pid: u32,
    cgroup_id: u64,
    process_name: &str,
    op: u32,
    calls: u64,
    duration_sum_micros: u64,
    hist_slot: usize,
    timestamp: i64,
) -> Vec<Value> {
    let mut comm = [0u8; TASK_COMM_LEN];
    let bytes = process_name.as_bytes();
    let len = bytes.len().min(TASK_COMM_LEN);
    comm[..len].copy_from_slice(&bytes[..len]);
    let key = SyscallKey {
        pid,
        _pad: 0,
        cgroup_id,
        op,
        _pad2: 0,
        comm,
    };
    let mut hist = vec![0u64; ebpf_abi::HIST_SLOTS];
    hist[hist_slot] = calls;
    let delta = gse_agent_ebpf::SyscallAgg {
        calls,
        errors: 0,
        duration_sum_us: duration_sum_micros,
        duration_max_us: duration_sum_micros,
        hist,
    };
    duration_metrics(agent_id, item_id, &key, &delta, timestamp)
}

/// 一条 `ebpf_syscall_failures_total`（维度 `op` + `errno`）；`count == 0` 时产品侧不产出点。
pub fn ebpf_syscall_failure_record(
    agent_id: &str,
    item_id: &str,
    op: u32,
    errno: u32,
    count: u64,
    timestamp: i64,
) -> Option<Value> {
    failure_metrics(
        agent_id,
        item_id,
        &ebpf_abi::SyscallErrKey { errno, op },
        count,
        timestamp,
    )
}

/// 计数器按 `stats_metrics` 的输出顺序填 `1..=10`，便于断言「行没串位」。
const COUNTERS: [(&str, u64); 10] = [
    ("agent_ebpf_flushes_total", 1),
    ("agent_ebpf_edges_total", 2),
    ("agent_ebpf_metric_points_total", 3),
    ("agent_ebpf_filtered_total", 4),
    ("agent_ebpf_idle_keys_total", 5),
    ("agent_ebpf_read_errors_total", 6),
    ("agent_ebpf_map_overflow_dropped_total", 7),
    ("agent_ebpf_rate_limited_total", 8),
    ("agent_ebpf_buffer_dropped_total", 9),
    ("agent_ebpf_edges_skipped_total", 10),
];

/// 一轮 `agent_ebpf_*` 自监控上报：10 条计数器 + `agent_ebpf_cpu_percent` + `agent_ebpf_degraded`
/// + `agent_ebpf_capability`。
///
/// 走产品入口 `interval_records`（三个采集循环共用的「每轮必报」），所以 `record_id` 规则、
/// 点数与标签都和线上一致。计数器值固定为 [`COUNTERS`] 里的 `1..=10`。
///
/// `report` 传降级/不满足前置条件的报告时，capability 点的 `value` 为 `0` —— 这正是 TP 环境
/// 「没有 root/CAP_BPF」的真实形态，不是伪造。
pub fn agent_ebpf_interval_records(
    agent_id: &str,
    item_id: &str,
    cpu_percent: f64,
    degraded: bool,
    report: &PreflightReport,
) -> Vec<Value> {
    let stats = EbpfStats::default();
    let counters = [
        &stats.flushes,
        &stats.edges,
        &stats.metrics,
        &stats.filtered,
        &stats.idle_keys,
        &stats.read_errors,
        &stats.map_overflow_dropped,
        &stats.rate_limited,
        &stats.buffer_dropped,
        &stats.edges_skipped,
    ];
    for (counter, (_, value)) in counters.iter().zip(COUNTERS) {
        counter.store(value, Ordering::Relaxed);
    }
    stats.set_cpu(cpu_percent, degraded);
    interval_records(agent_id, item_id, report, &stats)
}

/// 不满足前置条件的 `PreflightReport`：`capability_ok=false`，`reason` 带唯一前缀。
pub fn unavailable_report(kernel_release: &str, reason: &str) -> PreflightReport {
    PreflightReport {
        kernel_release: kernel_release.to_string(),
        kernel_ok: true,
        btf_ok: true,
        capability_ok: false,
        reason: Some(reason.to_string()),
        advice: Some("TP 环境无 root/CAP_BPF，Agent 走降级路径".to_string()),
    }
}
