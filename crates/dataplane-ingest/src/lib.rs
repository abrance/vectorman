//! 采集信封 DTO、接入落盘、日志检索映射与 record_id 去重。
//!
//! 新增 `data_type=traces`（OTel 语义 span，见 [`trace`]）：明细写 `LogStore`，
//! 派生数据（trace 摘要、服务端点半）通过 [`trace::TraceSink`] 外置钩子交给
//! `dataplane-apm`，本 crate 不依赖具体存储实现。

use std::collections::BTreeMap;

use dataplane_core::{DataplaneError, ErrorCode};
use dataplane_kv::KvStore;
use dataplane_log::{clamp_log_limit, LogFilter, LogRecord, LogStore};
use dataplane_ts::{TimeSeriesStore, TsPoint};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod edge;
pub mod metric;
pub mod trace;

pub use edge::{EbpfEdge, EdgeSink};
pub use metric::MetricSink;
pub use trace::{SpanEvent, SpanLink, TraceSink, TraceSpan};

/// 采集类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    Metrics,
    Logs,
    Apm,
    Ebpf,
    /// eBPF 边聚合（与 `Ebpf` 原始事件区分：这是聚合，不是明细）。
    EbpfEdges,
    Traces,
}

impl DataType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Metrics => "metrics",
            Self::Logs => "logs",
            Self::Apm => "apm",
            Self::Ebpf => "ebpf",
            Self::EbpfEdges => "ebpf_edges",
            Self::Traces => "traces",
        }
    }
}

/// 一批采集记录的外包装。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataEnvelope {
    pub batch_id: String,
    pub data_type: DataType,
    pub data_id: String,
    pub agent_id: String,
    #[serde(default)]
    pub host_id: String,
    pub sent_at_micros: i64,
    pub records: Vec<Value>,
}

/// 单条失败原因。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordFailure {
    pub record_id: String,
    pub code: String,
    pub message: String,
}

/// 接入应答。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngestReply {
    pub batch_id: String,
    pub accepted: u32,
    pub status: String,
    #[serde(default)]
    pub failures: Vec<RecordFailure>,
}

/// 流索引值。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamIndex {
    pub last_seen_micros: i64,
    pub accepted: u32,
}

/// `metrics` 记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricsRecord {
    pub record_id: String,
    pub timestamp: i64,
    pub measurement: String,
    pub tags: BTreeMap<String, String>,
    pub field_name: String,
    pub field_value: f64,
}

/// `logs` 记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogsRecord {
    pub record_id: String,
    pub timestamp: i64,
    pub level: String,
    pub message: String,
    pub source: String,
    pub labels: BTreeMap<String, String>,
}

/// `apm` 记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApmRecord {
    pub record_id: String,
    pub timestamp: i64,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: String,
    pub service: String,
    pub operation: String,
    pub duration_micros: i64,
    pub status: String,
    pub labels: BTreeMap<String, String>,
}

/// `ebpf` 记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EbpfRecord {
    pub record_id: String,
    pub timestamp: i64,
    pub event_type: String,
    pub pid: i64,
    pub process_name: String,
    pub message: String,
    pub labels: BTreeMap<String, String>,
}

/// `POST /v1/logs/search` 过滤条件。未出现的字段不过滤。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogSearchQuery {
    pub data_type: Option<String>,
    pub agent_id: Option<String>,
    pub host_id: Option<String>,
    pub data_id: Option<String>,
    pub from_ts: Option<i64>,
    pub to_ts: Option<i64>,
    pub level: Option<String>,
    pub message_query: Option<String>,
    pub trace_id: Option<String>,
    pub event_type: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub limit: Option<usize>,
}

/// `ingest/{record_id}` 去重键。
pub fn ingest_key(record_id: &str) -> String {
    format!("ingest/{record_id}")
}

/// `stream/{agent_id}/{data_type}/{data_id}` 流索引键。
pub fn stream_key(agent_id: &str, data_type: DataType, data_id: &str) -> String {
    format!("stream/{}/{}/{}", agent_id, data_type.as_str(), data_id)
}

/// 将一批信封写入时序/日志存储，并用 KvStore 去重。（无派生数据钩子）
pub async fn apply(
    envelope: DataEnvelope,
    ts: &dyn TimeSeriesStore,
    log: &dyn LogStore,
    kv: &dyn KvStore,
) -> Result<IngestReply, DataplaneError> {
    apply_with_trace_sink(envelope, ts, log, kv, None).await
}

/// 接入一批记录，并把 `traces` 的派生数据交给 `trace_sink`（不计 eBPF 边）。
pub async fn apply_with_trace_sink(
    envelope: DataEnvelope,
    ts: &dyn TimeSeriesStore,
    log: &dyn LogStore,
    kv: &dyn KvStore,
    trace_sink: Option<&dyn TraceSink>,
) -> Result<IngestReply, DataplaneError> {
    apply_with_sinks(envelope, ts, log, kv, trace_sink, None, None).await
}

/// 接入一批记录，并把派生数据交给各自的出口
/// （`traces` → `trace_sink`，`ebpf_edges` → `edge_sink`，`metrics` 的维度补全 → `metric_sink`）。
///
/// 出口的失败**不影响**接入应答：明细已落库，重试整批只会造成明细重复，
/// 因此只记录到标准错误并由自监控计数（见 `apm-tracing` 设计「错误处理」）。
pub async fn apply_with_sinks(
    envelope: DataEnvelope,
    ts: &dyn TimeSeriesStore,
    log: &dyn LogStore,
    kv: &dyn KvStore,
    trace_sink: Option<&dyn TraceSink>,
    edge_sink: Option<&dyn EdgeSink>,
    metric_sink: Option<&dyn MetricSink>,
) -> Result<IngestReply, DataplaneError> {
    if envelope.agent_id.trim().is_empty() {
        return Err(DataplaneError::invalid_argument("agent_id is required"));
    }
    if envelope.records.is_empty() {
        return Err(DataplaneError::invalid_argument(
            "records must not be empty",
        ));
    }

    let mut accepted = 0u32;
    let mut failures = Vec::new();

    for raw in &envelope.records {
        let sinks = Sinks {
            trace: trace_sink,
            edge: edge_sink,
            metric: metric_sink,
        };
        match apply_one(&envelope, raw, ts, log, kv, &sinks).await {
            Ok(()) => accepted += 1,
            Err(ApplyRecordError::Invalid(failure)) => failures.push(failure),
            Err(ApplyRecordError::Engine(e)) => return Err(e),
        }
    }

    if accepted > 0 {
        upsert_stream(kv, &envelope, accepted).await?;
    }

    let status = if failures.is_empty() { "ok" } else { "partial" };
    Ok(IngestReply {
        batch_id: envelope.batch_id,
        accepted,
        status: status.to_string(),
        failures,
    })
}

/// 三个派生出口的打包：只在模块内传递，避免 `apply_one` 的参数表继续膨胀
/// （出口从 1 个长到 3 个，公参版本见 [`apply_with_sinks`]）。
#[derive(Clone, Copy, Default)]
struct Sinks<'a> {
    trace: Option<&'a dyn TraceSink>,
    edge: Option<&'a dyn EdgeSink>,
    metric: Option<&'a dyn MetricSink>,
}

enum ApplyRecordError {
    Invalid(RecordFailure),
    Engine(DataplaneError),
}

async fn apply_one(
    envelope: &DataEnvelope,
    raw: &Value,
    ts: &dyn TimeSeriesStore,
    log: &dyn LogStore,
    kv: &dyn KvStore,
    sinks: &Sinks<'_>,
) -> Result<(), ApplyRecordError> {
    let Sinks {
        trace: trace_sink,
        edge: edge_sink,
        metric: metric_sink,
    } = *sinks;
    match envelope.data_type {
        DataType::Metrics => {
            let rec: MetricsRecord = parse_record(raw)?;
            require_record_id(&rec.record_id, raw)?;
            if already_accepted(kv, &rec.record_id).await? {
                return Ok(());
            }
            let mut tags = rec.tags;
            merge_envelope_tags(&mut tags, envelope);
            // 维度补全放在去重之后：重复记录不值得再查一次名称映射。
            if let Some(sink) = metric_sink {
                for (key, value) in sink.metric_tags(&rec.measurement, &tags).await {
                    tags.insert(key, value);
                }
            }
            ts.write(TsPoint {
                measurement: rec.measurement,
                tags,
                field_name: rec.field_name,
                field_value: rec.field_value,
                timestamp: rec.timestamp,
            })
            .await
            .map_err(ApplyRecordError::Engine)?;
            mark_ingested(kv, &rec.record_id).await
        }
        DataType::Logs => {
            let rec: LogsRecord = parse_record(raw)?;
            require_record_id(&rec.record_id, raw)?;
            if already_accepted(kv, &rec.record_id).await? {
                return Ok(());
            }
            let mut labels = rec.labels;
            labels.insert("source".into(), rec.source);
            merge_common_labels(&mut labels, envelope);
            log.append(LogRecord {
                id: rec.record_id.clone(),
                timestamp: rec.timestamp,
                level: rec.level,
                message: rec.message,
                labels,
                payload: None,
            })
            .await
            .map_err(ApplyRecordError::Engine)?;
            mark_ingested(kv, &rec.record_id).await
        }
        DataType::Apm => {
            let rec: ApmRecord = parse_record(raw)?;
            require_record_id(&rec.record_id, raw)?;
            if already_accepted(kv, &rec.record_id).await? {
                return Ok(());
            }
            let level = if rec.status.is_empty() {
                "info".to_string()
            } else {
                rec.status.clone()
            };
            let message = format!(
                "{} {} {}us",
                rec.service, rec.operation, rec.duration_micros
            );
            let mut labels = rec.labels;
            labels.insert("trace_id".into(), rec.trace_id);
            labels.insert("span_id".into(), rec.span_id);
            labels.insert("parent_span_id".into(), rec.parent_span_id);
            labels.insert("service".into(), rec.service);
            labels.insert("operation".into(), rec.operation);
            labels.insert("duration_micros".into(), rec.duration_micros.to_string());
            labels.insert("status".into(), rec.status);
            merge_common_labels(&mut labels, envelope);
            log.append(LogRecord {
                id: rec.record_id.clone(),
                timestamp: rec.timestamp,
                level,
                message,
                labels,
                payload: None,
            })
            .await
            .map_err(ApplyRecordError::Engine)?;
            mark_ingested(kv, &rec.record_id).await
        }
        DataType::Ebpf => {
            let rec: EbpfRecord = parse_record(raw)?;
            require_record_id(&rec.record_id, raw)?;
            if already_accepted(kv, &rec.record_id).await? {
                return Ok(());
            }
            let mut labels = rec.labels;
            labels.insert("event_type".into(), rec.event_type);
            labels.insert("pid".into(), rec.pid.to_string());
            labels.insert("process_name".into(), rec.process_name);
            merge_common_labels(&mut labels, envelope);
            log.append(LogRecord {
                id: rec.record_id.clone(),
                timestamp: rec.timestamp,
                level: "info".into(),
                message: rec.message,
                labels,
                payload: None,
            })
            .await
            .map_err(ApplyRecordError::Engine)?;
            mark_ingested(kv, &rec.record_id).await
        }
        DataType::Traces => {
            if trace::exceeds_size_limit(raw) {
                return Err(ApplyRecordError::Invalid(RecordFailure {
                    record_id: raw
                        .get("record_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    code: "invalid_argument".into(),
                    message: format!(
                        "span exceeds {} bytes (256 KiB); reduce attributes/events or drop them at the collector",
                        trace::MAX_TRACE_SPAN_BYTES
                    ),
                }));
            }
            let span: TraceSpan = parse_record(raw)?;
            require_record_id(&span.record_id, raw)?;
            let span = trace::normalize(span);
            if let Err(reason) = trace::validate(&span) {
                return Err(ApplyRecordError::Invalid(RecordFailure {
                    record_id: span.record_id.clone(),
                    code: "invalid_argument".into(),
                    message: reason,
                }));
            }
            if already_accepted(kv, &span.record_id).await? {
                return Ok(());
            }
            // 明细是可选的高成本数据：低于阈值时只走摘要与聚合（阈值由 sink 提供）。
            let detail_min = trace_sink.map_or(0, |sink| sink.detail_min_duration_micros());
            if span.duration_micros() >= detail_min {
                log.append(trace::to_log_record(&span, envelope))
                    .await
                    .map_err(ApplyRecordError::Engine)?;
            }
            if let Some(sink) = trace_sink {
                if let Err(e) = sink.observe_span(&span, envelope).await {
                    eprintln!(
                        "apm: observe_span failed for {}: {}: {}",
                        span.record_id,
                        e.code.as_str(),
                        e.message
                    );
                }
            }
            mark_ingested(kv, &span.record_id).await
        }
        DataType::EbpfEdges => {
            let edge: edge::EbpfEdge = parse_record(raw)?;
            if let Err(reason) = edge.validate() {
                return Err(ApplyRecordError::Invalid(RecordFailure {
                    record_id: edge.record_id.clone(),
                    code: "invalid_argument".into(),
                    message: reason,
                }));
            }
            // 幂等：同 `record_id` 重发直接受理（边记录按 record_id 覆盖写，重放是安全的，
            // 但仍先挡一次，避免重复做反查与 sqlite 写）。
            if already_accepted(kv, &edge.record_id).await? {
                return Ok(());
            }
            if let Some(sink) = edge_sink {
                if let Err(e) = sink.observe_edge(&edge, envelope).await {
                    eprintln!(
                        "ebpf: observe_edge failed for {}: {}: {}",
                        edge.record_id,
                        e.code.as_str(),
                        e.message
                    );
                }
            }
            mark_ingested(kv, &edge.record_id).await
        }
    }
}

fn parse_record<T: for<'de> Deserialize<'de>>(raw: &Value) -> Result<T, ApplyRecordError> {
    serde_json::from_value(raw.clone()).map_err(|e| {
        ApplyRecordError::Invalid(RecordFailure {
            record_id: raw
                .get("record_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            code: "invalid_record".into(),
            message: e.to_string(),
        })
    })
}

fn require_record_id(record_id: &str, raw: &Value) -> Result<(), ApplyRecordError> {
    if record_id.is_empty() {
        Err(ApplyRecordError::Invalid(RecordFailure {
            record_id: raw
                .get("record_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            code: "invalid_record".into(),
            message: "record_id is required".into(),
        }))
    } else {
        Ok(())
    }
}

async fn already_accepted(kv: &dyn KvStore, record_id: &str) -> Result<bool, ApplyRecordError> {
    kv.exists(ingest_key(record_id).as_bytes())
        .await
        .map_err(ApplyRecordError::Engine)
}

async fn mark_ingested(kv: &dyn KvStore, record_id: &str) -> Result<(), ApplyRecordError> {
    kv.set(ingest_key(record_id).as_bytes(), &[])
        .await
        .map_err(ApplyRecordError::Engine)
}

fn merge_envelope_tags(tags: &mut BTreeMap<String, String>, envelope: &DataEnvelope) {
    tags.insert("agent_id".into(), envelope.agent_id.clone());
    tags.insert("item_id".into(), envelope.data_id.clone());
    if !envelope.host_id.is_empty() {
        tags.insert("host_id".into(), envelope.host_id.clone());
    }
}

pub(crate) fn merge_common_labels(labels: &mut BTreeMap<String, String>, envelope: &DataEnvelope) {
    labels.insert("data_type".into(), envelope.data_type.as_str().to_string());
    labels.insert("agent_id".into(), envelope.agent_id.clone());
    labels.insert("data_id".into(), envelope.data_id.clone());
    if !envelope.host_id.is_empty() {
        labels.insert("host_id".into(), envelope.host_id.clone());
    }
}

async fn upsert_stream(
    kv: &dyn KvStore,
    envelope: &DataEnvelope,
    accepted: u32,
) -> Result<(), DataplaneError> {
    let meta = StreamIndex {
        last_seen_micros: envelope.sent_at_micros,
        accepted,
    };
    let bytes = serde_json::to_vec(&meta).map_err(|e| {
        DataplaneError::new(
            ErrorCode::QueryFailed,
            format!("serialize stream index: {e}"),
        )
    })?;
    kv.set(
        stream_key(&envelope.agent_id, envelope.data_type, &envelope.data_id).as_bytes(),
        &bytes,
    )
    .await
}

/// 把检索条件编成 `LogFilter` 并查询。
pub async fn search(
    log: &dyn LogStore,
    query: LogSearchQuery,
) -> Result<Vec<LogRecord>, DataplaneError> {
    if let (Some(from_ts), Some(to_ts)) = (query.from_ts, query.to_ts) {
        if from_ts > to_ts {
            return Err(DataplaneError::invalid_argument(
                "from_ts must not be later than to_ts",
            ));
        }
    }
    let mut labels = query.labels;
    insert_label(&mut labels, "data_type", query.data_type);
    insert_label(&mut labels, "agent_id", query.agent_id);
    insert_label(&mut labels, "host_id", query.host_id);
    insert_label(&mut labels, "data_id", query.data_id);
    insert_label(&mut labels, "trace_id", query.trace_id);
    insert_label(&mut labels, "event_type", query.event_type);
    let filter = LogFilter {
        from_ts: query.from_ts,
        to_ts: query.to_ts,
        level: query.level,
        message_query: query.message_query,
        labels,
        limit: clamp_log_limit(query.limit.unwrap_or(0)),
    };
    log.search(filter).await
}

fn insert_label(labels: &mut BTreeMap<String, String>, key: &str, value: Option<String>) {
    if let Some(v) = value {
        labels.insert(key.to_string(), v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dataplane_kv::RedbKvStore;
    use dataplane_log::TantivyLogStore;
    use dataplane_ts::{TsRetentionConfig, TsinkTimeSeriesStore};
    use serde_json::json;

    struct Engines {
        ts: TsinkTimeSeriesStore,
        log: TantivyLogStore,
        kv: RedbKvStore,
        _dir: tempfile::TempDir,
    }

    fn engines() -> Engines {
        let dir = tempfile::tempdir().unwrap();
        let ts_dir = dir.path().join("ts");
        let log_dir = dir.path().join("log");
        std::fs::create_dir_all(&ts_dir).unwrap();
        std::fs::create_dir_all(&log_dir).unwrap();
        // 测试用固定历史时间戳，关闭保留执行。
        let ts = TsinkTimeSeriesStore::new(
            &ts_dir,
            TsRetentionConfig {
                enforced: false,
                ..TsRetentionConfig::default()
            },
        )
        .unwrap();
        let log = TantivyLogStore::new(&log_dir).unwrap();
        let kv = RedbKvStore::new(dir.path().join("kv.redb")).unwrap();
        Engines {
            ts,
            log,
            kv,
            _dir: dir,
        }
    }

    fn envelope(data_type: &str, records: Vec<Value>) -> DataEnvelope {
        serde_json::from_value(json!({
            "batch_id": "b1",
            "data_type": data_type,
            "data_id": "item-1",
            "agent_id": "agent-1",
            "host_id": "host-1",
            "sent_at_micros": 1_710_000_000_000_000i64,
            "records": records,
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn metrics_roundtrip() {
        let e = engines();
        let env = envelope(
            "metrics",
            vec![json!({
                "record_id": "m1",
                "timestamp": 1_710_000_000_000_000i64,
                "measurement": "cpu_usage",
                "tags": {"role": "web"},
                "field_name": "value",
                "field_value": 12.5
            })],
        );
        let reply = apply(env, &e.ts, &e.log, &e.kv).await.unwrap();
        assert_eq!(reply.status, "ok");
        assert_eq!(reply.accepted, 1);
        let found =
            e.ts.query_instant(
                r#"cpu_usage{agent_id="agent-1",item_id="item-1"}"#,
                Some(1_710_000_000_000_000),
            )
            .await
            .unwrap();
        assert_eq!(found.result.len(), 1);
        assert_eq!(found.result[0].value.unwrap().1, 12.5);
    }

    /// 只给 `ebpf_process_*` 补 service 的假 sink（记录调用次数，便于断言「不该调的不调」）。
    #[derive(Default)]
    struct RecordingMetricSink {
        calls: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl metric::MetricSink for RecordingMetricSink {
        async fn metric_tags(
            &self,
            measurement: &str,
            tags: &BTreeMap<String, String>,
        ) -> Vec<(String, String)> {
            self.calls.lock().unwrap().push(measurement.to_string());
            if !measurement.starts_with("ebpf_process_") {
                return Vec::new();
            }
            let name = tags.get("process_name").cloned().unwrap_or_default();
            vec![("service".into(), format!("svc-{name}"))]
        }
    }

    #[tokio::test]
    async fn metric_sink_enriches_process_metrics_only() {
        let e = engines();
        let sink = RecordingMetricSink::default();
        let process = json!({
            "record_id": "p1",
            "timestamp": 1_710_000_000_000_000i64,
            "measurement": "ebpf_process_exec_total",
            "tags": {"process_name": "java", "pid": "42"},
            "field_name": "value",
            "field_value": 3.0
        });
        let other = json!({
            "record_id": "m2",
            "timestamp": 1_710_000_000_000_000i64,
            "measurement": "cpu_usage",
            "tags": {"role": "web"},
            "field_name": "value",
            "field_value": 1.0
        });
        let env = envelope("metrics", vec![process, other]);
        let reply = apply_with_sinks(
            env,
            &e.ts,
            &e.log,
            &e.kv,
            None,
            None,
            Some(&sink as &dyn metric::MetricSink),
        )
        .await
        .unwrap();
        assert_eq!(reply.status, "ok");

        // sink 看得到合并信封标签之后的最终标签集。
        let enriched =
            e.ts.query_instant(
                r#"ebpf_process_exec_total{service="svc-java",agent_id="agent-1"}"#,
                Some(1_710_000_000_000_000),
            )
            .await
            .unwrap();
        assert_eq!(enriched.result.len(), 1, "进程指标应带补全的 service 维度");
        // 其它指标原样落库：没有 service 标签，也不会因为 sink 返回空而丢点。
        let untouched =
            e.ts.query_instant(
                r#"cpu_usage{agent_id="agent-1"}"#,
                Some(1_710_000_000_000_000),
            )
            .await
            .unwrap();
        assert_eq!(untouched.result.len(), 1);
        assert_eq!(
            sink.calls.lock().unwrap().len(),
            2,
            "每个点问一次，由 sink 自行判断"
        );

        // 没有 sink 时行为不变（不补维度也不报错）。
        let plain = serde_json::from_value::<DataEnvelope>(json!({
            "batch_id": "b2", "data_type": "metrics", "data_id": "item-1",
            "agent_id": "agent-1", "host_id": "host-1",
            "sent_at_micros": 1_710_000_000_000_000i64,
            "records": [{
                "record_id": "p9", "timestamp": 1_710_000_000_000_000i64,
                "measurement": "ebpf_process_exec_total",
                "tags": {"process_name": "go"}, "field_name": "value", "field_value": 1.0
            }],
        }))
        .unwrap();
        let reply = apply(plain, &e.ts, &e.log, &e.kv).await.unwrap();
        assert_eq!(reply.status, "ok");
        let raw =
            e.ts.query_instant(
                r#"ebpf_process_exec_total{process_name="go"}"#,
                Some(1_710_000_000_000_000),
            )
            .await
            .unwrap();
        assert_eq!(raw.result.len(), 1);
    }

    #[tokio::test]
    async fn logs_roundtrip() {
        let e = engines();
        let env = envelope(
            "logs",
            vec![json!({
                "record_id": "l1",
                "timestamp": 1_710_000_000_000_000i64,
                "level": "error",
                "message": "listen failed",
                "source": "/var/log/app.log",
                "labels": {}
            })],
        );
        apply(env, &e.ts, &e.log, &e.kv).await.unwrap();
        let hits = search(
            &e.log,
            LogSearchQuery {
                data_type: Some("logs".into()),
                agent_id: Some("agent-1".into()),
                message_query: Some("listen".into()),
                ..LogSearchQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "l1");
        assert_eq!(hits[0].level, "error");
        assert_eq!(hits[0].labels.get("source").unwrap(), "/var/log/app.log");
        assert_eq!(hits[0].labels.get("data_id").unwrap(), "item-1");
        assert_eq!(hits[0].labels.get("host_id").unwrap(), "host-1");
    }

    #[tokio::test]
    async fn apm_roundtrip() {
        let e = engines();
        let env = envelope(
            "apm",
            vec![json!({
                "record_id": "a1",
                "timestamp": 1_710_000_000_000_000i64,
                "trace_id": "t-1",
                "span_id": "s-1",
                "parent_span_id": "",
                "service": "api",
                "operation": "GET /x",
                "duration_micros": 1500,
                "status": "ok",
                "labels": {}
            })],
        );
        apply(env, &e.ts, &e.log, &e.kv).await.unwrap();
        let hits = search(
            &e.log,
            LogSearchQuery {
                data_type: Some("apm".into()),
                trace_id: Some("t-1".into()),
                ..LogSearchQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].level, "ok");
        assert_eq!(hits[0].message, "api GET /x 1500us");
        assert_eq!(hits[0].labels.get("span_id").unwrap(), "s-1");
    }

    #[tokio::test]
    async fn ebpf_roundtrip() {
        let e = engines();
        let env = envelope(
            "ebpf",
            vec![json!({
                "record_id": "e1",
                "timestamp": 1_710_000_000_000_000i64,
                "event_type": "exec",
                "pid": 42,
                "process_name": "bash",
                "message": "execve",
                "labels": {}
            })],
        );
        apply(env, &e.ts, &e.log, &e.kv).await.unwrap();
        let hits = search(
            &e.log,
            LogSearchQuery {
                data_type: Some("ebpf".into()),
                event_type: Some("exec".into()),
                ..LogSearchQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].level, "info");
        assert_eq!(hits[0].labels.get("pid").unwrap(), "42");
        assert_eq!(hits[0].labels.get("process_name").unwrap(), "bash");
    }

    #[tokio::test]
    async fn missing_field_is_partial() {
        let e = engines();
        let env = envelope(
            "metrics",
            vec![
                json!({
                    "record_id": "good",
                    "timestamp": 1,
                    "measurement": "cpu_usage",
                    "tags": {},
                    "field_name": "value",
                    "field_value": 1.0
                }),
                json!({
                    "record_id": "bad",
                    "timestamp": 1,
                    "measurement": "cpu_usage",
                    "tags": {},
                    "field_value": 2.0
                }),
            ],
        );
        let reply = apply(env, &e.ts, &e.log, &e.kv).await.unwrap();
        assert_eq!(reply.status, "partial");
        assert_eq!(reply.accepted, 1);
        assert_eq!(reply.failures.len(), 1);
        assert_eq!(reply.failures[0].record_id, "bad");
        assert_eq!(reply.failures[0].code, "invalid_record");
    }

    #[tokio::test]
    async fn duplicate_record_id_skips_second_write() {
        let e = engines();
        let rec = json!({
            "record_id": "l-dup",
            "timestamp": 10,
            "level": "info",
            "message": "once",
            "source": "src",
            "labels": {}
        });
        let env = envelope("logs", vec![rec.clone()]);
        apply(env, &e.ts, &e.log, &e.kv).await.unwrap();
        let env2 = envelope("logs", vec![rec]);
        let reply = apply(env2, &e.ts, &e.log, &e.kv).await.unwrap();
        assert_eq!(reply.status, "ok");
        assert_eq!(reply.accepted, 1);
        let hits = search(
            &e.log,
            LogSearchQuery {
                data_type: Some("logs".into()),
                ..LogSearchQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn search_limit_one() {
        let e = engines();
        let records = (0..3)
            .map(|i| {
                json!({
                    "record_id": format!("n{i}"),
                    "timestamp": i,
                    "level": "info",
                    "message": format!("row-{i}"),
                    "source": "src",
                    "labels": {}
                })
            })
            .collect();
        apply(envelope("logs", records), &e.ts, &e.log, &e.kv)
            .await
            .unwrap();
        let hits = search(
            &e.log,
            LogSearchQuery {
                data_type: Some("logs".into()),
                limit: Some(1),
                ..LogSearchQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn stream_index_upsert() {
        let e = engines();
        let env = envelope(
            "logs",
            vec![json!({
                "record_id": "s1",
                "timestamp": 1,
                "level": "info",
                "message": "hi",
                "source": "src",
                "labels": {}
            })],
        );
        apply(env, &e.ts, &e.log, &e.kv).await.unwrap();
        let raw =
            e.kv.get(stream_key("agent-1", DataType::Logs, "item-1").as_bytes())
                .await
                .unwrap();
        let meta: StreamIndex = serde_json::from_slice(&raw).unwrap();
        assert_eq!(meta.last_seen_micros, 1_710_000_000_000_000);
        assert_eq!(meta.accepted, 1);
    }

    #[tokio::test]
    async fn empty_records_invalid_argument() {
        let e = engines();
        let env = envelope("logs", vec![]);
        let err = apply(env, &e.ts, &e.log, &e.kv).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    /// 记录型 sink：断言派生数据钩子被调用与入参。
    #[derive(Default)]
    struct RecordingSink {
        spans: std::sync::Mutex<Vec<String>>,
        fail: bool,
        detail_min: i64,
    }

    #[async_trait::async_trait]
    impl TraceSink for RecordingSink {
        fn detail_min_duration_micros(&self) -> i64 {
            self.detail_min
        }

        async fn observe_span(
            &self,
            span: &TraceSpan,
            envelope: &DataEnvelope,
        ) -> Result<(), DataplaneError> {
            self.spans
                .lock()
                .unwrap()
                .push(format!("{}@{}", span.record_id, envelope.data_id));
            if self.fail {
                return Err(DataplaneError::new(ErrorCode::QueryFailed, "sink boom"));
            }
            Ok(())
        }
    }

    /// 记录型边 sink：断言 `ebpf_edges` 分支调用与失败隔离。
    #[derive(Default)]
    struct RecordingEdgeSink {
        edges: std::sync::Mutex<Vec<String>>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl edge::EdgeSink for RecordingEdgeSink {
        async fn observe_edge(
            &self,
            edge: &edge::EbpfEdge,
            envelope: &DataEnvelope,
        ) -> Result<(), DataplaneError> {
            self.edges
                .lock()
                .unwrap()
                .push(format!("{}@{}", edge.record_id, envelope.data_id));
            if self.fail {
                return Err(DataplaneError::new(ErrorCode::QueryFailed, "sink boom"));
            }
            Ok(())
        }
    }

    fn ebpf_edge_json(record_id: &str, connections: u64, failures: u64) -> Value {
        json!({
            "record_id": record_id,
            "timestamp": 1_710_000_000_000_000i64,
            "bucket_micros": 10_000_000i64,
            "protocol": "tcp",
            "src_ip": "10.0.0.5",
            "src_port": 40000,
            "dst_ip": "10.0.0.9",
            "dst_port": 8080,
            "src_process": "java",
            "connections": connections,
            "bytes_sent": 100,
            "bytes_recv": 200,
            "duration_micros_sum": 10,
            "duration_micros_max": 10,
            "tcp_retrans": 0,
            "tcp_resets": 0,
            "failures": failures,
            "failure_reason": if failures > 0 { "refused" } else { "" },
            "latency_hist": [1]
        })
    }

    #[tokio::test]
    async fn ebpf_edges_go_to_edge_sink_and_validate() {
        let e = engines();
        let sink = RecordingEdgeSink::default();
        let env = envelope(
            "ebpf_edges",
            vec![
                ebpf_edge_json("agent-1:1:a", 2, 1),
                // failures > connections：整条拒绝。
                ebpf_edge_json("agent-1:1:b", 1, 5),
            ],
        );
        let reply = apply_with_sinks(
            env,
            &e.ts,
            &e.log,
            &e.kv,
            None,
            Some(&sink as &dyn edge::EdgeSink),
            None,
        )
        .await
        .unwrap();
        assert_eq!(reply.accepted, 1);
        assert_eq!(reply.status, "partial");
        assert_eq!(reply.failures.len(), 1);
        assert_eq!(reply.failures[0].record_id, "agent-1:1:b");
        assert!(reply.failures[0].message.contains("failures"));

        let seen = sink.edges.lock().unwrap().clone();
        assert_eq!(seen, vec!["agent-1:1:a@item-1".to_string()]);

        // 幂等重放：同一条不再进 sink。
        let env = envelope("ebpf_edges", vec![ebpf_edge_json("agent-1:1:a", 2, 1)]);
        let reply = apply_with_sinks(
            env,
            &e.ts,
            &e.log,
            &e.kv,
            None,
            Some(&sink as &dyn edge::EdgeSink),
            None,
        )
        .await
        .unwrap();
        assert_eq!(reply.accepted, 1);
        assert_eq!(sink.edges.lock().unwrap().len(), 1, "重放不重复反查/落库");
    }

    #[tokio::test]
    async fn edge_sink_failure_does_not_fail_batch() {
        let e = engines();
        let sink = RecordingEdgeSink {
            fail: true,
            ..Default::default()
        };
        let env = envelope("ebpf_edges", vec![ebpf_edge_json("agent-1:2:a", 1, 0)]);
        let reply = apply_with_sinks(
            env,
            &e.ts,
            &e.log,
            &e.kv,
            None,
            Some(&sink as &dyn edge::EdgeSink),
            None,
        )
        .await
        .unwrap();
        assert_eq!(reply.status, "ok", "sink 失败不影响接入应答");
        assert_eq!(reply.accepted, 1);
    }

    fn trace_span_json(trace_id: &str, span_id: &str, parent: &str) -> Value {
        json!({
            "record_id": format!("{trace_id}:{span_id}"),
            "timestamp": 1_710_000_000_000_000i64,
            "trace_id": trace_id,
            "span_id": span_id,
            "parent_span_id": parent,
            "name": "GET /orders/{id}",
            "kind": "server",
            "start_unix_nano": 1_710_000_000_000_000_000i64,
            "end_unix_nano": 1_710_000_000_012_000_000i64,
            "status_code": "ok",
            "service": "order-api",
            "collector": "otlp",
            "labels": {}
        })
    }

    #[tokio::test]
    async fn traces_roundtrip_writes_detail_and_calls_sink() {
        let e = engines();
        let sink = RecordingSink::default();
        let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
        let span_id = "00f067aa0ba902b7";
        let env = envelope("traces", vec![trace_span_json(trace_id, span_id, "")]);
        let reply = apply_with_trace_sink(env, &e.ts, &e.log, &e.kv, Some(&sink as &dyn TraceSink))
            .await
            .unwrap();
        assert_eq!(reply.status, "ok");
        assert_eq!(reply.accepted, 1);
        assert!(reply.failures.is_empty());

        let hits = search(
            &e.log,
            LogSearchQuery {
                data_type: Some("traces".into()),
                trace_id: Some(trace_id.into()),
                ..LogSearchQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message, "order-api GET /orders/{id} 12000us");
        assert_eq!(hits[0].level, "info");
        assert_eq!(hits[0].labels.get("service").unwrap(), "order-api");
        assert_eq!(hits[0].labels.get("data_type").unwrap(), "traces");
        assert_eq!(
            sink.spans.lock().unwrap().as_slice(),
            [format!("{trace_id}:{span_id}@item-1")]
        );

        // 幂等：重放不新增明细，也不再回调 sink。
        let env = envelope("traces", vec![trace_span_json(trace_id, span_id, "")]);
        let reply = apply_with_trace_sink(env, &e.ts, &e.log, &e.kv, Some(&sink as &dyn TraceSink))
            .await
            .unwrap();
        assert_eq!(reply.accepted, 1, "重复记录仍计入 accepted");
        let hits = search(
            &e.log,
            LogSearchQuery {
                data_type: Some("traces".into()),
                trace_id: Some(trace_id.into()),
                ..LogSearchQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 1, "重复 record_id 不应新增明细");
        assert_eq!(sink.spans.lock().unwrap().len(), 1, "重复记录不再回调 sink");
    }

    #[tokio::test]
    async fn traces_normalizes_ids_and_derives_timestamp() {
        let e = engines();
        let mut raw = trace_span_json("4BF92F3577B34DA6A3CE929D0E0E4736", "00F067AA0BA902B7", "");
        raw["record_id"] = json!("4BF92F3577B34DA6A3CE929D0E0E4736:00F067AA0BA902B7");
        raw["timestamp"] = json!(0);
        let env = envelope("traces", vec![raw]);
        let reply = apply(env, &e.ts, &e.log, &e.kv).await.unwrap();
        assert_eq!(reply.status, "ok", "{reply:?}");

        let hits = search(
            &e.log,
            LogSearchQuery {
                data_type: Some("traces".into()),
                trace_id: Some("4bf92f3577b34da6a3ce929d0e0e4736".into()),
                ..LogSearchQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].timestamp, 1_710_000_000_000_000);
        assert_eq!(hits[0].labels.get("service").unwrap(), "order-api");
    }

    #[tokio::test]
    async fn traces_invalid_records_become_partial_failures() {
        let e = engines();
        let good = trace_span_json("4bf92f3577b34da6a3ce929d0e0e4736", "00f067aa0ba902b7", "");
        let mut bad_trace =
            trace_span_json("4bf92f3577b34da6a3ce929d0e0e4736", "00f067aa0ba902b7", "");
        bad_trace["trace_id"] = json!("short");
        let mut bad_span =
            trace_span_json("4bf92f3577b34da6a3ce929d0e0e4736", "00f067aa0ba902b7", "");
        bad_span["span_id"] = json!("zzz");
        let mut oversized =
            trace_span_json("4bf92f3577b34da6a3ce929d0e0e4736", "00f067aa0ba902b7", "");
        oversized["attributes"] = json!({"big": "x".repeat(256 * 1024)});
        let env = envelope("traces", vec![good, bad_trace, bad_span, oversized]);

        let reply = apply(env, &e.ts, &e.log, &e.kv).await.unwrap();
        assert_eq!(reply.status, "partial");
        assert_eq!(reply.accepted, 1);
        assert_eq!(reply.failures.len(), 3);
        assert!(reply.failures.iter().all(|f| f.code == "invalid_argument"));
        assert!(
            reply.failures.iter().any(|f| f.message.contains("256 KiB")),
            "超限原因应说明上限: {:?}",
            reply.failures
        );
    }

    #[tokio::test]
    async fn traces_detail_keeps_full_span_payload() {
        let e = engines();
        let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
        let span_id = "00f067aa0ba902b7";
        let mut raw = trace_span_json(trace_id, span_id, "");
        raw["attributes"] =
            json!({"http.request.method": "GET", "http.response.status_code": "200"});
        raw["resource"] = json!({"service.name": "order-api", "k8s.pod.name": "order-api-1"});
        raw["events"] = json!([{"name": "exception", "time_unix_nano": 1_710_000_000_005_000_000i64,
                              "attributes": {"exception.type": "Timeout"}}]);
        raw["links"] = json!([{"trace_id": "a".repeat(32), "span_id": "b".repeat(16)}]);
        raw["status_message"] = json!("upstream timeout");
        raw["dropped_events"] = json!(3);

        let env = envelope("traces", vec![raw]);
        apply(env, &e.ts, &e.log, &e.kv).await.unwrap();

        let hits = search(
            &e.log,
            LogSearchQuery {
                data_type: Some("traces".into()),
                trace_id: Some(trace_id.into()),
                ..LogSearchQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 1);
        let payload = hits[0].payload.as_deref().expect("span 原文应写入 payload");
        let parsed: Value = serde_json::from_str(payload).unwrap();
        assert_eq!(
            parsed["attributes"]["http.request.method"],
            json!("GET"),
            "标签投影放不下的富字段必须能从原文还原"
        );
        assert_eq!(parsed["resource"]["k8s.pod.name"], json!("order-api-1"));
        assert_eq!(parsed["events"][0]["name"], json!("exception"));
        assert_eq!(
            parsed["events"][0]["attributes"]["exception.type"],
            json!("Timeout")
        );
        assert_eq!(parsed["links"][0]["span_id"], json!("b".repeat(16)));
        assert_eq!(parsed["status_message"], json!("upstream timeout"));
        assert_eq!(parsed["dropped_events"], json!(3));
        assert_eq!(
            parsed["start_unix_nano"],
            json!(1_710_000_000_000_000_000i64)
        );
        assert_eq!(parsed["end_unix_nano"], json!(1_710_000_000_012_000_000i64));
    }

    #[tokio::test]
    async fn traces_detail_threshold_skips_detail_but_keeps_sink() {
        let e = engines();
        let sink = RecordingSink {
            detail_min: 10_000,
            ..RecordingSink::default()
        };
        let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
        let span_id = "00f067aa0ba902b7";
        // span 耗时 12_000us >= 阈值 → 写明细。
        let env = envelope("traces", vec![trace_span_json(trace_id, span_id, "")]);
        apply_with_trace_sink(env, &e.ts, &e.log, &e.kv, Some(&sink as &dyn TraceSink))
            .await
            .unwrap();
        assert_eq!(
            search(
                &e.log,
                LogSearchQuery {
                    data_type: Some("traces".into()),
                    trace_id: Some(trace_id.into()),
                    ..LogSearchQuery::default()
                },
            )
            .await
            .unwrap()
            .len(),
            1
        );

        // 低于阈值 → 只走 sink（摘要/聚合），不写明细。
        let mut short = trace_span_json("5bf92f3577b34da6a3ce929d0e0e4737", "00f067aa0ba902b8", "");
        short["end_unix_nano"] = json!(1_710_000_000_001_000_000i64);
        let env = envelope("traces", vec![short]);
        let reply = apply_with_trace_sink(env, &e.ts, &e.log, &e.kv, Some(&sink as &dyn TraceSink))
            .await
            .unwrap();
        assert_eq!(reply.status, "ok", "{reply:?}");
        assert_eq!(reply.accepted, 1);
        assert_eq!(
            sink.spans.lock().unwrap().len(),
            2,
            "sink 仍然收到两条 span"
        );
        assert!(
            search(
                &e.log,
                LogSearchQuery {
                    trace_id: Some("5bf92f3577b34da6a3ce929d0e0e4737".into()),
                    ..LogSearchQuery::default()
                },
            )
            .await
            .unwrap()
            .is_empty(),
            "低于阈值的 span 不写明细"
        );
    }

    #[tokio::test]
    async fn traces_sink_failure_does_not_fail_the_batch() {
        let e = engines();
        let sink = RecordingSink {
            fail: true,
            ..RecordingSink::default()
        };
        let env = envelope(
            "traces",
            vec![trace_span_json(
                "4bf92f3577b34da6a3ce929d0e0e4736",
                "00f067aa0ba902b7",
                "",
            )],
        );
        let reply = apply_with_trace_sink(env, &e.ts, &e.log, &e.kv, Some(&sink as &dyn TraceSink))
            .await
            .unwrap();
        assert_eq!(reply.status, "ok", "派生数据失败不影响接入应答");
        assert_eq!(sink.spans.lock().unwrap().len(), 1);
    }
}
