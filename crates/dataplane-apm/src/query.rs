//! APM 查询：trace 列表/详情、边列表、服务清单。
//!
//! 对应 `.monkeycode/specs/apm-tracing/requirements.md` Requirement 5、6、12 与
//! `design.md`「HTTP 路由」。实现要点：
//!
//! - 列表与详情都读 sqlite `apm_trace_summary`；span 明细读 `LogStore` 的 v2
//!   索引（`IndexedLogFilter::by_trace_id`），不用 post-filter。
//! - 详情返回条数少于 `span_count` 时带 `partial` 与 `reason`，让前端能区分
//!   「索引重建窗口」「明细已过保留期」「明细被阈值过滤」。
//! - 边列表有两个来源：`apm_edge_summary`（OTLP，span 配对）与 `ebpf_edges`（eBPF 边聚合）。
//!   `source` 指定其一时只查那一路（SQL 分页）；缺省时两路按
//!   `(bucket_ts, src_service, dst_service, protocol)` 合并汇总，`source` 记为 `merged`。

use dataplane_core::{DataplaneError, SqlValue};
use dataplane_log::{IndexedLogFilter, LogRecord, LogStore};
use dataplane_sql::RelationalStore;
use std::collections::BTreeMap;

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
    /// `otlp` 或 `ebpf`。
    pub collector: String,
    pub agent_id: String,
    pub host_id: String,
    pub data_id: String,
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
    /// `tcp` | `udp`；OTLP 侧的边没有协议，指定后只剩 eBPF 侧。
    #[serde(default)]
    pub protocol: Option<String>,
    /// `otlp` | `ebpf`；缺省时两路按 `(bucket_ts, src_service, dst_service, protocol)` 合并汇总。
    pub source: Option<String>,
    pub agent_id: Option<String>,
    pub min_requests: Option<i64>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// 边列表的一行。
///
/// `calls` / `errors` 对两侧都成立（eBPF 侧分别是连接数与失败数），保留旧字段以免前端与 CLI 失效；
/// `connections` / `failures` / 字节 / 重传 / IP 端口这些是 eBPF 侧独有的，OTLP 行留空或 0。
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
    #[serde(default)]
    pub src_ip: String,
    #[serde(default)]
    pub dst_ip: String,
    #[serde(default)]
    pub dst_port: i64,
    #[serde(default)]
    pub protocol: String,
    pub connections: i64,
    pub failures: i64,
    pub bytes_sent: i64,
    pub bytes_recv: i64,
    pub duration_avg_micros: i64,
    pub tcp_retrans: i64,
}

/// 合并两路来源时使用的 `source` 取值：客户端据此区分「这一行是两路相加的结果」。
pub const SOURCE_MERGED: &str = "merged";

/// 边列表分页结果。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EdgeSearchPage {
    pub total: i64,
    pub edges: Vec<EdgeRow>,
}

/// 服务清单里的一个端点实例（来自端点半表）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceInstance {
    pub instance_id: String,
    pub pod_name: String,
    pub node_name: String,
    pub host_ip: String,
    pub listen_port: i64,
    pub collector: String,
    pub first_seen_ts: i64,
    pub last_seen_ts: i64,
}

/// 服务清单的一行：服务 + 其实例明细。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceRow {
    pub service: String,
    pub instance_count: i64,
    pub last_seen_ts: i64,
    pub instances: Vec<ServiceInstance>,
}

/// 摘要行统一列清单（列表与详情共用，避免两处 SELECT 漂移）。
const TRACE_SUMMARY_COLUMNS: &str =
    "trace_id, start_ts, duration_micros, root_service, root_operation, span_count, error_count, \
     status, services_json, collector, agent_id, host_id, data_id";

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
                "SELECT {TRACE_SUMMARY_COLUMNS}
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
                "SELECT {TRACE_SUMMARY_COLUMNS} FROM {} WHERE trace_id = ?1",
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
///
/// `source` 缺省时合并两路来源（需求 13.3）：按 `(bucket_ts, src_service, dst_service, protocol)`
/// 汇总，`source` 记为 [`SOURCE_MERGED`]；指定来源时按 SQL 分页，避免把整表读进内存。
pub async fn search_edges(
    sql: &dyn RelationalStore,
    query: &EdgeSearchQuery,
) -> Result<EdgeSearchPage, DataplaneError> {
    let (from_ts, to_ts) = resolve_window(query.from_ts, query.to_ts, dataplane_apm_now())?;
    match query.source.as_deref() {
        Some("otlp") => search_otlp_edges(sql, query, from_ts, to_ts).await,
        Some("ebpf") => search_ebpf_edges(sql, query, from_ts, to_ts).await,
        None => {
            // 合并：两路各取全量候选（受同一个 limit/offset 约束时无法正确汇总，
            // 因此先在内存里合并再分页）。上限保护：单路最多取 `MERGE_SCAN_LIMIT` 行。
            let otlp = search_otlp_edges(sql, &unpaged(query), from_ts, to_ts).await?;
            let ebpf = search_ebpf_edges(sql, &unpaged(query), from_ts, to_ts).await?;
            let mut rows = merge_sources(otlp.edges, ebpf.edges);
            rows.sort_by(|a, b| {
                b.bucket_ts
                    .cmp(&a.bucket_ts)
                    .then(b.calls.cmp(&a.calls))
                    .then(a.src_service.cmp(&b.src_service))
            });
            let total = rows.len() as i64;
            let offset = query.offset.unwrap_or(0).min(rows.len());
            let limit = clamp_limit(query.limit);
            let edges = rows.into_iter().skip(offset).take(limit).collect();
            Ok(EdgeSearchPage { total, edges })
        }
        // 未知来源返回空集（与既有行为一致），不报错以免前端切换时整页失败。
        Some(_) => Ok(EdgeSearchPage {
            total: 0,
            edges: Vec::new(),
        }),
    }
}

/// 单路合并的候选上限：防止把整张边表读进内存。
const MERGE_SCAN_LIMIT: usize = 10_000;

/// 合并模式下的查询副本：分页在合并后做，单路先按 `MERGE_SCAN_LIMIT` 取候选。
fn unpaged(query: &EdgeSearchQuery) -> EdgeSearchQuery {
    EdgeSearchQuery {
        limit: Some(MERGE_SCAN_LIMIT),
        offset: Some(0),
        ..query.clone()
    }
}

/// 按 `(bucket_ts, src_service, dst_service)` 汇总两路来源。
///
/// **与需求 13.3 的偏差（已在 spec 记录）**：需求写的是按 `(bucket_ts, src_service, dst_service,
/// protocol)` 汇总，但 OTLP 侧的边**没有协议**（span 配对不产生协议信息），把 `protocol` 放进
/// 键里会让两路永远合不到一起，合并模式就失去意义。因此协议作为**行的字段**而不是身份：
/// 同一对服务的两路数据合并成一行，协议取首个非空值；同一分钟内同一条边出现多种协议时记为
/// `mixed` 并把计数相加（这一限制只影响合并视图；只看 eBPF 时协议仍是分组维度）。
#[must_use]
pub fn merge_sources(otlp: Vec<EdgeRow>, ebpf: Vec<EdgeRow>) -> Vec<EdgeRow> {
    let mut merged: BTreeMap<(i64, String, String), EdgeRow> = BTreeMap::new();
    for mut row in otlp.into_iter().chain(ebpf) {
        let key = (
            row.bucket_ts,
            row.src_service.clone(),
            row.dst_service.clone(),
        );
        match merged.get_mut(&key) {
            Some(current) => {
                current.calls += row.calls;
                current.connections += row.connections;
                current.errors += row.errors;
                current.failures += row.failures;
                current.bytes_sent += row.bytes_sent;
                current.bytes_recv += row.bytes_recv;
                current.duration_sum += row.duration_sum;
                current.duration_max = current.duration_max.max(row.duration_max);
                current.tcp_retrans += row.tcp_retrans;
                current.duration_avg_micros = if current.connections > 0 {
                    current.duration_sum / current.connections
                } else {
                    0
                };
                merge_protocol(&mut current.protocol, &row.protocol);
                if current.span_kind.is_empty() {
                    current.span_kind = std::mem::take(&mut row.span_kind);
                }
                if current.agent_id.is_empty() {
                    current.agent_id = std::mem::take(&mut row.agent_id);
                }
            }
            None => {
                row.source = SOURCE_MERGED.to_string();
                row.duration_avg_micros = if row.connections > 0 {
                    row.duration_sum / row.connections
                } else {
                    0
                };
                merged.insert(key, row);
            }
        }
    }
    merged.into_values().collect()
}

/// 协议归并：首个非空值优先，冲突记为 `mixed`（合并视图里同一对边混合了多种协议）。
fn merge_protocol(current: &mut String, incoming: &str) {
    if incoming.is_empty() {
        return;
    }
    if current.is_empty() {
        *current = incoming.to_string();
    } else if current != incoming {
        *current = "mixed".to_string();
    }
}

/// OTLP 侧边（`apm_edge_summary`）。
async fn search_otlp_edges(
    sql: &dyn RelationalStore,
    query: &EdgeSearchQuery,
    from_ts: i64,
    to_ts: i64,
) -> Result<EdgeSearchPage, DataplaneError> {
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
    // OTLP 侧没有协议与 IP：只要按这些维度过滤，结果必然为空。
    if query.protocol.is_some()
        || query.src_ip.is_some()
        || query.dst_ip.is_some()
        || query.dst_port.is_some()
    {
        return Ok(EdgeSearchPage {
            total: 0,
            edges: Vec::new(),
        });
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
            ] => {
                let calls = as_i64(calls);
                let duration_sum = as_i64(sum);
                Some(EdgeRow {
                    bucket_ts: *bucket,
                    src_service: src.clone(),
                    dst_service: dst.clone(),
                    span_kind: kind.clone(),
                    calls,
                    errors: as_i64(errors),
                    duration_sum,
                    duration_max: as_i64(max),
                    source: "otlp".to_string(),
                    agent_id: agent.clone(),
                    // OTLP 没有连接/IP/字节语义，保持空值。
                    src_ip: String::new(),
                    dst_ip: String::new(),
                    dst_port: 0,
                    protocol: String::new(),
                    connections: calls,
                    failures: as_i64(errors),
                    bytes_sent: 0,
                    bytes_recv: 0,
                    duration_avg_micros: if calls > 0 { duration_sum / calls } else { 0 },
                    tcp_retrans: 0,
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    Ok(EdgeSearchPage { total, edges })
}

/// eBPF 侧边（`ebpf_edges`）。
async fn search_ebpf_edges(
    sql: &dyn RelationalStore,
    query: &EdgeSearchQuery,
    from_ts: i64,
    to_ts: i64,
) -> Result<EdgeSearchPage, DataplaneError> {
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
        "src_ip",
        query.src_ip.as_deref(),
    );
    push_eq(
        &mut where_sql,
        &mut params,
        "dst_ip",
        query.dst_ip.as_deref(),
    );
    push_eq(
        &mut where_sql,
        &mut params,
        "protocol",
        query.protocol.as_deref(),
    );
    push_eq(
        &mut where_sql,
        &mut params,
        "agent_id",
        query.agent_id.as_deref(),
    );
    if let Some(port) = query.dst_port {
        params.push(SqlValue::Integer(port));
        where_sql.push_str(&format!(" AND dst_port = ?{}", params.len()));
    }
    if let Some(min) = query.min_requests {
        params.push(SqlValue::Integer(min));
        where_sql.push_str(&format!(" AND connections >= ?{}", params.len()));
    }

    let total = scalar_i64(
        sql,
        &format!("SELECT COUNT(*) FROM {}{where_sql}", tables::EBPF_EDGES),
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
                "SELECT bucket_start, src_service, dst_service, protocol, src_ip, dst_ip, dst_port,
                        SUM(connections), SUM(failures), SUM(bytes_sent), SUM(bytes_recv),
                        SUM(duration_sum), MAX(duration_max), SUM(tcp_retrans), agent_id
                 FROM {}{where_sql}
                 GROUP BY bucket_start, src_service, dst_service, protocol, src_ip, dst_ip, dst_port
                 ORDER BY bucket_start DESC, SUM(connections) DESC LIMIT ?{} OFFSET ?{}",
                tables::EBPF_EDGES,
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
                SqlValue::Text(protocol),
                SqlValue::Text(src_ip),
                SqlValue::Text(dst_ip),
                dst_port,
                connections,
                failures,
                bytes_sent,
                bytes_recv,
                duration_sum,
                duration_max,
                tcp_retrans,
                SqlValue::Text(agent),
            ] => {
                let connections = as_i64(connections);
                let duration_sum = as_i64(duration_sum);
                Some(EdgeRow {
                    bucket_ts: *bucket,
                    src_service: src.clone(),
                    dst_service: dst.clone(),
                    // eBPF 不产生 span，`span_kind` 固定为空：拓扑页按 `(src,dst)` 合并时不受影响。
                    span_kind: String::new(),
                    calls: connections,
                    errors: as_i64(failures),
                    duration_sum,
                    duration_max: as_i64(duration_max),
                    source: "ebpf".to_string(),
                    agent_id: agent.clone(),
                    src_ip: src_ip.clone(),
                    dst_ip: dst_ip.clone(),
                    dst_port: as_i64(dst_port),
                    protocol: protocol.clone(),
                    connections,
                    failures: as_i64(failures),
                    bytes_sent: as_i64(bytes_sent),
                    bytes_recv: as_i64(bytes_recv),
                    duration_avg_micros: if connections > 0 {
                        duration_sum / connections
                    } else {
                        0
                    },
                    tcp_retrans: as_i64(tcp_retrans),
                })
            }
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
                "SELECT service, instance_id, pod_name, node_name, host_ip, listen_port,
                        collector, first_seen_ts, last_seen_ts
                 FROM {} ORDER BY service, instance_id",
                tables::SERVICE_ENDPOINT
            ),
            &[],
        )
        .await?;

    // 按服务归并实例；同时给出实例数与最近出现时间（与旧响应字段兼容）。
    let mut rows: Vec<ServiceRow> = Vec::new();
    for row in result.rows.iter() {
        let [SqlValue::Text(service), SqlValue::Text(instance_id), SqlValue::Text(pod_name), SqlValue::Text(node_name), SqlValue::Text(host_ip), SqlValue::Integer(listen_port), SqlValue::Text(collector), SqlValue::Integer(first_seen), SqlValue::Integer(last_seen)] =
            row.as_slice()
        else {
            continue;
        };
        let instance = ServiceInstance {
            instance_id: instance_id.clone(),
            pod_name: pod_name.clone(),
            node_name: node_name.clone(),
            host_ip: host_ip.clone(),
            listen_port: *listen_port,
            collector: collector.clone(),
            first_seen_ts: *first_seen,
            last_seen_ts: *last_seen,
        };
        match rows.last_mut() {
            Some(current) if current.service == *service => {
                current.instance_count += 1;
                current.last_seen_ts = current.last_seen_ts.max(*last_seen);
                current.instances.push(instance);
            }
            _ => rows.push(ServiceRow {
                service: service.clone(),
                instance_count: 1,
                last_seen_ts: *last_seen,
                instances: vec![instance],
            }),
        }
    }
    Ok(rows)
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
    let [SqlValue::Text(trace_id), SqlValue::Integer(start_ts), SqlValue::Integer(duration), SqlValue::Text(root_service), SqlValue::Text(root_operation), span_count, error_count, SqlValue::Text(status), SqlValue::Text(services_json), SqlValue::Text(collector), SqlValue::Text(agent_id), SqlValue::Text(host_id), SqlValue::Text(data_id)] =
        row.as_slice()
    else {
        return None;
    };
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
        collector: collector.clone(),
        agent_id: agent_id.clone(),
        host_id: host_id.clone(),
        data_id: data_id.clone(),
    })
}

/// span 详情：优先返回写入时的完整 OTel 原文，并补一个派生字段 `duration_micros`
/// （原文只有纳秒起止；瀑布图需要直接可用的耗时）。没有原文（v3 之前的索引、
/// 或写入时序列化失败）时退化为标签投影。
fn span_to_json(record: &LogRecord) -> Value {
    let mut value = record
        .payload
        .as_deref()
        .and_then(|payload| serde_json::from_str::<Value>(payload).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| {
            json!({
                "id": record.id,
                "timestamp": record.timestamp,
                "level": record.level,
                "message": record.message,
                "labels": record.labels,
            })
        });
    if let Some(object) = value.as_object_mut() {
        let duration = match (
            object.get("start_unix_nano").and_then(Value::as_i64),
            object.get("end_unix_nano").and_then(Value::as_i64),
        ) {
            (Some(start), Some(end)) if end >= start => Some((end - start) / 1_000),
            _ => None,
        };
        if let Some(duration) = duration {
            object.insert("duration_micros".to_string(), json!(duration));
        } else {
            object
                .entry("duration_micros".to_string())
                .or_insert(json!(0));
        }
        // 便于前端直接使用，原文里没有 record_id 时补上。
        object
            .entry("record_id".to_string())
            .or_insert(json!(record.id));
    }
    value
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
