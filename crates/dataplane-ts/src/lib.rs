//! 时序存储接口、tsink 引擎与 Prom 投影查询。
//!
//! 对应设计文档 `TimeSeriesStore`（Requirement 8）。写入使用 Influx 形
//! （measurement/tags/fields），查询使用 Prom 形（metric/labels/value）。
//! 所有时间戳为 Unix 微秒（i64）。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use dataplane_core::{DataplaneError, ErrorCode};
use tsink::promql::{Engine, PromqlError, PromqlValue};
use tsink::{DataPoint, Label, Row, StorageBuilder, TimestampPrecision, Value};

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
}

impl TsinkTimeSeriesStore {
    /// 在数据路径下初始化 tsink 存储，时间精度为 Unix 微秒。
    pub fn new(data_path: impl AsRef<Path>) -> Result<Self, DataplaneError> {
        let storage = StorageBuilder::new()
            .with_data_path(data_path)
            .with_timestamp_precision(TimestampPrecision::Microseconds)
            .build()
            .map_err(|e| DataplaneError::new(ErrorCode::QueryFailed, format!("init tsink: {e}")))?;
        let engine = Engine::with_precision(storage.clone(), TimestampPrecision::Microseconds);
        Ok(Self {
            storage,
            engine: Arc::new(engine),
        })
    }
}

#[async_trait]
impl TimeSeriesStore for TsinkTimeSeriesStore {
    async fn write(&self, point: TsPoint) -> Result<(), DataplaneError> {
        let storage = self.storage.clone();
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
            storage.insert_rows(&[row]).map_err(|e| {
                DataplaneError::new(ErrorCode::QueryFailed, format!("tsink write: {e}"))
            })?;
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
}
