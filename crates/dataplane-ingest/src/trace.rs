//! `data_type=traces` 的记录模型、校验、日志映射与派生数据挂钩点。
//!
//! 对应设计：`/.monkeycode/specs/observability-data-model/design.md` 的
//! 「TraceSpan（OTel 语义）」与 `/.monkeycode/specs/apm-tracing/design.md` 的
//! 「接入分支」。字段命名对齐 OpenTelemetry Trace 数据模型，保留原始
//! resource / attributes / events / links，便于无损回溯。
//!
//! 本模块只做「记录级」的事：默认值补全、合法性校验、映射为 `LogRecord`。
//! trace 摘要累加与端点登记等派生数据通过 [`TraceSink`] 钩子外置，由
//! `dataplane-apm` 实现（`dataplane-ingest` 不依赖存储层实现）。

use std::collections::BTreeMap;

use async_trait::async_trait;
use dataplane_core::DataplaneError;
use dataplane_log::LogRecord;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::DataEnvelope;

/// 单条 span 序列化后的上限（256 KiB）。超限记为非法记录，不截断。
pub const MAX_TRACE_SPAN_BYTES: usize = 256 * 1024;

/// 缺失 `service.name` 时的占位值（对齐 OTel 默认值）。
pub const UNKNOWN_SERVICE: &str = "unknown_service";

/// span 内的事件。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpanEvent {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub time_unix_nano: i64,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

/// span 的关联链接。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpanLink {
    #[serde(default)]
    pub trace_id: String,
    #[serde(default)]
    pub span_id: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

/// `data_type=traces` 的记录元素。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceSpan {
    /// 固定为 `"{trace_id}:{span_id}"`。
    pub record_id: String,
    /// Unix 微秒，等于 `start_unix_nano / 1000`。
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
    pub events: Vec<SpanEvent>,
    #[serde(default)]
    pub links: Vec<SpanLink>,
    #[serde(default)]
    pub dropped_attributes: u32,
    #[serde(default)]
    pub dropped_events: u32,
    #[serde(default)]
    pub dropped_links: u32,
    /// `otlp`（应用插桩）或 `ebpf`（内核侧推导）。
    #[serde(default)]
    pub collector: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

impl TraceSpan {
    /// span 时长（微秒），`end < start` 时取 0。
    #[must_use]
    pub fn duration_micros(&self) -> i64 {
        self.end_unix_nano
            .saturating_sub(self.start_unix_nano)
            .max(0)
            / 1_000
    }

    /// 根 span：`parent_span_id` 为空。
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.parent_span_id.is_empty()
    }
}

/// 补全默认值并归一化大小写。
///
/// `trace_id` / `span_id` / `parent_span_id` 统一转小写 hex，保证同一 span 的
/// `record_id` 在重复上报时可稳定去重。非法（长度不符或非 hex）时保持原值，
/// 由 [`validate`] 拒绝。
#[must_use]
pub fn normalize(mut span: TraceSpan) -> TraceSpan {
    span.trace_id = span.trace_id.trim().to_ascii_lowercase();
    span.span_id = span.span_id.trim().to_ascii_lowercase();
    span.parent_span_id = span.parent_span_id.trim().to_ascii_lowercase();
    span.record_id = span.record_id.trim().to_ascii_lowercase();
    if span.collector.is_empty() {
        span.collector = "otlp".to_string();
    }
    if span.service.is_empty() {
        span.service = UNKNOWN_SERVICE.to_string();
    }
    if span.kind.is_empty() {
        span.kind = "internal".to_string();
    }
    if span.status_code.is_empty() {
        span.status_code = "unset".to_string();
    }
    if span.timestamp <= 0 {
        span.timestamp = span.start_unix_nano / 1_000;
    }
    if span.record_id.is_empty() && !span.trace_id.is_empty() && !span.span_id.is_empty() {
        span.record_id = format!("{}:{}", span.trace_id, span.span_id);
    }
    span
}

/// 校验 id 与时间字段；返回可读的失败原因。
pub fn validate(span: &TraceSpan) -> Result<(), String> {
    if !is_hex(&span.trace_id, 32) {
        return Err(format!(
            "trace_id must be 32 hex chars, got {:?}",
            span.trace_id
        ));
    }
    if !is_hex(&span.span_id, 16) {
        return Err(format!(
            "span_id must be 16 hex chars, got {:?}",
            span.span_id
        ));
    }
    if !span.parent_span_id.is_empty() && !is_hex(&span.parent_span_id, 16) {
        return Err(format!(
            "parent_span_id must be empty or 16 hex chars, got {:?}",
            span.parent_span_id
        ));
    }
    if span.record_id != format!("{}:{}", span.trace_id, span.span_id) {
        return Err(format!(
            "record_id must equal trace_id:span_id, got {:?}",
            span.record_id
        ));
    }
    if span.timestamp <= 0 {
        return Err(format!(
            "timestamp must be positive micros, got {}",
            span.timestamp
        ));
    }
    Ok(())
}

/// 单条记录序列化后是否超过 [`MAX_TRACE_SPAN_BYTES`]。
#[must_use]
pub fn exceeds_size_limit(raw: &Value) -> bool {
    // `Value` 的序列化长度即接入请求中的字节量级；超限时不做部分截断。
    serde_json::to_string(raw)
        .map(|s| s.len() > MAX_TRACE_SPAN_BYTES)
        .unwrap_or(false)
}

/// 映射为 `LogStore` 明细记录。
///
/// `labels` 先取 span 自带标签，再被信封公共标签覆盖（与既有 `apm` 映射一致），
/// 最后写入 trace 专有标签；`trace_id` / `service` / `data_id` 三个键会被
/// `LogStore` 提升为索引字段，供 trace 详情全倒排检索。
#[must_use]
pub fn to_log_record(span: &TraceSpan, envelope: &DataEnvelope) -> LogRecord {
    let mut labels = span.labels.clone();
    crate::merge_common_labels(&mut labels, envelope);
    labels.insert("trace_id".into(), span.trace_id.clone());
    labels.insert("span_id".into(), span.span_id.clone());
    labels.insert("parent_span_id".into(), span.parent_span_id.clone());
    labels.insert("service".into(), span.service.clone());
    labels.insert("kind".into(), span.kind.clone());
    labels.insert("status_code".into(), span.status_code.clone());
    labels.insert("collector".into(), span.collector.clone());

    let level = if span.status_code == "error" {
        "error"
    } else {
        "info"
    };
    LogRecord {
        id: span.record_id.clone(),
        timestamp: span.timestamp,
        level: level.to_string(),
        message: format!(
            "{} {} {}us",
            span.service,
            span.name,
            span.duration_micros()
        ),
        labels,
        // 明细原文：标签只投影了少量字段，耗时/attributes/events/links 只能从原文还原
        // （瀑布图与 span 详情依赖它）。序列化失败时退化为「只有标签」，不阻断接入。
        payload: serde_json::to_string(span).ok(),
    }
}

/// trace 摘要与端点登记等派生数据的挂钩点。
///
/// 由 `dataplane-apm` 实现。调用方把失败视为「派生数据失败」而非接入失败：
/// 明细已落库，重试整批只会造成明细重复。
#[async_trait]
pub trait TraceSink: Send + Sync {
    /// 只写明细的耗时下限（微秒）。0 表示全部写明细。
    ///
    /// 策略由 sink（APM 配置）持有：明细是可选的高成本数据，摘要与聚合才是聚合口径的
    /// 来源，因此低于阈值的 span 仍然 `observe_span`、只是不落 `LogStore`。
    fn detail_min_duration_micros(&self) -> i64 {
        0
    }

    /// 观察一条已通过校验与归一化的 span。
    async fn observe_span(
        &self,
        span: &TraceSpan,
        envelope: &DataEnvelope,
    ) -> Result<(), DataplaneError>;
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes().all(|b| b.is_ascii_hexdigit())
        && !s.bytes().any(|b| b.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DataType;

    fn envelope(data_id: &str) -> DataEnvelope {
        DataEnvelope {
            batch_id: "b1".into(),
            data_type: DataType::Traces,
            data_id: data_id.into(),
            agent_id: "agent-1".into(),
            host_id: "host-1".into(),
            sent_at_micros: 1,
            records: Vec::new(),
        }
    }

    pub(crate) fn span() -> TraceSpan {
        TraceSpan {
            record_id: "4bf92f3577b34da6a3ce929d0e0e4736:00f067aa0ba902b7".into(),
            timestamp: 1_700_000_000_000_000,
            trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".into(),
            span_id: "00f067aa0ba902b7".into(),
            parent_span_id: String::new(),
            name: "GET /orders/{id}".into(),
            kind: "server".into(),
            start_unix_nano: 1_700_000_000_000_000_000,
            end_unix_nano: 1_700_000_000_012_000_000,
            status_code: "ok".into(),
            status_message: String::new(),
            trace_flags: 1,
            service: "order-api".into(),
            resource: BTreeMap::new(),
            scope_name: "test".into(),
            scope_version: "1.0".into(),
            attributes: BTreeMap::new(),
            events: Vec::new(),
            links: Vec::new(),
            dropped_attributes: 0,
            dropped_events: 0,
            dropped_links: 0,
            collector: "otlp".into(),
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn normalize_fills_defaults_and_lowercases_ids() {
        let mut s = span();
        s.trace_id = "4BF92F3577B34DA6A3CE929D0E0E4736".into();
        s.span_id = "00F067AA0BA902B7".into();
        s.record_id = String::new();
        s.service = String::new();
        s.kind = String::new();
        s.status_code = String::new();
        s.collector = String::new();
        s.timestamp = 0;
        let n = normalize(s);
        assert_eq!(n.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(n.span_id, "00f067aa0ba902b7");
        assert_eq!(
            n.record_id,
            "4bf92f3577b34da6a3ce929d0e0e4736:00f067aa0ba902b7"
        );
        assert_eq!(n.service, UNKNOWN_SERVICE);
        assert_eq!(n.kind, "internal");
        assert_eq!(n.status_code, "unset");
        assert_eq!(n.collector, "otlp");
        assert_eq!(n.timestamp, 1_700_000_000_000_000);
        assert!(validate(&n).is_ok());
    }

    #[test]
    fn validate_rejects_bad_ids_and_timestamp() {
        for (trace_id, span_id, parent, timestamp) in [
            (
                "short".to_string(),
                "00f067aa0ba902b7".to_string(),
                String::new(),
                1i64,
            ),
            (
                "4bf92f3577b34da6a3ce929d0e0e4736".to_string(),
                "zz".to_string(),
                String::new(),
                1,
            ),
            (
                "4bf92f3577b34da6a3ce929d0e0e4736".to_string(),
                "00f067aa0ba902b7".to_string(),
                "bad".to_string(),
                1,
            ),
            (
                "4bf92f3577b34da6a3ce929d0e0e4736".to_string(),
                "00f067aa0ba902b7".to_string(),
                String::new(),
                0,
            ),
        ] {
            let mut s = span();
            s.trace_id = trace_id;
            s.span_id = span_id;
            s.parent_span_id = parent;
            s.timestamp = timestamp;
            s.record_id = format!("{}:{}", s.trace_id, s.span_id);
            assert!(validate(&s).is_err(), "应拒绝: {s:?}");
        }
    }

    #[test]
    fn to_log_record_maps_message_level_and_labels() {
        let mut s = span();
        s.status_code = "error".into();
        s.labels.insert("env".into(), "prod".into());
        let rec = to_log_record(&s, &envelope("item-1"));
        assert_eq!(rec.id, s.record_id);
        assert_eq!(rec.timestamp, s.timestamp);
        assert_eq!(rec.level, "error");
        assert_eq!(rec.message, "order-api GET /orders/{id} 12000us");
        assert_eq!(rec.labels["trace_id"], s.trace_id);
        assert_eq!(rec.labels["span_id"], s.span_id);
        assert_eq!(rec.labels["service"], "order-api");
        assert_eq!(rec.labels["kind"], "server");
        assert_eq!(rec.labels["status_code"], "error");
        assert_eq!(rec.labels["collector"], "otlp");
        assert_eq!(rec.labels["data_type"], "traces");
        assert_eq!(rec.labels["agent_id"], "agent-1");
        assert_eq!(rec.labels["data_id"], "item-1");
        assert_eq!(rec.labels["host_id"], "host-1");
        assert_eq!(rec.labels["env"], "prod", "span 自带标签应保留");
    }

    #[test]
    fn size_limit_flags_oversized_records() {
        let mut s = span();
        s.attributes
            .insert("big".into(), "x".repeat(MAX_TRACE_SPAN_BYTES));
        let raw = serde_json::to_value(&s).unwrap();
        assert!(exceeds_size_limit(&raw));
        let small = serde_json::to_value(span()).unwrap();
        assert!(!exceeds_size_limit(&small));
    }
}
