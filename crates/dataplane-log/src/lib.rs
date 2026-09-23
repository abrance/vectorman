//! 日志检索接口与 tantivy+jieba 引擎。
//!
//! 对应设计文档 `LogStore`（Requirement 9）与 `observability-data-model` 的
//! 「LogStore 索引版本 v2」。时间戳为 Unix 微秒。`id` 由实现生成 UUID。
//!
//! 索引版本 v2：`labels` 仍以 `labels_json` 存储（供展示与 post-filter），
//! 但 `trace_id` / `service` / `data_id` 三个键会被提升为独立的索引字段，
//! 供 [`LogStore::search_indexed`] 全倒排检索与按 `data_id` 的高效清理；
//! 现有 v1 索引不会被原地改写，而是切换到相邻的 `*-v2` 目录重建。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dataplane_core::{DataplaneError, ErrorCode};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, RangeQuery, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, TantivyDocument, TextFieldIndexing, Value};
use tantivy::tokenizer::{TokenStream, Tokenizer};
use tantivy::{Index, IndexReader, IndexWriter, Order, Term};
use tantivy_jieba::JiebaTokenizer;
use uuid::Uuid;

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1000;
const DELETE_SCAN_LIMIT: usize = 100_000;
/// 索引 schema 版本，写入 `<index_dir>/schema_version`。
///
/// v2 → v3：新增索引字段 `data_type`（按类型清理必需——没有它就只能靠 post-filter
/// 扫全量，受 `DELETE_SCAN_LIMIT` 上限约束）。
///
/// v3 → v4：新增 **仅存储** 字段 `payload`，用于保存记录原文（trace span 的完整
/// OTel JSON）。老的投影字段（labels/message）不足以还原 span 的耗时、attributes、
/// events、links，瀑布图与 span 详情需要原文。
const SCHEMA_VERSION: u32 = 4;
const SCHEMA_VERSION_FILE: &str = "schema_version";
/// `search_indexed` 的默认与上限，trace 详情需要一次拉全一个 trace 的 span。
const INDEXED_DEFAULT_LIMIT: usize = 100;
const INDEXED_MAX_LIMIT: usize = 10_000;
/// `delete_matching` 走索引路径时的单批大小与轮数上限。
const DELETE_BATCH_LIMIT: usize = 10_000;
const DELETE_BATCH_MAX_ROUNDS: usize = 10_000;
const FIELD_TIMESTAMP: &str = "timestamp";
const FIELD_LEVEL: &str = "level";
const FIELD_MESSAGE: &str = "message";
const FIELD_LABELS_JSON: &str = "labels_json";
const FIELD_ID: &str = "id";
const FIELD_TRACE_ID: &str = "trace_id";
const FIELD_DATA_TYPE: &str = "data_type";
const FIELD_PAYLOAD: &str = "payload";
const FIELD_SERVICE: &str = "service";
const FIELD_DATA_ID: &str = "data_id";

/// 一条日志记录。
#[derive(Debug, Clone, PartialEq, Default)]
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
    /// 记录原文（trace span 的完整 OTel JSON 等）。仅存储不索引，回读时原样返回。
    #[allow(clippy::doc_markdown)]
    pub payload: Option<String>,
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

/// 全倒排检索的时间排序方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeOrder {
    /// 时间升序（瀑布图按发生顺序）。
    Asc,
    /// 时间降序（检索页默认）。
    #[default]
    Desc,
}

/// 全倒排检索过滤条件。
///
/// 与 [`LogFilter`] 的区别：条件全部走索引字段（`trace_id` / `service` /
/// `data_id` / `level` / `message` / `timestamp`），没有 `labels_json` 的
/// post-filter，因此结果条数不受 `MAX_LIMIT` 扫描上限影响，可按 `trace_id`
/// 一次拉全一个 trace 的全部 span。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedLogFilter {
    /// 时间范围起点（含）。
    pub from_ts: Option<i64>,
    /// 时间范围终点（含）。
    pub to_ts: Option<i64>,
    /// `trace_id` 索引字段精确匹配。
    pub trace_id: Option<String>,
    /// `service` 索引字段精确匹配。
    pub service: Option<String>,
    /// `data_type` 索引字段精确匹配（`logs` / `traces` / `ebpf` ...）。
    pub data_type: Option<String>,
    /// `data_id` 索引字段精确匹配（采集项 `item_id`）。
    pub data_id: Option<String>,
    /// `level` 精确匹配。
    pub level: Option<String>,
    /// message 关键词查询（jieba 分词后 AND）。
    pub message_query: Option<String>,
    /// 返回条数。0 视为默认 100，上限 10_000。
    pub limit: usize,
    /// 时间排序方向。
    pub order: TimeOrder,
}

impl Default for IndexedLogFilter {
    fn default() -> Self {
        Self {
            from_ts: None,
            to_ts: None,
            trace_id: None,
            data_type: None,
            service: None,
            data_id: None,
            level: None,
            message_query: None,
            limit: INDEXED_DEFAULT_LIMIT,
            order: TimeOrder::Desc,
        }
    }
}

impl IndexedLogFilter {
    /// 按 `trace_id` 拉全一个 trace 的 span 的构造入口（升序）。
    #[must_use]
    pub fn by_trace_id(trace_id: impl Into<String>) -> Self {
        Self {
            trace_id: Some(trace_id.into()),
            limit: INDEXED_MAX_LIMIT,
            order: TimeOrder::Asc,
            ..Self::default()
        }
    }
}

/// 将调用方传入的 limit 规范为默认 100、上限 1000。
pub fn clamp_log_limit(limit: usize) -> usize {
    let n = if limit == 0 { DEFAULT_LIMIT } else { limit };
    n.min(MAX_LIMIT)
}

/// 将 `IndexedLogFilter.limit` 规范为默认 100、上限 10_000。
#[must_use]
pub fn clamp_indexed_limit(limit: usize) -> usize {
    let n = if limit == 0 {
        INDEXED_DEFAULT_LIMIT
    } else {
        limit
    };
    n.min(INDEXED_MAX_LIMIT)
}

/// 日志检索抽象。
#[async_trait]
pub trait LogStore: Send + Sync {
    /// 追加一条日志记录并提交索引。
    async fn append(&self, record: LogRecord) -> Result<(), DataplaneError>;

    /// 按过滤条件检索日志记录。
    async fn search(&self, filter: LogFilter) -> Result<Vec<LogRecord>, DataplaneError>;

    /// 全倒排检索（索引版本 v2）：条件全部走索引字段，无 post-filter。
    ///
    /// 默认实现返回 `unimplemented`，使未升级的后端无需改动即可编译。
    async fn search_indexed(
        &self,
        filter: IndexedLogFilter,
    ) -> Result<Vec<LogRecord>, DataplaneError> {
        let _ = filter;
        Err(DataplaneError::new(
            ErrorCode::Unimplemented,
            "search_indexed is not implemented",
        ))
    }

    /// 按时间上界与 labels 删除匹配记录，返回删除条数。
    async fn delete_matching(&self, filter: LogFilter) -> Result<u64, DataplaneError>;

    /// 删除后回收磁盘（合并/清理不再被引用的段文件），返回删除的文件数。
    ///
    /// 默认实现不做任何事：只有真正能在删除后压缩空间的引擎才需要覆盖。
    async fn reclaim_space(&self) -> Result<u64, DataplaneError> {
        Ok(0)
    }
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

/// 取 labels 中待提升为索引字段的值，缺失时为空串（tantivy 不会为空串建 term）。
fn promoted(labels: &BTreeMap<String, String>, key: &str) -> String {
    labels.get(key).cloned().unwrap_or_default()
}

#[derive(Debug, Clone)]
struct LogFields {
    timestamp: Field,
    level: Field,
    message: Field,
    labels_json: Field,
    id: Field,
    trace_id: Field,
    data_type: Field,
    service: Field,
    data_id: Field,
    payload: Field,
}

/// 解析实际使用的索引目录，处理 schema 版本切换。
///
/// 版本文件缺失或低于当前版本时，改用相邻的 `<name>-v<版本>` 目录重建，
/// 旧目录原样保留供人工恢复。返回第二个值为需要输出到 stderr 的提示行
/// （仅版本切换时非空）。
fn resolve_index_dir(requested: &Path) -> Result<(PathBuf, Option<String>), DataplaneError> {
    let current: Option<u32> = std::fs::read_to_string(requested.join(SCHEMA_VERSION_FILE))
        .ok()
        .and_then(|s| s.trim().parse().ok());
    if current == Some(SCHEMA_VERSION) {
        return Ok((requested.to_path_buf(), None));
    }
    let has_existing_index = requested.join("meta.json").exists();
    if !has_existing_index {
        return Ok((requested.to_path_buf(), None));
    }
    let name = requested
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            DataplaneError::new(
                ErrorCode::ConfigInvalid,
                format!("log index path has no file name: {}", requested.display()),
            )
        })?;
    let upgraded = requested.with_file_name(format!("{name}-v{SCHEMA_VERSION}"));
    let warn = format!(
        "log index schema upgraded: rebuilding into {}, old index kept at {}",
        upgraded.display(),
        requested.display()
    );
    Ok((upgraded, Some(warn)))
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
    // v2：从 labels 提升的三个索引字段（tantivy 的文本字段默认即建倒排，
    // `INDEXED` 标志不能与 `STRING` 叠加）。
    b.add_text_field(FIELD_TRACE_ID, STRING | STORED);
    b.add_text_field(FIELD_DATA_TYPE, STRING | STORED);
    b.add_text_field(FIELD_SERVICE, STRING | STORED);
    b.add_text_field(FIELD_DATA_ID, STRING | STORED);
    // 仅存储、不建倒排：原文只用于回读，不参与检索。
    b.add_text_field(FIELD_PAYLOAD, STORED);
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
    /// 在数据路径下创建或打开日志索引（索引版本 v2）。
    pub fn new(data_path: impl AsRef<Path>) -> Result<Self, DataplaneError> {
        let requested = data_path.as_ref();
        std::fs::create_dir_all(requested).map_err(|e| {
            DataplaneError::new(
                ErrorCode::ConfigInvalid,
                format!("create log index dir {}: {e}", requested.display()),
            )
        })?;
        let (index_dir, warn) = resolve_index_dir(requested)?;
        if let Some(warn) = warn {
            eprintln!("{warn}");
        }
        // `Index::create_in_dir` 不会建目录：升级到 `*-v2` 时父目录必须已存在。
        std::fs::create_dir_all(&index_dir).map_err(|e| {
            DataplaneError::new(
                ErrorCode::ConfigInvalid,
                format!("create log index dir {}: {e}", index_dir.display()),
            )
        })?;
        let schema = build_schema();
        let index = match Index::create_in_dir(&index_dir, schema.clone()) {
            Ok(i) => i,
            Err(tantivy::TantivyError::IndexAlreadyExists) => {
                Index::open_in_dir(&index_dir).map_err(dp_err)?
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
            trace_id: index.schema().get_field(FIELD_TRACE_ID).map_err(dp_err)?,
            data_type: index.schema().get_field(FIELD_DATA_TYPE).map_err(dp_err)?,
            service: index.schema().get_field(FIELD_SERVICE).map_err(dp_err)?,
            data_id: index.schema().get_field(FIELD_DATA_ID).map_err(dp_err)?,
            payload: index.schema().get_field(FIELD_PAYLOAD).map_err(dp_err)?,
        };

        // 版本文件在索引成功建立后写入，避免半成品目录被误判为 v2。
        std::fs::write(
            index_dir.join(SCHEMA_VERSION_FILE),
            SCHEMA_VERSION.to_string(),
        )
        .map_err(|e| {
            DataplaneError::new(
                ErrorCode::QueryFailed,
                format!("write schema version in {}: {e}", index_dir.display()),
            )
        })?;

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
            // v2：把 labels 中的三个键提升为索引字段（对既有调用方零改动）。
            doc.add_text(fields.trace_id, promoted(&record.labels, "trace_id"));
            doc.add_text(fields.data_type, promoted(&record.labels, "data_type"));
            doc.add_text(fields.service, promoted(&record.labels, "service"));
            doc.add_text(fields.data_id, promoted(&record.labels, "data_id"));
            if let Some(payload) = &record.payload {
                doc.add_text(fields.payload, payload);
            }
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

    async fn search_indexed(
        &self,
        filter: IndexedLogFilter,
    ) -> Result<Vec<LogRecord>, DataplaneError> {
        let reader = self.reader.clone();
        let fields = self.fields.clone();
        let mut tokenizer = self.tokenizer.clone();
        blocking(move || {
            let top_docs = indexed_top_docs(&reader, &fields, &mut tokenizer, &filter)?;
            let mut out = Vec::with_capacity(top_docs.len());
            let searcher = reader.searcher();
            for (_ts, doc_addr) in top_docs {
                out.push(doc_to_record(&searcher, &fields, doc_addr)?);
            }
            Ok(out)
        })
        .await
    }

    async fn reclaim_space(&self) -> Result<u64, DataplaneError> {
        let writer = self.writer.clone();
        blocking(move || {
            let guard = writer.lock().map_err(|_| {
                DataplaneError::new(ErrorCode::QueryFailed, "log writer lock poisoned")
            })?;
            // `garbage_collect_files` 删除不再被 meta 引用的文件（删除与合并后的旧段）。
            let result = guard.garbage_collect_files().wait().map_err(dp_err)?;
            Ok(result.deleted_files.len() as u64)
        })
        .await
    }

    async fn delete_matching(&self, filter: LogFilter) -> Result<u64, DataplaneError> {
        // 过滤条件全部落在索引字段上时走索引路径：无 post-filter，无扫描上限，
        // 且整个删除在一次阻塞任务里循环到删完为止（供保存周期清理调用）。
        if let Some(indexed) = to_indexed_filter(&filter) {
            let reader = self.reader.clone();
            let fields = self.fields.clone();
            let writer = self.writer.clone();
            let mut tokenizer = self.tokenizer.clone();
            return blocking(move || {
                let mut total = 0u64;
                for _ in 0..DELETE_BATCH_MAX_ROUNDS {
                    let mut batch = indexed.clone();
                    batch.limit = DELETE_BATCH_LIMIT;
                    batch.order = TimeOrder::Asc;
                    let top_docs = indexed_top_docs(&reader, &fields, &mut tokenizer, &batch)?;
                    if top_docs.is_empty() {
                        return Ok(total);
                    }
                    let searcher = reader.searcher();
                    let ids: Vec<String> = top_docs
                        .iter()
                        .map(|(_, addr)| doc_to_record(&searcher, &fields, *addr).map(|r| r.id))
                        .collect::<Result<Vec<_>, _>>()?;
                    let n = ids.len() as u64;
                    let mut guard = writer.lock().map_err(|_| {
                        DataplaneError::new(ErrorCode::QueryFailed, "log writer lock poisoned")
                    })?;
                    let id_field = fields.id;
                    for id in ids.iter().filter(|id| !id.is_empty()) {
                        guard.delete_term(Term::from_field_text(id_field, id));
                    }
                    guard.commit().map_err(dp_err)?;
                    drop(guard);
                    total += n;
                    if (n as usize) < DELETE_BATCH_LIMIT {
                        return Ok(total);
                    }
                }
                Err(DataplaneError::new(
                    ErrorCode::QueryFailed,
                    format!("delete_matching exceeded {DELETE_BATCH_MAX_ROUNDS} rounds"),
                ))
            })
            .await;
        }

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

/// 构造全倒排查询并取 TopDocs（按 `timestamp` fast field 排序）。
fn indexed_top_docs(
    reader: &IndexReader,
    fields: &LogFields,
    tokenizer: &mut JiebaTokenizer,
    filter: &IndexedLogFilter,
) -> Result<Vec<(Option<i64>, tantivy::DocAddress)>, DataplaneError> {
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
    let exact: [(&Field, &Option<String>); 5] = [
        (&fields.trace_id, &filter.trace_id),
        (&fields.data_type, &filter.data_type),
        (&fields.service, &filter.service),
        (&fields.data_id, &filter.data_id),
        (&fields.level, &filter.level),
    ];
    for (field, value) in exact {
        if let Some(value) = value {
            let tq = TermQuery::new(
                Term::from_field_text(*field, value),
                IndexRecordOption::Basic,
            );
            clauses.push((Occur::Must, Box::new(tq)));
        }
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
    let limit = clamp_indexed_limit(filter.limit);
    let order = match filter.order {
        TimeOrder::Asc => Order::Asc,
        TimeOrder::Desc => Order::Desc,
    };
    searcher
        .search(
            &bq,
            &TopDocs::with_limit(limit).order_by_fast_field::<i64>(FIELD_TIMESTAMP, order),
        )
        .map_err(dp_err)
}

/// 把命中的文档还原为 `LogRecord`。
fn doc_to_record(
    searcher: &tantivy::Searcher,
    fields: &LogFields,
    doc_addr: tantivy::DocAddress,
) -> Result<LogRecord, DataplaneError> {
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
    let payload = doc
        .get_first(fields.payload)
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok(LogRecord {
        id,
        timestamp,
        level,
        message,
        labels,
        payload,
    })
}

/// 把 [`LogFilter`] 翻译为 [`IndexedLogFilter`]；出现未建索引的 label 键时返回 `None`。
fn to_indexed_filter(filter: &LogFilter) -> Option<IndexedLogFilter> {
    let mut out = IndexedLogFilter {
        from_ts: filter.from_ts,
        to_ts: filter.to_ts,
        level: filter.level.clone(),
        message_query: filter.message_query.clone(),
        limit: filter.limit,
        ..IndexedLogFilter::default()
    };
    for (key, value) in &filter.labels {
        match key.as_str() {
            "trace_id" => out.trace_id = Some(value.clone()),
            "data_type" => out.data_type = Some(value.clone()),
            "service" => out.service = Some(value.clone()),
            "data_id" => out.data_id = Some(value.clone()),
            _ => return None,
        }
    }
    Some(out)
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
        let record = doc_to_record(&searcher, fields, doc_addr)?;
        if filter
            .labels
            .iter()
            .all(|(k, v)| record.labels.get(k) == Some(v))
        {
            out.push(record);
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
            payload: None,
        }
    }

    /// 构造一条 span 形日志：labels 带 `trace_id` / `service` / `data_id`。
    fn span(id: &str, ts: i64, trace_id: &str, service: &str, level: &str) -> LogRecord {
        let mut labels = BTreeMap::new();
        labels.insert("data_id".into(), "apm-1".into());
        labels.insert("data_type".into(), "traces".into());
        labels.insert("trace_id".into(), trace_id.into());
        labels.insert("service".into(), service.into());
        LogRecord {
            id: id.into(),
            timestamp: ts,
            level: level.into(),
            message: format!("{service} op 10us"),
            labels,
            payload: None,
        }
    }

    #[tokio::test]
    async fn search_indexed_by_trace_id_returns_all_spans_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = TantivyLogStore::new(dir.path()).unwrap();
        // 3 个 trace 各 5 条 span，外加 20 条普通日志。
        for t in 0..3 {
            let trace_id = format!("trace-{t}");
            for i in 0..5 {
                store
                    .append(span(
                        &format!("{trace_id}:{i}"),
                        100 + i,
                        &trace_id,
                        "order-api",
                        "info",
                    ))
                    .await
                    .unwrap();
            }
        }
        for i in 0..20 {
            store
                .append(rec(&format!("log-{i}"), 1_000 + i, "plain", "logs-1"))
                .await
                .unwrap();
        }

        let hits = store
            .search_indexed(IndexedLogFilter::by_trace_id("trace-1"))
            .await
            .unwrap();
        assert_eq!(hits.len(), 5, "只应命中该 trace 的 span");
        assert!(hits.iter().all(|r| r.labels["trace_id"] == "trace-1"));
        assert!(
            hits.windows(2).all(|w| w[0].timestamp <= w[1].timestamp),
            "默认按时间升序"
        );
    }

    #[tokio::test]
    async fn search_indexed_uses_promoted_data_id_for_plain_logs() {
        let dir = tempfile::tempdir().unwrap();
        let store = TantivyLogStore::new(dir.path()).unwrap();
        store.append(rec("a", 1, "one", "item-1")).await.unwrap();
        store.append(rec("b", 2, "two", "item-2")).await.unwrap();

        let hits = store
            .search_indexed(IndexedLogFilter {
                data_id: Some("item-1".into()),
                ..IndexedLogFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "a");
    }

    #[tokio::test]
    async fn search_indexed_filters_time_level_and_orders_desc() {
        let dir = tempfile::tempdir().unwrap();
        let store = TantivyLogStore::new(dir.path()).unwrap();
        store
            .append(span("s0", 10, "t", "svc", "info"))
            .await
            .unwrap();
        store
            .append(span("s1", 20, "t", "svc", "error"))
            .await
            .unwrap();
        store
            .append(span("s2", 30, "t", "svc", "error"))
            .await
            .unwrap();

        let errors = store
            .search_indexed(IndexedLogFilter {
                service: Some("svc".into()),
                level: Some("error".into()),
                order: TimeOrder::Desc,
                ..IndexedLogFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].id, "s2", "降序时最新在前");
        assert_eq!(errors[1].id, "s1");

        let ranged = store
            .search_indexed(IndexedLogFilter {
                from_ts: Some(20),
                to_ts: Some(20),
                ..IndexedLogFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(ranged.len(), 1);
        assert_eq!(ranged[0].id, "s1");
    }

    #[tokio::test]
    async fn search_indexed_clamps_limit() {
        assert_eq!(clamp_indexed_limit(0), 100);
        assert_eq!(clamp_indexed_limit(50), 50);
        assert_eq!(clamp_indexed_limit(usize::MAX), 10_000);

        let dir = tempfile::tempdir().unwrap();
        let store = TantivyLogStore::new(dir.path()).unwrap();
        for i in 0..150 {
            store
                .append(rec(&format!("r{i}"), i, "m", "item"))
                .await
                .unwrap();
        }
        let hits = store
            .search_indexed(IndexedLogFilter {
                data_id: Some("item".into()),
                limit: 0,
                ..IndexedLogFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(hits.len(), 100, "limit=0 视为默认 100");
    }

    /// 批量直写：绕过 `append` 的每记录 commit，用于需要大量文档的用例。
    fn bulk_append(store: &TantivyLogStore, start: usize, count: usize, data_id: &str) {
        let mut guard = store.writer.lock().unwrap();
        for i in start..start + count {
            let mut doc = TantivyDocument::new();
            doc.add_i64(store.fields.timestamp, i as i64);
            doc.add_text(store.fields.level, "info");
            doc.add_text(store.fields.message, "bulk");
            let mut labels = BTreeMap::new();
            labels.insert("data_id".to_string(), data_id.to_string());
            doc.add_text(
                store.fields.labels_json,
                serde_json::to_string(&labels).unwrap(),
            );
            doc.add_text(store.fields.id, format!("d{i}"));
            doc.add_text(store.fields.data_id, data_id);
            guard.add_document(doc).unwrap();
        }
        guard.commit().unwrap();
    }

    #[tokio::test]
    async fn delete_matching_indexed_path_deletes_beyond_scan_cap() {
        let dir = tempfile::tempdir().unwrap();
        let store = TantivyLogStore::new(dir.path()).unwrap();
        // 超过旧 post-filter 路径的 MAX_LIMIT(1000)，验证索引路径无扫描上限。
        bulk_append(&store, 0, 1200, "item-bulk");
        store
            .append(rec("keep", 9_999, "keep", "item-keep"))
            .await
            .unwrap();

        let mut labels = BTreeMap::new();
        labels.insert("data_id".into(), "item-bulk".into());
        let deleted = store
            .delete_matching(LogFilter {
                labels,
                ..LogFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(deleted, 1200, "索引路径应删完所有匹配记录");

        let left = store.search(LogFilter::default()).await.unwrap();
        let ids: Vec<&str> = left.iter().map(|r| r.id.as_str()).collect();
        assert!(!ids.contains(&"d0"));
        assert!(ids.contains(&"keep"));
    }

    #[tokio::test]
    async fn older_index_switches_to_versioned_sibling() {
        let root = tempfile::tempdir().unwrap();
        let v1_dir = root.path().join("logs");
        std::fs::create_dir_all(&v1_dir).unwrap();
        // 模拟 v1：目录里已有索引但没有版本文件。
        Index::create_in_dir(&v1_dir, build_schema()).unwrap();
        assert!(!v1_dir.join(SCHEMA_VERSION_FILE).exists());

        let store = TantivyLogStore::new(&v1_dir).unwrap();
        store
            .append(rec("v2", 1, "after upgrade", "item"))
            .await
            .unwrap();
        let v2_dir = root.path().join(format!("logs-v{SCHEMA_VERSION}"));
        assert!(
            v2_dir.join("meta.json").exists(),
            "应切换到 -v{SCHEMA_VERSION} 目录"
        );
        assert_eq!(
            std::fs::read_to_string(v2_dir.join(SCHEMA_VERSION_FILE)).unwrap(),
            SCHEMA_VERSION.to_string()
        );
        assert!(
            !v1_dir.join(SCHEMA_VERSION_FILE).exists(),
            "旧目录不应被改写"
        );

        // 重启一次：仍然落在同一个 -v2 目录，且数据可读。
        drop(store);
        let reopened = TantivyLogStore::new(&v1_dir).unwrap();
        let hits = reopened.search(LogFilter::default()).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "v2");
    }

    #[tokio::test]
    async fn payload_round_trips_and_does_not_affect_filters() {
        let dir = tempfile::tempdir().unwrap();
        let store = TantivyLogStore::new(dir.path()).unwrap();
        let mut record = rec("p1", 10, "span", "item-1");
        record.labels.insert("data_type".into(), "traces".into());
        record.payload = Some(r#"{"attributes":{"http.method":"GET"},"events":[]}"#.into());
        store.append(record).await.unwrap();
        store
            .append(rec("p2", 11, "plain", "item-1"))
            .await
            .unwrap();

        let hits = store
            .search_indexed(IndexedLogFilter {
                data_id: Some("item-1".into()),
                ..IndexedLogFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        let with_payload = hits.iter().find(|r| r.id == "p1").unwrap();
        assert_eq!(
            with_payload.payload.as_deref(),
            Some(r#"{"attributes":{"http.method":"GET"},"events":[]}"#)
        );
        assert!(
            hits.iter()
                .find(|r| r.id == "p2")
                .unwrap()
                .payload
                .is_none(),
            "没有原文的记录 payload 为空"
        );
    }

    #[tokio::test]
    async fn indexed_filter_by_data_type_and_reclaim() {
        let dir = tempfile::tempdir().unwrap();
        let store = TantivyLogStore::new(dir.path()).unwrap();
        let mut traces = rec("t1", 10, "span", "apm-1");
        traces.labels.insert("data_type".into(), "traces".into());
        let mut logs = rec("l1", 20, "line", "log-1");
        logs.labels.insert("data_type".into(), "logs".into());
        store.append(traces).await.unwrap();
        store.append(logs).await.unwrap();

        let hits = store
            .search_indexed(IndexedLogFilter {
                data_type: Some("traces".into()),
                ..IndexedLogFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "t1");

        // 按类型 + 时间界删除（`data_type` 建了索引，不走 post-filter）。
        let deleted = store
            .delete_matching(LogFilter {
                to_ts: Some(15),
                labels: [("data_type".to_string(), "traces".to_string())]
                    .into_iter()
                    .collect(),
                ..LogFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(deleted, 1, "只删该类型且时间界内的记录");
        assert_eq!(
            store
                .search(LogFilter::default())
                .await
                .unwrap()
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            vec!["l1"],
            "logs 类型不受影响"
        );

        // `reclaim_space` 可调用且不报错（默认实现返回 0，tantivy 回收段文件）。
        store.reclaim_space().await.unwrap();
    }

    #[tokio::test]
    async fn search_indexed_default_impl_reports_unimplemented() {
        struct StubStore;
        #[async_trait]
        impl LogStore for StubStore {
            async fn append(&self, _record: LogRecord) -> Result<(), DataplaneError> {
                Ok(())
            }
            async fn search(&self, _filter: LogFilter) -> Result<Vec<LogRecord>, DataplaneError> {
                Ok(Vec::new())
            }
            async fn delete_matching(&self, _filter: LogFilter) -> Result<u64, DataplaneError> {
                Ok(0)
            }
        }
        let err = StubStore
            .search_indexed(IndexedLogFilter::default())
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Unimplemented);
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
