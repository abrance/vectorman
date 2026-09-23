//! eBPF 采集项配置：过滤、桶宽、资源上限与原始事件开关。
//!
//! 设计（`/.monkeycode/specs/ebpf-observability/requirements.md` Requirement 2 与
//! `design.md`「资源限制」）：所有字段都可缺省，越界值在这里夹取（GSE 侧已有一道校验）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 缺省桶宽（秒）。
pub const DEFAULT_BUCKET_SECS: i64 = 10;
/// 缺省 flush 间隔（秒）。
pub const DEFAULT_FLUSH_INTERVAL_SECS: u64 = 10;
/// 缺省直方图槽数。
pub const DEFAULT_HIST_SLOTS: usize = 24;
/// 缺省 per-CPU map 条目上限。
pub const DEFAULT_MAP_MAX_ENTRIES: u64 = 16_384;
/// 缺省 CPU 占用上限（百分比）。
pub const DEFAULT_MAX_CPU_PERCENT: u32 = 5;
/// 缺省每秒事件上限。
pub const DEFAULT_MAX_EVENTS_PER_SEC: u64 = 50_000;
/// 缺省环缓冲容量（字节）。
pub const DEFAULT_RING_BUFFER_BYTES: usize = 256 * 1024;
/// 缺省直方图槽上限（与内核态编译期数组一致）。
pub const MAX_HIST_SLOTS: usize = 32;

/// eBPF 采集配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EbpfConfig {
    pub bucket_secs: i64,
    pub flush_interval_secs: u64,
    pub hist_slots: usize,
    pub cgroup_include: Vec<String>,
    pub cgroup_exclude: Vec<String>,
    pub process_include: Vec<String>,
    pub process_exclude: Vec<String>,
    pub port_include: Vec<u16>,
    pub port_exclude: Vec<u16>,
    pub include_loopback: bool,
    pub raw_events_enabled: bool,
    pub raw_events_sample_ratio: f64,
    pub max_cpu_percent: u32,
    pub max_events_per_sec: u64,
    pub map_max_entries: u64,
    pub ring_buffer_bytes: usize,
}

impl Default for EbpfConfig {
    fn default() -> Self {
        Self {
            bucket_secs: DEFAULT_BUCKET_SECS,
            flush_interval_secs: DEFAULT_FLUSH_INTERVAL_SECS,
            hist_slots: DEFAULT_HIST_SLOTS,
            cgroup_include: Vec::new(),
            cgroup_exclude: Vec::new(),
            process_include: Vec::new(),
            process_exclude: Vec::new(),
            port_include: Vec::new(),
            port_exclude: Vec::new(),
            include_loopback: false,
            raw_events_enabled: false,
            raw_events_sample_ratio: 0.01,
            max_cpu_percent: DEFAULT_MAX_CPU_PERCENT,
            max_events_per_sec: DEFAULT_MAX_EVENTS_PER_SEC,
            map_max_entries: DEFAULT_MAP_MAX_ENTRIES,
            ring_buffer_bytes: DEFAULT_RING_BUFFER_BYTES,
        }
    }
}

impl EbpfConfig {
    /// 从采集项 `collector` JSON 解析；缺省与越界值按设计夹取。
    #[must_use]
    pub fn from_value(value: &Value) -> Self {
        let default = Self::default();
        let strings = |key: &str| -> Vec<String> {
            value
                .get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let ports = |key: &str| -> Vec<u16> {
            value
                .get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_u64)
                        .filter_map(|port| u16::try_from(port).ok())
                        .collect()
                })
                .unwrap_or_default()
        };
        let bucket_secs = value
            .get("bucket_secs")
            .and_then(Value::as_i64)
            .map_or(default.bucket_secs, |v| v.clamp(1, 60));
        let flush = value
            .get("flush_interval_secs")
            .and_then(Value::as_u64)
            .map_or(default.flush_interval_secs, |v| v.clamp(1, 60));
        let hist_slots = value
            .get("hist_slots")
            .and_then(Value::as_u64)
            .map_or(default.hist_slots, |v| {
                (v as usize).clamp(1, MAX_HIST_SLOTS)
            });
        let ratio = value
            .get("raw_events_sample_ratio")
            .and_then(Value::as_f64)
            .map_or(default.raw_events_sample_ratio, |v| v.clamp(0.0, 1.0));
        Self {
            bucket_secs,
            flush_interval_secs: flush,
            hist_slots,
            cgroup_include: strings("cgroup_include"),
            cgroup_exclude: strings("cgroup_exclude"),
            process_include: strings("process_include"),
            process_exclude: strings("process_exclude"),
            port_include: ports("port_include"),
            port_exclude: ports("port_exclude"),
            include_loopback: value
                .get("include_loopback")
                .and_then(Value::as_bool)
                .unwrap_or(default.include_loopback),
            raw_events_enabled: value
                .get("raw_events_enabled")
                .and_then(Value::as_bool)
                .unwrap_or(default.raw_events_enabled),
            raw_events_sample_ratio: ratio,
            max_cpu_percent: value
                .get("max_cpu_percent")
                .and_then(Value::as_u64)
                .map_or(default.max_cpu_percent, |v| v.min(100) as u32),
            max_events_per_sec: value
                .get("max_events_per_sec")
                .and_then(Value::as_u64)
                .unwrap_or(default.max_events_per_sec),
            map_max_entries: value
                .get("map_max_entries")
                .and_then(Value::as_u64)
                .map_or(default.map_max_entries, |v| v.clamp(64, 1_048_576)),
            ring_buffer_bytes: value
                .get("ring_buffer_bytes")
                .and_then(Value::as_u64)
                .map_or(default.ring_buffer_bytes, |v| {
                    (v as usize).clamp(4_096, 16 * 1024 * 1024)
                }),
        }
    }
}

/// 过滤输入：内核态只有键与（反查得到的）进程上下文。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterInput {
    pub src_ip: String,
    pub dst_ip: String,
    pub src_port: u16,
    pub dst_port: u16,
    pub cgroup_id: u64,
    pub process_name: String,
    pub pod_name: String,
}

/// 是否保留该连接的增量。
///
/// 规则：回环默认丢弃；`port_include` 非空时只保留命中的端口（源或目标）；
/// `port_exclude` 命中即丢弃；`cgroup_*` 与 `process_*` 用前缀匹配（cgroup 用十进制字符串）。
#[must_use]
pub fn keep(input: &FilterInput, cfg: &EbpfConfig) -> bool {
    if !cfg.include_loopback && (is_loopback(&input.src_ip) || is_loopback(&input.dst_ip)) {
        return false;
    }
    if !cfg.port_include.is_empty()
        && !cfg.port_include.contains(&input.src_port)
        && !cfg.port_include.contains(&input.dst_port)
    {
        return false;
    }
    if cfg.port_exclude.contains(&input.src_port) || cfg.port_exclude.contains(&input.dst_port) {
        return false;
    }
    let cgroup = input.cgroup_id.to_string();
    if cfg
        .cgroup_exclude
        .iter()
        .any(|prefix| cgroup.starts_with(prefix))
    {
        return false;
    }
    if !cfg.cgroup_include.is_empty()
        && !cfg
            .cgroup_include
            .iter()
            .any(|prefix| cgroup.starts_with(prefix))
    {
        return false;
    }
    if cfg
        .process_exclude
        .iter()
        .any(|name| !input.process_name.is_empty() && input.process_name == *name)
    {
        return false;
    }
    if !cfg.process_include.is_empty() {
        let hit = cfg
            .process_include
            .iter()
            .any(|name| !input.process_name.is_empty() && input.process_name == *name);
        if !hit {
            return false;
        }
    }
    true
}

#[must_use]
pub fn is_loopback(ip: &str) -> bool {
    ip.starts_with("127.") || ip == "::1" || ip.starts_with("::ffff:127.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_clamping() {
        let default = EbpfConfig::default();
        assert_eq!(default.bucket_secs, 10);
        assert_eq!(default.hist_slots, 24);
        assert!(!default.include_loopback);
        assert!(!default.raw_events_enabled);
        assert_eq!(default.max_cpu_percent, 5);

        let cfg = EbpfConfig::from_value(&serde_json::json!({
            "bucket_secs": 999,
            "flush_interval_secs": 0,
            "hist_slots": 99,
            "raw_events_sample_ratio": 5.0,
            "max_cpu_percent": 500,
            "map_max_entries": 1,
            "ring_buffer_bytes": 1,
            "include_loopback": true,
            "port_exclude": [22, 70000],
            "cgroup_include": ["123"],
        }));
        assert_eq!(cfg.bucket_secs, 60, "上限 60 秒");
        assert_eq!(cfg.flush_interval_secs, 1, "下限 1 秒");
        assert_eq!(cfg.hist_slots, MAX_HIST_SLOTS);
        assert_eq!(cfg.raw_events_sample_ratio, 1.0);
        assert_eq!(cfg.max_cpu_percent, 100);
        assert_eq!(cfg.map_max_entries, 64);
        assert_eq!(cfg.ring_buffer_bytes, 4_096);
        assert!(cfg.include_loopback);
        assert_eq!(cfg.port_exclude, vec![22], "超出 u16 的端口被忽略");
        assert_eq!(cfg.cgroup_include, vec!["123".to_string()]);
    }

    fn input() -> FilterInput {
        FilterInput {
            src_ip: "10.0.0.5".into(),
            dst_ip: "10.0.0.9".into(),
            src_port: 40_000,
            dst_port: 8_080,
            cgroup_id: 1_234,
            process_name: "java".into(),
            pod_name: "order-api-1".into(),
        }
    }

    #[test]
    fn loopback_and_port_filters() {
        let mut cfg = EbpfConfig::default();
        assert!(keep(&input(), &cfg));
        assert!(!keep(
            &FilterInput {
                dst_ip: "127.0.0.1".into(),
                ..input()
            },
            &cfg
        ));
        cfg.include_loopback = true;
        assert!(keep(
            &FilterInput {
                dst_ip: "127.0.0.1".into(),
                ..input()
            },
            &cfg
        ));

        let mut cfg = EbpfConfig {
            port_include: vec![8_080],
            ..EbpfConfig::default()
        };
        assert!(keep(&input(), &cfg), "目标端口命中");
        assert!(!keep(
            &FilterInput {
                dst_port: 5_432,
                ..input()
            },
            &cfg
        ));
        cfg.port_exclude = vec![8_080];
        assert!(!keep(&input(), &cfg), "exclude 优先于 include");
    }

    #[test]
    fn cgroup_and_process_filters() {
        let mut cfg = EbpfConfig {
            cgroup_include: vec!["12".into()],
            ..EbpfConfig::default()
        };
        assert!(keep(&input(), &cfg), "1234 以 12 开头");
        cfg.cgroup_include = vec!["99".into()];
        assert!(!keep(&input(), &cfg));
        cfg.cgroup_include.clear();
        cfg.cgroup_exclude = vec!["123".into()];
        assert!(!keep(&input(), &cfg), "exclude 命中即丢弃");

        let mut cfg = EbpfConfig {
            process_include: vec!["java".into()],
            ..EbpfConfig::default()
        };
        assert!(keep(&input(), &cfg));
        assert!(!keep(
            &FilterInput {
                process_name: "python".into(),
                ..input()
            },
            &cfg
        ));
        cfg.process_include.clear();
        cfg.process_exclude = vec!["java".into()];
        assert!(!keep(&input(), &cfg));
        // 进程名未知时不因 include/exclude 被误杀（内核态可能取不到）。
        cfg.process_exclude.clear();
        cfg.process_include = vec!["java".into()];
        assert!(!keep(&FilterInput::default(), &cfg));
    }
}
