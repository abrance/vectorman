//! Redis `KvStore` 适配器。
//!
//! 键映射：`{prefix}{key}`，键值均为原始字节（Redis 键值是二进制安全的）。
//!
//! 语义与 redb 本地引擎对齐，只有两处固有差异，调用方必须知道：
//!
//! - **持久化由服务端配置决定，不由本适配器决定。** 服务端 `appendonly no` 时仍会
//!   返回成功，但重启后丢全部键（`ingest/` 是去重键，丢了会重复入库，不是丢数据）。
//!   生产前置条件见 `docs/external-storage.md`。
//! - **没有事务。** 本地引擎里「写 `retain/` 再删 `spec-live/`」是两条独立命令，
//!   中途失败会残留，但残留结果是「多留一天」而不是丢数据；调用方已容忍。
//!
//! 百万级前缀（`ingest/`）只走 [`RedisKvStore::prune_prefix_before`]，用 `SCAN`
//! 流式处理；`scan_prefix` 仍是一次性 `Vec`，只给 `spec-live/` / `retain/` /
//! `stream/` 这类有界集合用（`stream/` 是 采集项 × 类型，规模在千级）。

use std::time::Duration;

use async_trait::async_trait;
use dataplane_core::{DataplaneError, ErrorCode};
use dataplane_kv::KvStore;
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, Client};

/// Redis 连接与键空间配置。
#[derive(Debug, Clone)]
pub struct RedisConfig {
    /// `redis://[:password@]host:port/0`
    pub url: String,
    /// 独立密码（优先生效，等价于握手后发 `AUTH`）。
    pub password: Option<String>,
    /// 键前缀，避免与同库其它业务撞键。缺省 `dp:`。
    pub prefix: String,
    /// 连接/单次命令超时。
    pub timeout: Duration,
    /// `SCAN` 每批提示条数。
    pub scan_count: usize,
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            url: "redis://127.0.0.1:6379/0".to_string(),
            password: None,
            prefix: "dp:".to_string(),
            timeout: Duration::from_secs(5),
            scan_count: 500,
        }
    }
}

/// Redis 远程 KV 存储。
///
/// `ConnectionManager` 自带断线重连；`clone` 只是共享同一个多路复用连接。
#[derive(Clone)]
pub struct RedisKvStore {
    conn: ConnectionManager,
    prefix: Vec<u8>,
    timeout: Duration,
    scan_count: usize,
}

impl std::fmt::Debug for RedisKvStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisKvStore")
            .field("prefix", &String::from_utf8_lossy(&self.prefix))
            .finish_non_exhaustive()
    }
}

fn io_err(msg: &str, e: impl std::fmt::Display) -> DataplaneError {
    DataplaneError::new(ErrorCode::Unavailable, format!("redis {msg}: {e}"))
}

fn map_redis_error(msg: &str, e: redis::RedisError) -> DataplaneError {
    let text = format!("redis {msg}: {e}");
    // `maxmemory` + `noeviction` 触顶是 **硬失败**（`OOM command not allowed when used
    // memory > maxmemory`），不是慢：对调用方必须是可重试的 `unavailable`，否则
    // `ingest/` 去重键一写就打穿整条接入链路。LOADING/READONLY/TRYAGAIN 同理。
    let lower = text.to_ascii_lowercase();
    let transient = e.is_io_error()
        || e.is_connection_dropped()
        || e.is_timeout()
        || lower.contains("oom command not allowed")
        || lower.contains("loading")
        || lower.contains("readonly")
        || lower.contains("try again");
    if transient {
        DataplaneError::new(ErrorCode::Unavailable, text)
    } else {
        DataplaneError::new(ErrorCode::QueryFailed, text)
    }
}

/// 把前缀转成 `SCAN MATCH` 用的 glob 模式，转义 glob 元字符。
fn glob_pattern(prefix: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(prefix.len() + 1);
    for b in prefix {
        if matches!(b, b'*' | b'?' | b'[' | b']' | b'\\' | b'^' | b'-') {
            out.push(b'\\');
        }
        out.push(*b);
    }
    out.push(b'*');
    out
}

fn key_with_prefix(prefix: &[u8], key: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(prefix.len() + key.len());
    out.extend_from_slice(prefix);
    out.extend_from_slice(key);
    out
}

/// 去掉前缀，把服务端键还原成调用方看到的键。
fn key_without_prefix(prefix: &[u8], key: Vec<u8>) -> Vec<u8> {
    if key.starts_with(prefix) {
        key[prefix.len()..].to_vec()
    } else {
        key
    }
}

/// `ingest/{id}` 的值是 8 字节小端微秒时间戳；解析失败按「已过期」处理（见 trait 注释）。
fn value_is_stale(value: &[u8], cutoff_micros: i64) -> bool {
    if value.len() != 8 {
        return true;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(value);
    i64::from_le_bytes(buf) < cutoff_micros
}

impl RedisKvStore {
    /// 建连并握手（`PING`），失败即启动失败，不把半死连接带进服务。
    pub async fn new(cfg: RedisConfig) -> Result<Self, DataplaneError> {
        let client = Client::open(cfg.url.as_str())
            .map_err(|e| DataplaneError::config_invalid(format!("redis url: {e}")))?;
        let mut conn =
            match tokio::time::timeout(cfg.timeout, client.get_connection_manager()).await {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => {
                    return Err(DataplaneError::new(
                        ErrorCode::EngineInitFailed,
                        format!("connect redis {}: {e}", cfg.url),
                    ))
                }
                Err(_) => {
                    return Err(DataplaneError::new(
                        ErrorCode::EngineInitFailed,
                        format!("connect redis {}: timeout", cfg.url),
                    ))
                }
            };
        if let Some(pw) = &cfg.password {
            let r: Result<String, _> = redis::cmd("AUTH").arg(pw).query_async(&mut conn).await;
            // `AUTH` 对无密码实例返回 "ERR Client sent AUTH, but no password is set"，
            // 那是配置不一致，直接报出来，不吞。
            r.map_err(|e| {
                DataplaneError::new(
                    ErrorCode::EngineInitFailed,
                    format!("redis AUTH failed: {e}"),
                )
            })?;
        }
        let pong: Result<String, _> =
            tokio::time::timeout(cfg.timeout, redis::cmd("PING").query_async(&mut conn))
                .await
                .map_err(|_| {
                    DataplaneError::new(
                        ErrorCode::EngineInitFailed,
                        "redis PING timeout".to_string(),
                    )
                })?;
        pong.map_err(|e| {
            DataplaneError::new(ErrorCode::EngineInitFailed, format!("redis PING: {e}"))
        })?;
        Ok(Self {
            conn,
            prefix: cfg.prefix.into_bytes(),
            timeout: cfg.timeout,
            scan_count: cfg.scan_count.max(1),
        })
    }

    fn key(&self, key: &[u8]) -> Vec<u8> {
        key_with_prefix(&self.prefix, key)
    }

    fn unkey(&self, key: Vec<u8>) -> Vec<u8> {
        key_without_prefix(&self.prefix, key)
    }
}

#[async_trait]
impl KvStore for RedisKvStore {
    async fn get(&self, key: &[u8]) -> Result<Vec<u8>, DataplaneError> {
        let mut conn = self.conn.clone();
        let full = self.key(key);
        let got: Option<Vec<u8>> = tokio::time::timeout(self.timeout, conn.get(full))
            .await
            .map_err(|_| io_err("get", "timeout"))?
            .map_err(|e| map_redis_error("get", e))?;
        got.ok_or_else(|| {
            DataplaneError::new(
                ErrorCode::NotFound,
                format!("key not found: {}", String::from_utf8_lossy(key)),
            )
        })
    }

    async fn set(&self, key: &[u8], value: &[u8]) -> Result<(), DataplaneError> {
        let mut conn = self.conn.clone();
        let full = self.key(key);
        let _: () = tokio::time::timeout(self.timeout, conn.set(full, value))
            .await
            .map_err(|_| io_err("set", "timeout"))?
            .map_err(|e| map_redis_error("set", e))?;
        Ok(())
    }

    async fn delete(&self, key: &[u8]) -> Result<(), DataplaneError> {
        let mut conn = self.conn.clone();
        let full = self.key(key);
        let n: u64 = tokio::time::timeout(self.timeout, conn.del(full))
            .await
            .map_err(|_| io_err("del", "timeout"))?
            .map_err(|e| map_redis_error("del", e))?;
        if n == 0 {
            return Err(DataplaneError::new(
                ErrorCode::NotFound,
                format!("key not found: {}", String::from_utf8_lossy(key)),
            ));
        }
        Ok(())
    }

    async fn exists(&self, key: &[u8]) -> Result<bool, DataplaneError> {
        let mut conn = self.conn.clone();
        let full = self.key(key);
        let n: u64 = tokio::time::timeout(self.timeout, conn.exists(full))
            .await
            .map_err(|_| io_err("exists", "timeout"))?
            .map_err(|e| map_redis_error("exists", e))?;
        Ok(n > 0)
    }

    /// `MGET` 一次往返。逐键 `EXISTS` 的往返次数 = 批大小，接入去重会被 RTT 卡死。
    async fn exists_many(&self, keys: &[Vec<u8>]) -> Result<Vec<bool>, DataplaneError> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = self.conn.clone();
        let mut out = Vec::with_capacity(keys.len());
        // 分批只是给单条命令封一个大小上限（与 `SCAN` 用同一个提示条数）。
        for chunk in keys.chunks(self.scan_count.max(1)) {
            let mut cmd = redis::cmd("MGET");
            for key in chunk {
                cmd.arg(self.key(key));
            }
            let values: Vec<Option<Vec<u8>>> =
                tokio::time::timeout(self.timeout, cmd.query_async(&mut conn))
                    .await
                    .map_err(|_| io_err("mget", "timeout"))?
                    .map_err(|e| map_redis_error("mget", e))?;
            out.extend(values.into_iter().map(|v| v.is_some()));
        }
        Ok(out)
    }

    /// `MSET` 一次往返，原子提交整批（与 trait 的「不得只提交一部分」一致）。
    async fn set_many(&self, pairs: &[(Vec<u8>, Vec<u8>)]) -> Result<(), DataplaneError> {
        if pairs.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.clone();
        for chunk in pairs.chunks(self.scan_count.max(1)) {
            let mut cmd = redis::cmd("MSET");
            for (key, value) in chunk {
                cmd.arg(self.key(key)).arg(value.as_slice());
            }
            let _: () = tokio::time::timeout(self.timeout, cmd.query_async(&mut conn))
                .await
                .map_err(|_| io_err("mset", "timeout"))?
                .map_err(|e| map_redis_error("mset", e))?;
        }
        Ok(())
    }

    async fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, DataplaneError> {
        let mut conn = self.conn.clone();
        let pattern = glob_pattern(&self.key(prefix));
        let mut cursor: u64 = 0;
        let mut out = Vec::new();
        loop {
            let (next, keys): (u64, Vec<Vec<u8>>) = tokio::time::timeout(
                self.timeout,
                redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg(pattern.as_slice())
                    .arg("COUNT")
                    .arg(self.scan_count)
                    .query_async(&mut conn),
            )
            .await
            .map_err(|_| io_err("scan", "timeout"))?
            .map_err(|e| map_redis_error("scan", e))?;
            for chunk in keys.chunks(self.scan_count.max(1)) {
                let mut pipe = redis::pipe();
                for k in chunk {
                    pipe.cmd("GET").arg(k.as_slice());
                }
                let values: Vec<Option<Vec<u8>>> =
                    tokio::time::timeout(self.timeout, pipe.query_async(&mut conn))
                        .await
                        .map_err(|_| io_err("scan get", "timeout"))?
                        .map_err(|e| map_redis_error("scan get", e))?;
                for (k, v) in chunk.iter().zip(values) {
                    // SCAN 期间被删掉的键 GET 返回 nil，跳过即可。
                    if let Some(v) = v {
                        out.push((self.unkey(k.clone()), v));
                    }
                }
            }
            if next == 0 {
                break;
            }
            cursor = next;
        }
        Ok(out)
    }

    async fn prune_prefix_before(
        &self,
        prefix: &[u8],
        cutoff_micros: i64,
    ) -> Result<u64, DataplaneError> {
        let mut conn = self.conn.clone();
        let pattern = glob_pattern(&self.key(prefix));
        let mut cursor: u64 = 0;
        let mut removed = 0u64;
        loop {
            let (next, keys): (u64, Vec<Vec<u8>>) = tokio::time::timeout(
                self.timeout,
                redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg(pattern.as_slice())
                    .arg("COUNT")
                    .arg(self.scan_count)
                    .query_async(&mut conn),
            )
            .await
            .map_err(|_| io_err("prune scan", "timeout"))?
            .map_err(|e| map_redis_error("prune scan", e))?;
            // 按批 GET，再按批 DEL：整段前缀不进内存，这是 `ingest/` 百万级键的唯一活路。
            for chunk in keys.chunks(self.scan_count.max(1)) {
                let mut pipe = redis::pipe();
                for k in chunk {
                    pipe.cmd("GET").arg(k.as_slice());
                }
                let values: Vec<Option<Vec<u8>>> =
                    tokio::time::timeout(self.timeout, pipe.query_async(&mut conn))
                        .await
                        .map_err(|_| io_err("prune get", "timeout"))?
                        .map_err(|e| map_redis_error("prune get", e))?;
                let stale: Vec<&Vec<u8>> = chunk
                    .iter()
                    .zip(values.iter())
                    .filter(|(_, v)| match v {
                        Some(v) => value_is_stale(v, cutoff_micros),
                        // 扫描间隙被删/过期，视作已处理。
                        None => false,
                    })
                    .map(|(k, _)| k)
                    .collect();
                if stale.is_empty() {
                    continue;
                }
                let mut del = redis::pipe();
                for k in &stale {
                    del.cmd("DEL").arg(k.as_slice());
                }
                // 管道返回每条命令的结果，不是单个整数。
                let counts: Vec<u64> =
                    tokio::time::timeout(self.timeout, del.query_async(&mut conn))
                        .await
                        .map_err(|_| io_err("prune del", "timeout"))?
                        .map_err(|e| map_redis_error("prune del", e))?;
                removed += counts.into_iter().sum::<u64>();
            }
            if next == 0 {
                break;
            }
            cursor = next;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_pattern_escapes_meta() {
        assert_eq!(glob_pattern(b"ingest/"), b"ingest/*".to_vec());
        assert_eq!(glob_pattern(b"a*b"), b"a\\*b*".to_vec());
        assert_eq!(glob_pattern(b"a[1]"), b"a\\[1\\]*".to_vec());
    }

    #[test]
    fn stale_value_rules() {
        let old = (100i64).to_le_bytes();
        let fresh = (5_000i64).to_le_bytes();
        assert!(value_is_stale(&old, 1_000));
        assert!(!value_is_stale(&fresh, 1_000));
        // 旧版本写的空值：无法解析 → 按过期删掉。
        assert!(value_is_stale(b"", 0));
        assert!(value_is_stale(b"1234", 0));
    }

    #[test]
    fn oom_is_retryable_unavailable() {
        let e = redis::RedisError::from((
            redis::ErrorKind::ResponseError,
            "ERR",
            "OOM command not allowed when used memory > 'maxmemory'.".to_string(),
        ));
        let mapped = map_redis_error("set", e);
        assert_eq!(mapped.code, ErrorCode::Unavailable, "{}", mapped.message);

        let e = redis::RedisError::from((
            redis::ErrorKind::ResponseError,
            "ERR",
            "unknown command 'FOO'".to_string(),
        ));
        let mapped = map_redis_error("set", e);
        assert_eq!(mapped.code, ErrorCode::QueryFailed, "{}", mapped.message);
    }

    #[test]
    fn key_roundtrip_keeps_binary() {
        let full = key_with_prefix(b"dp:", b"ingest/\xff\x00");
        assert_eq!(full, b"dp:ingest/\xff\x00".to_vec());
        assert_eq!(
            key_without_prefix(b"dp:", full),
            b"ingest/\xff\x00".to_vec()
        );
    }
}
