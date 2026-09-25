//! `ebpf_syscall` 的用户态：快照差分、直方图与慢调用事件（需求 6）。
//!
//! 与边记录不同，syscall 的指标**由 Agent 直接产出**（`dataserver` 不需要再做反查，
//! 因为维度里只有 `op`/`process_name`/`service`，而 `service` 由服务端用静态映射补齐）。
//!
//! 产出：
//!
//! | 指标 | 维度 | 说明 |
//! | --- | --- | --- |
//! | `ebpf_syscall_duration_micros` | `op`、`process_name`、`field=avg\|p95` | 每个 flush 周期的调用耗时 |
//! | `ebpf_syscall_failures_total` | `op`、`errno` | 返回值为负的调用次数 |
//!
//! `p95` 由直方图槽**上界**近似（与边指标同一套 `hist_slot` 映射）：槽 `i` 覆盖
//! `[2^i, 2^(i+1))` 微秒，取累积到 95% 的槽上界，属于**近似值**，不能与精确分位数相加。
//!
//! 时间戳用**本次 flush 的时刻**而不是分钟桶：同一个分钟桶内多次 flush 会因
//! 「同 measurement+labels+timestamp 覆盖」而互相丢点（边指标在服务端派生时用分钟桶是对的，
//! 因为那边一次写一批；这里每个 flush 都在写新值）。

use std::collections::BTreeMap;

use ebpf_abi::{
    syscall_op_name, CfgIndex, SlowIoEvent, SyscallAggWire, SyscallErrKey, SyscallKey,
    SLOW_IO_PATH_LEN, SYSCALL_OP_COUNT,
};

use crate::now_micros;

/// 一个周期内的 per-CPU syscall 聚合快照。
pub type SyscallSnapshot = Vec<(SyscallKey, Vec<SyscallAggWire>)>;

/// 一个周期内的 per-CPU 错误码计数快照。
pub type SyscallErrSnapshot = Vec<(SyscallErrKey, Vec<u64>)>;

/// 用户态视图（直方图用 `Vec`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyscallAgg {
    pub calls: u64,
    pub errors: u64,
    pub duration_sum_us: u64,
    pub duration_max_us: u64,
    pub hist: Vec<u64>,
}

/// per-CPU 求和。
#[must_use]
pub fn sum_syscall(values: &[SyscallAggWire]) -> SyscallAgg {
    let mut out = SyscallAgg {
        hist: vec![0; ebpf_abi::HIST_SLOTS],
        ..Default::default()
    };
    for value in values {
        out.calls = out.calls.saturating_add(value.calls);
        out.errors = out.errors.saturating_add(value.errors);
        out.duration_sum_us = out.duration_sum_us.saturating_add(value.duration_sum_us);
        out.duration_max_us = out.duration_max_us.max(value.duration_max_us);
        for (slot, count) in value.hist.iter().enumerate() {
            if slot < out.hist.len() {
                out.hist[slot] = out.hist[slot].saturating_add(*count);
            }
        }
    }
    out
}

/// 是否为空增量（没有调用也没有错误）。
#[must_use]
pub fn syscall_is_empty(agg: &SyscallAgg) -> bool {
    agg.calls == 0 && agg.errors == 0
}

/// 直方图近似的 p95（槽上界）；样本为空返回 `None`。
#[must_use]
pub fn histogram_p95(hist: &[u64]) -> Option<u64> {
    let total: u64 = hist.iter().copied().fold(0u64, u64::saturating_add);
    if total == 0 {
        return None;
    }
    let target = total.saturating_mul(95).div_ceil(100);
    let mut cumulative = 0u64;
    for (slot, count) in hist.iter().enumerate() {
        cumulative = cumulative.saturating_add(*count);
        if cumulative >= target {
            return Some(slot_upper_micros(slot));
        }
    }
    Some(slot_upper_micros(hist.len().saturating_sub(1)))
}

/// 槽 `i` 的标称上界 `2^(i+1)`；**最后一槽**（`HIST_SLOTS-1`）是溢出槽，报 `u64::MAX`。
///
/// 判断只看 `HIST_SLOTS` 而不是调用方给出的切片长度：切片可能比 `HIST_SLOTS` 短
/// （历史数据/测试），那不代表它是溢出槽。
fn slot_upper_micros(slot: usize) -> u64 {
    if slot + 1 >= ebpf_abi::HIST_SLOTS {
        u64::MAX
    } else {
        1u64 << (slot + 1)
    }
}

/// 进程名（去掉结尾 NUL 并保证 UTF-8）。
#[must_use]
pub fn comm_str(comm: &[u8; ebpf_abi::TASK_COMM_LEN]) -> String {
    let end = comm.iter().position(|b| *b == 0).unwrap_or(comm.len());
    String::from_utf8_lossy(&comm[..end]).to_string()
}

/// 由增量产生 `ebpf_syscall_duration_micros` 指标点（`avg` 与 `p95` 各一条）。
///
/// `service` 维度留空：由 `dataserver` 用静态映射补齐（Agent 不知道全局服务表）。
#[must_use]
pub fn duration_metrics(
    agent_id: &str,
    item_id: &str,
    key: &SyscallKey,
    delta: &SyscallAgg,
    timestamp: i64,
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let op = syscall_op_name(key.op);
    let process_name = comm_str(&key.comm);
    // **不发 `service`**：Agent 不知道全局服务表，标签由 dataserver 用静态映射补
    // （发一个空值会让服务端的「已有 service 就不覆盖」判断提前命中，见 `MetricSink`）。
    let tags = |field: &str| {
        serde_json::json!({
            "op": op,
            "process_name": process_name,
            "field": field,
            "agent_id": agent_id,
            "item_id": item_id,
        })
    };
    if delta.calls > 0 {
        out.push(serde_json::json!({
            "record_id": format!("{agent_id}:{item_id}:syscall_avg:{op}:{timestamp}"),
            "timestamp": timestamp,
            "measurement": "ebpf_syscall_duration_micros",
            "tags": tags("avg"),
            "field_name": "value",
            "field_value": delta.duration_sum_us as f64 / delta.calls as f64,
        }));
    }
    if let Some(p95) = histogram_p95(&delta.hist) {
        out.push(serde_json::json!({
            "record_id": format!("{agent_id}:{item_id}:syscall_p95:{op}:{timestamp}"),
            "timestamp": timestamp,
            "measurement": "ebpf_syscall_duration_micros",
            "tags": tags("p95"),
            "field_name": "value",
            // 溢出槽的上界是 u64::MAX，转 f64 会失去精度但仍是「极大」；用 i64 上限更可读。
            "field_value": if p95 == u64::MAX { i64::MAX as f64 } else { p95 as f64 },
        }));
    }
    out
}

/// 由错误码增量产生 `ebpf_syscall_failures_total` 指标点（维度：`op`、`errno`）。
#[must_use]
pub fn failure_metrics(
    agent_id: &str,
    item_id: &str,
    key: &SyscallErrKey,
    count: u64,
    timestamp: i64,
) -> Option<serde_json::Value> {
    if count == 0 {
        return None;
    }
    let op = syscall_op_name(key.op);
    Some(serde_json::json!({
        "record_id": format!("{agent_id}:{item_id}:syscall_err:{op}:{}:{timestamp}", key.errno),
        "timestamp": timestamp,
        "measurement": "ebpf_syscall_failures_total",
        "tags": {
            "op": op,
            "errno": key.errno.to_string(),
            "agent_id": agent_id,
            "item_id": item_id,
        },
        "field_name": "value",
        "field_value": count as f64,
    }))
}

/// 慢调用事件 → `data_type=ebpf` 记录（`event_type=slow_io`）。
///
/// 字段名与 `dataserver` 的 `EbpfRecord` 对齐（`event_type`/`process_name`/`message`/`labels`）——
/// 早期原始事件用 `kind`/`comm` 导致整批被拒，这里沿用修好后的口径。
#[must_use]
pub fn slow_io_record(
    agent_id: &str,
    event: &SlowIoEvent,
    monotonic_offset_micros: i64,
) -> serde_json::Value {
    let op = syscall_op_name(event.op);
    let comm = comm_str(&event.comm);
    let path = slow_io_path(event);
    let timestamp = if event.timestamp_ns == 0 {
        now_micros()
    } else {
        (event.timestamp_ns / 1_000) as i64 + monotonic_offset_micros
    };
    let message = if path.is_empty() {
        format!("slow_io {op} {comm} {}us", event.duration_us)
    } else {
        format!("slow_io {op} {comm} {}us {path}", event.duration_us)
    };
    serde_json::json!({
        "record_id": format!(
            "{agent_id}:{}:slow_io:{}:{}",
            event.timestamp_ns, event.pid, event.op
        ),
        "timestamp": timestamp,
        "event_type": "slow_io",
        "pid": event.pid as i64,
        "process_name": comm,
        "message": message,
        "labels": {
            "op": op,
            "cgroup_id": event.cgroup_id.to_string(),
            "duration_us": event.duration_us.to_string(),
            "errno": event.errno.to_string(),
            "path": path,
        },
    })
}

/// 慢调用事件里的路径（截断到 [`SLOW_IO_PATH_LEN`]，去掉结尾 NUL）。
///
/// 内核写的是「字符串长度（含 NUL）」，这里按 NUL 截断后再做 UTF-8 容错。
#[must_use]
pub fn slow_io_path(event: &SlowIoEvent) -> String {
    let len = (event.path_len as usize).min(SLOW_IO_PATH_LEN);
    let bytes = &event.path[..len];
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).to_string()
}

/// 慢调用事件 → `Event` 分类用的 CFG 下标占位（让调用方不必依赖 `CfgIndex`）。
pub const SLOW_IO_CFG_HINT: CfgIndex = CfgIndex::SlowThresholdMicros;

/// 采集项支持的 op 数量（用户态建数组用）。
pub const OPS: usize = SYSCALL_OP_COUNT;

/// `op` → 中文/英文标签（指标维度用的就是 [`syscall_op_name`]，这里给日志用）。
#[must_use]
pub fn describe_op(op: u32) -> &'static str {
    syscall_op_name(op)
}

/// 便于测试与排障：把一批聚合转成 `(op, process_name, calls)` 三元组列表。
#[must_use]
pub fn summarize(snapshot: &SyscallSnapshot) -> BTreeMap<(String, String), u64> {
    let mut out: BTreeMap<(String, String), u64> = BTreeMap::new();
    for (key, values) in snapshot {
        let agg = sum_syscall(values);
        *out.entry((syscall_op_name(key.op).to_string(), comm_str(&key.comm)))
            .or_default() += agg.calls;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(op: u32, comm: &str) -> SyscallKey {
        let mut name = [0u8; ebpf_abi::TASK_COMM_LEN];
        name[..comm.len()].copy_from_slice(comm.as_bytes());
        SyscallKey {
            pid: 42,
            _pad: 0,
            cgroup_id: 7,
            op,
            _pad2: 0,
            comm: name,
        }
    }

    fn agg(calls: u64, sum: u64, max: u64, slot: usize) -> SyscallAggWire {
        let mut hist = [0u64; ebpf_abi::HIST_SLOTS];
        hist[slot] = calls;
        SyscallAggWire {
            calls,
            errors: 0,
            duration_sum_us: sum,
            duration_max_us: max,
            _pad: 0,
            hist,
        }
    }

    #[test]
    fn sums_per_cpu_copies() {
        let total = sum_syscall(&[
            agg(2, 100, 60, 3),
            agg(3, 200, 90, 3),
            SyscallAggWire::default(),
        ]);
        assert_eq!(total.calls, 5);
        assert_eq!(total.duration_sum_us, 300);
        assert_eq!(total.duration_max_us, 90, "最大值取 max 而不是相加");
        assert_eq!(total.hist[3], 5);
        assert_eq!(histogram_p95(&total.hist), Some(16), "槽 3 的上界是 16us");
    }

    #[test]
    fn histograms_yield_approximate_p95() {
        assert_eq!(histogram_p95(&[]), None);
        assert_eq!(histogram_p95(&[0, 0, 0]), None);
        // 95% 落在槽 2（上界 8us）。
        assert_eq!(histogram_p95(&[1, 3, 96]), Some(8));
        // 溢出槽：取极大值再在指标侧压成 i64::MAX。
        let mut hist = vec![0u64; ebpf_abi::HIST_SLOTS];
        hist[ebpf_abi::HIST_SLOTS - 1] = 5;
        assert_eq!(histogram_p95(&hist), Some(u64::MAX));
    }

    #[test]
    fn duration_metrics_carry_required_dimensions() {
        let points = duration_metrics(
            "agent-1",
            "item-1",
            &key(ebpf_abi::SYSCALL_OP_OPENAT, "nginx"),
            &SyscallAgg {
                calls: 4,
                errors: 0,
                duration_sum_us: 400,
                duration_max_us: 200,
                hist: vec![0, 0, 4],
            },
            1_700_000_000_000_000,
        );
        assert_eq!(points.len(), 2, "avg 与 p95 各一条");
        let avg = points
            .iter()
            .find(|p| p["tags"]["field"] == "avg")
            .expect("avg 点");
        assert_eq!(avg["measurement"], "ebpf_syscall_duration_micros");
        assert_eq!(avg["tags"]["op"], "openat");
        assert_eq!(avg["tags"]["process_name"], "nginx");
        assert!(
            avg["tags"].get("service").is_none(),
            "不发 service：由 dataserver 补（发空值会让服务端判定「已有 service」而不补）"
        );
        assert_eq!(avg["field_value"], 100.0);
        let p95 = points
            .iter()
            .find(|p| p["tags"]["field"] == "p95")
            .expect("p95 点");
        assert_eq!(p95["field_value"], 8.0, "槽 2 的上界");
        // 没有调用就不出 avg 点。
        assert!(duration_metrics(
            "a",
            "i",
            &key(ebpf_abi::SYSCALL_OP_READ, "x"),
            &SyscallAgg::default(),
            0
        )
        .is_empty());
    }

    #[test]
    fn failure_metrics_are_skipped_when_zero() {
        let key = SyscallErrKey {
            errno: 2,
            op: ebpf_abi::SYSCALL_OP_OPENAT,
        };
        assert!(failure_metrics("a", "i", &key, 0, 1).is_none());
        let point = failure_metrics("a", "i", &key, 3, 1).expect("有点");
        assert_eq!(point["measurement"], "ebpf_syscall_failures_total");
        assert_eq!(point["tags"]["op"], "openat");
        assert_eq!(point["tags"]["errno"], "2");
        assert_eq!(point["field_value"], 3.0);
    }

    #[test]
    fn slow_io_record_matches_record_contract() {
        let mut event = SlowIoEvent {
            op: ebpf_abi::SYSCALL_OP_OPENAT,
            path_len: 11,
            pid: 42,
            errno: 0,
            timestamp_ns: 1_700_000_000_123_456_789,
            cgroup_id: 7,
            duration_us: 250_000,
            comm: [0u8; ebpf_abi::TASK_COMM_LEN],
            path: [0u8; SLOW_IO_PATH_LEN],
        };
        event.comm[..5].copy_from_slice(b"nginx");
        event.path[..10].copy_from_slice(b"/data/a.db");
        let record = slow_io_record("agent-1", &event, -1_700_000_000_000_000);
        // 字段名必须与 dataserver 的 `EbpfRecord` 一致。
        assert_eq!(record["event_type"], "slow_io");
        assert_eq!(record["process_name"], "nginx");
        assert_eq!(record["pid"], 42);
        assert_eq!(record["labels"]["op"], "openat");
        assert_eq!(record["labels"]["path"], "/data/a.db");
        assert!(record["message"]
            .as_str()
            .unwrap()
            .contains("slow_io openat nginx 250000us /data/a.db"));
        // 时间戳 = 单调时钟 + 偏移。
        assert_eq!(record["timestamp"].as_i64().unwrap(), 123_456);
    }

    #[test]
    fn slow_io_path_is_truncated_and_lossy_safe() {
        let mut path = [0u8; SLOW_IO_PATH_LEN];
        path[..3].copy_from_slice(b"/a\0");
        let event = SlowIoEvent {
            path_len: 3,
            path,
            ..Default::default()
        };
        assert_eq!(slow_io_path(&event), "/a", "按 NUL 截断");
        // 非法 UTF-8 不 panic（用替换字符）。
        let mut path = [0u8; SLOW_IO_PATH_LEN];
        path[..3].copy_from_slice(&[0xff, 0xfe, 0x00]);
        let event = SlowIoEvent {
            path_len: 3,
            path,
            ..Default::default()
        };
        assert!(!slow_io_path(&event).is_empty());
        // 超长 path_len 不越界。
        let event = SlowIoEvent {
            path_len: u32::MAX,
            ..Default::default()
        };
        let _ = slow_io_path(&event);
    }

    #[test]
    fn summarize_groups_by_op_and_process() {
        let snapshot = vec![
            (
                key(ebpf_abi::SYSCALL_OP_READ, "java"),
                vec![agg(2, 20, 15, 1)],
            ),
            (
                key(ebpf_abi::SYSCALL_OP_READ, "java"),
                vec![agg(3, 30, 25, 1)],
            ),
            (
                key(ebpf_abi::SYSCALL_OP_WRITE, "java"),
                vec![agg(1, 5, 5, 0)],
            ),
        ];
        let summary = summarize(&snapshot);
        assert_eq!(
            summary
                .get(&("read".to_string(), "java".to_string()))
                .copied(),
            Some(5)
        );
        assert_eq!(
            summary
                .get(&("write".to_string(), "java".to_string()))
                .copied(),
            Some(1)
        );
    }
}
