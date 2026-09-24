//! eBPF 边落库与服务名反查（`dataplane-ingest::EdgeSink` 的实现）。
//!
//! 反查顺序（`/.monkeycode/specs/observability-data-model/design.md`「写入路径」，固定不重排）：
//!
//! 1. Agent 已填的 `src_service` / `dst_service` 直接用（Agent 能确定时不该被覆盖）；
//! 2. 静态映射 `apm_service_alias`（`process_name` → `process_prefix` → `pod_prefix` → `cidr`）；
//! 3. 端点表 `apm_service_endpoint`：先 `(host_ip, listen_port)` 精确命中，再 `(pod_name)`；
//! 4. 都没命中落 `unknown-<ip>`，**且不写入端点表**（避免未识别的 IP 污染端点表）。
//!
//! 反查结果的两级缓存都在既有实现里：`AliasCache`（快照 + 版本号失效）与
//! `EndpointRegistry`（命中与负面都缓存 `cache_ttl_secs`，缺省 60 秒）。
//!
//! 落库用 `INSERT OR REPLACE` + `record_id` 主键：同一桶重发就是覆盖写，天然幂等。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use dataplane_core::{DataplaneError, SqlValue};
use dataplane_ingest::{DataEnvelope, EbpfEdge, EdgeSink};
use dataplane_sql::RelationalStore;

use crate::alias::AliasCache;
use crate::endpoint::EndpointRegistry;
use crate::tables;

/// eBPF 边写入器。
pub struct EbpfEdgeSink {
    sql: Arc<dyn RelationalStore>,
    endpoints: Arc<EndpointRegistry>,
    aliases: Arc<AliasCache>,
    written: AtomicU64,
    started: std::time::Instant,
}

impl EbpfEdgeSink {
    #[must_use]
    pub fn new(
        sql: Arc<dyn RelationalStore>,
        endpoints: Arc<EndpointRegistry>,
        aliases: Arc<AliasCache>,
    ) -> Self {
        Self {
            sql,
            endpoints,
            aliases,
            written: AtomicU64::new(0),
            started: std::time::Instant::now(),
        }
    }

    /// 已写入的边记录数。
    #[must_use]
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    /// 运行时长（秒），自监控用。
    #[must_use]
    pub fn uptime_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// 源侧反查：Agent 给了服务名就用，否则按静态映射 → 端点表 → `unknown-<ip>`。
    pub async fn resolve_src(&self, edge: &EbpfEdge) -> Result<String, DataplaneError> {
        if !edge.src_service.is_empty() {
            return Ok(edge.src_service.clone());
        }
        self.resolve(
            &edge.src_ip,
            i64::from(edge.src_port),
            &edge.src_pod,
            &edge.src_process,
        )
        .await
    }

    /// 目标侧反查：Agent 侧没有对端进程名与 Pod，只能靠 IP/端口与静态映射。
    pub async fn resolve_dst(&self, edge: &EbpfEdge) -> Result<String, DataplaneError> {
        if !edge.dst_service.is_empty() {
            return Ok(edge.dst_service.clone());
        }
        self.resolve(&edge.dst_ip, edge.lookup_port(), "", "").await
    }

    async fn resolve(
        &self,
        host_ip: &str,
        port: i64,
        pod_name: &str,
        process_name: &str,
    ) -> Result<String, DataplaneError> {
        if let Some(service) = self
            .aliases
            .resolve(self.sql.as_ref(), host_ip, pod_name, process_name)
            .await?
        {
            return Ok(service);
        }
        if let Some(service) = self
            .endpoints
            .lookup_by_ip_port(self.sql.as_ref(), host_ip, port)
            .await?
        {
            return Ok(service);
        }
        if !pod_name.is_empty() {
            if let Some(service) = self
                .endpoints
                .lookup_by_pod(self.sql.as_ref(), pod_name)
                .await?
            {
                return Ok(service);
            }
        }
        // 未命中：保持 `unknown-<ip>`，且**不**写端点表。
        Ok(unknown_service(host_ip))
    }

    /// 写入（覆盖写）一条边。
    pub async fn upsert(
        &self,
        edge: &EbpfEdge,
        envelope: &DataEnvelope,
    ) -> Result<(), DataplaneError> {
        let src_service = self.resolve_src(edge).await?;
        let dst_service = self.resolve_dst(edge).await?;
        let hist = serde_json::to_string(&edge.latency_hist).map_err(|e| {
            DataplaneError::new(dataplane_core::ErrorCode::QueryFailed, e.to_string())
        })?;
        self.sql
            .execute(
                &format!(
                    "INSERT OR REPLACE INTO {} (
                        record_id, bucket_start, bucket_micros, protocol,
                        src_ip, src_port, dst_ip, dst_port,
                        src_pod, src_container_id, src_process, src_service, dst_service,
                        connections, bytes_sent, bytes_recv, duration_sum, duration_max,
                        tcp_retrans, tcp_resets, failures, failure_reason, latency_hist,
                        agent_id, data_id)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25)",
                    tables::EBPF_EDGES
                ),
                &[
                    SqlValue::Text(edge.record_id.clone()),
                    SqlValue::Integer(edge.timestamp),
                    SqlValue::Integer(edge.bucket_micros),
                    SqlValue::Text(edge.protocol.clone()),
                    SqlValue::Text(edge.src_ip.clone()),
                    SqlValue::Integer(i64::from(edge.src_port)),
                    SqlValue::Text(edge.dst_ip.clone()),
                    SqlValue::Integer(i64::from(edge.dst_port)),
                    SqlValue::Text(edge.src_pod.clone()),
                    SqlValue::Text(edge.src_container_id.clone()),
                    SqlValue::Text(edge.src_process.clone()),
                    SqlValue::Text(src_service),
                    SqlValue::Text(dst_service),
                    SqlValue::Integer(edge.connections as i64),
                    SqlValue::Integer(edge.bytes_sent as i64),
                    SqlValue::Integer(edge.bytes_recv as i64),
                    SqlValue::Integer(edge.duration_micros_sum as i64),
                    SqlValue::Integer(edge.duration_micros_max as i64),
                    SqlValue::Integer(edge.tcp_retrans as i64),
                    SqlValue::Integer(edge.tcp_resets as i64),
                    SqlValue::Integer(edge.failures as i64),
                    SqlValue::Text(edge.failure_reason.clone()),
                    SqlValue::Text(hist),
                    SqlValue::Text(envelope.agent_id.clone()),
                    SqlValue::Text(envelope.data_id.clone()),
                ],
            )
            .await?;
        self.written.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[async_trait]
impl EdgeSink for EbpfEdgeSink {
    async fn observe_edge(
        &self,
        edge: &EbpfEdge,
        envelope: &DataEnvelope,
    ) -> Result<(), DataplaneError> {
        self.upsert(edge, envelope).await
    }

    fn written_edges(&self) -> u64 {
        self.written()
    }
}

/// 未识别服务的归一值：前端拓扑图单独列「未识别」，不要写成空串。
#[must_use]
pub fn unknown_service(ip: &str) -> String {
    if ip.is_empty() {
        "unknown".to_string()
    } else {
        format!("unknown-{ip}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_service_keeps_ip_for_grouping() {
        assert_eq!(unknown_service("10.0.0.9"), "unknown-10.0.0.9");
        assert_eq!(unknown_service(""), "unknown");
    }
}
