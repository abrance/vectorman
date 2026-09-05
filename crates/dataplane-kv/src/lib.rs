//! 字节 KV 接口与 redb 引擎。
//!
//! 对应设计文档 `KvStore`（Requirement 6）。键与值均为字节序列，无 TTL。

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use dataplane_core::{DataplaneError, ErrorCode};
use redb::{Database, ReadableDatabase, TableDefinition};

/// 字节键值存储抽象。
#[async_trait]
pub trait KvStore: Send + Sync {
    /// 读取指定键。键不存在返回 `not_found`。
    async fn get(&self, key: &[u8]) -> Result<Vec<u8>, DataplaneError>;

    /// 覆盖写入键值。
    async fn set(&self, key: &[u8], value: &[u8]) -> Result<(), DataplaneError>;

    /// 删除指定键。键不存在返回 `not_found`。
    async fn delete(&self, key: &[u8]) -> Result<(), DataplaneError>;

    /// 键是否存在。
    async fn exists(&self, key: &[u8]) -> Result<bool, DataplaneError>;

    /// 返回键以给定前缀开头的键值对。
    async fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, DataplaneError>;
}

const TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("kv");

fn dp_err<E: std::fmt::Display>(msg: &str, e: E) -> DataplaneError {
    DataplaneError::new(ErrorCode::QueryFailed, format!("{msg}: {e}"))
}

async fn blocking<F, R>(f: F) -> Result<R, DataplaneError>
where
    F: FnOnce() -> Result<R, DataplaneError> + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| DataplaneError::new(ErrorCode::QueryFailed, format!("blocking task panicked: {e}")))?
}

/// redb 本地引擎。
pub struct RedbKvStore {
    db: Arc<Database>,
}

impl RedbKvStore {
    /// 打开或创建 redb 数据库文件，确保 `kv` 表存在。
    pub fn new(path: impl AsRef<Path>) -> Result<Self, DataplaneError> {
        let db = Database::create(path).map_err(|e| dp_err("open redb", e))?;
        let write_txn = db.begin_write().map_err(|e| dp_err("begin write txn", e))?;
        {
            let _table = write_txn.open_table(TABLE).map_err(|e| dp_err("open kv table", e))?;
        }
        write_txn.commit().map_err(|e| dp_err("commit init", e))?;
        Ok(Self { db: Arc::new(db) })
    }
}

#[async_trait]
impl KvStore for RedbKvStore {
    async fn get(&self, key: &[u8]) -> Result<Vec<u8>, DataplaneError> {
        let db = self.db.clone();
        let key = key.to_vec();
        blocking(move || {
            let read_txn = db.begin_read().map_err(|e| dp_err("begin read txn", e))?;
            let table = read_txn.open_table(TABLE).map_err(|e| dp_err("open kv table", e))?;
            match table.get(key.as_slice()).map_err(|e| dp_err("get key", e))? {
                Some(v) => Ok(v.value().to_vec()),
                None => Err(DataplaneError::new(ErrorCode::NotFound, "key not found")),
            }
        })
        .await
    }

    async fn set(&self, key: &[u8], value: &[u8]) -> Result<(), DataplaneError> {
        let db = self.db.clone();
        let key = key.to_vec();
        let value = value.to_vec();
        blocking(move || {
            let write_txn = db.begin_write().map_err(|e| dp_err("begin write txn", e))?;
            {
                let mut table = write_txn.open_table(TABLE).map_err(|e| dp_err("open kv table", e))?;
                table
                    .insert(key.as_slice(), value.as_slice())
                    .map_err(|e| dp_err("set key", e))?;
            }
            write_txn.commit().map_err(|e| dp_err("commit set", e))?;
            Ok(())
        })
        .await
    }

    async fn delete(&self, key: &[u8]) -> Result<(), DataplaneError> {
        let db = self.db.clone();
        let key = key.to_vec();
        blocking(move || {
            let write_txn = db.begin_write().map_err(|e| dp_err("begin write txn", e))?;
            let removed;
            {
                let mut table = write_txn.open_table(TABLE).map_err(|e| dp_err("open kv table", e))?;
                removed = table.remove(key.as_slice()).map_err(|e| dp_err("delete key", e))?.is_some();
            }
            write_txn.commit().map_err(|e| dp_err("commit delete", e))?;
            if removed {
                Ok(())
            } else {
                Err(DataplaneError::new(ErrorCode::NotFound, "key not found"))
            }
        })
        .await
    }

    async fn exists(&self, key: &[u8]) -> Result<bool, DataplaneError> {
        let db = self.db.clone();
        let key = key.to_vec();
        blocking(move || {
            let read_txn = db.begin_read().map_err(|e| dp_err("begin read txn", e))?;
            let table = read_txn.open_table(TABLE).map_err(|e| dp_err("open kv table", e))?;
            let found = table
                .get(key.as_slice())
                .map_err(|e| dp_err("get key", e))?
                .is_some();
            Ok(found)
        })
        .await
    }

    async fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, DataplaneError> {
        let db = self.db.clone();
        let prefix = prefix.to_vec();
        blocking(move || {
            let read_txn = db.begin_read().map_err(|e| dp_err("begin read txn", e))?;
            let table = read_txn.open_table(TABLE).map_err(|e| dp_err("open kv table", e))?;
            let mut out = Vec::new();
            for item in table.range(prefix.as_slice()..).map_err(|e| dp_err("scan range", e))? {
                let (k, v) = item.map_err(|e| dp_err("scan item", e))?;
                let key: &[u8] = &k.value();
                if !key.starts_with(prefix.as_slice()) {
                    break;
                }
                out.push((key.to_vec(), v.value().to_vec()));
            }
            Ok(out)
        })
        .await
    }
}