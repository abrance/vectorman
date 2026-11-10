//! 资源生成：造出**产品代码真能接受**的 metrics / traces / eBPF 边 / eBPF 事件 / logs 记录。
//!
//! 生成器只拼 `serde_json::Value`，不依赖 `dataplane-ingest` 的 Rust 类型——测试要断言的是
//! 线上 HTTP 契约（网关解析的就是 JSON），用产品侧类型构造反而会把契约漂移一起绿掉。
//!
//! 每个生成器都带 `record_id` 参数或前缀，调用方负责加 `util::unique_prefix()`，
//! 这样同一台机器并行跑、或 CI 复用同一个 VictoriaMetrics 时不会互相踩。

use serde_json::{json, Value};

/// 通用信封：`POST /v1/ingest` 的请求体。
pub fn envelope(
    data_type: &str,
    batch_id: &str,
    data_id: &str,
    agent_id: &str,
    host_id: &str,
    records: Vec<Value>,
) -> String {
    json!({
        "batch_id": batch_id,
        "data_type": data_type,
        "data_id": data_id,
        "agent_id": agent_id,
        "host_id": host_id,
        "sent_at_micros": crate::util::now_micros(),
        "records": records,
    })
    .to_string()
}

// ---------------------------------------------------------------- metrics

pub fn metric_record(
    record_id: &str,
    measurement: &str,
    tags: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    field_name: &str,
    field_value: f64,
    timestamp: i64,
) -> Value {
    let tags: serde_json::Map<String, Value> = tags
        .into_iter()
        .map(|(k, v)| (k.into(), Value::String(v.into())))
        .collect();
    json!({
        "record_id": record_id,
        "timestamp": timestamp,
        "measurement": measurement,
        "tags": tags,
        "field_name": field_name,
        "field_value": field_value,
    })
}

/// 一条 `metrics` 记录的信封。
pub fn metrics_envelope(
    batch_id: &str,
    data_id: &str,
    agent_id: &str,
    host_id: &str,
    records: Vec<Value>,
) -> String {
    envelope("metrics", batch_id, data_id, agent_id, host_id, records)
}

/// 单条记录的信封（最常用）。
pub fn one_metric_envelope(
    record_id: &str,
    measurement: &str,
    tags: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    value: f64,
    timestamp: i64,
) -> String {
    metrics_envelope(
        &format!("batch-{record_id}"),
        "tp-item",
        "tp-agent",
        "tp-host",
        vec![metric_record(
            record_id,
            measurement,
            tags,
            "value",
            value,
            timestamp,
        )],
    )
}

/// `count` 条唯一记录，第 `i` 条的值为 `first + i as f64`，时间戳按 `step_micros` 递增。
pub fn metric_series(
    prefix: &str,
    measurement: &str,
    count: usize,
    first: f64,
    timestamp: i64,
    step_micros: i64,
) -> Vec<Value> {
    (0..count)
        .map(|i| {
            metric_record(
                &format!("{prefix}-{i}"),
                measurement,
                [("series", prefix), ("idx", &i.to_string())],
                "value",
                first + i as f64,
                timestamp + i as i64 * step_micros,
            )
        })
        .collect()
}

// ---------------------------------------------------------------- traces

/// 32 位十六进制 id（trace_id）或 16 位（span_id）。
pub fn hex_id(n: u64, width: usize) -> String {
    format!("{n:0width$x}", n = n)
}

/// 一条 OTLP span。`timestamp` 必须等于 `start_unix_nano / 1000`（产品侧 normalize 的口径）。
#[allow(clippy::too_many_arguments)]
pub fn trace_span(
    trace_seq: u64,
    span_seq: u64,
    service: &str,
    operation: &str,
    kind: &str,
    status_code: &str,
    start_micros: i64,
    duration_micros: i64,
) -> Value {
    let trace_id = hex_id(trace_seq, 32);
    let span_id = hex_id(span_seq, 16);
    json!({
        "record_id": format!("{trace_id}:{span_id}"),
        "timestamp": start_micros,
        "trace_id": trace_id,
        "span_id": span_id,
        "parent_span_id": "",
        "name": operation,
        "kind": kind,
        "start_unix_nano": start_micros * 1_000,
        "end_unix_nano": (start_micros + duration_micros) * 1_000,
        "status_code": status_code,
        "status_message": "",
        "trace_flags": 1,
        "service": service,
        "resource": {},
        "scope_name": "tp",
        "scope_version": "1",
        "attributes": {},
        "events": [],
        "links": [],
        "dropped_attributes": 0,
        "dropped_events": 0,
        "collector": "otlp",
    })
}

pub fn traces_envelope(
    batch_id: &str,
    data_id: &str,
    agent_id: &str,
    host_id: &str,
    spans: Vec<Value>,
) -> String {
    envelope("traces", batch_id, data_id, agent_id, host_id, spans)
}

// ---------------------------------------------------------------- eBPF 边

/// 一条 `ebpf_edges` 聚合记录。
///
/// `bucket_start_micros` 是 10 秒桶起点；`connections >= 1`、`failures <= connections`、`source` 固定
/// `ebpf`，这三条是接入侧的硬校验。`latency_hist[i]` 覆盖 `[2^i, 2^(i+1))` 微秒。
#[allow(clippy::too_many_arguments)]
pub fn ebpf_edge(
    agent_id: &str,
    bucket_start_micros: i64,
    src: (&str, u16, &str),
    dst: (&str, u16, &str),
    protocol: &str,
    connections: u64,
    bytes_sent: u64,
    bytes_recv: u64,
    failures: u64,
    failure_reason: &str,
    duration_sum_micros: u64,
    duration_max_micros: u64,
    latency_hist: Vec<u64>,
) -> Value {
    let (src_ip, src_port, src_service) = src;
    let (dst_ip, dst_port, dst_service) = dst;
    json!({
        "record_id": format!(
            "{agent_id}:{bucket_start_micros}:{src_ip}:{src_port}:{dst_ip}:{dst_port}:{protocol}"
        ),
        "timestamp": bucket_start_micros,
        "bucket_micros": 10_000_000,
        "protocol": protocol,
        "src_ip": src_ip,
        "src_port": src_port,
        "dst_ip": dst_ip,
        "dst_port": dst_port,
        "src_pod": "",
        "src_container_id": "",
        "src_process": "",
        "src_service": src_service,
        "dst_service": dst_service,
        "connections": connections,
        "bytes_sent": bytes_sent,
        "bytes_recv": bytes_recv,
        "duration_micros_sum": duration_sum_micros,
        "duration_micros_max": duration_max_micros,
        "tcp_retrans": 0,
        "tcp_resets": 0,
        "failures": failures,
        "failure_reason": failure_reason,
        "latency_hist": latency_hist,
        "source": "ebpf",
    })
}

pub fn ebpf_edges_envelope(
    batch_id: &str,
    data_id: &str,
    agent_id: &str,
    host_id: &str,
    edges: Vec<Value>,
) -> String {
    envelope("ebpf_edges", batch_id, data_id, agent_id, host_id, edges)
}

// ---------------------------------------------------------------- eBPF 原始事件

/// 一条 `ebpf` 原始事件：只进日志引擎，**不进时序库**。
pub fn ebpf_event(
    record_id: &str,
    timestamp: i64,
    event_type: &str,
    pid: i64,
    process_name: &str,
    message: &str,
) -> Value {
    json!({
        "record_id": record_id,
        "timestamp": timestamp,
        "event_type": event_type,
        "pid": pid,
        "process_name": process_name,
        "message": message,
        "labels": {"collector": "ebpf"},
    })
}

pub fn ebpf_events_envelope(
    batch_id: &str,
    data_id: &str,
    agent_id: &str,
    host_id: &str,
    events: Vec<Value>,
) -> String {
    envelope("ebpf", batch_id, data_id, agent_id, host_id, events)
}

// ---------------------------------------------------------------- logs

pub fn log_record(record_id: &str, timestamp: i64, level: &str, message: &str) -> Value {
    json!({
        "record_id": record_id,
        "timestamp": timestamp,
        "level": level,
        "message": message,
        "source": "tp",
        "labels": {},
    })
}

pub fn logs_envelope(
    batch_id: &str,
    data_id: &str,
    agent_id: &str,
    host_id: &str,
    records: Vec<Value>,
) -> String {
    envelope("logs", batch_id, data_id, agent_id, host_id, records)
}

/// 带自定义标签的日志记录。
///
/// `log_record` 的 `labels` 固定为空；检索契约里的 `trace_id` / 任意 `labels` 过滤需要记录自带标签
/// （`trace_id` 会被 `LogStore` 提升为索引字段，其余键走 `labels_json` post-filter）。
/// 信封公共标签（`data_type` / `agent_id` / `data_id` / `host_id`）由接入侧另行补全，
/// 因此这里只放记录级标签。
pub fn log_record_with_labels(
    record_id: &str,
    timestamp: i64,
    level: &str,
    message: &str,
    labels: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
) -> Value {
    let labels: serde_json::Map<String, Value> = labels
        .into_iter()
        .map(|(k, v)| (k.into(), Value::String(v.into())))
        .collect();
    json!({
        "record_id": record_id,
        "timestamp": timestamp,
        "level": level,
        "message": message,
        "source": "tp",
        "labels": labels,
    })
}
