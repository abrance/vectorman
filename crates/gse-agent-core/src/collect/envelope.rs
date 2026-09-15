//! 上报信封与四类记录的最小 DTO：Agent 侧只负责构造，不依赖存储实现。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 采集类型标识。
pub const DATA_TYPE_METRICS: &str = "metrics";
pub const DATA_TYPE_LOGS: &str = "logs";

/// 一批采集记录的外包装，字段与 dataserver 接入接口一致。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DataEnvelope {
    pub batch_id: String,
    pub data_type: String,
    pub data_id: String,
    pub agent_id: String,
    #[serde(default)]
    pub host_id: String,
    pub sent_at_micros: i64,
    pub records: Vec<serde_json::Value>,
}

impl DataEnvelope {
    pub fn record_count(&self) -> usize {
        self.records.len()
    }
}

/// `metrics` 记录，写入时序存储。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricsRecord {
    pub record_id: String,
    pub timestamp: i64,
    pub measurement: String,
    pub tags: BTreeMap<String, String>,
    pub field_name: String,
    pub field_value: f64,
}

/// `logs` 记录，写入日志检索存储。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogsRecord {
    pub record_id: String,
    pub timestamp: i64,
    pub level: String,
    pub message: String,
    pub source: String,
    pub labels: BTreeMap<String, String>,
}

/// dataserver 接入应答；`status` 取 `ok` / `partial`。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct IngestReply {
    #[serde(default)]
    pub batch_id: String,
    #[serde(default)]
    pub accepted: u32,
    pub status: String,
    #[serde(default)]
    pub code: Option<String>,
}
