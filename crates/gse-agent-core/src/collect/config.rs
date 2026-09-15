//! 采集端配置：从下发 `collector` JSON 解析为强类型，缺省值按规格。

use serde::{Deserialize, Serialize};

use super::clean::CleanConfig;

fn default_interval() -> u64 {
    15
}

fn default_start_mode() -> String {
    "tail".to_string()
}

fn default_batch_max() -> usize {
    100
}

fn default_flush() -> u64 {
    5
}

/// 单个采集项的采集端配置；各 kind 共用同一结构，按需取值。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollectorConfig {
    /// metrics_host 采集间隔（秒），缺省 15。
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    /// log_file 路径 glob（单层，`*` 不跨目录）。
    #[serde(default)]
    pub path_patterns: Vec<String>,
    /// log_k8s_stdout 精确命名空间。
    #[serde(default)]
    pub namespace: String,
    /// log_k8s_stdout Pod 名 glob。
    #[serde(default)]
    pub pod_name_pattern: String,
    /// 精确容器名；空表示该 Pod 全部容器。
    #[serde(default)]
    pub container: String,
    /// kubeconfig 路径；空表示默认 `~/.kube/config`。
    #[serde(default)]
    pub kubeconfig: String,
    /// `head` | `tail`。
    #[serde(default = "default_start_mode")]
    pub start_mode: String,
    #[serde(default)]
    pub start_n: u64,
    #[serde(default = "default_batch_max")]
    pub batch_max_records: usize,
    #[serde(default = "default_flush")]
    pub flush_interval_secs: u64,
    #[serde(default)]
    pub clean: CleanConfig,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            interval_secs: default_interval(),
            path_patterns: Vec::new(),
            namespace: String::new(),
            pod_name_pattern: String::new(),
            container: String::new(),
            kubeconfig: String::new(),
            start_mode: default_start_mode(),
            start_n: 0,
            batch_max_records: default_batch_max(),
            flush_interval_secs: default_flush(),
            clean: CleanConfig::default(),
        }
    }
}

impl CollectorConfig {
    /// 解析下发 JSON；字段缺失走缺省，未知字段忽略。
    pub fn from_value(value: &serde_json::Value) -> Self {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }

    /// 规整后的开始模式：非 `head` 一律按 `tail`。
    pub fn start_mode(&self) -> &str {
        if self.start_mode == "head" {
            "head"
        } else {
            "tail"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_fields_absent() {
        let cfg = CollectorConfig::from_value(&serde_json::json!({}));
        assert_eq!(cfg.interval_secs, 15);
        assert_eq!(cfg.start_mode(), "tail");
        assert_eq!(cfg.start_n, 0);
        assert_eq!(cfg.batch_max_records, 100);
        assert_eq!(cfg.flush_interval_secs, 5);
        assert!(cfg.path_patterns.is_empty());
    }

    #[test]
    fn parses_full_value_and_normalizes_start_mode() {
        let cfg = CollectorConfig::from_value(&serde_json::json!({
            "interval_secs": 5,
            "path_patterns": ["/var/log/*.log"],
            "start_mode": "head",
            "start_n": 3,
            "batch_max_records": 10,
            "flush_interval_secs": 2,
            "clean": {"include_regex": "a", "extract": [{"kind":"json","expr":"k","label":"l"}]}
        }));
        assert_eq!(cfg.interval_secs, 5);
        assert_eq!(cfg.path_patterns, vec!["/var/log/*.log"]);
        assert_eq!(cfg.start_mode(), "head");
        assert_eq!(cfg.start_n, 3);
        assert_eq!(cfg.batch_max_records, 10);
        assert_eq!(cfg.flush_interval_secs, 2);
        assert_eq!(cfg.clean.include_regex.as_deref(), Some("a"));
        assert_eq!(cfg.clean.extract.len(), 1);

        let weird = CollectorConfig::from_value(&serde_json::json!({"start_mode": "bogus"}));
        assert_eq!(weird.start_mode(), "tail");
    }
}
