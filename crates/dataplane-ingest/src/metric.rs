//! `metrics` 记录的派生出口。

use std::collections::BTreeMap;

use async_trait::async_trait;

/// 指标点的派生出口：给时序点**补维度**。
///
/// 与 [`crate::trace::TraceSink`] / [`crate::EdgeSink`] 的差别：
///
/// - 不改写数据、不做去重，只按需追加标签；
/// - **不返回错误**：补维度是尽力而为，失败绝不能把指标本身丢掉（明细/聚合口径不能因为
///   一个名称映射查不到就少一条序列）。
///
/// 目前唯一的用途是把 `ebpf_process_*` 的 `process_name` 归一成 `service` —— 静态映射
/// （`apm_service_alias`）是**服务端配置**，Agent 无从得知，而需求要求这两个维度都在。
#[async_trait]
pub trait MetricSink: Send + Sync {
    /// 返回需要补充的标签；返回空表示该测量项不需要补。
    ///
    /// `tags` 是**已合并信封公共标签之后的最终标签集**，因此 sink 能直接看到 `agent_id`、
    /// `process_name` 等。sink 自己决定哪些 `measurement` 需要处理。
    async fn metric_tags(
        &self,
        measurement: &str,
        tags: &BTreeMap<String, String>,
    ) -> Vec<(String, String)>;
}
