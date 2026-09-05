//! PostgreSQL 远程适配器（v1 占位）。后续实现 RelationalStore。

use async_trait::async_trait;
use dataplane_core::{DataplaneError, ErrorCode, SqlResult, SqlValue};
use dataplane_sql::RelationalStore;

fn unimplemented_store() -> DataplaneError {
    DataplaneError::new(
        ErrorCode::Unimplemented,
        "dataplane-adapter-postgres: not implemented in v1",
    )
}

/// PostgreSQL 远程存储（v1 占位）。
#[derive(Debug, Clone, Default)]
pub struct PostgresStore;

#[async_trait]
impl RelationalStore for PostgresStore {
    async fn execute(&self, _sql: &str, _params: &[SqlValue]) -> Result<SqlResult, DataplaneError> {
        Err(unimplemented_store())
    }
}
