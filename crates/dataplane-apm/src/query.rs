//! APM 查询：trace 列表/详情、边列表、服务清单。
//!
//! 对应 `.monkeycode/specs/apm-tracing/requirements.md` Requirement 5、6、12 与
//! `design.md`「HTTP 路由」。实现要点：
//!
//! - 列表与详情都读 sqlite `apm_trace_summary`；span 明细读 `LogStore` 的 v2
//!   索引（`IndexedLogFilter::by_trace_id`），不用 post-filter。
//! - 详情返回条数少于 `span_count` 时带 `partial` 与 `reason`，让前端能区分
//!   「索引重建窗口」「明细已过保留期」「明细被阈值过滤」。
//! - 边列表当前只有 `source=otlp`（`apm_edge_summary`）；`ebpf_edges` 由
//!   `ebpf-observability` 落地后接入同一接口。

use dataplane_core::{DataplaneError, SqlValue};
use dataplane_log::{IndexedLogFilter, LogRecord, LogStore};
use dataplane_sql::RelationalStore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::tables;

/// 列表默认返回条数。
pub const DEFAULT_LIMIT: usize = 50;
/// 列表最大返回条数。
pub const MAX_LIMIT: usize = 500;
/// 未指定时间范围时的默认窗口（1 小时）。
pub const DEFAULT_WINDOW_MICROS: i64 = 3_600 * 1_000_000;
/// 详情一次取回的 span 上限（与 `LogStore` v2 的 `INDEXED_MAX_LIMIT` 对齐）。
pub const DETAIL_SPAN_LIMIT: usize = 10_000;

/// trace 列表过滤条件。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TraceSearchQuery {
    pub from_ts: Option<i64>,
    pub to_ts: Option<i64>,
    pub service: Option<String>,
    pub operation: Option<String>,
    pub status: Option<String>,
    pub min_duration_micros: Option<i64>,
    pub agent_id: Option<String>,
    pub host_id: Option<String>,
    pub data_id: Option<String>,
    /// `start_ts`（缺省）或 `duration_micros`。
    pub sort: Option<String>,
    /// `desc`（缺省）或 `asc`。
    pub order: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// 列表响应中的一行。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TraceSummary {
    pub trace_id: String,
    pub start_ts: i64,
    pub duration_micros: i64,
    pub root_service: String,
    pub root_operation: String,
    pub span_count: i64,
    pub error_count: i64,
    pub status: String,
    pub services: Vec<String>,
}

/// 列表分页结果。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TraceSearchPage {
    pub total: i64,
    pub traces: Vec<TraceSummary>,
}

/// trace 详情。
#[derive(Debug, Clone, Serialize)]
pub struct TraceDetail {
    pub summary: TraceSummary,
    pub spans: Vec<Value>,
    /// 明细条数少于摘要 `span_count` 时为 true。
    pub partial: bool,
    pub reason: Option<String>,
    pub expected_span_count: i64,
}

/// 边列表过滤条件。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EdgeSearchQuery {
    pub from_ts: Option<i64>,
    pub to_ts: Option<i64>,
    pub src_service: Option<String>,
    pub dst_service: Option<String>,
    pub src_ip: Option<String>,
    pub dst_ip: Option<String>,
    pub dst_port: Option<i64>,
    /// `otlp`（缺省表示合并；当前仅有 otlp 数据源）。
    pub source: Option<String>,
    pub agent_id: Option<String>,
    pub min_requests: Option<i64>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// 边列表的一行。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EdgeRow {
    pub bucket_ts: i64,
    pub src_service: String,
    pub dst_service: String,
    pub span_kind: String,
    pub calls: i64,
    pub errors: i64,
    pub duration_sum: i64,
    pub duration_max: i64,
    pub source: String,
    pub agent_id: String,
}

/// 边列表分页结果。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EdgeSearchPage {
    pub total: i64,
    pub edges: Vec<EdgeRow>,
}

/// 服务清单的一行。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceRow {
    pub service: String,
    pub instance_count: i64,
    pub last_seen_ts: i64,
}

/// trace 列表查询。
pub async fn search_traces(
    sql: &dyn RelationalStore,
    query: &TraceSearchQuery,
) -> Result<TraceSearchPage, DataplaneError> {
    let (from_ts, to_ts) = resolve_window(query.from_ts, query.to_ts, dataplane_apm_now())?;
    let mut params: Vec<SqlValue> = vec![SqlValue::Integer(from_ts), SqlValue::Integer(to_ts)];
    let mut where_sql = String::from(" WHERE start_ts >= ?1 AND start_ts <= ?2");
    push_eq(
        &mut where_sql,
        &mut params,
        "root_service",
        query.service.as_deref(),
    );
    push_eq(
        &mut where_sql,
        &mut params,
        "root_operation",
        query.operation.as_deref(),
    );
    push_eq(
        &mut where_sql,
        &mut params,
        "status",
        query.status.as_deref(),
    );
    push_eq(
        &mut where_sql,
        &mut params,
        "agent_id",
        query.agent_id.as_deref(),
    );
    push_eq(
        &mut where_sql,
        &mut params,
        "host_id",
        query.host_id.as_deref(),
    );
    push_eq(
        &mut where_sql,
        &mut params,
        "data_id",
        query.data_id.as_deref(),
    );
    if let Some(min) = query.min_duration_micros {
        params.push(SqlValue::Integer(min));
        where_sql.push_str(&format!(" AND duration_micros >= ?{}", params.len()));
    }

    let total = scalar_i64(
        sql,
        &format!("SELECT COUNT(*) FROM {}{where_sql}", tables::TRACE_SUMMARY),
        &params,
    )
    .await?;

    let sort = match query.sort.as_deref() {
        None | Some("start_ts") => "start_ts",
        Some("duration_micros") => "duration_micros",
        Some(other) => {
            return Err(DataplaneError::invalid_argument(format!(
                "unsupported sort: {other}"
            )))
        }
    };
    let order = match query.order.as_deref() {
        None | Some("desc") => "DESC",
        Some("asc") => "ASC",
        Some(other) => {
            return Err(DataplaneError::invalid_argument(format!(
                "unsupported order: {other}"
            )))
        }
    };
    let limit = clamp_limit(query.limit);
    let offset = query.offset.unwrap_or(0);
    let mut page_params = params.clone();
    page_params.push(SqlValue::Integer(limit as i64));
    page_params.push(SqlValue::Integer(offset as i64));

    let result = sql
        .execute(
            &format!(
                "SELECT trace_id, start_ts, duration_micros, root_service, root_operation,
                        span_count, error_count, status, services_json
                 FROM {}{where_sql} ORDER BY {sort} {order} LIMIT ?{} OFFSET ?{}",
                tables::TRACE_SUMMARY,
                params.len() + 1,
                params.len() + 2
            ),
            &page_params,
        )
        .await?;

    let traces = result
        .rows
        .iter()
        .filter_map(summary_from_row)
        .collect::<Vec<_>>();
    Ok(TraceSearchPage { total, traces })
}

/// trace 详情：摘要 + 明细。
pub async fn get_trace(
    sql: &dyn RelationalStore,
    log: &dyn LogStore,
    trace_id: &str,
    detail_min_duration_micros: i64,
) -> Result<TraceDetail, DataplaneError> {
    if trace_id.len() != 32 || !trace_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(DataplaneError::invalid_argument(format!(
            "trace_id must be 32 hex chars, got {trace_id:?}"
        )));
    }
    let result = sql
        .execute(
            &format!(
                "SELECT trace_id, start_ts, duration_micros, root_service, root_operation,
                        span_count, error_count, status, services_json
                 FROM {} WHERE trace_id = ?1",
                tables::TRACE_SUMMARY
            ),
            &[SqlValue::Text(trace_id.to_lowercase())],
        )
        .await?;
    let Some(summary) = result.rows.first().and_then(summary_from_row) else {
        return Err(DataplaneError::not_found(format!(
            "trace not found: {trace_id}"
        )));
    };

    let mut filter = IndexedLogFilter::by_trace_id(trace_id.to_lowercase());
    filter.limit = DETAIL_SPAN_LIMIT;
    let spans: Vec<LogRecord> = log.search_indexed(filter).await?;

    let expected = summary.span_count;
    let partial = (spans.len() as i64) < expected;
    let reason = if partial {
        Some(if spans.is_empty() {
            // 一条明细都没有时有两种可能：整条 trace 都没到明细阈值（trace 自身耗时
            // 低于阈值），或明细已过保留期（明细短于摘要）。
            if detail_min_duration_micros > 0
                && summary.duration_micros < detail_min_duration_micros
            {
                "detail_filtered".to_string()
            } else {
                "retention_expired".to_string()
            }
        } else {
            // 部分写入：索引重建窗口，或只有部分 span 达到明细阈值。
            "detail_filtered".to_string()
        })
    } else {
        None
    };

    Ok(TraceDetail {
        summary,
        spans: spans.iter().map(span_to_json).collect(),
        partial,
        reason,
        expected_span_count: expected,
    })
}

/// 边列表查询。
pub async fn search_edges(
    sql: &dyn RelationalStore,
    query: &EdgeSearchQuery,
) -> Result<EdgeSearchPage, DataplaneError> {
    let (from_ts, to_ts) = resolve_window(query.from_ts, query.to_ts, dataplane_apm_now())?;
    // 目前只有 otlp 数据源；请求 ebpf 时返回空集（由 ebpf-observability 接入）。
    if let Some(source) = query.source.as_deref() {
        if source != "otlp" {
            return Ok(EdgeSearchPage {
                total: 0,
                edges: Vec::new(),
            });
        }
    }

    let mut params: Vec<SqlValue> = vec![SqlValue::Integer(from_ts), SqlValue::Integer(to_ts)];
    let mut where_sql = String::from(" WHERE bucket_start >= ?1 AND bucket_start <= ?2");
    push_eq(
        &mut where_sql,
        &mut params,
        "src_service",
        query.src_service.as_deref(),
    );
    push_eq(
        &mut where_sql,
        &mut params,
        "dst_service",
        query.dst_service.as_deref(),
    );
    push_eq(
        &mut where_sql,
        &mut params,
        "agent_id",
        query.agent_id.as_deref(),
    );
    if let Some(min) = query.min_requests {
        params.push(SqlValue::Integer(min));
        where_sql.push_str(&format!(" AND calls >= ?{}", params.len()));
    }

    let total = scalar_i64(
        sql,
        &format!("SELECT COUNT(*) FROM {}{where_sql}", tables::EDGE_SUMMARY),
        &params,
    )
    .await?;

    let limit = clamp_limit(query.limit);
    let offset = query.offset.unwrap_or(0);
    let mut page_params = params.clone();
    page_params.push(SqlValue::Integer(limit as i64));
    page_params.push(SqlValue::Integer(offset as i64));
    let result = sql
        .execute(
            &format!(
                "SELECT bucket_start, src_service, dst_service, span_kind, SUM(calls), SUM(errors),
                        SUM(duration_sum), MAX(duration_max), agent_id
                 FROM {}{where_sql}
                 GROUP BY bucket_start, src_service, dst_service, span_kind
                 ORDER BY bucket_start DESC, calls DESC LIMIT ?{} OFFSET ?{}",
                tables::EDGE_SUMMARY,
                params.len() + 1,
                params.len() + 2
            ),
            &page_params,
        )
        .await?;

    let edges = result
        .rows
        .iter()
        .filter_map(|row| match row.as_slice() {
            [
                SqlValue::Integer(bucket),
                SqlValue::Text(src),
                SqlValue::Text(dst),
                SqlValue::Text(kind),
                calls,
                errors,
                sum,
                max,
                SqlValue::Text(agent),
            ] => Some(EdgeRow {
                bucket_ts: *bucket,
                src_service: src.clone(),
                dst_service: dst.clone(),
                span_kind: kind.clone(),
                calls: as_i64(calls),
                errors: as_i64(errors),
                duration_sum: as_i64(sum),
                duration_max: as_i64(max),
                source: "otlp".to_string(),
                agent_id: agent.clone(),
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    Ok(EdgeSearchPage { total, edges })
}

/// 服务清单（来自端点半表）。
pub async fn list_services(sql: &dyn RelationalStore) -> Result<Vec<ServiceRow>, DataplaneError> {
    let result = sql
        .execute(
            &format!(
                "SELECT service, COUNT(*), MAX(last_seen_ts) FROM {}
                 GROUP BY service ORDER BY service",
                tables::SERVICE_ENDPOINT
            ),
            &[],
        )
        .await?;
    Ok(result
        .rows
        .iter()
        .filter_map(|row| match row.as_slice() {
            [SqlValue::Text(service), count, SqlValue::Integer(last)] => Some(ServiceRow {
                service: service.clone(),
                instance_count: as_i64(count),
                last_seen_ts: *last,
            }),
            _ => None,
        })
        .collect())
}

/// 解析时间窗：缺省为最近 1 小时；起点晚于终点报 `invalid_argument`。
pub fn resolve_window(
    from_ts: Option<i64>,
    to_ts: Option<i64>,
    now_ts: i64,
) -> Result<(i64, i64), DataplaneError> {
    let to_ts = to_ts.unwrap_or(now_ts);
    let from_ts = from_ts.unwrap_or_else(|| to_ts - DEFAULT_WINDOW_MICROS);
    if from_ts > to_ts {
        return Err(DataplaneError::invalid_argument(
            "from_ts must not be later than to_ts",
        ));
    }
    Ok((from_ts, to_ts))
}

/// 列表条数规整：缺省 50，上限 500。
#[must_use]
pub fn clamp_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn push_eq(where_sql: &mut String, params: &mut Vec<SqlValue>, column: &str, value: Option<&str>) {
    if let Some(value) = value {
        params.push(SqlValue::Text(value.to_string()));
        where_sql.push_str(&format!(" AND {column} = ?{}", params.len()));
    }
}

fn summary_from_row(row: &Vec<SqlValue>) -> Option<TraceSummary> {
    match row.as_slice() {
        [SqlValue::Text(trace_id), SqlValue::Integer(start_ts), SqlValue::Integer(duration), SqlValue::Text(root_service), SqlValue::Text(root_operation), span_count, error_count, SqlValue::Text(status), SqlValue::Text(services_json)] => {
            Some(TraceSummary {
                trace_id: trace_id.clone(),
                start_ts: *start_ts,
                duration_micros: *duration,
                root_service: root_service.clone(),
                root_operation: root_operation.clone(),
                span_count: as_i64(span_count),
                error_count: as_i64(error_count),
                status: status.clone(),
                services: serde_json::from_str(services_json).unwrap_or_default(),
            })
        }
        _ => None,
    }
}

fn span_to_json(record: &LogRecord) -> Value {
    json!({
        "id": record.id,
        "timestamp": record.timestamp,
        "level": record.level,
        "message": record.message,
        "labels": record.labels,
    })
}

async fn scalar_i64(
    sql: &dyn RelationalStore,
    statement: &str,
    params: &[SqlValue],
) -> Result<i64, DataplaneError> {
    let result = sql.execute(statement, params).await?;
    Ok(result
        .rows
        .first()
        .and_then(|row| row.first())
        .map_or(0, as_i64))
}

fn as_i64(value: &SqlValue) -> i64 {
    match value {
        SqlValue::Integer(i) => *i,
        SqlValue::Real(f) => *f as i64,
        _ => 0,
    }
}

/// 当前 Unix 微秒（查询侧默认时间窗用；与 `crate::now_micros` 同源）。
fn dataplane_apm_now() -> i64 {
    crate::now_micros()
}
