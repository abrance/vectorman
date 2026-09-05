//! Doris 远程适配器（v1 占位）。后续实现 LogStore。

use async_trait::async_trait;
use dataplane_core::{DataplaneError, ErrorCode};
use dataplane_log::{LogFilter, LogRecord, LogStore};

fn unimplemented_store() -> DataplaneError {
    DataplaneError::new(
        ErrorCode::Unimplemented,
        "dataplane-adapter-doris: not implemented in v1",
    )
}

/// Doris 远程日志存储（v1 占位）。
#[derive(Debug, Clone, Default)]
pub struct DorisStore;

#[async_trait]
impl LogStore for DorisStore {
    async fn append(&self, _record: LogRecord) -> Result<(), DataplaneError> {
        Err(unimplemented_store())
    }

    async fn search(&self, _filter: LogFilter) -> Result<Vec<LogRecord>, DataplaneError> {
        Err(unimplemented_store())
    }
}