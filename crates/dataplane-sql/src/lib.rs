//! 关系型存储接口与 sqlite 引擎。
//!
//! 对应设计文档 `RelationalStore`（Requirement 7.1）。

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dataplane_core::{DataplaneError, ErrorCode, SqlResult, SqlValue};
use rusqlite::types::ValueRef;
use rusqlite::Connection;

/// 关系型存储抽象。每次调用执行单条 SQL 语句。
#[async_trait]
pub trait RelationalStore: Send + Sync {
    /// 执行单条 SQL 语句并返回列名与行数据。
    async fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<SqlResult, DataplaneError>;
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

/// 将 `SqlValue` 转换为 rusqlite 参数值。
fn to_rusqlite_value(v: &SqlValue) -> rusqlite::types::Value {
    match v {
        SqlValue::Null => rusqlite::types::Value::Null,
        SqlValue::Integer(i) => rusqlite::types::Value::Integer(*i),
        SqlValue::Real(f) => rusqlite::types::Value::Real(*f),
        SqlValue::Text(s) => rusqlite::types::Value::Text(s.clone()),
        SqlValue::Blob(b) => rusqlite::types::Value::Blob(b.clone()),
    }
}

/// 从 rusqlite 值引用转换为 `SqlValue`。
fn from_value_ref(v: ValueRef<'_>) -> SqlValue {
    match v {
        ValueRef::Null => SqlValue::Null,
        ValueRef::Integer(i) => SqlValue::Integer(i),
        ValueRef::Real(f) => SqlValue::Real(f),
        ValueRef::Text(t) => SqlValue::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => SqlValue::Blob(b.to_vec()),
    }
}

fn map_execute_error(e: rusqlite::Error) -> DataplaneError {
    match e {
        rusqlite::Error::MultipleStatement => DataplaneError::new(
            ErrorCode::InvalidArgument,
            "multiple SQL statements are not allowed",
        ),
        other => DataplaneError::new(ErrorCode::QueryFailed, other.to_string()),
    }
}

/// sqlite 本地引擎。
pub struct SqliteRelationalStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteRelationalStore {
    /// 打开或创建 sqlite 数据库文件。
    pub fn new(path: impl AsRef<Path>) -> Result<Self, DataplaneError> {
        let conn = Connection::open(path).map_err(|e| {
            DataplaneError::new(ErrorCode::QueryFailed, format!("open sqlite: {e}"))
        })?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
}

#[async_trait]
impl RelationalStore for SqliteRelationalStore {
    async fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<SqlResult, DataplaneError> {
        let conn = self.conn.clone();
        let sql = sql.to_string();
        let params: Vec<rusqlite::types::Value> = params.iter().map(to_rusqlite_value).collect();
        blocking(move || {
            let conn = conn.lock().map_err(|_| {
                DataplaneError::new(ErrorCode::QueryFailed, "sqlite connection lock poisoned")
            })?;
            let mut stmt = conn.prepare(&sql).map_err(map_execute_error)?;
            let column_count = stmt.column_count();
            let columns: Vec<String> = (0..column_count)
                .map(|i| stmt.column_name(i).unwrap_or_default().to_string())
                .collect();
            let mut rows = Vec::new();
            let mut row_iter = stmt
                .query(rusqlite::params_from_iter(params))
                .map_err(map_execute_error)?;
            while let Some(row) = row_iter.next().map_err(map_execute_error)? {
                let mut row_vals = Vec::with_capacity(column_count);
                for i in 0..column_count {
                    let vr = row.get_ref(i).map_err(map_execute_error)?;
                    row_vals.push(from_value_ref(vr));
                }
                rows.push(row_vals);
            }
            Ok(SqlResult { columns, rows })
        })
        .await
    }
}
