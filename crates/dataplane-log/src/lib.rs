//! 日志检索接口与 tantivy+jieba 引擎。
//!
//! 对应设计文档 `LogStore`（Requirement 9）。时间戳为 Unix 微秒。
//! `id` 由实现生成 UUID。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dataplane_core::{DataplaneError, ErrorCode};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, RangeQuery, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, TantivyDocument, TextFieldIndexing, Value};
use tantivy::tokenizer::{TokenStream, Tokenizer};
use tantivy::{Index, IndexReader, IndexWriter, Term};
use tantivy_jieba::JiebaTokenizer;
use uuid::Uuid;

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1000;
const DELETE_SCAN_LIMIT: usize = 100_000;
const FIELD_TIMESTAMP: &str = "timestamp";
const FIELD_LEVEL: &str = "level";
const FIELD_MESSAGE: &str = "message";
const FIELD_LABELS_JSON: &str = "labels_json";
const FIELD_ID: &str = "id";

/// 一条日志记录。
#[derive(Debug, Clone, PartialEq)]
pub struct LogRecord {
    /// 记录 ID（append 时若为空则生成 UUID；接入侧可填 `record_id`）。
    pub id: String,
    /// 记录时间，Unix 微秒。
    pub timestamp: i64,
    /// 日志级别，例如 `info`、`error`。
    pub level: String,
    /// 日志正文。
    pub message: String,
    /// 附加标签。
    pub labels: BTreeMap<String, String>,
}

/// 日志检索过滤条件。未指定的条件不过滤。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFilter {
    /// 时间范围起点，Unix 微秒（含）。
    pub from_ts: Option<i64>,
    /// 时间范围终点，Unix 微秒（含）。
    pub to_ts: Option<i64>,
    /// 日志级别精确匹配。
    pub level: Option<String>,
    /// message 关键词查询（jieba 分词后 AND）。
    pub message_query: Option<String>,
    /// 标签精确匹配（全部命中才通过）。
    pub labels: BTreeMap<String, String>,
    /// 返回条数。0 视为默认 100，上限 1000。
    pub limit: usize,
}

impl Default for LogFilter {
    fn default() -> Self {
        Self {
            from_ts: None,
            to_ts: None,
            level: None,
            message_query: None,
            labels: BTreeMap::new(),
            limit: DEFAULT_LIMIT,
        }
    }
}

/// 将调用方传入的 limit 规范为默认 100、上限 1000。
pub fn clamp_log_limit(limit: usize) -> usize {
    let n = if limit == 0 { DEFAULT_LIMIT } else { limit };
    n.min(MAX_LIMIT)
}

/// 日志检索抽象。
#[async_trait]
pub trait LogStore: Send + Sync {
    /// 追加一条日志记录并提交索引。
    async fn append(&self, record: LogRecord) -> Result<(), DataplaneError>;

    /// 按过滤条件检索日志记录。
    async fn search(&self, filter: LogFilter) -> Result<Vec<LogRecord>, DataplaneError>;

    /// 按时间上界与 labels 删除匹配记录，返回删除条数。
    async fn delete_matching(&self, filter: LogFilter) -> Result<u64, DataplaneError>;
}

async fn blocking<F, R>(f: F) -> Result<R, DataplaneError>
where
    F: FnOnce() -> Result<R, DataplaneError> + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(|e| {
        DataplaneError::new(
            ErrorCode::QueryFailed,
            format!("blocking task panicked: {e}"),
        )
    })?
}

fn dp_err(e: impl std::fmt::Display) -> DataplaneError {
    DataplaneError::new(ErrorCode::QueryFailed, e.to_string())
}

#[derive(Debug, Clone)]
struct LogFields {
    timestamp: Field,
    level: Field,
    message: Field,
    labels_json: Field,
    id: Field,
}

fn build_schema() -> tantivy::schema::Schema {
    use tantivy::schema::{NumericOptions, Schema, INDEXED, STORED, STRING, TEXT};
    let mut b = Schema::builder();
    let ts_opts = NumericOptions::from(INDEXED).set_stored().set_fast();
    b.add_i64_field(FIELD_TIMESTAMP, ts_opts);
    b.add_text_field(FIELD_LEVEL, STRING | STORED);
    let message_opts =
        (TEXT | STORED).set_indexing_options(TextFieldIndexing::default().set_tokenizer("jieba"));
    b.add_text_field(FIELD_MESSAGE, message_opts);
    b.add_text_field(FIELD_LABELS_JSON, STORED);
    b.add_text_field(FIELD_ID, STRING | STORED);
    b.build()
}

/// tantivy+jieba 本地引擎。
pub struct TantivyLogStore {
    writer: Arc<Mutex<IndexWriter<TantivyDocument>>>,
    reader: IndexReader,
    fields: LogFields,
    tokenizer: JiebaTokenizer,
}

impl TantivyLogStore {
    /// 在数据路径下创建或打开日志索引。
    pub fn new(data_path: impl AsRef<Path>) -> Result<Self, DataplaneError> {
        let schema = build_schema();
        let index = match Index::create_in_dir(data_path.as_ref(), schema.clone()) {
            Ok(i) => i,
            Err(tantivy::TantivyError::IndexAlreadyExists) => {
                Index::open_in_dir(data_path.as_ref()).map_err(dp_err)?
            }
            Err(e) => return Err(dp_err(e)),
        };
        index.tokenizers().register("jieba", JiebaTokenizer::new());

        let fields = LogFields {
            timestamp: index.schema().get_field(FIELD_TIMESTAMP).map_err(dp_err)?,
            level: index.schema().get_field(FIELD_LEVEL).map_err(dp_err)?,
            message: index.schema().get_field(FIELD_MESSAGE).map_err(dp_err)?,
            labels_json: index
                .schema()
                .get_field(FIELD_LABELS_JSON)
                .map_err(dp_err)?,
            id: index.schema().get_field(FIELD_ID).map_err(dp_err)?,
        };

        let writer: IndexWriter<TantivyDocument> = index.writer(50_000_000).map_err(dp_err)?;
        let reader = index.reader().map_err(dp_err)?;

        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
            reader,
            fields,
            tokenizer: JiebaTokenizer::new(),
        })
    }
}

#[async_trait]
impl LogStore for TantivyLogStore {
    async fn append(&self, record: LogRecord) -> Result<(), DataplaneError> {
        let writer = self.writer.clone();
        let fields = self.fields.clone();
        let id = if record.id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            record.id.clone()
        };
        let labels_json = serde_json::to_string(&record.labels).map_err(|e| {
            DataplaneError::new(ErrorCode::QueryFailed, format!("serialize labels: {e}"))
        })?;
        blocking(move || {
            let mut guard = writer.lock().map_err(|_| {
                DataplaneError::new(ErrorCode::QueryFailed, "log writer lock poisoned")
            })?;
            let mut doc = TantivyDocument::new();
            doc.add_i64(fields.timestamp, record.timestamp);
            doc.add_text(fields.level, &record.level);
            doc.add_text(fields.message, &record.message);
            doc.add_text(fields.labels_json, &labels_json);
            doc.add_text(fields.id, &id);
            guard.add_document(doc).map_err(dp_err)?;
            guard.commit().map_err(dp_err)?;
            Ok(())
        })
        .await
    }

    async fn search(&self, filter: LogFilter) -> Result<Vec<LogRecord>, DataplaneError> {
        let reader = self.reader.clone();
        let fields = self.fields.clone();
        let mut tokenizer = self.tokenizer.clone();
        blocking(move || {
            let limit = clamp_log_limit(filter.limit);
            let mut hits = collect_hits(&reader, &fields, &mut tokenizer, &filter, MAX_LIMIT)?;
            hits.truncate(limit);
            Ok(hits)
        })
        .await
    }

    async fn delete_matching(&self, filter: LogFilter) -> Result<u64, DataplaneError> {
        let reader = self.reader.clone();
        let fields = self.fields.clone();
        let mut tokenizer = self.tokenizer.clone();
        let hits = blocking(move || {
            collect_hits(&reader, &fields, &mut tokenizer, &filter, DELETE_SCAN_LIMIT)
        })
        .await?;
        let ids: Vec<String> = hits
            .into_iter()
            .map(|r| r.id)
            .filter(|id| !id.is_empty())
            .collect();
        let n = ids.len() as u64;
        let writer = self.writer.clone();
        let id_field = self.fields.id;
        blocking(move || {
            let mut guard = writer.lock().map_err(|_| {
                DataplaneError::new(ErrorCode::QueryFailed, "log writer lock poisoned")
            })?;
            for id in &ids {
                guard.delete_term(Term::from_field_text(id_field, id));
            }
            guard.commit().map_err(dp_err)?;
            Ok(n)
        })
        .await
    }
}

fn collect_hits(
    reader: &IndexReader,
    fields: &LogFields,
    tokenizer: &mut JiebaTokenizer,
    filter: &LogFilter,
    top_n: usize,
) -> Result<Vec<LogRecord>, DataplaneError> {
    let mut clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();
    let lower = Term::from_field_i64(fields.timestamp, filter.from_ts.unwrap_or(i64::MIN));
    let upper = Term::from_field_i64(fields.timestamp, filter.to_ts.unwrap_or(i64::MAX));
    clauses.push((
        Occur::Must,
        Box::new(RangeQuery::new(
            std::ops::Bound::Included(lower),
            std::ops::Bound::Included(upper),
        )),
    ));
    if let Some(level) = &filter.level {
        let tq = TermQuery::new(
            Term::from_field_text(fields.level, level),
            IndexRecordOption::Basic,
        );
        clauses.push((Occur::Must, Box::new(tq)));
    }
    if let Some(message_query) = &filter.message_query {
        let mut stream = tokenizer.token_stream(message_query);
        while stream.advance() {
            let term = Term::from_field_text(fields.message, &stream.token().text);
            let tq = TermQuery::new(term, IndexRecordOption::Basic);
            clauses.push((Occur::Must, Box::new(tq)));
        }
    }
    let bq = BooleanQuery::new(clauses);

    reader.reload().map_err(dp_err)?;
    let searcher = reader.searcher();
    let top_n = top_n.max(1);
    let top_docs = searcher
        .search(&bq, &TopDocs::with_limit(top_n).order_by_score())
        .map_err(dp_err)?;

    let mut out = Vec::with_capacity(top_docs.len());
    for (_score, doc_addr) in top_docs {
        let doc = searcher.doc::<TantivyDocument>(doc_addr).map_err(dp_err)?;
        let timestamp = doc
            .get_first(fields.timestamp)
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let level = doc
            .get_first(fields.level)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let message = doc
            .get_first(fields.message)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let id = doc
            .get_first(fields.id)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let labels: BTreeMap<String, String> = doc
            .get_first(fields.labels_json)
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        if filter.labels.iter().all(|(k, v)| labels.get(k) == Some(v)) {
            out.push(LogRecord {
                id,
                timestamp,
                level,
                message,
                labels,
            });
            if out.len() >= top_n {
                break;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, ts: i64, message: &str, data_id: &str) -> LogRecord {
        let mut labels = BTreeMap::new();
        labels.insert("data_id".into(), data_id.into());
        LogRecord {
            id: id.into(),
            timestamp: ts,
            level: "info".into(),
            message: message.into(),
            labels,
        }
    }

    #[tokio::test]
    async fn limit_one_of_three() {
        let dir = tempfile::tempdir().unwrap();
        let store = TantivyLogStore::new(dir.path()).unwrap();
        store.append(rec("a", 1, "one", "item")).await.unwrap();
        store.append(rec("b", 2, "two", "item")).await.unwrap();
        store.append(rec("c", 3, "three", "item")).await.unwrap();
        let hits = store
            .search(LogFilter {
                limit: 1,
                ..LogFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn delete_matching_by_data_id_and_to_ts() {
        let dir = tempfile::tempdir().unwrap();
        let store = TantivyLogStore::new(dir.path()).unwrap();
        store.append(rec("old", 10, "old", "item-1")).await.unwrap();
        store
            .append(rec("keep", 50, "keep", "item-1"))
            .await
            .unwrap();
        store
            .append(rec("other", 10, "other", "item-2"))
            .await
            .unwrap();
        let mut labels = BTreeMap::new();
        labels.insert("data_id".into(), "item-1".into());
        let deleted = store
            .delete_matching(LogFilter {
                to_ts: Some(20),
                labels,
                ..LogFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(deleted, 1);
        let left = store.search(LogFilter::default()).await.unwrap();
        let ids: Vec<_> = left.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"keep"));
        assert!(ids.contains(&"other"));
        assert!(!ids.contains(&"old"));
    }
}
