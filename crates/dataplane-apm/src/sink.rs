//! 组合 sink：把 `dataplane-ingest` 的 [`TraceSink`] 钩子接到摘要累加器与端点表。
//!
//! dataserver 在接入 `data_type=traces` 时传入本对象；`observe_span` 只做内存操作
//! （不 await 存储），因此不会拖慢接入路径。落库由 dataserver 的后台任务调用
//! [`ApmSink::flush_due`] 与 [`ApmSink::purge_expired`]。

use std::sync::Arc;

use async_trait::async_trait;
use dataplane_core::DataplaneError;
use dataplane_ingest::trace::{TraceSink, TraceSpan};
use dataplane_ingest::DataEnvelope;
use dataplane_sql::RelationalStore;

use crate::accumulator::{ApmSinkConfig, TraceSummaryAccumulator};
use crate::edge::{EdgeAccumulator, ServiceResolver};
use crate::endpoint::EndpointRegistry;

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
    endpoints: EndpointRegistry,
    edges: EdgeAccumulator,
}

/// 把端点表适配成边目标归一用的解析器（需要同时拿到 sql 与 registry）。
struct EndpointResolver<'a> {
    sql: &'a dyn RelationalStore,
    registry: &'a EndpointRegistry,
}

#[async_trait]
impl ServiceResolver for EndpointResolver<'_> {
    async fn resolve(
        &self,
        host_ip: &str,
        port: i64,
        pod_name: &str,
    ) -> Result<Option<String>, DataplaneError> {
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
    pub fn new(sql: Arc<dyn RelationalStore>, config: ApmSinkConfig) -> Self {
        let endpoints = EndpointRegistry::new(
            config.endpoint_retention_days,
            config.endpoint_cache_ttl_secs,
        );
        let edges = EdgeAccumulator::new(config.edge_pending_capacity);
        Self {
            sql,
            accumulator: TraceSummaryAccumulator::new(config.clone()),
            config,
            endpoints,
            edges,
        }
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
        if self.edges.dirty_len() == 0 {
            return Ok(0);
        }
        let resolver = EndpointResolver {
            sql: self.sql.as_ref(),
            registry: &self.endpoints,
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

#[async_trait]
impl TraceSink for ApmSink {
    async fn observe_span(
        &self,
        span: &TraceSpan,
        envelope: &DataEnvelope,
    ) -> Result<(), DataplaneError> {
        let now_ts = crate::now_micros();
        self.accumulator.observe(span, envelope);
        self.endpoints.observe(span, now_ts);
        self.edges.observe(span, envelope);
        Ok(())
    }
}
