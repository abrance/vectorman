//! 服务端点半表：OTLP resource 自动登记 + 反查缓存。
//!
//! 用途（`/.monkeycode/specs/observability-data-model/design.md`「服务标识与反查」）：
//! eBPF 边只有 `(ip, port)` 与 Pod 名，需要映射回服务名；静态映射表
//! `apm_service_alias` 的 CRUD 由 `apm-tracing` 的 HTTP 接口负责，本模块只做端点
//! 登记与反查（含负面缓存），供后续 eBPF 边归一与拓扑兜底使用。

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use dataplane_core::{DataplaneError, ErrorCode, SqlValue};
use dataplane_ingest::trace::TraceSpan;
use dataplane_sql::RelationalStore;

use crate::tables;

/// 一条端点记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub service: String,
    pub instance_id: String,
    pub pod_name: String,
    pub node_name: String,
    pub host_ip: String,
    pub listen_port: i64,
    pub collector: String,
}

impl Endpoint {
    /// 从 span 的 resource 提取端点；缺少可用标识时返回 `None`。
    ///
    /// `instance_id` 依次取 `service.instance.id`、`service@pod_name`、服务名本身，
    /// 保证同一服务的多副本不会被合并成一行。
    #[must_use]
    pub fn from_span(span: &TraceSpan) -> Option<Self> {
        if span.service.is_empty() {
            return None;
        }
        let get = |key: &str| span.resource.get(key).cloned().unwrap_or_default();
        let pod_name = get("k8s.pod.name");
        let instance_id = {
            let explicit = get("service.instance.id");
            if !explicit.is_empty() {
                explicit
            } else if !pod_name.is_empty() {
                format!("{}@{pod_name}", span.service)
            } else {
                span.service.clone()
            }
        };
        // 监听端口取自 span 属性，缺失记 0（后续按 Pod 名反查）。
        let listen_port = span
            .attributes
            .get("server.port")
            .or_else(|| span.attributes.get("net.host.port"))
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        Some(Self {
            service: span.service.clone(),
            instance_id,
            pod_name,
            node_name: get("k8s.node.name"),
            host_ip: get("host.ip"),
            listen_port,
            collector: span.collector.clone(),
        })
    }
}

/// 端点登记与反查。
#[derive(Debug)]
pub struct EndpointRegistry {
    retention_days: u32,
    cache_ttl: Duration,
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    dirty: BTreeMap<(String, String), (Endpoint, i64)>,
    /// 反查缓存：`(host_ip, listen_port)` → 服务名（含 `None` 表示未命中）。
    cache_ip: BTreeMap<(String, i64), (Option<String>, Instant)>,
    cache_pod: BTreeMap<String, (Option<String>, Instant)>,
    upserted: u64,
}

impl EndpointRegistry {
    #[must_use]
    pub fn new(retention_days: u32, cache_ttl_secs: u64) -> Self {
        Self {
            retention_days,
            cache_ttl: Duration::from_secs(cache_ttl_secs),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// 登记一条 span 对应的端点（内存操作）。
    pub fn observe(&self, span: &TraceSpan, now_ts: i64) {
        let Some(endpoint) = Endpoint::from_span(span) else {
            return;
        };
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.dirty.insert(
            (endpoint.service.clone(), endpoint.instance_id.clone()),
            (endpoint, now_ts),
        );
    }

    /// 待写入的端点数。
    #[must_use]
    pub fn dirty_len(&self) -> usize {
        self.inner.lock().map(|i| i.dirty.len()).unwrap_or(0)
    }

    /// 写入 sqlite（幂等 upsert），并返回写入条数。
    pub async fn flush(&self, sql: &dyn RelationalStore) -> Result<usize, DataplaneError> {
        let batch: Vec<(Endpoint, i64)> = {
            let mut inner = self.inner.lock().map_err(|_| lock_error())?;
            let items = inner.dirty.values().cloned().collect::<Vec<_>>();
            inner.dirty.clear();
            items
        };
        let mut written = 0usize;
        for (endpoint, now_ts) in batch {
            if let Err(e) = upsert(sql, &endpoint, now_ts).await {
                eprintln!(
                    "apm: endpoint upsert failed for {}/{}: {}",
                    endpoint.service, endpoint.instance_id, e.message
                );
                continue;
            }
            written += 1;
        }
        let mut inner = self.inner.lock().map_err(|_| lock_error())?;
        inner.upserted += written as u64;
        Ok(written)
    }

    /// 按 `(host_ip, listen_port)` 反查服务名；未命中同样缓存 `cache_ttl`。
    pub async fn lookup_by_ip_port(
        &self,
        sql: &dyn RelationalStore,
        host_ip: &str,
        listen_port: i64,
    ) -> Result<Option<String>, DataplaneError> {
        if host_ip.is_empty() || listen_port == 0 {
            return Ok(None);
        }
        let key = (host_ip.to_string(), listen_port);
        if let Some(hit) = self.cached_ip(&key) {
            return Ok(hit);
        }
        let result = sql
            .execute(
                &format!(
                    "SELECT service FROM {} WHERE host_ip = ?1 AND listen_port = ?2 LIMIT 1",
                    tables::SERVICE_ENDPOINT
                ),
                &[
                    SqlValue::Text(host_ip.to_string()),
                    SqlValue::Integer(listen_port),
                ],
            )
            .await?;
        let found = result.rows.first().and_then(|row| match row.first() {
            Some(SqlValue::Text(s)) => Some(s.clone()),
            _ => None,
        });
        self.cache_ip(key, found.clone());
        Ok(found)
    }

    /// 按 Pod 名反查服务名；未命中同样缓存。
    pub async fn lookup_by_pod(
        &self,
        sql: &dyn RelationalStore,
        pod_name: &str,
    ) -> Result<Option<String>, DataplaneError> {
        if pod_name.is_empty() {
            return Ok(None);
        }
        if let Some(hit) = self.cached_pod(pod_name) {
            return Ok(hit);
        }
        let result = sql
            .execute(
                &format!(
                    "SELECT service FROM {} WHERE pod_name = ?1 LIMIT 1",
                    tables::SERVICE_ENDPOINT
                ),
                &[SqlValue::Text(pod_name.to_string())],
            )
            .await?;
        let found = result.rows.first().and_then(|row| match row.first() {
            Some(SqlValue::Text(s)) => Some(s.clone()),
            _ => None,
        });
        self.cache_pod(pod_name, found.clone());
        Ok(found)
    }

    /// 清理超过保留期的端点。
    pub async fn purge_expired(
        &self,
        sql: &dyn RelationalStore,
        now_ts: i64,
    ) -> Result<u64, DataplaneError> {
        let cutoff = now_ts.saturating_sub(i64::from(self.retention_days) * 86_400 * 1_000_000);
        sql.execute(
            &format!(
                "DELETE FROM {} WHERE last_seen_ts < ?1",
                tables::SERVICE_ENDPOINT
            ),
            &[SqlValue::Integer(cutoff)],
        )
        .await?;
        // `DELETE` 不返回行（`RelationalStore` 也没有 changes 接口），同一连接上
        // 用 `SELECT changes()` 取受影响行数。
        let result = sql.execute("SELECT changes()", &[]).await?;
        Ok(result
            .rows
            .first()
            .and_then(|r| r.first())
            .map_or(0, |v| match v {
                SqlValue::Integer(i) => *i as u64,
                _ => 0,
            }))
    }

    /// 已写入的端点数（自监控用）。
    #[must_use]
    pub fn upserted(&self) -> u64 {
        self.inner.lock().map(|i| i.upserted).unwrap_or(0)
    }

    fn cached_ip(&self, key: &(String, i64)) -> Option<Option<String>> {
        let mut inner = self.inner.lock().ok()?;
        let (value, at) = inner.cache_ip.get(key)?.clone();
        if at.elapsed() < self.cache_ttl {
            return Some(value);
        }
        inner.cache_ip.remove(key);
        None
    }

    fn cache_ip(&self, key: (String, i64), value: Option<String>) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.cache_ip.insert(key, (value, Instant::now()));
        }
    }

    fn cached_pod(&self, key: &str) -> Option<Option<String>> {
        let mut inner = self.inner.lock().ok()?;
        let (value, at) = inner.cache_pod.get(key)?.clone();
        if at.elapsed() < self.cache_ttl {
            return Some(value);
        }
        inner.cache_pod.remove(key);
        None
    }

    fn cache_pod(&self, key: &str, value: Option<String>) {
        if let Ok(mut inner) = self.inner.lock() {
            inner
                .cache_pod
                .insert(key.to_string(), (value, Instant::now()));
        }
    }
}

async fn upsert(
    sql: &dyn RelationalStore,
    endpoint: &Endpoint,
    now_ts: i64,
) -> Result<(), DataplaneError> {
    sql.execute(
        &format!(
            "INSERT INTO {} (service, instance_id, pod_name, node_name, host_ip, listen_port,
                collector, first_seen_ts, last_seen_ts)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)
             ON CONFLICT(service, instance_id) DO UPDATE SET
                pod_name = excluded.pod_name,
                node_name = excluded.node_name,
                host_ip = excluded.host_ip,
                listen_port = excluded.listen_port,
                collector = excluded.collector,
                last_seen_ts = excluded.last_seen_ts",
            tables::SERVICE_ENDPOINT
        ),
        &[
            SqlValue::Text(endpoint.service.clone()),
            SqlValue::Text(endpoint.instance_id.clone()),
            SqlValue::Text(endpoint.pod_name.clone()),
            SqlValue::Text(endpoint.node_name.clone()),
            SqlValue::Text(endpoint.host_ip.clone()),
            SqlValue::Integer(endpoint.listen_port),
            SqlValue::Text(endpoint.collector.clone()),
            SqlValue::Integer(now_ts),
        ],
    )
    .await?;
    Ok(())
}

fn lock_error() -> DataplaneError {
    DataplaneError::new(ErrorCode::QueryFailed, "endpoint registry lock poisoned")
}
