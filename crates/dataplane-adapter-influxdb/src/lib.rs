//! InfluxDB 远程适配器（v1 占位）。后续实现 TimeSeriesStore。

use async_trait::async_trait;
use dataplane_core::{DataplaneError, ErrorCode};
use dataplane_ts::{PromResult, TimeSeriesStore, TsPoint};

fn unimplemented_store() -> DataplaneError {
    DataplaneError::new(
        ErrorCode::Unimplemented,
        "dataplane-adapter-influxdb: not implemented in v1",
    )
}

/// InfluxDB 远程时序存储（v1 占位）。
#[derive(Debug, Clone, Default)]
pub struct InfluxDbStore;

#[async_trait]
impl TimeSeriesStore for InfluxDbStore {
    async fn write(&self, _point: TsPoint) -> Result<(), DataplaneError> {
        Err(unimplemented_store())
    }

    async fn query_instant(
        &self,
        _expr: &str,
        _eval_time: Option<i64>,
    ) -> Result<PromResult, DataplaneError> {
        Err(unimplemented_store())
    }

    async fn query_range(
        &self,
        _expr: &str,
        _start: i64,
        _end: i64,
        _step: i64,
    ) -> Result<PromResult, DataplaneError> {
        Err(unimplemented_store())
    }
}
