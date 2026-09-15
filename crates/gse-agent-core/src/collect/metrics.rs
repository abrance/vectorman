//! 主机指标采集：读取 `/proc/stat` 与 `/proc/meminfo`。
//!
//! 第一轮只记录 CPU 快照不发点；此后每轮产出 `cpu_usage` 与 `mem_usage` 各一条。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::envelope::{MetricsRecord, DATA_TYPE_METRICS};
use super::CollectShared;

/// 聚合 CPU 计数快照（各分量之和与 idle 分量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuSnapshot {
    pub total: u64,
    pub idle: u64,
}

/// 解析 `/proc/stat` 首行 `cpu ...` 的数字字段。
pub fn parse_proc_stat(text: &str) -> Option<CpuSnapshot> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let nums: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|s| s.parse::<u64>().ok())
        .collect();
    if nums.len() < 5 {
        return None;
    }
    let total: u64 = nums.iter().sum();
    let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
    Some(CpuSnapshot { total, idle })
}

/// 两轮快照间的非 idle 占比（0–100）。
pub fn cpu_usage_percent(prev: CpuSnapshot, cur: CpuSnapshot) -> f64 {
    let total_delta = cur.total.saturating_sub(prev.total);
    if total_delta == 0 {
        return 0.0;
    }
    let idle_delta = cur.idle.saturating_sub(prev.idle);
    let busy = total_delta.saturating_sub(idle_delta) as f64;
    (busy / total_delta as f64) * 100.0
}

/// 解析 `/proc/meminfo`，返回 `(MemTotal - MemAvailable) / MemTotal * 100`。
pub fn parse_mem_usage(text: &str) -> Option<f64> {
    let mut total: Option<f64> = None;
    let mut available: Option<f64> = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("MemTotal:") {
            total = kb_value(v);
        } else if let Some(v) = line.strip_prefix("MemAvailable:") {
            available = kb_value(v);
        }
    }
    let (total, available) = (total?, available?);
    if total <= 0.0 {
        return None;
    }
    Some(((total - available) / total * 100.0).clamp(0.0, 100.0))
}

fn kb_value(rest: &str) -> Option<f64> {
    rest.split_whitespace().next()?.parse::<f64>().ok()
}

/// 稳定标签指纹：排序后的 `k=v` 拼接。
fn tag_fingerprint(tags: &BTreeMap<String, String>) -> String {
    tags.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// 构造一条指标记录；`record_id` 含标签指纹，保证标签集合区分。
pub fn metrics_record(
    agent_id: &str,
    item_id: &str,
    host_id: Option<&str>,
    measurement: &str,
    field_value: f64,
    timestamp: i64,
) -> MetricsRecord {
    let mut tags = BTreeMap::new();
    tags.insert("agent_id".to_string(), agent_id.to_string());
    tags.insert("item_id".to_string(), item_id.to_string());
    if let Some(h) = host_id.filter(|h| !h.is_empty()) {
        tags.insert("host_id".to_string(), h.to_string());
    }
    let fingerprint = tag_fingerprint(&tags);
    MetricsRecord {
        record_id: format!("{agent_id}:{item_id}:{measurement}:{fingerprint}:{timestamp}"),
        timestamp,
        measurement: measurement.to_string(),
        tags,
        field_name: "value".to_string(),
        field_value,
    }
}

fn read_proc(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// 采集任务：按间隔轮询 `/proc`，每轮把两条指标放入 `data_type=metrics` 批次。
pub async fn run(shared: Arc<CollectShared>, item_id: String, interval_secs: u64) {
    let interval = interval_secs.max(1);
    let mut prev: Option<CpuSnapshot> = None;
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let stat = read_proc("/proc/stat").and_then(|t| parse_proc_stat(&t));
        let mem = read_proc("/proc/meminfo").and_then(|t| parse_mem_usage(&t));
        let now = super::now_micros();
        let host_id = shared.host_id().await;

        let mut records: Vec<Value> = Vec::new();
        if let Some(cur) = stat {
            if let Some(previous) = prev {
                let usage = cpu_usage_percent(previous, cur);
                let rec = metrics_record(
                    &shared.agent_id,
                    &item_id,
                    host_id.as_deref(),
                    "cpu_usage",
                    usage,
                    now,
                );
                records.push(serde_json::to_value(rec).unwrap_or(Value::Null));
            }
            prev = Some(cur);
        }
        if let Some(usage) = mem {
            let rec = metrics_record(
                &shared.agent_id,
                &item_id,
                host_id.as_deref(),
                "mem_usage",
                usage,
                now,
            );
            records.push(serde_json::to_value(rec).unwrap_or(Value::Null));
        }
        if !records.is_empty() {
            shared.push(DATA_TYPE_METRICS, &item_id, records).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT_A: &str = "cpu  100 0 100 800 0 0 0 0 0 0\n";
    const STAT_B: &str = "cpu  200 0 200 1200 0 0 0 0 0 0\n";

    #[test]
    fn parses_proc_stat_and_computes_usage() {
        let a = parse_proc_stat(STAT_A).expect("a");
        let b = parse_proc_stat(STAT_B).expect("b");
        assert_eq!(a.total, 1000);
        assert_eq!(a.idle, 800);
        assert_eq!(b.total, 1600);
        let usage = cpu_usage_percent(a, b);
        assert!((usage - 33.333).abs() < 0.01, "{usage}");
    }

    #[test]
    fn cpu_usage_zero_when_no_delta() {
        let a = parse_proc_stat(STAT_A).unwrap();
        assert_eq!(cpu_usage_percent(a, a), 0.0);
    }

    #[test]
    fn parses_meminfo_usage() {
        let text = "MemTotal:       1000 kB\nMemFree: 100 kB\nMemAvailable:  250 kB\n";
        let usage = parse_mem_usage(text).expect("usage");
        assert!((usage - 75.0).abs() < f64::EPSILON, "{usage}");
    }

    #[test]
    fn metric_record_id_and_tags() {
        let rec = metrics_record("a-1", "i-1", Some("h-1"), "cpu_usage", 12.5, 42);
        assert_eq!(rec.measurement, "cpu_usage");
        assert_eq!(rec.field_name, "value");
        assert_eq!(rec.field_value, 12.5);
        assert_eq!(rec.tags.get("agent_id").map(String::as_str), Some("a-1"));
        assert_eq!(rec.tags.get("item_id").map(String::as_str), Some("i-1"));
        assert_eq!(rec.tags.get("host_id").map(String::as_str), Some("h-1"));
        assert!(rec.record_id.starts_with("a-1:i-1:cpu_usage:"));
        assert!(rec.record_id.ends_with(":42"));

        let no_host = metrics_record("a-1", "i-1", None, "mem_usage", 1.0, 42);
        assert!(!no_host.tags.contains_key("host_id"));
    }
}
