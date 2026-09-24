//! 组合 sink：把 `dataplane-ingest` 的 [`TraceSink`] 钩子接到摘要累加器与端点表。
//!
//! dataserver 在接入 `data_type=traces` 时传入本对象；`observe_span` 只做内存操作
//! （不 await 存储），因此不会拖慢接入路径。落库由 dataserver 的后台任务调用
//! [`ApmSink::flush_due`] 与 [`ApmSink::purge_expired`]。

use std::sync::Arc;

use async_trait::async_trait;
use dataplane_core::DataplaneError;
use dataplane_ingest::trace::{TraceSink, TraceSpan};
use dataplane_ingest::{DataEnvelope, EbpfEdge, EdgeSink};
use dataplane_sql::RelationalStore;

use crate::accumulator::{ApmSinkConfig, TraceSummaryAccumulator};
use crate::alias::{AliasCache, AliasRecord, AliasUpsert};
use crate::ebpf_edge::EbpfEdgeSink;
use crate::edge::{EdgeAccumulator, ServiceResolver};
use crate::endpoint::EndpointRegistry;
use crate::red::RedSamples;

pub use crate::accumulator::ApmSinkConfig as Config;

/// 一次 flush 的结果，供自监控使用。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlushReport {
    pub traces: usize,
    pub endpoints: usize,
    pub edges: usize,
}

/// dataserver 侧 APM 派生数据的聚合入口。
pub struct ApmSink {
    sql: Arc<dyn RelationalStore>,
    config: ApmSinkConfig,
    accumulator: TraceSummaryAccumulator,
    endpoints: Arc<EndpointRegistry>,
    edges: EdgeAccumulator,
    samples: Arc<RedSamples>,
    aliases: Arc<AliasCache>,
    /// eBPF 边写入器（与摘要共用同一份端点表与静态映射缓存）。
    ebpf_edges: EbpfEdgeSink,
}

/// 把静态映射与端点表适配成边目标归一用的解析器。
///
/// 优先级固定为：静态映射（`apm_service_alias`）> 端点表 > `unknown-<ip>`（调用方保持原值）。
struct ServiceNameResolver<'a> {
    sql: &'a dyn RelationalStore,
    registry: &'a EndpointRegistry,
    aliases: &'a AliasCache,
}

#[async_trait]
impl ServiceResolver for ServiceNameResolver<'_> {
    async fn resolve(
        &self,
        host_ip: &str,
        port: i64,
        pod_name: &str,
    ) -> Result<Option<String>, DataplaneError> {
        if let Some(service) = self
            .aliases
            .resolve(self.sql, host_ip, pod_name, "")
            .await?
        {
            return Ok(Some(service));
        }
        if let Some(service) = self
            .registry
            .lookup_by_ip_port(self.sql, host_ip, port)
            .await?
        {
            return Ok(Some(service));
        }
        self.registry.lookup_by_pod(self.sql, pod_name).await
    }
}

impl ApmSink {
    #[must_use]
    pub fn new(
        sql: Arc<dyn RelationalStore>,
        config: ApmSinkConfig,
        samples: Arc<RedSamples>,
    ) -> Self {
        let cache_ttl_secs = config.endpoint_cache_ttl_secs;
        let endpoints = Arc::new(EndpointRegistry::new(
            config.endpoint_retention_days,
            cache_ttl_secs,
        ));
        let aliases = Arc::new(AliasCache::new(cache_ttl_secs));
        let edges = EdgeAccumulator::new(config.edge_pending_capacity, samples.clone());
        let ebpf_edges = EbpfEdgeSink::new(sql.clone(), endpoints.clone(), aliases.clone());
        Self {
            sql,
            accumulator: TraceSummaryAccumulator::new(config.clone()),
            config,
            endpoints,
            edges,
            samples,
            aliases,
            ebpf_edges,
        }
    }

    /// eBPF 边写入器（自监控用）。
    #[must_use]
    pub fn ebpf_edges(&self) -> &EbpfEdgeSink {
        &self.ebpf_edges
    }

    /// 冷启动回载仍在写入的 trace。
    pub async fn reload(&self, now_ts: i64) -> Result<usize, DataplaneError> {
        self.accumulator.reload(self.sql.as_ref(), now_ts).await
    }

    /// 到点/超阈值时落地摘要与端点。
    pub async fn flush_due(&self, now_ts: i64) -> Result<FlushReport, DataplaneError> {
        let traces = self
            .accumulator
            .flush_due(self.sql.as_ref(), now_ts)
            .await?;
        let endpoints = if self.endpoints.dirty_len() > 0 {
            self.endpoints.flush(self.sql.as_ref()).await?
        } else {
            0
        };
        let edges = self.flush_edges(now_ts).await?;
        Ok(FlushReport {
            traces,
            endpoints,
            edges,
        })
    }

    /// 立即落地（进程退出前或测试使用）。
    pub async fn flush_now(&self, now_ts: i64) -> Result<FlushReport, DataplaneError> {
        let traces = self.accumulator.flush(self.sql.as_ref(), now_ts).await?;
        let endpoints = self.endpoints.flush(self.sql.as_ref()).await?;
        let edges = self.flush_edges(now_ts).await?;
        Ok(FlushReport {
            traces,
            endpoints,
            edges,
        })
    }

    async fn flush_edges(&self, now_ts: i64) -> Result<usize, DataplaneError> {
        // 注意：不能只看「已配对的边是否脏」——兜底边是在桶关闭时由**待配对 span** 产生的，
        // 若此处按 `dirty_len() == 0` 提前返回，没有配对成功的调用就永远不会落库。
        if self.edges.dirty_len() == 0 && self.edges.pending_len() == 0 {
            return Ok(0);
        }
        let resolver = ServiceNameResolver {
            sql: self.sql.as_ref(),
            registry: &self.endpoints,
            aliases: &self.aliases,
        };
        self.edges
            .flush(self.sql.as_ref(), now_ts, Some(&resolver))
            .await
    }

    /// 清理超过保留期的端点。
    pub async fn purge_expired(&self, now_ts: i64) -> Result<u64, DataplaneError> {
        self.endpoints
            .purge_expired(self.sql.as_ref(), now_ts)
            .await
    }

    /// 列出静态服务名映射。
    pub async fn list_aliases(
        &self,
        match_kind: Option<&str>,
        enabled: Option<bool>,
    ) -> Result<Vec<AliasRecord>, DataplaneError> {
        crate::alias::list_aliases(self.sql.as_ref(), match_kind, enabled).await
    }

    /// 新建/覆盖静态映射，并立即使反查缓存失效。
    pub async fn upsert_alias(
        &self,
        upsert: &AliasUpsert,
        now_ts: i64,
    ) -> Result<AliasRecord, DataplaneError> {
        let record = crate::alias::upsert_alias(self.sql.as_ref(), upsert, now_ts).await?;
        self.aliases.invalidate();
        Ok(record)
    }

    /// 开关静态映射，并立即使反查缓存失效。
    pub async fn set_alias_enabled(
        &self,
        alias_id: &str,
        enabled: bool,
        now_ts: i64,
    ) -> Result<bool, DataplaneError> {
        let hit =
            crate::alias::set_alias_enabled(self.sql.as_ref(), alias_id, enabled, now_ts).await?;
        if hit {
            self.aliases.invalidate();
        }
        Ok(hit)
    }

    /// 删除静态映射，并立即使反查缓存失效。
    pub async fn delete_alias(&self, alias_id: &str) -> Result<bool, DataplaneError> {
        let hit = crate::alias::delete_alias(self.sql.as_ref(), alias_id).await?;
        if hit {
            self.aliases.invalidate();
        }
        Ok(hit)
    }

    #[must_use]
    pub fn aliases(&self) -> &AliasCache {
        &self.aliases
    }

    /// 端点反查：优先 `(host_ip, listen_port)`，其次 Pod 名。
    pub async fn lookup_service(
        &self,
        host_ip: &str,
        listen_port: i64,
        pod_name: &str,
    ) -> Result<Option<String>, DataplaneError> {
        if let Some(service) = self
            .endpoints
            .lookup_by_ip_port(self.sql.as_ref(), host_ip, listen_port)
            .await?
        {
            return Ok(Some(service));
        }
        self.endpoints
            .lookup_by_pod(self.sql.as_ref(), pod_name)
            .await
    }

    #[must_use]
    pub fn config(&self) -> &ApmSinkConfig {
        &self.config
    }

    #[must_use]
    pub fn live_traces(&self) -> usize {
        self.accumulator.live_len()
    }

    #[must_use]
    pub fn dirty_traces(&self) -> usize {
        self.accumulator.dirty_len()
    }

    #[must_use]
    pub fn flushed_traces(&self) -> u64 {
        self.accumulator.flushed_traces()
    }

    #[must_use]
    pub fn upserted_endpoints(&self) -> u64 {
        self.endpoints.upserted()
    }

    #[must_use]
    pub fn samples(&self) -> &Arc<RedSamples> {
        &self.samples
    }

    #[must_use]
    pub fn edges(&self) -> &EdgeAccumulator {
        &self.edges
    }

    #[must_use]
    pub fn paired_edges(&self) -> u64 {
        self.edges.paired_edges()
    }

    #[must_use]
    pub fn pending_spans(&self) -> usize {
        self.edges.pending_len()
    }

    #[must_use]
    pub fn endpoints(&self) -> &EndpointRegistry {
        &self.endpoints
    }
}

/// eBPF 边：反查服务名 → 覆盖写 `ebpf_edges`（与 trace 摘要共用端点表与静态映射缓存）。
#[async_trait]
impl EdgeSink for ApmSink {
    async fn observe_edge(
        &self,
        edge: &EbpfEdge,
        envelope: &DataEnvelope,
    ) -> Result<(), DataplaneError> {
        self.ebpf_edges.observe_edge(edge, envelope).await
    }

    fn written_edges(&self) -> u64 {
        self.ebpf_edges.written()
    }
}

#[async_trait]
impl TraceSink for ApmSink {
    fn detail_min_duration_micros(&self) -> i64 {
        self.config.detail_min_duration_micros
    }

    async fn observe_span(
        &self,
        span: &TraceSpan,
        envelope: &DataEnvelope,
    ) -> Result<(), DataplaneError> {
        let now_ts = crate::now_micros();
        self.accumulator.observe(span, envelope);
        self.endpoints.observe(span, now_ts);
        self.edges.observe(span, envelope);
        // 服务 RED 只用 OTLP 来源（eBPF 推导的 span 不计入 apm_service_*）。
        if span.collector == "otlp" {
            self.samples.observe_span(
                crate::edge::bucket_of_public(span.timestamp),
                &span.service,
                &span.name,
                &span.kind,
                &span.status_code,
                span.duration_micros(),
            );
        }
        Ok(())
    }
}
