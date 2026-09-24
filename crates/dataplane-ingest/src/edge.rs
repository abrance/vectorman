//! `EbpfEdge`（拓扑边统一模型）的 DTO、校验与下行出口。
//!
//! 字段定义见 `/.monkeycode/specs/observability-data-model/design.md`（与 Agent 侧
//! `gse-agent-ebpf::aggregate::EbpfEdgeRecord` 逐字段对应，Agent 序列化后由这里反序列化）。
//!
//! 两条不变量在**接入**这里挡住（而不是等到查询/前端）：
//! `connections >= 1`、`failures <= connections`。字段不合法整条记 `partial`，不落库。

use std::collections::BTreeMap;

use async_trait::async_trait;
use dataplane_core::DataplaneError;
use serde::{Deserialize, Serialize};

use crate::DataEnvelope;

/// 直方图槽上限（与 `ebpf-abi::HIST_SLOTS` 一致；超出的记录整条拒绝）。
pub const MAX_HIST_SLOTS: usize = 32;

/// 一条 eBPF 边聚合（默认 10 秒桶内同一条连接）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EbpfEdge {
    /// `"{agent_id}:{bucket_start_micros}:{src_ip}:{src_port}:{dst_ip}:{dst_port}:{protocol}"`。
    pub record_id: String,
    /// 桶起点（Unix 微秒）。
    pub timestamp: i64,
    /// 桶宽（微秒），默认 10 秒。
    pub bucket_micros: i64,
    /// `tcp` | `udp`。
    pub protocol: String,
    pub src_ip: String,
    pub src_port: u16,
    pub dst_ip: String,
    pub dst_port: u16,
    #[serde(default)]
    pub src_pod: String,
    #[serde(default)]
    pub src_container_id: String,
    #[serde(default)]
    pub src_process: String,
    /// Agent 取不到时为空，dataserver 反查回填。
    #[serde(default)]
    pub src_service: String,
    #[serde(default)]
    pub dst_service: String,
    pub connections: u64,
    pub bytes_sent: u64,
    pub bytes_recv: u64,
    pub duration_micros_sum: u64,
    pub duration_micros_max: u64,
    pub tcp_retrans: u64,
    pub tcp_resets: u64,
    pub failures: u64,
    /// `refused` | `timeout` | `unreachable` | `reset` | `other` | `""`。
    #[serde(default)]
    pub failure_reason: String,
    /// log2 直方图槽：槽 `i` 覆盖 `[2^i, 2^(i+1))` 微秒。
    #[serde(default)]
    pub latency_hist: Vec<u64>,
    /// 固定 `ebpf`。
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

fn default_source() -> String {
    "ebpf".to_string()
}

impl EbpfEdge {
    /// 字段校验；返回 `Err(reason)` 时整条记录按 `partial` 拒绝。
    pub fn validate(&self) -> Result<(), String> {
        if self.record_id.trim().is_empty() {
            return Err("record_id is required".to_string());
        }
        if self.bucket_micros <= 0 {
            return Err(format!("bucket_micros must be > 0: {}", self.bucket_micros));
        }
        if self.source != "ebpf" {
            return Err(format!("source must be \"ebpf\": {}", self.source));
        }
        match self.protocol.as_str() {
            "tcp" | "udp" => {}
            other => return Err(format!("protocol must be tcp or udp: {other}")),
        }
        if self.connections == 0 {
            return Err("connections must be >= 1".to_string());
        }
        if self.failures > self.connections {
            return Err(format!(
                "failures ({}) must be <= connections ({})",
                self.failures, self.connections
            ));
        }
        if self.latency_hist.len() > MAX_HIST_SLOTS {
            return Err(format!(
                "latency_hist has {} slots, max is {MAX_HIST_SLOTS}",
                self.latency_hist.len()
            ));
        }
        if self.timestamp <= 0 {
            return Err(format!("timestamp must be > 0: {}", self.timestamp));
        }
        Ok(())
    }

    /// 反查的目标端口（`dst_port` 为 0 时用源端口兜底：监听方通常是 `dst_port`）。
    #[must_use]
    pub fn lookup_port(&self) -> i64 {
        if self.dst_port != 0 {
            i64::from(self.dst_port)
        } else {
            i64::from(self.src_port)
        }
    }
}

/// `data_type=ebpf_edges` 的派生出口（服务名反查 + 落库 + 指标）。
///
/// 与 `TraceSink` 同样的约定：实现失败**不影响**接入应答（记录已受理），
/// 只在 dataserver 日志里留一行，避免因为反查/落库抖动让 Agent 整批重试。
#[async_trait]
pub trait EdgeSink: Send + Sync {
    /// 处理一条边记录。
    async fn observe_edge(
        &self,
        edge: &EbpfEdge,
        envelope: &DataEnvelope,
    ) -> Result<(), DataplaneError>;

    /// 已写入的边记录数（自监控）。
    fn written_edges(&self) -> u64 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> EbpfEdge {
        serde_json::from_value(serde_json::json!({
            "record_id": "agent-1:1700000000000000:10.0.0.5:40000:10.0.0.9:8080:tcp",
            "timestamp": 1_700_000_000_000_000i64,
            "bucket_micros": 10_000_000i64,
            "protocol": "tcp",
            "src_ip": "10.0.0.5",
            "src_port": 40000,
            "dst_ip": "10.0.0.9",
            "dst_port": 8080,
            "src_pod": "order-api-1",
            "src_container_id": "c1",
            "src_process": "java",
            "src_service": "",
            "dst_service": "",
            "connections": 3,
            "bytes_sent": 1024,
            "bytes_recv": 2048,
            "duration_micros_sum": 900,
            "duration_micros_max": 500,
            "tcp_retrans": 1,
            "tcp_resets": 0,
            "failures": 1,
            "failure_reason": "refused",
            "latency_hist": [1, 2, 0, 0],
            "source": "ebpf",
            "labels": {"k": "v"}
        }))
        .expect("反序列化样例")
    }

    #[test]
    fn json_round_trip_keeps_all_fields() {
        let edge = sample();
        assert!(edge.validate().is_ok(), "{:?}", edge.validate());
        let text = serde_json::to_string(&edge).unwrap();
        let back: EbpfEdge = serde_json::from_str(&text).unwrap();
        assert_eq!(back, edge);
        assert_eq!(back.labels.get("k").map(String::as_str), Some("v"));
        assert_eq!(back.lookup_port(), 8080);
    }

    #[test]
    fn optional_fields_have_defaults() {
        // Agent 侧对可选字段用了 `#[serde(default)]`，这里必须一致，
        // 否则一次字段裁剪就会让整条记录变成 partial。
        let edge: EbpfEdge = serde_json::from_value(serde_json::json!({
            "record_id": "r1",
            "timestamp": 1,
            "bucket_micros": 10_000_000i64,
            "protocol": "tcp",
            "src_ip": "10.0.0.1",
            "src_port": 1,
            "dst_ip": "10.0.0.2",
            "dst_port": 2,
            "connections": 1,
            "bytes_sent": 0,
            "bytes_recv": 0,
            "duration_micros_sum": 0,
            "duration_micros_max": 0,
            "tcp_retrans": 0,
            "tcp_resets": 0,
            "failures": 0
        }))
        .unwrap();
        assert_eq!(edge.source, "ebpf", "缺省 source 视为 ebpf");
        assert!(edge.latency_hist.is_empty());
        assert!(edge.src_service.is_empty());
        assert!(edge.validate().is_ok());
    }

    #[test]
    fn validation_rules() {
        let mut edge = sample();
        edge.connections = 0;
        assert!(edge.validate().unwrap_err().contains("connections"));

        let mut edge = sample();
        edge.failures = 4; // connections = 3
        assert!(edge.validate().unwrap_err().contains("failures"));

        let mut edge = sample();
        edge.latency_hist = vec![0; MAX_HIST_SLOTS + 1];
        assert!(edge.validate().unwrap_err().contains("latency_hist"));

        let mut edge = sample();
        edge.protocol = "http".into();
        assert!(edge.validate().unwrap_err().contains("protocol"));

        let mut edge = sample();
        edge.source = "otlp".into();
        assert!(edge.validate().unwrap_err().contains("source"));

        let mut edge = sample();
        edge.record_id = "  ".into();
        assert!(edge.validate().unwrap_err().contains("record_id"));

        let mut edge = sample();
        edge.bucket_micros = 0;
        assert!(edge.validate().unwrap_err().contains("bucket_micros"));

        let mut edge = sample();
        edge.timestamp = 0;
        assert!(edge.validate().unwrap_err().contains("timestamp"));
    }

    #[test]
    fn lookup_port_falls_back_to_source() {
        let mut edge = sample();
        edge.dst_port = 0;
        edge.src_port = 9100;
        assert_eq!(edge.lookup_port(), 9100);
    }
}
