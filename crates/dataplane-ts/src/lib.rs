//! 时序存储接口、tsink 引擎与 Prom 投影查询。
//!
//! 对应设计文档 `TimeSeriesStore`（Requirement 8）与
//! `dataplane-ts-retention`（保留与删除）。写入使用 Influx 形
//! （measurement/tags/fields），查询使用 Prom 形（metric/labels/value）。
//! 所有时间戳为 Unix 微秒（i64）。
//!
//! 保留注意：tsink 的 `retention` 缺省存在（14 天）但 `retention_enforced`
//! 缺省为 `false`，即窗口不执行。本模块通过 [`TsRetentionConfig`] 显式启用，
//! 并保证「先 `with_retention` 再 `with_retention_enforced`」的调用顺序。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use dataplane_core::{DataplaneError, ErrorCode};
use tsink::promql::{Engine, PromqlError, PromqlValue};
use tsink::{
    DataPoint, Label, Row, SeriesMatcher, SeriesMatcherOp, SeriesSelection, StorageBuilder,
    TimestampPrecision, TsinkError, Value,
};

const MICROS_PER_DAY: i64 = 86_400 * 1_000_000;

/// 时序保留与资源上限配置。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TsRetentionConfig {
    /// 全局保留窗口（天）。0 表示不设窗口，需同时 `enforced = false`。
    pub retention_days: u32,
    /// 是否真正拒绝/过滤超出窗口的点。
    pub enforced: bool,
    /// 序列数上限，0 表示不限。
    pub cardinality_limit: usize,
    /// 内存预算（字节），0 表示不限。
    pub memory_limit_bytes: usize,
    /// WAL 字节上限，0 表示不限。
    pub wal_size_limit_bytes: usize,
}

impl Default for TsRetentionConfig {
    fn default() -> Self {
        Self {
            retention_days: 30,
            enforced: true,
            cardinality_limit: 0,
            memory_limit_bytes: 0,
            wal_size_limit_bytes: 0,
        }
    }
}

/// label matcher 的比较方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TsMatcherOp {
    Equal,
    NotEqual,
    RegexMatch,
    RegexNoMatch,
}

impl TsMatcherOp {
    fn to_tsink(self) -> SeriesMatcherOp {
        match self {
            Self::Equal => SeriesMatcherOp::Equal,
            Self::NotEqual => SeriesMatcherOp::NotEqual,
            Self::RegexMatch => SeriesMatcherOp::RegexMatch,
            Self::RegexNoMatch => SeriesMatcherOp::RegexNoMatch,
        }
    }
}

/// 删除与统计用的 label matcher。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TsMatcher {
    pub name: String,
    pub op: TsMatcherOp,
    pub value: String,
}

/// 序列选择：measurement（可选）+ label matchers + 时间范围（必填双端）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TsSeriesSelection {
    /// 指标名；`None` 表示不限。
    pub measurement: Option<String>,
    /// label matchers；空表示不限。
    pub matchers: Vec<TsMatcher>,
    /// 时间范围起点（含），Unix 微秒。
    pub from_ts: i64,
    /// 时间范围终点（不含），Unix 微秒，必须大于 `from_ts`。
    pub to_ts: i64,
}

/// 删除结果。`tombstones_applied` 只统计墓碑状态发生变化的序列。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TsDeleteReport {
    pub matched_series: u64,
    pub tombstones_applied: u64,
}

/// 存储运行状态摘要，供自监控与运维接口读取。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TsStorageStats {
    pub series_count: u64,
    pub memory_used_bytes: usize,
    pub memory_budget_bytes: usize,
    pub wal_size_bytes: u64,
    pub retention_days: u32,
    pub retention_enforced: bool,
    pub expired_segments_total: u64,
    pub future_skew_points_total: u64,
    pub background_errors_total: u64,
    pub degraded: bool,
    pub last_background_error: Option<String>,
    /// 本次采样时刻（Unix 微秒）；`series_count` 等字段的新鲜度以此为准。
    pub sampled_at_ts: i64,
}

/// 当前时间，Unix 微秒。
fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// tsink 错误到数据面错误的映射。
fn map_tsink_error(e: TsinkError, retention_days: u32) -> DataplaneError {
    match e {
        TsinkError::InvalidTimeRange { start, end } => DataplaneError::new(
            ErrorCode::InvalidArgument,
            format!("invalid time range: start={start} end={end}"),
        ),
        TsinkError::OutOfRetention { timestamp } => {
            let cutoff = now_micros().saturating_sub(i64::from(retention_days) * MICROS_PER_DAY);
            DataplaneError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "point out of retention: timestamp={timestamp} cutoff={cutoff} retention_days={retention_days}"
                ),
            )
        }
        TsinkError::CardinalityLimitExceeded { .. } => DataplaneError::new(
            ErrorCode::QueryFailed,
            format!("cardinality limit exceeded: {e}"),
        ),
        // `delete_series` 的后端默认实现走这里：必须报错，不能假成功。
        TsinkError::InvalidConfiguration(msg) if msg.contains("not implemented") => {
            DataplaneError::new(ErrorCode::QueryFailed, msg)
        }
        TsinkError::UnsupportedOperation { operation, reason } => DataplaneError::new(
            ErrorCode::QueryFailed,
            format!("unsupported operation {operation}: {reason}"),
        ),
        other => DataplaneError::new(ErrorCode::QueryFailed, format!("tsink: {other}")),
    }
}

/// 单个时序数据点。v1 一个 point 恰好一个数值 field（以类型体现）。
#[derive(Debug, Clone, PartialEq)]
pub struct TsPoint {
    /// 写入模型中的 measurement，查询时作为指标名。
    pub measurement: String,
    /// 写入模型中的 tags，查询时作为 labels。
    pub tags: BTreeMap<String, String>,
    /// 该 point 唯一的数值 field 名称。
    pub field_name: String,
    /// 该 point 唯一的数值 field 值。
    pub field_value: f64,
    /// 采样时间，Unix 微秒。
    pub timestamp: i64,
}

/// Prom 查询结果类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromResultType {
    #[default]
    Vector,
    Matrix,
}

/// Prom 查询结果中的一条序列。
#[derive(Debug, Clone, PartialEq)]
pub struct PromSeries {
    /// 查询结果标签集（含指标名 trait 的映射结果）。
    pub metric: BTreeMap<String, String>,
    /// instant 查询：`(timestamp_us, value)`。
    pub value: Option<(i64, f64)>,
    /// range 查询：按时间升序的 `(timestamp_us, value)` 序列。
    pub values: Option<Vec<(i64, f64)>>,
}

/// Prom 查询结果。`result_type` 为 `vector` 或 `matrix`。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PromResult {
    pub result_type: PromResultType,
    pub result: Vec<PromSeries>,
}

/// 时序存储抽象。
#[async_trait]
pub trait TimeSeriesStore: Send + Sync {
    /// 写入一个数据点。
    async fn write(&self, point: TsPoint) -> Result<(), DataplaneError>;

    /// instant 查询：`expr` 为 PromQL 子集表达式，`eval_time` 缺省为当前时间。
    async fn query_instant(
        &self,
        expr: &str,
        eval_time: Option<i64>,
    ) -> Result<PromResult, DataplaneError>;

    /// range 查询：`start`/`end` 为 Unix 微秒，`step` 为秒。
    async fn query_range(
        &self,
        expr: &str,
        start: i64,
        end: i64,
        step: i64,
    ) -> Result<PromResult, DataplaneError>;

    /// 按序列选择写删除墓矴，返回命中与生效的序列数。
    ///
    /// 默认实现返回 `query_failed`，使未支持删除的后端（测试桩、占位适配器）
    /// 无需改动即可编译，也避免把「未实现」当成「删成功了」。
    async fn delete_series(
        &self,
        selection: TsSeriesSelection,
    ) -> Result<TsDeleteReport, DataplaneError> {
        let _ = selection;
        Err(DataplaneError::new(
            ErrorCode::QueryFailed,
            "delete_series is not implemented",
        ))
    }

    /// 存储运行状态摘要（保留、基数、内存、WAL）。默认返回零值。
    async fn storage_stats(&self) -> Result<TsStorageStats, DataplaneError> {
        Ok(TsStorageStats::default())
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

fn map_promql_error(e: PromqlError) -> DataplaneError {
    match e {
        PromqlError::UnknownFunction(name) => DataplaneError::new(
            ErrorCode::Unimplemented,
            format!("function not supported: {name}"),
        ),
        PromqlError::Parse(msg) => DataplaneError::new(
            ErrorCode::InvalidArgument,
            format!("promql parse error: {msg}"),
        ),
        other => DataplaneError::new(ErrorCode::QueryFailed, other.to_string()),
    }
}

/// 将 tsink 的 PromQL 结果投影为数据平面的 Prom 结果。
fn promql_value_to_result(v: PromqlValue) -> Result<PromResult, DataplaneError> {
    match v {
        PromqlValue::Scalar(value, timestamp) => Ok(PromResult {
            result_type: PromResultType::Vector,
            result: vec![PromSeries {
                metric: BTreeMap::new(),
                value: Some((timestamp, value)),
                values: None,
            }],
        }),
        PromqlValue::InstantVector(samples) => Ok(PromResult {
            result_type: PromResultType::Vector,
            result: samples
                .into_iter()
                .map(|s| PromSeries {
                    metric: s.labels.into_iter().map(|l| (l.name, l.value)).collect(),
                    value: Some((s.timestamp, s.value)),
                    values: None,
                })
                .collect(),
        }),
        PromqlValue::RangeVector(series_list) => Ok(PromResult {
            result_type: PromResultType::Matrix,
            result: series_list
                .into_iter()
                .map(|s| PromSeries {
                    metric: s.labels.into_iter().map(|l| (l.name, l.value)).collect(),
                    value: None,
                    values: Some(s.samples.into_iter().collect()),
                })
                .collect(),
        }),
        PromqlValue::String(_, _) => Err(DataplaneError::new(
            ErrorCode::InvalidArgument,
            "promql string result is not supported",
        )),
    }
}

/// tsink 本地引擎。查询通过 tsink 自带的 PromQL 引擎执行。
pub struct TsinkTimeSeriesStore {
    storage: Arc<dyn tsink::Storage>,
    engine: Arc<Engine>,
    retention: TsRetentionConfig,
}

impl TsinkTimeSeriesStore {
    /// 在数据路径下初始化 tsink 存储，时间精度为 Unix 微秒。
    ///
    /// 保留与上限按 `retention` 落地；`with_retention` 内部会把
    /// `retention_enforced` 置为 true，因此必须在它之后再调用
    /// `with_retention_enforced` 落地最终值，否则 `enforced = false` 会被静默覆盖。
    pub fn new(
        data_path: impl AsRef<Path>,
        retention: TsRetentionConfig,
    ) -> Result<Self, DataplaneError> {
        if retention.retention_days == 0 && retention.enforced {
            return Err(DataplaneError::config_invalid(
                "ts_retention_days must be > 0 when retention is enforced",
            ));
        }
        let mut builder = StorageBuilder::new()
            .with_data_path(data_path)
            .with_timestamp_precision(TimestampPrecision::Microseconds);
        if retention.retention_days > 0 {
            builder = builder.with_retention(Duration::from_secs(
                u64::from(retention.retention_days) * 86_400,
            ));
        }
        builder = builder.with_retention_enforced(retention.enforced);
        if retention.cardinality_limit > 0 {
            builder = builder.with_cardinality_limit(retention.cardinality_limit);
        }
        if retention.memory_limit_bytes > 0 {
            builder = builder.with_memory_limit(retention.memory_limit_bytes);
        }
        if retention.wal_size_limit_bytes > 0 {
            builder = builder.with_wal_size_limit(retention.wal_size_limit_bytes);
        }
        let storage = builder.build().map_err(|e| {
            DataplaneError::new(ErrorCode::EngineInitFailed, format!("init tsink: {e}"))
        })?;
        let engine = Engine::with_precision(storage.clone(), TimestampPrecision::Microseconds);
        Ok(Self {
            storage,
            engine: Arc::new(engine),
            retention,
        })
    }
}

#[async_trait]
impl TimeSeriesStore for TsinkTimeSeriesStore {
    async fn write(&self, point: TsPoint) -> Result<(), DataplaneError> {
        let storage = self.storage.clone();
        let retention_days = self.retention.retention_days;
        blocking(move || {
            let labels: Vec<Label> = point
                .tags
                .into_iter()
                .map(|(name, value)| Label::new(name, value))
                .collect();
            let row = Row::with_labels(
                point.measurement,
                labels,
                DataPoint::new(point.timestamp, Value::F64(point.field_value)),
            );
            storage
                .insert_rows(&[row])
                .map_err(|e| map_tsink_error(e, retention_days))?;
            Ok(())
        })
        .await
    }

    async fn query_instant(
        &self,
        expr: &str,
        eval_time: Option<i64>,
    ) -> Result<PromResult, DataplaneError> {
        let engine = self.engine.clone();
        let expr = expr.to_string();
        blocking(move || {
            let ts = eval_time.unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_micros() as i64)
                    .unwrap_or(0)
            });
            let result = engine.instant_query(&expr, ts).map_err(map_promql_error)?;
            promql_value_to_result(result)
        })
        .await
    }

    async fn query_range(
        &self,
        expr: &str,
        start: i64,
        end: i64,
        step: i64,
    ) -> Result<PromResult, DataplaneError> {
        let engine = self.engine.clone();
        let expr = expr.to_string();
        blocking(move || {
            let step_us = step.checked_mul(1_000_000).ok_or_else(|| {
                DataplaneError::new(ErrorCode::InvalidArgument, "step overflow in microseconds")
            })?;
            let result = engine
                .range_query(&expr, start, end, step_us)
                .map_err(map_promql_error)?;
            promql_value_to_result(result)
        })
        .await
    }

    async fn delete_series(
        &self,
        selection: TsSeriesSelection,
    ) -> Result<TsDeleteReport, DataplaneError> {
        if selection.from_ts >= selection.to_ts {
            return Err(DataplaneError::invalid_argument(format!(
                "from_ts ({}) must be earlier than to_ts ({})",
                selection.from_ts, selection.to_ts
            )));
        }
        let storage = self.storage.clone();
        let retention_days = self.retention.retention_days;
        blocking(move || {
            let mut sel = SeriesSelection::new();
            if let Some(measurement) = selection.measurement {
                sel = sel.with_metric(measurement);
            }
            let matchers: Vec<SeriesMatcher> = selection
                .matchers
                .into_iter()
                .map(|m| SeriesMatcher::new(m.name, m.op.to_tsink(), m.value))
                .collect();
            let sel = sel
                .with_matchers(matchers)
                .with_time_range(selection.from_ts, selection.to_ts);
            let report = storage
                .delete_series(&sel)
                .map_err(|e| map_tsink_error(e, retention_days))?;
            Ok(TsDeleteReport {
                matched_series: report.matched_series,
                tombstones_applied: report.tombstones_applied,
            })
        })
        .await
    }

    async fn storage_stats(&self) -> Result<TsStorageStats, DataplaneError> {
        let storage = self.storage.clone();
        let retention = self.retention.clone();
        blocking(move || {
            let snapshot = storage.observability_snapshot();
            // `list_metrics` 在大基数下昂贵，仅在此处按需采样；失败不影响其余字段。
            let series_count = storage
                .list_metrics()
                .map(|series| series.len() as u64)
                .unwrap_or(0);
            Ok(TsStorageStats {
                series_count,
                memory_used_bytes: snapshot
                    .memory
                    .active_and_sealed_bytes
                    .saturating_add(snapshot.memory.registry_bytes),
                memory_budget_bytes: snapshot.memory.budgeted_bytes,
                wal_size_bytes: snapshot.wal.size_bytes,
                retention_days: retention.retention_days,
                retention_enforced: retention.enforced,
                expired_segments_total: snapshot.flush.expired_segments_total,
                future_skew_points_total: snapshot.retention.future_skew_points_total,
                background_errors_total: snapshot.health.background_errors_total,
                degraded: snapshot.health.degraded,
                last_background_error: snapshot.health.last_background_error,
                sampled_at_ts: now_micros(),
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(measurement: &str, service: &str, ts: i64, value: f64) -> TsPoint {
        let mut tags = BTreeMap::new();
        tags.insert("service".to_string(), service.to_string());
        tags.insert("item_id".to_string(), format!("item-{service}"));
        TsPoint {
            measurement: measurement.to_string(),
            tags,
            field_name: "value".to_string(),
            field_value: value,
            timestamp: ts,
        }
    }

    fn store_with(root: &std::path::Path, retention: TsRetentionConfig) -> TsinkTimeSeriesStore {
        TsinkTimeSeriesStore::new(root.join("ts"), retention).unwrap()
    }

    /// 精确时刻的 instant 取值。
    ///
    /// 不能用 range 查询断言“某个点是否存在”：tsink 的 range 结果是按 step 对齐、
    /// 用前一个样本前向填充的，删除某个点后后面的 step 仍会带值。instant 查询在
    /// 时间戳落于被删区间时不再回看到被删样本，因此能区分。
    async fn instant_value(store: &TsinkTimeSeriesStore, expr: &str, ts: i64) -> Option<f64> {
        let r = store.query_instant(expr, Some(ts)).await.unwrap();
        r.result.into_iter().find_map(|s| s.value.map(|(_, v)| v))
    }

    #[tokio::test]
    async fn enforced_retention_rejects_out_of_window_point() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(
            dir.path(),
            TsRetentionConfig {
                retention_days: 1,
                enforced: true,
                ..TsRetentionConfig::default()
            },
        );
        let now = now_micros();
        let err = store
            .write(point("m", "a", now - 2 * MICROS_PER_DAY, 1.0))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(
            err.message.contains("out of retention"),
            "unexpected message: {}",
            err.message
        );
        assert!(
            err.message.contains("retention_days=1"),
            "message should carry the window: {}",
            err.message
        );

        store.write(point("m", "a", now, 2.0)).await.unwrap();
    }

    #[tokio::test]
    async fn unenforced_retention_accepts_out_of_window_point() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(
            dir.path(),
            TsRetentionConfig {
                retention_days: 1,
                enforced: false,
                ..TsRetentionConfig::default()
            },
        );
        let now = now_micros();
        let old = now - 2 * MICROS_PER_DAY;
        store.write(point("m", "a", old, 1.0)).await.unwrap();
        assert_eq!(
            instant_value(&store, "m", old).await,
            Some(1.0),
            "关闭保留执行时旧点仍可写入与查询"
        );
    }

    #[tokio::test]
    async fn zero_window_with_enforcement_is_config_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let err = TsinkTimeSeriesStore::new(
            dir.path().join("ts"),
            TsRetentionConfig {
                retention_days: 0,
                enforced: true,
                ..TsRetentionConfig::default()
            },
        )
        .err()
        .expect("应拒绝无窗口但启用执行的配置");
        assert_eq!(err.code, ErrorCode::ConfigInvalid);
    }

    #[tokio::test]
    async fn delete_series_removes_only_selected_range() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(
            dir.path(),
            TsRetentionConfig {
                enforced: false,
                ..TsRetentionConfig::default()
            },
        );
        let now = now_micros();
        let (t0, t1, t2) = (now - 300_000_000, now - 240_000_000, now - 180_000_000);
        for (ts, v) in [(t0, 1.0), (t1, 2.0), (t2, 3.0)] {
            store.write(point("m", "order-api", ts, v)).await.unwrap();
        }
        store.write(point("m", "payment", t1, 9.0)).await.unwrap();
        assert_eq!(
            instant_value(&store, "m{service=\"order-api\"}", t0).await,
            Some(1.0)
        );

        let selection = || TsSeriesSelection {
            measurement: Some("m".to_string()),
            matchers: vec![TsMatcher {
                name: "service".to_string(),
                op: TsMatcherOp::Equal,
                value: "order-api".to_string(),
            }],
            from_ts: t0,
            to_ts: t1,
        };
        let report = store.delete_series(selection()).await.unwrap();
        assert!(report.matched_series >= 1);
        assert!(report.tombstones_applied >= 1);

        assert_eq!(
            instant_value(&store, "m{service=\"order-api\"}", t0).await,
            None,
            "范围内的点应被删除"
        );
        assert_eq!(
            instant_value(&store, "m{service=\"order-api\"}", t2).await,
            Some(3.0),
            "范围外的点应保留"
        );
        assert_eq!(
            instant_value(&store, "m{service=\"payment\"}", t1).await,
            Some(9.0),
            "未选中的序列不受影响"
        );

        // 幂等：第二次没有新的墓碑。
        let again = store.delete_series(selection()).await.unwrap();
        assert_eq!(again.tombstones_applied, 0, "重复删除不应新增墓碑");
    }

    #[tokio::test]
    async fn delete_series_rejects_invalid_time_range() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(dir.path(), TsRetentionConfig::default());
        for (from, to) in [(100, 100), (200, 100)] {
            let err = store
                .delete_series(TsSeriesSelection {
                    measurement: Some("m".to_string()),
                    matchers: Vec::new(),
                    from_ts: from,
                    to_ts: to,
                })
                .await
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument);
        }
    }

    #[tokio::test]
    async fn storage_stats_reports_config_and_series() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(
            dir.path(),
            TsRetentionConfig {
                retention_days: 7,
                enforced: true,
                cardinality_limit: 1_000,
                ..TsRetentionConfig::default()
            },
        );
        store
            .write(point("m", "a", now_micros(), 1.0))
            .await
            .unwrap();
        let stats = store.storage_stats().await.unwrap();
        assert_eq!(stats.retention_days, 7);
        assert!(stats.retention_enforced);
        assert_eq!(stats.series_count, 1, "写入一个序列后基数应为 1");
        assert!(!stats.degraded);
    }

    #[tokio::test]
    async fn cardinality_limit_rejects_new_series() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(
            dir.path(),
            TsRetentionConfig {
                enforced: false,
                cardinality_limit: 1,
                ..TsRetentionConfig::default()
            },
        );
        let now = now_micros();
        store.write(point("m", "a", now, 1.0)).await.unwrap();
        let err = store.write(point("m", "b", now, 1.0)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::QueryFailed);
        assert!(
            err.message.contains("cardinality"),
            "unexpected message: {}",
            err.message
        );
        // 已达上限时既有序列仍可写。
        store.write(point("m", "a", now + 1, 2.0)).await.unwrap();
    }

    #[tokio::test]
    async fn default_impls_report_unimplemented_and_zero_stats() {
        struct StubStore;

        #[async_trait]
        impl TimeSeriesStore for StubStore {
            async fn write(&self, _point: TsPoint) -> Result<(), DataplaneError> {
                Ok(())
            }
            async fn query_instant(
                &self,
                _expr: &str,
                _eval_time: Option<i64>,
            ) -> Result<PromResult, DataplaneError> {
                Ok(PromResult::default())
            }
            async fn query_range(
                &self,
                _expr: &str,
                _start: i64,
                _end: i64,
                _step: i64,
            ) -> Result<PromResult, DataplaneError> {
                Ok(PromResult::default())
            }
        }

        let err = StubStore
            .delete_series(TsSeriesSelection {
                measurement: None,
                matchers: Vec::new(),
                from_ts: 0,
                to_ts: 1,
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::QueryFailed);
        assert!(err.message.contains("not implemented"));
        assert_eq!(
            StubStore.storage_stats().await.unwrap(),
            TsStorageStats::default()
        );
    }
}
