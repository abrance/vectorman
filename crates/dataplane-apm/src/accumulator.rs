//! trace 摘要累加器：内存累积 span 增量，按周期读改写落 sqlite。
//!
//! 设计要点（`/.monkeycode/specs/apm-tracing/design.md`「TraceSummaryAccumulator」）：
//!
//! - span 分批、乱序到达，汇总只能在 dataserver 完成；每条 span 都读写 sqlite
//!   成本过高，因此内存累计 + 周期 flush。
//! - flush 采用**读改写**：先读回已有行，把内存增量合并成绝对值再写回，因此
//!   「进程重启后重新累加」「条目被淘汰后重建」都不会重复计数或丢计数。
//! - 根 span 取 `start_ts` 最小者，因此必须在 sqlite 里记住 `root_start_ts`，
//!   否则跨 flush 无法比较出更早的根。
//! - 崩溃取舍：最多丢一个 flush 周期或 `dirty_threshold` 条的摘要增量。明细已在
//!   `LogStore`，因此详情页以实际返回条数为准并在不一致时提示。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use dataplane_core::{DataplaneError, ErrorCode, SqlValue};
use dataplane_ingest::trace::TraceSpan;
use dataplane_ingest::DataEnvelope;
use dataplane_sql::RelationalStore;

use crate::tables;

/// 累计器与端点 registry 的运行参数。
#[derive(Debug, Clone)]
pub struct ApmSinkConfig {
    /// 内存中同时保留的 trace 数上限；超限时优先 flush 并淘汰最早的条目。
    pub max_live_traces: usize,
    /// flush 周期（秒）。
    pub flush_interval_secs: u64,
    /// 累计多少个待写入 trace 就立即 flush。
    pub dirty_threshold: usize,
    /// 端点半保留期（天）。
    pub endpoint_retention_days: u32,
    /// 端点反查缓存 TTL（秒），含未命中的负面结果。
    pub endpoint_cache_ttl_secs: u64,
    /// 冷启动回载窗口（秒）：`max_end_ts` 在此窗口内的 trace 会被重新载入。
    pub reload_window_secs: i64,
    /// 边配对时待配对 span 的内存上限（缺省 `max_live_traces * 8`）。
    pub edge_pending_capacity: usize,
    /// 只写明细的耗时下限（微秒）；0 表示全部写明细。
    pub detail_min_duration_micros: i64,
}

impl Default for ApmSinkConfig {
    fn default() -> Self {
        Self {
            max_live_traces: 20_000,
            flush_interval_secs: 1,
            dirty_threshold: 500,
            endpoint_retention_days: 30,
            endpoint_cache_ttl_secs: 60,
            reload_window_secs: 300,
            edge_pending_capacity: 160_000,
            detail_min_duration_micros: 0,
        }
    }
}

/// 一个 trace 自上次 flush 以来的增量。
#[derive(Debug, Default, Clone)]
struct TraceDelta {
    /// 增量中最早的 start（未设置时为 `i64::MAX`）。
    start_ts: i64,
    /// 增量中最晚的 end。
    max_end_ts: i64,
    /// 增量 span 数（按去重后的 span 计）。
    spans: u32,
    errors: u32,
    services: BTreeSet<String>,
    /// 增量中 `start_ts` 最小的根 span（`(start_ts, service, name)`）。
    root: Option<(i64, String, String)>,
    collector: String,
    agent_id: String,
    host_id: String,
    data_id: String,
}

impl TraceDelta {
    fn new(span: &TraceSpan, envelope: &DataEnvelope) -> Self {
        let mut delta = Self {
            start_ts: i64::MAX,
            ..Self::default()
        };
        delta.collector = span.collector.clone();
        delta.agent_id = envelope.agent_id.clone();
        delta.host_id = envelope.host_id.clone();
        delta.data_id = envelope.data_id.clone();
        delta
    }

    fn is_empty(&self) -> bool {
        self.spans == 0
    }
}

/// trace 摘要累加器。内部状态用 `std::sync::Mutex` 保护，观测路径不 await。
#[derive(Debug)]
pub struct TraceSummaryAccumulator {
    config: ApmSinkConfig,
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    deltas: BTreeMap<String, TraceDelta>,
    /// 上次成功 flush 的时间，用于 `flush_due`。
    last_flush: Option<Instant>,
    flushed_traces: u64,
    dropped_traces: u64,
}

impl TraceSummaryAccumulator {
    #[must_use]
    pub fn new(config: ApmSinkConfig) -> Self {
        Self {
            config,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// 冷启动回载：把 `reload_window_secs` 内仍在写入的 trace 重新登记（增量为零）。
    pub async fn reload(
        &self,
        sql: &dyn RelationalStore,
        now_ts: i64,
    ) -> Result<usize, DataplaneError> {
        let since = now_ts.saturating_sub(self.config.reload_window_secs.saturating_mul(1_000_000));
        let result = sql
            .execute(
                &format!(
                    "SELECT trace_id FROM {} WHERE max_end_ts > ?1",
                    tables::TRACE_SUMMARY
                ),
                &[SqlValue::Integer(since)],
            )
            .await?;
        let mut inner = self.inner.lock().map_err(|_| lock_error())?;
        let mut loaded = 0usize;
        for row in result.rows {
            let Some(SqlValue::Text(trace_id)) = row.first() else {
                continue;
            };
            inner.deltas.entry(trace_id.clone()).or_default();
            loaded += 1;
        }
        Ok(loaded)
    }

    /// 记录一条 span（内存操作，不接触存储）。
    pub fn observe(&self, span: &TraceSpan, envelope: &DataEnvelope) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let trace_id = span.trace_id.clone();
        let delta = inner
            .deltas
            .entry(trace_id)
            .or_insert_with(|| TraceDelta::new(span, envelope));
        delta.start_ts = delta.start_ts.min(span.timestamp);
        delta.max_end_ts = delta
            .max_end_ts
            .max(span.timestamp + span.duration_micros());
        delta.spans = delta.spans.saturating_add(1);
        if span.status_code == "error" {
            delta.errors = delta.errors.saturating_add(1);
        }
        delta.services.insert(span.service.clone());
        if !span.collector.is_empty() {
            delta.collector = span.collector.clone();
        }
        if span.is_root() {
            let candidate = (span.timestamp, span.service.clone(), span.name.clone());
            delta.root = match &delta.root {
                Some(current) if current.0 <= candidate.0 => Some(current.clone()),
                _ => Some(candidate),
            };
        }
    }

    /// 待写入的 trace 数。
    #[must_use]
    pub fn dirty_len(&self) -> usize {
        self.inner
            .lock()
            .map(|inner| inner.deltas.values().filter(|d| !d.is_empty()).count())
            .unwrap_or(0)
    }

    /// 内存中登记的 trace 数。
    #[must_use]
    pub fn live_len(&self) -> usize {
        self.inner
            .lock()
            .map(|inner| inner.deltas.len())
            .unwrap_or(0)
    }

    /// 到点或超过阈值时 flush；返回本次写入的 trace 数。
    pub async fn flush_due(
        &self,
        sql: &dyn RelationalStore,
        now_ts: i64,
    ) -> Result<usize, DataplaneError> {
        let due = {
            let inner = self.inner.lock().map_err(|_| lock_error())?;
            let dirty = inner.deltas.values().filter(|d| !d.is_empty()).count();
            if dirty == 0 {
                false
            } else {
                dirty >= self.config.dirty_threshold
                    || inner
                        .last_flush
                        .map(|t| {
                            t.elapsed() >= Duration::from_secs(self.config.flush_interval_secs)
                        })
                        .unwrap_or(true)
            }
        };
        if !due {
            return Ok(0);
        }
        self.flush(sql, now_ts).await
    }

    /// 立即 flush 全部非空增量，返回写入的 trace 数。
    pub async fn flush(
        &self,
        sql: &dyn RelationalStore,
        now_ts: i64,
    ) -> Result<usize, DataplaneError> {
        // 先取快照并重置增量：即使写库失败，增量也已丢（崩溃取舍一致），
        // 避免失败时无限重试同一条把 span_count 累加多次。
        let batch = {
            let mut inner = self.inner.lock().map_err(|_| lock_error())?;
            let mut batch = Vec::new();
            for (trace_id, delta) in inner.deltas.iter_mut() {
                if delta.is_empty() {
                    continue;
                }
                batch.push((trace_id.clone(), delta.clone()));
                *delta = TraceDelta::default();
            }
            inner.last_flush = Some(Instant::now());
            batch
        };

        let mut written = 0usize;
        for (trace_id, delta) in batch {
            if let Err(e) = flush_one(sql, &trace_id, &delta, now_ts).await {
                // 已重置增量，失败即丢这一轮；记错但不让整批失败。
                eprintln!(
                    "apm: trace summary flush failed for {trace_id}: {}",
                    e.message
                );
                continue;
            }
            written += 1;
        }

        self.evict_if_needed();
        let mut inner = self.inner.lock().map_err(|_| lock_error())?;
        inner.flushed_traces += written as u64;
        Ok(written)
    }

    /// 超过容量上限时丢掉最老的干净条目（已持久化，重新到达会从库里的绝对值继续）。
    fn evict_if_needed(&self) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.deltas.len() <= self.config.max_live_traces {
            return;
        }
        let mut clean: Vec<(String, i64)> = inner
            .deltas
            .iter()
            .filter(|(_, d)| d.is_empty())
            .map(|(k, d)| (k.clone(), d.max_end_ts))
            .collect();
        clean.sort_by_key(|(_, end)| *end);
        let overflow = inner.deltas.len() - self.config.max_live_traces;
        for (trace_id, _) in clean.into_iter().take(overflow) {
            inner.deltas.remove(&trace_id);
            inner.dropped_traces += 1;
        }
    }

    /// 已成功 flush 的 trace 数（自监控用）。
    #[must_use]
    pub fn flushed_traces(&self) -> u64 {
        self.inner.lock().map(|i| i.flushed_traces).unwrap_or(0)
    }

    /// 因容量上限被丢弃的条目数（自监控用）。
    #[must_use]
    pub fn dropped_traces(&self) -> u64 {
        self.inner.lock().map(|i| i.dropped_traces).unwrap_or(0)
    }
}

/// 读回已有行 → 合并 → 写回绝对值。
async fn flush_one(
    sql: &dyn RelationalStore,
    trace_id: &str,
    delta: &TraceDelta,
    now_ts: i64,
) -> Result<(), DataplaneError> {
    let existing = sql
        .execute(
            &format!(
                "SELECT start_ts, max_end_ts, root_service, root_operation, root_start_ts,
                        span_count, error_count, status, services_json, collector,
                        agent_id, host_id, data_id
                 FROM {} WHERE trace_id = ?1",
                tables::TRACE_SUMMARY
            ),
            &[SqlValue::Text(trace_id.to_string())],
        )
        .await?;

    let mut row = existing.rows.first().cloned().unwrap_or_default();
    while row.len() < 13 {
        row.push(SqlValue::Null);
    }
    let db_start = as_i64(&row[0]);
    let db_end = as_i64(&row[1]);
    let db_root_start = as_i64(&row[4]);
    let db_spans = as_i64(&row[5]).max(0) as u32;
    let db_errors = as_i64(&row[6]).max(0) as u32;
    let db_services: BTreeSet<String> = as_text(&row[8])
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
        .into_iter()
        .collect();

    let delta_start = if delta.start_ts == i64::MAX {
        db_start
    } else {
        delta.start_ts
    };
    let start_ts = if db_spans == 0 && db_start == 0 {
        delta_start
    } else {
        db_start.min(delta_start)
    };
    let max_end_ts = db_end.max(delta.max_end_ts);
    let span_count = db_spans.saturating_add(delta.spans);
    let error_count = db_errors.saturating_add(delta.errors);
    let mut services = db_services;
    services.extend(delta.services.iter().cloned());
    let services_json =
        serde_json::to_string(&services.iter().collect::<Vec<_>>()).map_err(|e| {
            DataplaneError::new(ErrorCode::QueryFailed, format!("serialize services: {e}"))
        })?;

    // 根 span 取 start_ts 最小者；库里已有的根与本次增量里的根比较。
    let mut root_service = as_text(&row[2]).unwrap_or_default();
    let mut root_operation = as_text(&row[3]).unwrap_or_default();
    let mut root_start_ts = db_root_start;
    if let Some((cand_start, cand_service, cand_name)) = &delta.root {
        if root_start_ts == 0 || *cand_start < root_start_ts {
            root_start_ts = *cand_start;
            root_service = cand_service.clone();
            root_operation = cand_name.clone();
        }
    }

    let status = if error_count > 0 { "error" } else { "ok" };
    let collector = pick(&delta.collector, as_text(&row[9]));
    let agent_id = pick(&delta.agent_id, as_text(&row[10]));
    let host_id = pick(&delta.host_id, as_text(&row[11]));
    let data_id = pick(&delta.data_id, as_text(&row[12]));

    sql.execute(
        &format!(
            "INSERT OR REPLACE INTO {} (trace_id, start_ts, max_end_ts, duration_micros,
                root_service, root_operation, root_start_ts, span_count, error_count, status,
                services_json, collector, agent_id, host_id, data_id, updated_ts)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            tables::TRACE_SUMMARY
        ),
        &[
            SqlValue::Text(trace_id.to_string()),
            SqlValue::Integer(start_ts),
            SqlValue::Integer(max_end_ts),
            SqlValue::Integer(max_end_ts.saturating_sub(start_ts).max(0)),
            SqlValue::Text(root_service),
            SqlValue::Text(root_operation),
            SqlValue::Integer(root_start_ts),
            SqlValue::Integer(i64::from(span_count)),
            SqlValue::Integer(i64::from(error_count)),
            SqlValue::Text(status.to_string()),
            SqlValue::Text(services_json),
            SqlValue::Text(collector),
            SqlValue::Text(agent_id),
            SqlValue::Text(host_id),
            SqlValue::Text(data_id),
            SqlValue::Integer(now_ts),
        ],
    )
    .await?;
    Ok(())
}

fn pick(incoming: &str, existing: Option<String>) -> String {
    if !incoming.is_empty() {
        incoming.to_string()
    } else {
        existing.unwrap_or_default()
    }
}

fn as_i64(value: &SqlValue) -> i64 {
    match value {
        SqlValue::Integer(i) => *i,
        SqlValue::Real(f) => *f as i64,
        _ => 0,
    }
}

fn as_text(value: &SqlValue) -> Option<String> {
    match value {
        SqlValue::Text(s) => Some(s.clone()),
        _ => None,
    }
}

fn lock_error() -> DataplaneError {
    DataplaneError::new(ErrorCode::QueryFailed, "apm accumulator lock poisoned")
}
