//! span 配对与边摘要：`client` span 与对端 `server` span 配对成拓扑边。
//!
//! 规则（`/.monkeycode/specs/apm-tracing/requirements.md` Requirement 8 与
//! `design.md`「span 配对与边摘要」）：
//!
//! - 边方向固定：`src_service` 取 `kind=client` 一侧，`dst_service` 取对端
//!   `kind=server` 一侧；不会出现方向翻转。
//! - 配对条件：同 `trace_id`，且 `server.parent_span_id == client.span_id`。
//! - 配对不到时用 client 的 `attributes` 兜底：`server.address` / `net.peer.name`
//!   / `net.peer.ip`，值前缀 `unknown:`；全缺时用 `unknown`。
//! - 一条 client span 匹配到多条 server span 时只算**一次调用**（延迟样本取 client 持续时间）。
//! - span 到达顺序不保证：client 先到则等 server，server 先到则等 client（反向补齐）。
//! - 边延迟样本取 client span 耗时（含网络与对端处理）。
//!
//! 落库是「读回已有行 → 合并增量 → 写绝对值」，与摘要累加器同一套理由：条目可以被
//! 淘汰后重建，只要每次都读回就不会重复计数。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dataplane_core::{DataplaneError, ErrorCode, SqlValue};
use dataplane_ingest::trace::TraceSpan;
use dataplane_ingest::DataEnvelope;
use dataplane_sql::RelationalStore;

use crate::red::RedSamples;
use crate::tables;

/// 边桶宽（秒）：与 RED 指标的 1 分钟聚合对齐。
pub const EDGE_BUCKET_SECS: i64 = 60;

/// 未配对 span 与配对记录的保留时长（秒）。跨分钟再配对出的边已不属于同一桶。
pub const PENDING_TTL_SECS: i64 = 120;

/// 把 `unknown:*` 目标归一为真实服务名的解析器（由端点表实现）。
#[async_trait]
pub trait ServiceResolver: Send + Sync {
    /// 依次按 `(host_ip, port)`、Pod 名解析；未命中返回 `None`。
    async fn resolve(
        &self,
        host_ip: &str,
        port: i64,
        pod_name: &str,
    ) -> Result<Option<String>, DataplaneError>;
}

/// 一条边的增量。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct EdgeDelta {
    calls: u64,
    errors: u64,
    duration_sum: i64,
    duration_max: i64,
    data_id: String,
    /// 归一 `unknown:*` 时需要的对端标识（ip / port / pod）。
    peer_host: String,
    peer_port: i64,
    peer_pod: String,
}

/// 边的聚合键：桶 + 源 + 目标 + 对端 kind（当前恒为 `server`）+ agent。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EdgeKey {
    bucket_start: i64,
    src_service: String,
    dst_service: String,
    span_kind: String,
    agent_id: String,
}

/// 已见但对端还没到的 span。
#[derive(Debug, Clone)]
struct PendingSpan {
    service: String,
    pod_name: String,
    timestamp: i64,
    /// 仅 client 侧使用：兜底目标与归一所需的对端地址。
    peer_host: String,
    peer_port: i64,
    fallback_dst: String,
    duration_micros: i64,
    status_code: String,
    /// 兜底边落库时需要（待配对期间 span 与信封分离，必须随 span 一起记住）。
    agent_id: String,
    data_id: String,
}

impl PendingSpan {
    fn from_span(span: &TraceSpan, envelope: &DataEnvelope) -> Self {
        let peer_host = attr(span, &["server.address", "net.peer.name", "net.peer.ip"]);
        let peer_port = attr(span, &["server.port", "net.peer.port"])
            .parse::<i64>()
            .unwrap_or(0);
        let fallback_dst = if peer_host.is_empty() {
            "unknown".to_string()
        } else {
            format!("unknown:{peer_host}")
        };
        Self {
            service: span.service.clone(),
            pod_name: span
                .resource
                .get("k8s.pod.name")
                .cloned()
                .unwrap_or_default(),
            timestamp: span.timestamp,
            peer_host,
            peer_port,
            fallback_dst,
            duration_micros: span.duration_micros(),
            status_code: span.status_code.clone(),
            agent_id: envelope.agent_id.clone(),
            data_id: envelope.data_id.clone(),
        }
    }
}

fn attr(span: &TraceSpan, keys: &[&str]) -> String {
    for key in keys {
        if let Some(value) = span.attributes.get(*key) {
            if !value.is_empty() {
                return value.clone();
            }
        }
    }
    String::new()
}

#[derive(Debug, Default)]
struct Inner {
    /// `server` 侧等待 client：键为 `(trace_id, parent_span_id)`，即对端 client 的 span_id。
    waiting_client: BTreeMap<(String, String), Vec<PendingSpan>>,
    /// `client` 侧等待 server：键为 `(trace_id, span_id)`。
    waiting_server: BTreeMap<(String, String), PendingSpan>,
    /// 已计过数的 client span（键 → 该 client 的时间戳）：既做重放去重，也用于过期清理。
    recorded: BTreeMap<(String, String), i64>,
    edges: BTreeMap<EdgeKey, EdgeDelta>,
}

/// 边摘要累加器。
#[derive(Debug)]
pub struct EdgeAccumulator {
    /// 待配对 span 的内存上限（超过时按时间淘汰最旧）。
    pending_capacity: usize,
    /// 边延迟样本（供聚合任务算 p95）；桶关闭后由聚合任务取走。
    samples: Arc<RedSamples>,
    inner: Mutex<Inner>,
    dropped_pending: Mutex<u64>,
    paired_edges: Mutex<u64>,
}

impl EdgeAccumulator {
    #[must_use]
    pub fn new(pending_capacity: usize, samples: Arc<RedSamples>) -> Self {
        Self {
            pending_capacity: pending_capacity.max(1),
            samples,
            inner: Mutex::new(Inner::default()),
            dropped_pending: Mutex::new(0),
            paired_edges: Mutex::new(0),
        }
    }

    /// 观察一条 span；返回是否完成了配对。
    pub fn observe(&self, span: &TraceSpan, envelope: &DataEnvelope) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        let mut paired = false;
        match span.kind.as_str() {
            "client" => {
                let key_self = (span.trace_id.clone(), span.span_id.clone());
                let info = PendingSpan::from_span(span, envelope);
                if let Some(servers) = inner.waiting_client.remove(&key_self) {
                    // server 先到：反向补齐（一条 client 可对应多条 server，只计一次）。
                    for server in servers {
                        paired |= self.record(&mut inner, span, envelope, &info, Some(&server));
                    }
                }
                inner.waiting_server.insert(key_self, info);
            }
            "server" if !span.parent_span_id.is_empty() => {
                let waiting_key = (span.trace_id.clone(), span.parent_span_id.clone());
                let info = PendingSpan::from_span(span, envelope);
                if let Some(client) = inner.waiting_server.remove(&waiting_key) {
                    paired = self.record(&mut inner, span, envelope, &client, Some(&info));
                } else {
                    inner
                        .waiting_client
                        .entry(waiting_key)
                        .or_default()
                        .push(info);
                }
            }
            _ => return false,
        }
        self.evict(&mut inner, span.timestamp);
        paired
    }

    /// 记一条边：同一 client span 只计一次。
    fn record(
        &self,
        inner: &mut Inner,
        server: &TraceSpan,
        envelope: &DataEnvelope,
        client: &PendingSpan,
        paired_server: Option<&PendingSpan>,
    ) -> bool {
        let client_key = (server.trace_id.clone(), server.parent_span_id.clone());
        if inner.recorded.contains_key(&client_key) {
            return false;
        }
        inner.recorded.insert(client_key, client.timestamp);

        let (dst_service, peer_pod) = match paired_server {
            Some(s) => (s.service.clone(), s.pod_name.clone()),
            None => (client.fallback_dst.clone(), String::new()),
        };
        // 桶由发起端（client）时间戳决定：一条调用归属它发起的那个分钟。
        let bucket_start = bucket_of(client.timestamp);
        let key = EdgeKey {
            bucket_start,
            src_service: client.service.clone(),
            dst_service,
            span_kind: "server".to_string(),
            agent_id: envelope.agent_id.clone(),
        };
        let dst_for_samples = key.dst_service.clone();
        let entry = inner.edges.entry(key).or_insert_with(|| EdgeDelta {
            data_id: envelope.data_id.clone(),
            peer_host: client.peer_host.clone(),
            peer_port: client.peer_port,
            peer_pod: peer_pod.clone(),
            ..EdgeDelta::default()
        });
        entry.calls = entry.calls.saturating_add(1);
        if client.status_code == "error" {
            entry.errors = entry.errors.saturating_add(1);
        }
        entry.duration_sum = entry.duration_sum.saturating_add(client.duration_micros);
        entry.duration_max = entry.duration_max.max(client.duration_micros);
        entry.data_id = envelope.data_id.clone();
        if entry.peer_host.is_empty() {
            entry.peer_host = client.peer_host.clone();
            entry.peer_port = client.peer_port;
        }
        if entry.peer_pod.is_empty() {
            entry.peer_pod = peer_pod;
        }
        self.samples.observe_edge(
            bucket_start,
            &client.service,
            &dst_for_samples,
            "server",
            &client.status_code,
            client.duration_micros,
        );
        if let Ok(mut count) = self.paired_edges.lock() {
            *count += 1;
        }
        true
    }

    /// 过期与容量淘汰。
    fn evict(&self, inner: &mut Inner, now_ts: i64) {
        let ttl_cutoff = now_ts - PENDING_TTL_SECS * 1_000_000;
        let mut dropped = 0u64;
        inner.waiting_client.retain(|_, spans| {
            spans.retain(|s| s.timestamp >= ttl_cutoff);
            !spans.is_empty()
        });
        let before = inner.waiting_server.len();
        inner
            .waiting_server
            .retain(|_, s| s.timestamp >= ttl_cutoff);
        dropped += (before - inner.waiting_server.len()) as u64;
        // 去重记录只需覆盖重放窗口，同样按 TTL 清理，避免无限增长。
        let before_recorded = inner.recorded.len();
        inner.recorded.retain(|_, ts| *ts >= ttl_cutoff);
        dropped += (before_recorded - inner.recorded.len()) as u64;

        let total =
            inner.waiting_server.len() + inner.waiting_client.values().map(Vec::len).sum::<usize>();
        if total > self.pending_capacity {
            let overflow = total - self.pending_capacity;
            let mut oldest: Vec<((String, String), i64)> = inner
                .waiting_server
                .iter()
                .map(|(k, v)| (k.clone(), v.timestamp))
                .collect();
            oldest.sort_by_key(|(_, ts)| *ts);
            for (key, _) in oldest.into_iter().take(overflow) {
                inner.waiting_server.remove(&key);
                dropped += 1;
            }
        }
        if dropped > 0 {
            if let Ok(mut count) = self.dropped_pending.lock() {
                *count += dropped;
            }
        }
    }

    /// 待写入的边数。
    #[must_use]
    pub fn dirty_len(&self) -> usize {
        self.inner.lock().map(|i| i.edges.len()).unwrap_or(0)
    }

    /// 待配对的 span 数。
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.inner
            .lock()
            .map(|i| {
                i.waiting_server.len() + i.waiting_client.values().map(Vec::len).sum::<usize>()
            })
            .unwrap_or(0)
    }

    /// 已完成配对的边次数（自监控用）。
    #[must_use]
    pub fn paired_edges(&self) -> u64 {
        self.paired_edges.lock().map(|c| *c).unwrap_or(0)
    }

    /// 因超时/超容量丢弃的待配对 span 数（自监控用）。
    #[must_use]
    pub fn dropped_pending(&self) -> u64 {
        self.dropped_pending.lock().map(|c| *c).unwrap_or(0)
    }

    /// 落地边摘要；`resolver` 非空时把 `unknown:*` 目标归一为真实服务名。
    ///
    /// 除了已配对的边，还会把「桶已关闭但始终没有 server 对端」的 client span 作为
    /// 兜底边落库（`dst_service` 为 `unknown*`）：配对是异步的，只有等桶关闭才能判定
    /// 「找不到对端」。已计数的 client 会被去重集合拦住，因此晚到的 server 不会让同一次
    /// 调用被计两次。
    pub async fn flush(
        &self,
        sql: &dyn RelationalStore,
        now_ts: i64,
        resolver: Option<&dyn ServiceResolver>,
    ) -> Result<usize, DataplaneError> {
        let batch: Vec<(EdgeKey, EdgeDelta)> = {
            let mut inner = self.inner.lock().map_err(|_| lock_error())?;
            let mut batch: Vec<(EdgeKey, EdgeDelta)> = Vec::new();
            // 1) 桶已关闭且仍在等 server 的 client span → 兜底边。
            let closed: Vec<((String, String), PendingSpan)> = inner
                .waiting_server
                .iter()
                .filter(|(_, span)| span.timestamp + EDGE_BUCKET_SECS * 1_000_000 <= now_ts)
                .map(|(key, span)| (key.clone(), span.clone()))
                .collect();
            for (key, span) in closed {
                inner.waiting_server.remove(&key);
                if inner.recorded.contains_key(&key) {
                    continue;
                }
                inner.recorded.insert(key.clone(), span.timestamp);
                self.samples.observe_edge(
                    bucket_of(span.timestamp),
                    &span.service,
                    &span.fallback_dst,
                    "server",
                    &span.status_code,
                    span.duration_micros,
                );
                batch.push((fallback_edge_key(&span), fallback_edge_delta(&span)));
            }
            // 2) 已配对的边。
            let keys: Vec<EdgeKey> = inner.edges.keys().cloned().collect();
            for key in keys {
                if let Some(delta) = inner.edges.remove(&key) {
                    batch.push((key, delta));
                }
            }
            batch
        };

        let mut written = 0usize;
        for (mut key, delta) in batch {
            if key.dst_service.starts_with("unknown") {
                if let Some(resolver) = resolver {
                    // 归一失败保持原值：前端拓扑图例会单列「未识别」。
                    match resolver
                        .resolve(&delta.peer_host, delta.peer_port, &delta.peer_pod)
                        .await
                    {
                        Ok(Some(service)) => key.dst_service = service,
                        Ok(None) => {}
                        Err(e) => eprintln!(
                            "apm: edge target resolve failed for {}: {}",
                            key.dst_service, e.message
                        ),
                    }
                }
            }
            if let Err(e) = flush_one(sql, &key, &delta).await {
                eprintln!(
                    "apm: edge summary flush failed for {}->{}: {}",
                    key.src_service, key.dst_service, e.message
                );
                continue;
            }
            written += 1;
        }
        Ok(written)
    }
}

/// 把时间戳对齐到分钟桶起点（供 sink 记录 span 样本复用）。
#[must_use]
pub fn bucket_of_public(timestamp: i64) -> i64 {
    bucket_of(timestamp)
}

/// 把时间戳对齐到分钟桶起点。
fn bucket_of(timestamp: i64) -> i64 {
    timestamp.div_euclid(EDGE_BUCKET_SECS * 1_000_000) * EDGE_BUCKET_SECS * 1_000_000
}

/// 兜底边的键：`dst_service` 用 client 的 `unknown*` 目标。
fn fallback_edge_key(client: &PendingSpan) -> EdgeKey {
    EdgeKey {
        bucket_start: bucket_of(client.timestamp),
        src_service: client.service.clone(),
        dst_service: client.fallback_dst.clone(),
        span_kind: "server".to_string(),
        agent_id: client.agent_id.clone(),
    }
}

fn fallback_edge_delta(client: &PendingSpan) -> EdgeDelta {
    EdgeDelta {
        calls: 1,
        errors: u64::from(client.status_code == "error"),
        duration_sum: client.duration_micros,
        duration_max: client.duration_micros,
        data_id: client.data_id.clone(),
        peer_host: client.peer_host.clone(),
        peer_port: client.peer_port,
        ..EdgeDelta::default()
    }
}

/// 读回已有行 → 合并 → 写绝对值。
async fn flush_one(
    sql: &dyn RelationalStore,
    key: &EdgeKey,
    delta: &EdgeDelta,
) -> Result<(), DataplaneError> {
    let existing = sql
        .execute(
            &format!(
                "SELECT calls, errors, duration_sum, duration_max, data_id FROM {}
                 WHERE bucket_start = ?1 AND src_service = ?2 AND dst_service = ?3
                   AND span_kind = ?4 AND agent_id = ?5",
                tables::EDGE_SUMMARY
            ),
            &[
                SqlValue::Integer(key.bucket_start),
                SqlValue::Text(key.src_service.clone()),
                SqlValue::Text(key.dst_service.clone()),
                SqlValue::Text(key.span_kind.clone()),
                SqlValue::Text(key.agent_id.clone()),
            ],
        )
        .await?;
    let row = existing.rows.first().cloned().unwrap_or_default();
    let calls = as_i64(row.first()) + delta.calls as i64;
    let errors = as_i64(row.get(1)) + delta.errors as i64;
    let duration_sum = as_i64(row.get(2)) + delta.duration_sum;
    let duration_max = as_i64(row.get(3)).max(delta.duration_max);
    let data_id = match row.get(4) {
        Some(SqlValue::Text(s)) if !s.is_empty() => s.clone(),
        _ => delta.data_id.clone(),
    };

    sql.execute(
        &format!(
            "INSERT OR REPLACE INTO {} (bucket_start, src_service, dst_service, span_kind,
                calls, errors, duration_sum, duration_max, agent_id, data_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            tables::EDGE_SUMMARY
        ),
        &[
            SqlValue::Integer(key.bucket_start),
            SqlValue::Text(key.src_service.clone()),
            SqlValue::Text(key.dst_service.clone()),
            SqlValue::Text(key.span_kind.clone()),
            SqlValue::Integer(calls),
            SqlValue::Integer(errors),
            SqlValue::Integer(duration_sum),
            SqlValue::Integer(duration_max),
            SqlValue::Text(key.agent_id.clone()),
            SqlValue::Text(data_id),
        ],
    )
    .await?;
    Ok(())
}

fn as_i64(value: Option<&SqlValue>) -> i64 {
    match value {
        Some(SqlValue::Integer(i)) => *i,
        Some(SqlValue::Real(f)) => *f as i64,
        _ => 0,
    }
}

fn lock_error() -> DataplaneError {
    DataplaneError::new(ErrorCode::QueryFailed, "edge accumulator lock poisoned")
}
