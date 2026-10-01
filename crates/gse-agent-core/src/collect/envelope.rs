//! 上报信封与四类记录的最小 DTO：Agent 侧只负责构造，不依赖存储实现。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 采集类型标识。
pub const DATA_TYPE_METRICS: &str = "metrics";
pub const DATA_TYPE_LOGS: &str = "logs";
pub const DATA_TYPE_TRACES: &str = "traces";

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
    /// 被拒记录（`status=partial` 时非空）。
    ///
    /// 必须解析出来：Agent 此前**完全忽略**它 —— 服务端把整批边拒成 `partial`
    /// （实测 6400+ 条只入库 1 条），而 Agent 侧零日志、零重试，数据静默消失。
    #[serde(default)]
    pub failures: Vec<IngestFailure>,
}

/// 单条被拒记录的原因。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct IngestFailure {
    #[serde(default)]
    pub record_id: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub message: String,
}

/// span 内的事件（对齐 OTel `Span.Event`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanEventRecord {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub time_unix_nano: i64,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

/// span 的关联链接（对齐 OTel `Span.Link`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanLinkRecord {
    #[serde(default)]
    pub trace_id: String,
    #[serde(default)]
    pub span_id: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

/// `data_type=traces` 的记录：字段与 dataserver 的 `TraceSpan` 一一对应。
///
/// Agent 侧只做「OTLP → 本结构」的转封，不做采样、不做聚合，也不落本地存储。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceSpanRecord {
    pub record_id: String,
    pub timestamp: i64,
    pub trace_id: String,
    pub span_id: String,
    #[serde(default)]
    pub parent_span_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub start_unix_nano: i64,
    #[serde(default)]
    pub end_unix_nano: i64,
    #[serde(default)]
    pub status_code: String,
    #[serde(default)]
    pub status_message: String,
    #[serde(default)]
    pub trace_flags: u8,
    #[serde(default)]
    pub service: String,
    #[serde(default)]
    pub resource: BTreeMap<String, String>,
    #[serde(default)]
    pub scope_name: String,
    #[serde(default)]
    pub scope_version: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    #[serde(default)]
    pub events: Vec<SpanEventRecord>,
    #[serde(default)]
    pub links: Vec<SpanLinkRecord>,
    #[serde(default)]
    pub dropped_attributes: u32,
    #[serde(default)]
    pub dropped_events: u32,
    #[serde(default)]
    pub dropped_links: u32,
    #[serde(default)]
    pub collector: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::IngestReply;

    /// **静默丢数回归**：服务端把部分记录拒了会回 `status=partial` + `failures`，
    /// Agent 必须能解析出来才能把原因打到日志里。
    /// 此前 `IngestReply` 没有这个字段 → 「拒了 6400 条只入库 1 条」在 Agent 侧零信号。
    #[test]
    fn parses_partial_reply_failures() {
        let raw = br#"{
            "batch_id":"b-1",
            "accepted":0,
            "status":"partial",
            "failures":[{
                "record_id":"a-1:123:10.0.0.1:1:10.0.0.2:2:tcp",
                "code":"invalid_argument",
                "message":"connections must be >= 1"
            }]
        }"#;
        let reply: IngestReply = serde_json::from_slice(raw).expect("partial 应答必须可解析");
        assert_eq!(reply.status, "partial");
        assert_eq!(reply.accepted, 0);
        assert_eq!(reply.failures.len(), 1);
        assert_eq!(reply.failures[0].message, "connections must be >= 1");
    }

    /// 正常应答不带 `failures` 字段（旧版 dataserver 也不带），必须仍能解析。
    #[test]
    fn parses_ok_reply_without_failures() {
        let raw = br#"{"batch_id":"b-2","accepted":3,"status":"ok"}"#;
        let reply: IngestReply = serde_json::from_slice(raw).expect("ok 应答必须可解析");
        assert_eq!(reply.accepted, 3);
        assert!(reply.failures.is_empty());
    }
}
