//! APM 保留策略：按天过期 + 全局容量上限（超限淘汰最久远的数据）。
//!
//! 设计（`/.monkeycode/specs/apm-tracing/requirements.md` Requirement 11 与
//! `/.monkeycode/specs/observability-data-model/design.md`）：
//!
//! - **过期**：trace 摘要与边摘要按 `default_retention_days`；span 明细按 `data_type`
//!   索引（`dataplane-log` v3）按同一天数删除；端点半按 `endpoint_retention_days`；
//!   聚合点（`apm_*` measurement）按同一窗口打墓碑。
//! - **全局容量上限**：`max_bytes`（0 表示不限）对 **`data_path` 下全部数据**计量
//!   （dataserver 只有一个数据目录，磁盘告急时告急的是整个目录）。超限时按「最久远
//!   优先」淘汰 **APM 数据**：每轮把淘汰时间界往前推 `evict_step_secs`，删明细/摘要/
//!   边摘要/聚合点，然后重新计量，直到回到预算内或没有可删的 APM 数据。
//! - 只删 APM 数据：日志、eBPF、自监控指标不在本策略的删除范围内。若 APM 已无可删而
//!   仍超限，报告 `stopped_reason` 并交由运维处理，绝不越权删别的类型。
//! - **淘汰方向**：删除条件是 `ts < cutoff`，因此超限时是把 cutoff 从保留界**抬高**
//!   向 `now` 推进（最久远的先删）。步长取「保存窗口 / 最大轮数」与 `evict_step_secs`
//!   的较大者，保证 `max_rounds` 轮内覆盖整个窗口。
//! - 删完调用 `LogStore::reclaim_space`（tantivy 合并/清理不再引用的段文件）；时序库的
//!   墓碑空间由 tsink 的 compaction 回收，接口不承诺立即释放。

use std::path::Path;

use dataplane_core::{DataplaneError, SqlValue};
use dataplane_log::{LogFilter, LogStore};
use dataplane_sql::RelationalStore;
use dataplane_ts::{TimeSeriesStore, TsSeriesSelection};

use crate::tables;

/// 一天的微秒数。
pub const MICROS_PER_DAY: i64 = 86_400 * 1_000_000;

/// 聚合指标里属于 APM 的 measurement（按这些名字打墓碑）。
pub const APM_MEASUREMENTS: &[&str] = &[
    "apm_service_requests_total",
    "apm_service_errors_total",
    "apm_service_duration_micros",
    "apm_edge_requests_total",
    "apm_edge_errors_total",
    "apm_edge_duration_micros",
];

/// 保留策略配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApmRetentionConfig {
    /// 明细/摘要/边摘要的默认保留天数。
    pub default_retention_days: u32,
    /// 端点半保留天数。
    pub endpoint_retention_days: u32,
    /// `data_path` 全局容量上限（字节）；0 表示不限。
    pub max_bytes: u64,
    /// 超限淘汰时每轮推进的时间步长（秒）。
    pub evict_step_secs: i64,
    /// 超限淘汰的最大轮数。
    pub max_rounds: usize,
}

impl Default for ApmRetentionConfig {
    fn default() -> Self {
        Self {
            default_retention_days: 3,
            endpoint_retention_days: 30,
            max_bytes: 0,
            evict_step_secs: 3_600,
            max_rounds: 24,
        }
    }
}

/// 一次保留策略执行的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionReport {
    pub details_deleted: u64,
    pub summaries_deleted: u64,
    pub edges_deleted: u64,
    pub endpoints_deleted: u64,
    pub ts_tombstoned: u64,
    pub reclaimed_files: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
    /// 超限淘汰的轮数。
    pub evict_rounds: usize,
    /// 停止原因：`no_apm_data_left`（APM 已无可删）、`max_rounds_reached`、`None`（达标或未配置上限）。
    pub stopped_reason: Option<String>,
}

/// APM 保留策略执行器。
#[derive(Debug, Clone)]
pub struct ApmRetention {
    config: ApmRetentionConfig,
}

impl ApmRetention {
    #[must_use]
    pub fn new(config: ApmRetentionConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn config(&self) -> &ApmRetentionConfig {
        &self.config
    }

    /// 执行一轮：先按天过期，再按容量上限淘汰最久远的数据。
    pub async fn run(
        &self,
        sql: &dyn RelationalStore,
        log: &dyn LogStore,
        ts: &dyn TimeSeriesStore,
        data_path: &Path,
        now_ts: i64,
    ) -> Result<RetentionReport, DataplaneError> {
        let mut report = RetentionReport {
            bytes_before: dir_size(data_path),
            ..RetentionReport::default()
        };

        // 1) 按天过期。
        let cutoff = now_ts
            .saturating_sub(i64::from(self.config.default_retention_days.max(1)) * MICROS_PER_DAY);
        self.evict_older_than(sql, log, ts, cutoff, &mut report)
            .await?;
        let endpoint_cutoff = now_ts
            .saturating_sub(i64::from(self.config.endpoint_retention_days.max(1)) * MICROS_PER_DAY);
        report.endpoints_deleted += delete_endpoints(sql, endpoint_cutoff).await?;
        report.reclaimed_files += log.reclaim_space().await?;

        // 2) 全局容量上限：超限则按「最久远优先」继续淘汰 APM 数据。
        //
        //    注意方向：删除条件是 `ts < cutoff`，所以**抬高** cutoff 才会删得更多。
        //    步长取「保存窗口 / 最大轮数」与 `evict_step_secs` 的较大者，保证在
        //    `max_rounds` 轮内能覆盖整个窗口；否则近期数据永远落在 cutoff 之后。
        if self.config.max_bytes > 0 {
            let mut current = dir_size(data_path);
            let mut rounds = 0usize;
            let mut cursor = cutoff;
            let mut reason = None;
            let span = (now_ts - cutoff).max(1);
            let step = (span / self.config.max_rounds.max(1) as i64)
                .max(self.config.evict_step_secs.max(1) * 1_000_000)
                .max(1);
            while current > self.config.max_bytes {
                // 先判「已到窗口尽头」：此时 APM 数据确实都删完了，不该报轮数用尽。
                if cursor >= now_ts {
                    reason = Some("no_apm_data_left".to_string());
                    break;
                }
                if rounds >= self.config.max_rounds {
                    reason = Some("max_rounds_reached".to_string());
                    break;
                }
                cursor = cursor.saturating_add(step).min(now_ts);
                let mut round = RetentionReport::default();
                self.evict_older_than(sql, log, ts, cursor, &mut round)
                    .await?;
                rounds += 1;
                report.details_deleted += round.details_deleted;
                report.summaries_deleted += round.summaries_deleted;
                report.edges_deleted += round.edges_deleted;
                report.ts_tombstoned += round.ts_tombstoned;
                report.reclaimed_files += log.reclaim_space().await?;
                current = dir_size(data_path);
            }
            report.evict_rounds = rounds;
            report.stopped_reason = reason;
        }

        report.bytes_after = dir_size(data_path);
        Ok(report)
    }

    /// 删除 `cutoff` 之前的 APM 数据（明细、摘要、边摘要、聚合点）。
    async fn evict_older_than(
        &self,
        sql: &dyn RelationalStore,
        log: &dyn LogStore,
        ts: &dyn TimeSeriesStore,
        cutoff: i64,
        report: &mut RetentionReport,
    ) -> Result<(), DataplaneError> {
        // span 明细：走 `data_type` 索引，不受 post-filter 扫描上限约束。
        let mut labels = std::collections::BTreeMap::new();
        labels.insert("data_type".to_string(), "traces".to_string());
        report.details_deleted += log
            .delete_matching(LogFilter {
                to_ts: Some(cutoff),
                labels,
                ..LogFilter::default()
            })
            .await?;

        report.summaries_deleted +=
            delete_rows_older_than(sql, tables::TRACE_SUMMARY, "start_ts", cutoff).await?;
        report.edges_deleted +=
            delete_rows_older_than(sql, tables::EDGE_SUMMARY, "bucket_start", cutoff).await?;

        for measurement in APM_MEASUREMENTS {
            let deletion = ts
                .delete_series(TsSeriesSelection {
                    measurement: Some((*measurement).to_string()),
                    matchers: Vec::new(),
                    from_ts: 0,
                    to_ts: cutoff.max(1),
                })
                .await?;
            report.ts_tombstoned += deletion.tombstones_applied;
        }
        Ok(())
    }
}

/// `DELETE ... WHERE <time_column> < cutoff`，返回删除行数（用 `changes()` 取）。
async fn delete_rows_older_than(
    sql: &dyn RelationalStore,
    table: &str,
    time_column: &str,
    cutoff: i64,
) -> Result<u64, DataplaneError> {
    sql.execute(
        &format!("DELETE FROM {table} WHERE {time_column} < ?1"),
        &[SqlValue::Integer(cutoff)],
    )
    .await?;
    affected_rows(sql).await
}

async fn delete_endpoints(sql: &dyn RelationalStore, cutoff: i64) -> Result<u64, DataplaneError> {
    sql.execute(
        &format!(
            "DELETE FROM {} WHERE last_seen_ts < ?1",
            tables::SERVICE_ENDPOINT
        ),
        &[SqlValue::Integer(cutoff)],
    )
    .await?;
    affected_rows(sql).await
}

/// `DELETE` 不返回行；同一连接上的 `SELECT changes()` 给出受影响行数。
async fn affected_rows(sql: &dyn RelationalStore) -> Result<u64, DataplaneError> {
    let result = sql.execute("SELECT changes()", &[]).await?;
    Ok(result
        .rows
        .first()
        .and_then(|row| row.first())
        .map_or(0, |value| match value {
            SqlValue::Integer(i) => (*i).max(0) as u64,
            _ => 0,
        }))
}

/// 递归统计目录占用字节数；不可读的条目按 0 计（计量失败不该阻断清理）。
#[must_use]
pub fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        total = total.saturating_add(dir_size(&entry.path()));
    }
    total
}
