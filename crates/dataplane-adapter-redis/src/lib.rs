//! Redis 远程适配器（v1 占位）。后续实现 KvStore。

use async_trait::async_trait;
use dataplane_core::{DataplaneError, ErrorCode};
use dataplane_kv::KvStore;

fn unimplemented_store() -> DataplaneError {
    DataplaneError::new(
        ErrorCode::Unimplemented,
        "dataplane-adapter-redis: not implemented in v1",
    )
}

/// Redis 远程存储（v1 占位）。
#[derive(Debug, Clone, Default)]
pub struct RedisStore;

#[async_trait]
impl KvStore for RedisStore {
    async fn get(&self, _key: &[u8]) -> Result<Vec<u8>, DataplaneError> {
        Err(unimplemented_store())
    }

    async fn set(&self, _key: &[u8], _value: &[u8]) -> Result<(), DataplaneError> {
        Err(unimplemented_store())
    }

    async fn delete(&self, _key: &[u8]) -> Result<(), DataplaneError> {
        Err(unimplemented_store())
    }

    async fn exists(&self, _key: &[u8]) -> Result<bool, DataplaneError> {
        Err(unimplemented_store())
    }

    async fn scan_prefix(&self, _prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, DataplaneError> {
        Err(unimplemented_store())
    }
}