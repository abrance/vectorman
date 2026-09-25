//! eBPF 数据的保留期清理（`ebpf_edges` 边聚合 + `data_type=ebpf` 原始事件）。
//!
//! 对应 `/.monkeycode/specs/ebpf-observability/requirements.md` Requirement 14.1-14.3：
//!
//! - 保留期取采集项 `storage_json.retention_days`，**eBPF 采集项缺省 3 天**（日志类缺省 1 天）；
//! - `ebpf_edges` 按 `bucket_start` 删除：分批删（每批 [`BATCH_SIZE`] 行）直到删空，
//!   避免长事务卡住 sqlite；
//! - 采集项被删除后由 `retain/{item_id}` 到期触发全量删除（与日志同一套机制）。
//!
//! 聚合指标（`ebpf_*` / `apm_edge_*{source=ebpf}`）不在这里删：它们走全局时序保留
//! （`ts_retention_days`，缺省 30 天），由 `dataplane-ts-retention` 的清理任务负责。

use dataplane_core::{DataplaneError, ErrorCode, SqlValue};
use dataplane_sql::RelationalStore;

use crate::tables;

/// 单批删除行数。
///
/// 用 `rowid IN (SELECT ... LIMIT n)` 而不是 `DELETE ... LIMIT n`：后者需要 sqlite 编译期
/// 打开 `SQLITE_ENABLE_UPDATE_DELETE_LIMIT`，不是所有构建都带。
pub const BATCH_SIZE: i64 = 1_000;

/// 一批删除的结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EdgeDeleteReport {
    /// 删除的行数。
    pub deleted: u64,
    /// 执行的批次数（自监控与排障用：批次数异常高说明保留期突变或表被写爆）。
    pub batches: u32,
}

/// 删除某个采集项（`data_id`）在 `cutoff` 之前的边记录。
///
/// `data_id` 为空表示**所有**采集项（用于全局兜底清理）。
pub async fn delete_edges_before(
    sql: &dyn RelationalStore,
    data_id: &str,
    cutoff_micros: i64,
) -> Result<EdgeDeleteReport, DataplaneError> {
    delete_edges_before_batched(sql, data_id, cutoff_micros, BATCH_SIZE).await
}

/// 带批大小参数的版本（测试里用小批大小验证「分批直到删空」）。
pub async fn delete_edges_before_batched(
    sql: &dyn RelationalStore,
    data_id: &str,
    cutoff_micros: i64,
    batch_size: i64,
) -> Result<EdgeDeleteReport, DataplaneError> {
    let batch_size = batch_size.max(1);
    let mut report = EdgeDeleteReport::default();
    loop {
        let (statement, params) = if data_id.is_empty() {
            (
                format!(
                    "DELETE FROM {} WHERE rowid IN (
                        SELECT rowid FROM {} WHERE bucket_start < ?1 LIMIT ?2)",
                    tables::EBPF_EDGES,
                    tables::EBPF_EDGES
                ),
                vec![
                    SqlValue::Integer(cutoff_micros),
                    SqlValue::Integer(batch_size),
                ],
            )
        } else {
            (
                format!(
                    "DELETE FROM {} WHERE rowid IN (
                        SELECT rowid FROM {} WHERE data_id = ?1 AND bucket_start < ?2 LIMIT ?3)",
                    tables::EBPF_EDGES,
                    tables::EBPF_EDGES
                ),
                vec![
                    SqlValue::Text(data_id.to_string()),
                    SqlValue::Integer(cutoff_micros),
                    SqlValue::Integer(batch_size),
                ],
            )
        };
        sql.execute(&statement, &params).await?;
        // `DELETE` 不返回行数：同一连接上 `SELECT changes()` 取受影响行数。
        let affected = changes(sql).await?;
        report.deleted += affected;
        report.batches += 1;
        if affected < batch_size as u64 {
            return Ok(report);
        }
    }
}

/// 读 `SELECT changes()`（见 `dataplane-apm` 里同一做法的说明）。
async fn changes(sql: &dyn RelationalStore) -> Result<u64, DataplaneError> {
    let result = sql.execute("SELECT changes()", &[]).await?;
    Ok(result
        .rows
        .first()
        .and_then(|row| row.first())
        .map_or(0, |value| match value {
            SqlValue::Integer(i) => *i as u64,
            _ => 0,
        }))
}

/// 校验保留天数：eBPF 缺省 3 天，下限 1 天。
#[must_use]
pub fn clamp_retention_days(days: u32) -> u32 {
    days.max(1)
}

/// eBPF 采集项的保留天数缺省值（需求 14.1）。
pub const DEFAULT_RETENTION_DAYS: u32 = 3;

/// 从采集项 JSON 读 eBPF 保留天数。
///
/// 与日志侧的区别只在缺省值：eBPF 的边聚合体积远小于日志明细，且排障窗口更长。
#[must_use]
pub fn retention_days(value: &serde_json::Value) -> u32 {
    value
        .pointer("/storage/retention_days")
        .and_then(|v| v.as_u64())
        .or_else(|| value.get("retention_days").and_then(|v| v.as_u64()))
        .map_or(DEFAULT_RETENTION_DAYS, |days| {
            clamp_retention_days(days as u32)
        })
}

/// 计算截止时间（微秒）。
#[must_use]
pub fn cutoff_micros(now_micros: i64, days: u32) -> i64 {
    now_micros.saturating_sub(i64::from(clamp_retention_days(days)) * 86_400 * 1_000_000)
}

/// 解析错误辅助（保持错误码一致）。
#[must_use]
pub fn delete_error(message: String) -> DataplaneError {
    DataplaneError::new(ErrorCode::QueryFailed, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_days_defaults_to_three_for_ebpf() {
        assert_eq!(
            retention_days(&serde_json::json!({})),
            DEFAULT_RETENTION_DAYS
        );
        assert_eq!(retention_days(&serde_json::json!({"retention_days": 7})), 7);
        assert_eq!(
            retention_days(&serde_json::json!({"storage": {"retention_days": 5}})),
            5,
            "storage 优先于顶层（与采集项下发结构一致）"
        );
        assert_eq!(
            retention_days(&serde_json::json!({"retention_days": 0})),
            1,
            "0 归一为 1 天"
        );
    }

    #[test]
    fn cutoff_is_days_before_now() {
        let now = 1_710_000_000_000_000i64;
        assert_eq!(cutoff_micros(now, 3), now - 3 * 86_400_000_000);
        assert_eq!(cutoff_micros(now, 0), now - 86_400_000_000);
        // 时间戳极小也不能下溢成负数。
        assert!(cutoff_micros(10, 30) <= 10);
    }
}
