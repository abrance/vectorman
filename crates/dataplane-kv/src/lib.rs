//! 字节 KV 接口与 redb 引擎。
//!
//! 对应设计文档 `KvStore`（Requirement 6）。键与值均为字节序列，无 TTL。

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use dataplane_core::{DataplaneError, ErrorCode};
use redb::{Builder, Database, ReadableDatabase, TableDefinition};

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

    /// 删除 `prefix` 下已过期的键，返回删除数量。
    ///
    /// 值约定：8 字节小端微秒时间戳（`ingest/{record_id}` 的写法）。值无法解析
    /// （旧版本写的空值）按已过期处理一并删除。
    ///
    /// 实现必须按范围流式处理，不得把整段前缀读进内存 —— 线上 `ingest/` 是百万级键，
    /// `scan_prefix` 那种一次性 `Vec` 会直接把进程顶到内存上限。
    async fn prune_prefix_before(
        &self,
        prefix: &[u8],
        cutoff_micros: i64,
    ) -> Result<u64, DataplaneError>;
}

/// redb 页缓存缺省预算。
///
/// redb 4.x 的缺省是 1GiB：dataserver 容器内存上限 2GiB，光页缓存就占掉一半，
/// 数据文件一涨（`ingest/` 去重键从不清理）就 OOMKilled（2026-10-08 线上）。
/// 64MiB 对 B 树点查足够，省下的内存留给时序/日志引擎。
pub const DEFAULT_CACHE_BYTES: usize = 64 * 1024 * 1024;

const TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("kv");

fn dp_err<E: std::fmt::Display>(msg: &str, e: E) -> DataplaneError {
    DataplaneError::new(ErrorCode::QueryFailed, format!("{msg}: {e}"))
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

/// redb 本地引擎。
pub struct RedbKvStore {
    db: Arc<Database>,
}

impl RedbKvStore {
    /// 打开或创建 redb 数据库文件，确保 `kv` 表存在。页缓存用 [`DEFAULT_CACHE_BYTES`]。
    pub fn new(path: impl AsRef<Path>) -> Result<Self, DataplaneError> {
        Self::with_cache_size(path, DEFAULT_CACHE_BYTES)
    }

    /// 同 [`Self::new`]，但显式指定页缓存字节数（0 视为缺省）。
    pub fn with_cache_size(
        path: impl AsRef<Path>,
        cache_bytes: usize,
    ) -> Result<Self, DataplaneError> {
        let cache_bytes = if cache_bytes == 0 {
            DEFAULT_CACHE_BYTES
        } else {
            cache_bytes
        };
        let db = Builder::new()
            .set_cache_size(cache_bytes)
            .create(path)
            .map_err(|e| dp_err("open redb", e))?;
        let write_txn = db.begin_write().map_err(|e| dp_err("begin write txn", e))?;
        {
            let _table = write_txn
                .open_table(TABLE)
                .map_err(|e| dp_err("open kv table", e))?;
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
            let table = read_txn
                .open_table(TABLE)
                .map_err(|e| dp_err("open kv table", e))?;
            match table
                .get(key.as_slice())
                .map_err(|e| dp_err("get key", e))?
            {
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
                let mut table = write_txn
                    .open_table(TABLE)
                    .map_err(|e| dp_err("open kv table", e))?;
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
                let mut table = write_txn
                    .open_table(TABLE)
                    .map_err(|e| dp_err("open kv table", e))?;
                removed = table
                    .remove(key.as_slice())
                    .map_err(|e| dp_err("delete key", e))?
                    .is_some();
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
            let table = read_txn
                .open_table(TABLE)
                .map_err(|e| dp_err("open kv table", e))?;
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
            let table = read_txn
                .open_table(TABLE)
                .map_err(|e| dp_err("open kv table", e))?;
            let mut out = Vec::new();
            for item in table
                .range(prefix.as_slice()..)
                .map_err(|e| dp_err("scan range", e))?
            {
                let (k, v) = item.map_err(|e| dp_err("scan item", e))?;
                let key: &[u8] = k.value();
                if !key.starts_with(prefix.as_slice()) {
                    break;
                }
                out.push((key.to_vec(), v.value().to_vec()));
            }
            Ok(out)
        })
        .await
    }

    async fn prune_prefix_before(
        &self,
        prefix: &[u8],
        cutoff_micros: i64,
    ) -> Result<u64, DataplaneError> {
        let db = self.db.clone();
        let prefix = prefix.to_vec();
        blocking(move || {
            let write_txn = db.begin_write().map_err(|e| dp_err("begin write txn", e))?;
            let mut removed = 0u64;
            {
                let mut table = write_txn
                    .open_table(TABLE)
                    .map_err(|e| dp_err("open kv table", e))?;
                // 范围上界省略，用谓词里的 `starts_with` 收口（与 `scan_prefix` 同款写法）。
                let expired = table
                    .extract_from_if(prefix.as_slice().., |key: &[u8], value: &[u8]| {
                        if !key.starts_with(prefix.as_slice()) {
                            return false;
                        }
                        micros_of(value).is_none_or(|ts| ts < cutoff_micros)
                    })
                    .map_err(|e| dp_err("scan range", e))?;
                for item in expired {
                    item.map_err(|e| dp_err("prune item", e))?;
                    removed += 1;
                }
            }
            write_txn.commit().map_err(|e| dp_err("commit prune", e))?;
            Ok(removed)
        })
        .await
    }
}

/// 解析「8 字节小端微秒」的值；其它长度返回 `None`（旧版本的空值）。
fn micros_of(value: &[u8]) -> Option<i64> {
    <[u8; 8]>::try_from(value).ok().map(i64::from_le_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &std::path::Path) -> RedbKvStore {
        RedbKvStore::new(dir.join("kv.redb")).unwrap()
    }

    fn micros(ts: i64) -> Vec<u8> {
        ts.to_le_bytes().to_vec()
    }

    #[tokio::test]
    async fn prune_prefix_before_deletes_expired_and_legacy_only() {
        let dir = tempfile::tempdir().unwrap();
        let kv = store(dir.path());
        let cutoff = 1_000_000i64;

        kv.set(b"ingest/old", &micros(cutoff - 1)).await.unwrap();
        kv.set(b"ingest/fresh", &micros(cutoff + 1)).await.unwrap();
        // 旧版本写的空值：没有时间戳，按已过期处理。
        kv.set(b"ingest/legacy", &[]).await.unwrap();
        // 别的前缀不受影响（即使值同样是过期时间戳）。
        kv.set(b"stream/agent", &micros(1)).await.unwrap();

        let removed = kv.prune_prefix_before(b"ingest/", cutoff).await.unwrap();
        assert_eq!(removed, 2, "只删过期的 ingest 键");
        assert!(!kv.exists(b"ingest/old").await.unwrap());
        assert!(!kv.exists(b"ingest/legacy").await.unwrap());
        assert!(kv.exists(b"ingest/fresh").await.unwrap());
        assert!(kv.exists(b"stream/agent").await.unwrap());

        // 幂等：再跑一次没有可删的。
        assert_eq!(kv.prune_prefix_before(b"ingest/", cutoff).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn prune_prefix_before_stops_at_prefix_end() {
        let dir = tempfile::tempdir().unwrap();
        let kv = store(dir.path());
        // 字母序上 `ingest/` 之后紧接着的键（`ingestx/...`）不能被误删。
        kv.set(b"ingest/a", &micros(1)).await.unwrap();
        kv.set(b"ingestx/a", &micros(1)).await.unwrap();

        assert_eq!(
            kv.prune_prefix_before(b"ingest/", 100).await.unwrap(),
            1,
            "`ingestx/` 不以 `ingest/` 开头"
        );
        assert!(kv.exists(b"ingestx/a").await.unwrap());
    }
}
