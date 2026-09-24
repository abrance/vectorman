//! 用户态差分与聚合：内核态 per-CPU 计数 → 增量 → 边记录与 `ebpf_*` 指标。
//!
//! 设计（`/.monkeycode/specs/ebpf-observability/design.md`「内核态 map 布局」「用户态差分与聚合」）：
//!
//! - 高频路径只在 per-CPU map 里累加；用户态每 `flush_interval_secs` 读一次快照，
//!   与上一周期相减得到增量，再把内核侧计数**写零复位**。
//! - 首次出现的键按绝对值计（没有上一周期可比）。
//! - 增量全零的键不产生记录（也不产生指标点）。
//! - 桶由 `bucket_secs` 决定（缺省 10 秒）；指标按分钟汇总（缺省保留最近 6 个 10 秒桶）。
//!
//! 本模块只做纯计算，输入是「假 map 快照」，因此可以在无特权环境单测（设计里的测试策略）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// 连接键的唯一权威定义在 `ebpf-abi`（内核态与用户态共用）；这里只重导出。
use ebpf_abi::ConnAggWire;
pub use ebpf_abi::ConnKey;

/// 连接聚合值：与内核态 `ConnAgg` 一一对应。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnAgg {
    pub connections: u64,
    pub failures: u64,
    pub failure_reason: u32,
    pub bytes_sent: u64,
    pub bytes_recv: u64,
    pub duration_sum_us: u64,
    pub duration_max_us: u64,
    pub tcp_retrans: u64,
    pub tcp_resets: u64,
    /// log2 直方图槽（槽 i 表示 `[2^i, 2^(i+1))` 微秒）。
    pub latency_hist: Vec<u64>,
}

/// 失败原因枚举（与内核态 `failure_reason` 数值对应，唯一定义在 `ebpf-abi`）。
pub use ebpf_abi::{
    reason_from_errno, REASON_NONE, REASON_OTHER, REASON_REFUSED, REASON_RESET, REASON_TIMEOUT,
    REASON_UNREACHABLE,
};

/// 失败原因 → 稳定字符串。
#[must_use]
pub fn reason_str(reason: u32) -> &'static str {
    match reason {
        REASON_REFUSED => "refused",
        REASON_TIMEOUT => "timeout",
        REASON_UNREACHABLE => "unreachable",
        REASON_RESET => "reset",
        REASON_OTHER => "other",
        _ => "",
    }
}

/// 内核态 per-CPU 值 → 用户态视图（跨 CPU 求和）。
///
/// 这是 aya `PerCpuHashMap` 读取后的第一步：内核态值类型是固定 32 槽的数组，
/// 用户态视图用 `Vec` 以便后续差分时按槽数对齐。
#[must_use]
pub fn view_per_cpu(values: &[ConnAggWire]) -> ConnAgg {
    let mut out = ConnAgg {
        latency_hist: vec![0; ebpf_abi::HIST_SLOTS],
        ..ConnAgg::default()
    };
    for value in values {
        out.connections = out.connections.saturating_add(value.connections);
        out.failures = out.failures.saturating_add(value.failures);
        out.bytes_sent = out.bytes_sent.saturating_add(value.bytes_sent);
        out.bytes_recv = out.bytes_recv.saturating_add(value.bytes_recv);
        out.duration_sum_us = out.duration_sum_us.saturating_add(value.duration_sum_us);
        out.duration_max_us = out.duration_max_us.max(value.duration_max_us);
        out.tcp_retrans = out.tcp_retrans.saturating_add(value.tcp_retrans);
        out.tcp_resets = out.tcp_resets.saturating_add(value.tcp_resets);
        if value.failures > 0 {
            out.failure_reason = out.failure_reason.max(value.failure_reason);
        }
        for (index, count) in value.latency_hist.iter().enumerate() {
            out.latency_hist[index] = out.latency_hist[index].saturating_add(*count);
        }
    }
    out
}

/// 所有 CPU 副本求和；直方图按位相加，槽数以最长者为准。
#[must_use]
pub fn sum_per_cpu(values: &[ConnAgg]) -> ConnAgg {
    let slots = values
        .iter()
        .map(|v| v.latency_hist.len())
        .max()
        .unwrap_or(0);
    let mut out = ConnAgg {
        latency_hist: vec![0; slots],
        ..ConnAgg::default()
    };
    for value in values {
        out.connections = out.connections.saturating_add(value.connections);
        out.failures = out.failures.saturating_add(value.failures);
        out.bytes_sent = out.bytes_sent.saturating_add(value.bytes_sent);
        out.bytes_recv = out.bytes_recv.saturating_add(value.bytes_recv);
        out.duration_sum_us = out.duration_sum_us.saturating_add(value.duration_sum_us);
        out.duration_max_us = out.duration_max_us.max(value.duration_max_us);
        out.tcp_retrans = out.tcp_retrans.saturating_add(value.tcp_retrans);
        out.tcp_resets = out.tcp_resets.saturating_add(value.tcp_resets);
        // 内核态只记一个枚举值，这里按「较大的枚举优先」汇总（数值越大表示越具体）。
        if value.failures > 0 {
            out.failure_reason = out.failure_reason.max(value.failure_reason);
        }
        for (index, count) in value.latency_hist.iter().enumerate() {
            if index < out.latency_hist.len() {
                out.latency_hist[index] = out.latency_hist[index].saturating_add(*count);
            }
        }
    }
    out
}

/// 增量 = 本周期快照 − 上周期快照；`prev` 为空表示首次出现（按绝对值计）。
#[must_use]
pub fn diff(prev: Option<&ConnAgg>, cur: &ConnAgg) -> ConnAgg {
    let Some(prev) = prev else {
        return cur.clone();
    };
    let slots = cur.latency_hist.len();
    let mut latency_hist = Vec::with_capacity(slots);
    for index in 0..slots {
        let prev_value = prev.latency_hist.get(index).copied().unwrap_or(0);
        latency_hist.push(cur.latency_hist[index].saturating_sub(prev_value));
    }
    ConnAgg {
        connections: cur.connections.saturating_sub(prev.connections),
        failures: cur.failures.saturating_sub(prev.failures),
        failure_reason: if cur.failures > prev.failures {
            cur.failure_reason
        } else {
            REASON_NONE
        },
        bytes_sent: cur.bytes_sent.saturating_sub(prev.bytes_sent),
        bytes_recv: cur.bytes_recv.saturating_sub(prev.bytes_recv),
        duration_sum_us: cur.duration_sum_us.saturating_sub(prev.duration_sum_us),
        duration_max_us: cur.duration_max_us.max(prev.duration_max_us),
        tcp_retrans: cur.tcp_retrans.saturating_sub(prev.tcp_retrans),
        tcp_resets: cur.tcp_resets.saturating_sub(prev.tcp_resets),
        latency_hist,
    }
}

/// 增量是否为空（全零）。空的键不产生记录与指标。
#[must_use]
pub fn is_empty(delta: &ConnAgg) -> bool {
    delta.connections == 0
        && delta.failures == 0
        && delta.bytes_sent == 0
        && delta.bytes_recv == 0
        && delta.duration_sum_us == 0
        && delta.tcp_retrans == 0
        && delta.tcp_resets == 0
        && delta.latency_hist.iter().all(|count| *count == 0)
}

/// 连接键的展示视图（IP/端口；`saddr`/`daddr` 为网络序 IPv4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnView {
    pub src_ip: String,
    pub dst_ip: String,
    pub sport: u16,
    pub dport: u16,
    pub protocol: &'static str,
}

/// 把 `ConnKey` 转成可读视图（IPv4 网络序）。
#[must_use]
pub fn conn_view(key: &ConnKey) -> ConnView {
    ConnView {
        src_ip: ipv4_of(key.saddr),
        dst_ip: ipv4_of(key.daddr),
        sport: key.sport,
        dport: key.dport,
        protocol: if key.protocol == 6 { "tcp" } else { "udp" },
    }
}

#[must_use]
pub fn ipv4_of(raw: u32) -> String {
    let bytes = raw.to_be_bytes();
    format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3])
}

/// Agent 侧边记录：字段与共享模型的 `EbpfEdge` 对齐；服务名留空由 dataserver 反查。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EbpfEdgeRecord {
    pub record_id: String,
    pub timestamp: i64,
    pub bucket_micros: i64,
    pub protocol: String,
    pub src_ip: String,
    pub src_port: u16,
    pub dst_ip: String,
    pub dst_port: u16,
    #[serde(default)]
    pub src_pod: String,
    #[serde(default)]
    pub src_container_id: String,
    #[serde(default)]
    pub src_process: String,
    #[serde(default)]
    pub src_service: String,
    #[serde(default)]
    pub dst_service: String,
    pub connections: u64,
    pub bytes_sent: u64,
    pub bytes_recv: u64,
    pub duration_micros_sum: u64,
    pub duration_micros_max: u64,
    pub tcp_retrans: u64,
    pub tcp_resets: u64,
    pub failures: u64,
    #[serde(default)]
    pub failure_reason: String,
    pub latency_hist: Vec<u64>,
    pub source: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// 进程/容器上下文（由 cgroup 反查得到；PR-B 接 aya 时填充）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessContext {
    pub process_name: String,
    pub container_id: String,
    pub pod_name: String,
}

/// 桶起点：`floor(now / bucket_secs) * bucket_secs`（微秒）。
#[must_use]
pub fn bucket_start(now_micros: i64, bucket_secs: i64) -> i64 {
    let width = bucket_secs.max(1) * 1_000_000;
    now_micros.div_euclid(width) * width
}

/// 由增量构造边记录；增量为空返回 `None`。
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn edge_record(
    agent_id: &str,
    key: &ConnKey,
    delta: &ConnAgg,
    bucket_ts: i64,
    bucket_secs: i64,
    ctx: Option<&ProcessContext>,
) -> Option<EbpfEdgeRecord> {
    if is_empty(delta) {
        return None;
    }
    let view = conn_view(key);
    let ctx = ctx.cloned().unwrap_or_default();
    Some(EbpfEdgeRecord {
        record_id: format!(
            "{agent_id}:{bucket_ts}:{}:{}:{}:{}:{}",
            view.src_ip, view.sport, view.dst_ip, view.dport, view.protocol
        ),
        timestamp: bucket_ts,
        bucket_micros: bucket_secs.max(1) * 1_000_000,
        protocol: view.protocol.to_string(),
        src_ip: view.src_ip,
        src_port: view.sport,
        dst_ip: view.dst_ip,
        dst_port: view.dport,
        src_pod: ctx.pod_name,
        src_container_id: ctx.container_id,
        src_process: ctx.process_name,
        src_service: String::new(),
        dst_service: String::new(),
        connections: delta.connections,
        bytes_sent: delta.bytes_sent,
        bytes_recv: delta.bytes_recv,
        duration_micros_sum: delta.duration_sum_us,
        duration_micros_max: delta.duration_max_us,
        tcp_retrans: delta.tcp_retrans,
        tcp_resets: delta.tcp_resets,
        failures: delta.failures,
        failure_reason: reason_str(delta.failure_reason).to_string(),
        latency_hist: delta.latency_hist.clone(),
        source: "ebpf".to_string(),
        labels: BTreeMap::new(),
    })
}

/// 分钟桶累计值（指标用）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MinuteAgg {
    pub connections: u64,
    pub failures: u64,
    pub bytes_sent: u64,
    pub bytes_recv: u64,
    pub duration_sum_us: u64,
    pub duration_max_us: u64,
    pub tcp_retrans: u64,
    pub tcp_resets: u64,
}

impl MinuteAgg {
    fn add(&mut self, delta: &ConnAgg) {
        self.connections = self.connections.saturating_add(delta.connections);
        self.failures = self.failures.saturating_add(delta.failures);
        self.bytes_sent = self.bytes_sent.saturating_add(delta.bytes_sent);
        self.bytes_recv = self.bytes_recv.saturating_add(delta.bytes_recv);
        self.duration_sum_us = self.duration_sum_us.saturating_add(delta.duration_sum_us);
        self.duration_max_us = self.duration_max_us.max(delta.duration_max_us);
        self.tcp_retrans = self.tcp_retrans.saturating_add(delta.tcp_retrans);
        self.tcp_resets = self.tcp_resets.saturating_add(delta.tcp_resets);
    }
}

/// 分钟指标累加器：把 10 秒桶按分钟汇总，只输出已关闭的分钟桶（设计中的「最近 6 个桶」）。
#[derive(Debug, Default)]
pub struct MinuteAccumulator {
    /// 键：`(分钟桶起点, src_ip, sport, dst_ip, dport, protocol)`。
    buckets: BTreeMap<(i64, String, u16, String, u16, String), MinuteAgg>,
}

impl MinuteAccumulator {
    /// 收集一个边记录的增量。
    pub fn observe(&mut self, edge: &EbpfEdgeRecord) {
        let minute = edge.timestamp.div_euclid(60_000_000) * 60_000_000;
        let key = (
            minute,
            edge.src_ip.clone(),
            edge.src_port,
            edge.dst_ip.clone(),
            edge.dst_port,
            edge.protocol.clone(),
        );
        let delta = ConnAgg {
            connections: edge.connections,
            failures: edge.failures,
            bytes_sent: edge.bytes_sent,
            bytes_recv: edge.bytes_recv,
            duration_sum_us: edge.duration_micros_sum,
            duration_max_us: edge.duration_micros_max,
            tcp_retrans: edge.tcp_retrans,
            tcp_resets: edge.tcp_resets,
            ..ConnAgg::default()
        };
        self.buckets.entry(key).or_default().add(&delta);
    }

    /// 取出所有已关闭的分钟桶（`bucket + 60s <= now`），每条指标一个 JSON 记录。
    pub fn drain_closed(&mut self, now_micros: i64, agent_id: &str) -> Vec<Value> {
        let closed: Vec<_> = self
            .buckets
            .keys()
            .filter(|(minute, ..)| *minute + 60_000_000 <= now_micros)
            .cloned()
            .collect();
        let mut out = Vec::new();
        for key in closed {
            let Some(agg) = self.buckets.remove(&key) else {
                continue;
            };
            let (minute, src_ip, sport, dst_ip, dport, protocol) = key;
            let mut push = |measurement: &str, field: &str, value: f64, extra: &[(&str, &str)]| {
                let mut tags = BTreeMap::new();
                tags.insert("agent_id".to_string(), agent_id.to_string());
                tags.insert("src_ip".to_string(), src_ip.clone());
                tags.insert("src_port".to_string(), sport.to_string());
                tags.insert("dst_ip".to_string(), dst_ip.clone());
                tags.insert("dst_port".to_string(), dport.to_string());
                tags.insert("protocol".to_string(), protocol.clone());
                for (k, v) in extra {
                    tags.insert((*k).to_string(), (*v).to_string());
                }
                out.push(serde_json::json!({
                    "record_id": format!("{agent_id}:{minute}:{measurement}:{field}:{src_ip}:{sport}:{dst_ip}:{dport}"),
                    "timestamp": minute,
                    "measurement": measurement,
                    "tags": tags,
                    "field_name": field,
                    "field_value": value,
                }));
            };
            push(
                "ebpf_edge_connections_total",
                "value",
                agg.connections as f64,
                &[],
            );
            push(
                "ebpf_edge_bytes_total",
                "value",
                agg.bytes_sent as f64,
                &[("direction", "sent")],
            );
            push(
                "ebpf_edge_bytes_total",
                "value",
                agg.bytes_recv as f64,
                &[("direction", "recv")],
            );
            push(
                "ebpf_tcp_retrans_total",
                "value",
                agg.tcp_retrans as f64,
                &[],
            );
            if agg.connections > 0 {
                push(
                    "ebpf_edge_duration_micros",
                    "avg",
                    agg.duration_sum_us as f64 / agg.connections as f64,
                    &[],
                );
                push(
                    "ebpf_edge_duration_micros",
                    "max",
                    agg.duration_max_us as f64,
                    &[],
                );
            }
            if agg.failures > 0 {
                push("ebpf_tcp_failures_total", "value", agg.failures as f64, &[]);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(sport: u16, dport: u16) -> ConnKey {
        ConnKey {
            pid: 42,
            cgroup_id: 7,
            saddr: u32::from_be_bytes([10, 0, 0, 5]),
            daddr: u32::from_be_bytes([10, 0, 0, 9]),
            sport,
            dport,
            protocol: 6,
        }
    }

    fn agg(connections: u64, bytes: u64) -> ConnAgg {
        ConnAgg {
            connections,
            bytes_sent: bytes,
            latency_hist: vec![1, 2],
            ..ConnAgg::default()
        }
    }

    #[test]
    fn view_per_cpu_sums_copies_and_keeps_histogram_shape() {
        let mut cpu0 = ebpf_abi::ConnAggWire {
            connections: 2,
            bytes_sent: 100,
            duration_sum_us: 1_000,
            duration_max_us: 800,
            ..Default::default()
        };
        cpu0.latency_hist[10] = 1;
        let mut cpu1 = ebpf_abi::ConnAggWire {
            connections: 3,
            bytes_sent: 50,
            failures: 1,
            failure_reason: REASON_REFUSED,
            duration_max_us: 900,
            ..Default::default()
        };
        cpu1.latency_hist[10] = 2;

        let view = view_per_cpu(&[cpu0, cpu1]);
        assert_eq!(view.connections, 5);
        assert_eq!(view.bytes_sent, 150);
        assert_eq!(view.failures, 1);
        assert_eq!(view.failure_reason, REASON_REFUSED);
        assert_eq!(view.duration_sum_us, 1_000);
        assert_eq!(view.duration_max_us, 900, "最大值取单调上界");
        assert_eq!(view.latency_hist.len(), ebpf_abi::HIST_SLOTS);
        assert_eq!(view.latency_hist[10], 3, "直方图逐槽相加");
        assert_eq!(view_per_cpu(&[]).latency_hist.len(), ebpf_abi::HIST_SLOTS);
    }

    #[test]
    fn per_cpu_sum_and_histogram() {
        let mut first = agg(2, 100);
        first.latency_hist = vec![1, 0, 3];
        let mut second = agg(3, 50);
        second.latency_hist = vec![0, 2];
        let total = sum_per_cpu(&[first, second]);
        assert_eq!(total.connections, 5);
        assert_eq!(total.bytes_sent, 150);
        assert_eq!(total.latency_hist, vec![1, 2, 3], "槽数取最长并按位相加");
    }

    #[test]
    fn diff_is_absolute_for_first_seen_and_saturating_after() {
        let current = agg(5, 500);
        assert_eq!(diff(None, &current), current, "首次出现按绝对值计");

        let prev = agg(3, 200);
        let delta = diff(Some(&prev), &current);
        assert_eq!(delta.connections, 2);
        assert_eq!(delta.bytes_sent, 300);
        assert_eq!(delta.duration_max_us, current.duration_max_us, "最大值单调");

        // 快照回退（计数被内核侧复位）时不出现负数。
        let smaller = agg(1, 10);
        let delta = diff(Some(&current), &smaller);
        assert_eq!(delta.connections, 0);
        assert_eq!(delta.bytes_sent, 0);
        assert!(is_empty(&delta));
    }

    #[test]
    fn failure_reason_follows_new_failures_only() {
        let mut prev = agg(1, 10);
        prev.failures = 0;
        let mut cur = agg(1, 10);
        cur.failures = 3;
        cur.failure_reason = REASON_REFUSED;
        assert_eq!(diff(Some(&prev), &cur).failure_reason, REASON_REFUSED);

        // 失败数没有增加 → 不重复报告原因。
        let mut same = cur.clone();
        same.connections += 1;
        assert_eq!(diff(Some(&cur), &same).failure_reason, REASON_NONE);
    }

    #[test]
    fn edge_record_skips_empty_delta_and_builds_id() {
        let empty = ConnAgg::default();
        assert!(edge_record("a1", &key(40000, 8080), &empty, 60_000_000, 10, None).is_none());

        let delta = agg(2, 300);
        let record = edge_record(
            "a1",
            &key(40000, 8080),
            &delta,
            60_000_000,
            10,
            Some(&ProcessContext {
                process_name: "java".into(),
                container_id: "c1".into(),
                pod_name: "order-api-1".into(),
            }),
        )
        .unwrap();
        assert_eq!(record.src_ip, "10.0.0.5");
        assert_eq!(record.dst_ip, "10.0.0.9");
        assert_eq!(record.protocol, "tcp");
        assert_eq!(record.bucket_micros, 10_000_000);
        assert_eq!(record.source, "ebpf");
        assert_eq!(record.src_process, "java");
        assert_eq!(record.src_pod, "order-api-1");
        assert!(record.src_service.is_empty(), "服务名由 dataserver 反查");
        assert_eq!(
            record.record_id,
            "a1:60000000:10.0.0.5:40000:10.0.0.9:8080:tcp"
        );
        assert_eq!(record.latency_hist, vec![1, 2]);
    }

    #[test]
    fn bucket_alignment_and_minute_metrics() {
        assert_eq!(bucket_start(1_000_000_000, 10), 1_000_000_000);
        assert_eq!(bucket_start(1_000_000_001, 10), 1_000_000_000);
        assert_eq!(bucket_start(1_009_999_999, 10), 1_000_000_000);
        assert_eq!(bucket_start(1_010_000_000, 10), 1_010_000_000);

        let mut acc = MinuteAccumulator::default();
        // 取一个整分钟起点，使两个 10 秒桶落在同一分钟。
        let base = 1_020_000_000i64;
        assert_eq!(base % 60_000_000, 0);
        for (offset, connections, bytes) in [(0i64, 1u64, 100u64), (10_000_000, 2, 200)] {
            let mut delta = agg(connections, bytes);
            delta.duration_sum_us = 1_000 * connections;
            delta.duration_max_us = 900;
            let record =
                edge_record("a1", &key(40000, 8080), &delta, base + offset, 10, None).unwrap();
            acc.observe(&record);
        }

        // 未到分钟边界不输出。
        assert!(acc.drain_closed(base + 30_000_000, "a1").is_empty());
        let metrics = acc.drain_closed(base + 60_000_000, "a1");
        let connections: Vec<&Value> = metrics
            .iter()
            .filter(|m| m["measurement"] == "ebpf_edge_connections_total")
            .collect();
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0]["field_value"], 3.0, "跨 10 秒桶按分钟求和");
        assert_eq!(connections[0]["timestamp"], base, "指标时间戳为分钟桶起点");
        let bytes: Vec<&Value> = metrics
            .iter()
            .filter(|m| {
                m["measurement"] == "ebpf_edge_bytes_total" && m["tags"]["direction"] == "sent"
            })
            .collect();
        assert_eq!(bytes.len(), 1);
        assert_eq!(bytes[0]["field_value"], 300.0);
        let durations: Vec<&Value> = metrics
            .iter()
            .filter(|m| m["measurement"] == "ebpf_edge_duration_micros")
            .collect();
        assert_eq!(durations.len(), 2, "avg 与 max");
        assert_eq!(
            durations.iter().find(|m| m["field_name"] == "avg").unwrap()["field_value"],
            1_000.0
        );
        // 取出后不重复输出。
        assert!(acc.drain_closed(base + 60_000_000, "a1").is_empty());
    }
}
