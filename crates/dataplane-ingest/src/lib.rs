//! 采集信封 DTO、接入落盘、日志检索映射与 record_id 去重。

use std::collections::BTreeMap;

use dataplane_core::{DataplaneError, ErrorCode};
use dataplane_kv::KvStore;
use dataplane_log::{clamp_log_limit, LogFilter, LogRecord, LogStore};
use dataplane_ts::{TimeSeriesStore, TsPoint};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 采集类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    Metrics,
    Logs,
    Apm,
    Ebpf,
}

impl DataType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Metrics => "metrics",
            Self::Logs => "logs",
            Self::Apm => "apm",
            Self::Ebpf => "ebpf",
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

/// 将一批信封写入时序/日志存储，并用 KvStore 去重。
pub async fn apply(
    envelope: DataEnvelope,
    ts: &dyn TimeSeriesStore,
    log: &dyn LogStore,
    kv: &dyn KvStore,
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
        match apply_one(&envelope, raw, ts, log, kv).await {
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
) -> Result<(), ApplyRecordError> {
    match envelope.data_type {
        DataType::Metrics => {
            let rec: MetricsRecord = parse_record(raw)?;
            require_record_id(&rec.record_id, raw)?;
            if already_accepted(kv, &rec.record_id).await? {
                return Ok(());
            }
            let mut tags = rec.tags;
            merge_envelope_tags(&mut tags, envelope);
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
            })
            .await
            .map_err(ApplyRecordError::Engine)?;
            mark_ingested(kv, &rec.record_id).await
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

fn merge_common_labels(labels: &mut BTreeMap<String, String>, envelope: &DataEnvelope) {
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
    use dataplane_ts::TsinkTimeSeriesStore;
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
        let ts = TsinkTimeSeriesStore::new(&ts_dir).unwrap();
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
}
