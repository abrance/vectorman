//! OTLP/HTTP trace 接收：应用直连本机 Agent，转成 `data_type=traces` 上报。
//!
//! 设计（`/.monkeycode/specs/apm-tracing/design.md`「Agent：OTLP receiver」）：
//!
//! - 阻塞式 HTTP（`tiny_http`）独立线程，不引入第二套异步 runtime；解码后用
//!   `tokio::sync::mpsc` 交给异步侧攒批并交给既有上行通道。
//! - 只做「解码 → 校验 → 转封 → 过滤」：采样由应用侧 SDK 决定，转封不改
//!   `trace_flags`；分批上报与重试复用既有缓冲。
//! - 默认监听 `0.0.0.0:4318`：应用与 Agent 通常不同容器网络命名空间，
//!   `127.0.0.1` 在 docker compose / 同节点 k8s Pod 两种形态下都不可达。
//! - token 与来源 CIDR 都是可选的「收窄」手段；两者都为空时输出一行 warn。
//! - 一个 listen 地址只允许一个接收器：重复监听会失败并输出 warn（多采集项共用
//!   同一 `otlp_listen` 属配置错误，不做隐式合并）。

use std::collections::BTreeMap;
use std::io::Read;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue};
use opentelemetry_proto::tonic::trace::v1::{span, ResourceSpans, Span};
use prost::Message;
use serde_json::Value;
use tokio::sync::mpsc;

use super::envelope::{SpanEventRecord, SpanLinkRecord, TraceSpanRecord, DATA_TYPE_TRACES};
use super::CollectShared;

/// 默认监听地址：`0.0.0.0` 是刻意的（见模块注释）。
pub const DEFAULT_LISTEN: &str = "0.0.0.0:4318";
/// 默认请求体上限（8 MiB）。
pub const DEFAULT_MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// 默认批次上限。
pub const DEFAULT_BATCH_MAX_RECORDS: usize = 100;
/// 默认上报间隔（秒）。
pub const DEFAULT_FLUSH_INTERVAL_SECS: u64 = 5;
/// 缺失 `service.name` 时的占位值（对齐 OTel 默认值）。
pub const UNKNOWN_SERVICE: &str = "unknown_service";

/// 接收器运行统计（自监控与测试）。
#[derive(Debug, Default)]
pub struct OtlpStats {
    pub requests: AtomicU64,
    /// token 校验失败。
    pub denied_auth: AtomicU64,
    /// 来源不在 `allowed_cidrs`。
    pub denied_cidr: AtomicU64,
    /// 请求体超过上限。
    pub too_large: AtomicU64,
    /// 解码失败（含非 OTLP 请求）。
    pub parse_errors: AtomicU64,
    /// 请求体 gzip 解压失败。
    pub gzip_errors: AtomicU64,
    /// 收到的 span 条数。
    pub spans_in: AtomicU64,
    /// 被名单过滤掉的 span 条数。
    pub spans_dropped: AtomicU64,
    /// 入队批次数。
    pub batches: AtomicU64,
    /// 非法 span（id 长度不符、全零等）。
    pub spans_invalid: AtomicU64,
}

impl OtlpStats {
    #[must_use]
    pub fn snapshot(&self) -> OtlpSnapshot {
        OtlpSnapshot {
            requests: self.requests.load(Ordering::Relaxed),
            denied_auth: self.denied_auth.load(Ordering::Relaxed),
            denied_cidr: self.denied_cidr.load(Ordering::Relaxed),
            too_large: self.too_large.load(Ordering::Relaxed),
            parse_errors: self.parse_errors.load(Ordering::Relaxed),
            gzip_errors: self.gzip_errors.load(Ordering::Relaxed),
            spans_in: self.spans_in.load(Ordering::Relaxed),
            spans_dropped: self.spans_dropped.load(Ordering::Relaxed),
            batches: self.batches.load(Ordering::Relaxed),
            spans_invalid: self.spans_invalid.load(Ordering::Relaxed),
        }
    }
}

/// 统计快照。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct OtlpSnapshot {
    pub requests: u64,
    pub denied_auth: u64,
    pub denied_cidr: u64,
    pub too_large: u64,
    pub parse_errors: u64,
    pub gzip_errors: u64,
    pub spans_in: u64,
    pub spans_dropped: u64,
    pub batches: u64,
    pub spans_invalid: u64,
}

/// 接收器配置：进程级（listen/token/上限）与采集项级（名单/攒批）合并后的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtlpConfig {
    pub listen: String,
    pub max_body_bytes: usize,
    pub token: String,
    /// 允许的来源网段（空表示不限）。
    pub allowed_cidrs: Vec<(IpAddr, u8)>,
    pub batch_max_records: usize,
    pub flush_interval_secs: u64,
    pub service_allowlist: Vec<String>,
    pub service_denylist: Vec<String>,
    /// span 属性白名单（空表示全量保留）。
    pub attribute_allowlist: Vec<String>,
    pub item_id: String,
}

impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            listen: DEFAULT_LISTEN.to_string(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            token: String::new(),
            allowed_cidrs: Vec::new(),
            batch_max_records: DEFAULT_BATCH_MAX_RECORDS,
            flush_interval_secs: DEFAULT_FLUSH_INTERVAL_SECS,
            service_allowlist: Vec::new(),
            service_denylist: Vec::new(),
            attribute_allowlist: Vec::new(),
            item_id: "apm-otlp".to_string(),
        }
    }
}

impl OtlpConfig {
    /// 采集项 `collector` 字段 + Agent 进程配置合并。
    #[must_use]
    pub fn from_item(
        item_id: &str,
        collector: &Value,
        listen: &str,
        max_body_bytes: usize,
        token: &str,
        allowed_cidrs: &[String],
    ) -> Self {
        let strings = |key: &str| -> Vec<String> {
            collector
                .get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let batch = collector
            .get("batch_max_records")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_BATCH_MAX_RECORDS, |v| v.clamp(1, 5_000) as usize);
        let flush = collector
            .get("flush_interval_secs")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_FLUSH_INTERVAL_SECS, |v| v.clamp(1, 60));
        Self {
            listen: listen.to_string(),
            max_body_bytes: max_body_bytes.max(1_024),
            token: token.to_string(),
            allowed_cidrs: parse_cidrs(allowed_cidrs),
            batch_max_records: batch,
            flush_interval_secs: flush,
            service_allowlist: strings("service_allowlist"),
            service_denylist: strings("service_denylist"),
            attribute_allowlist: strings("attribute_allowlist"),
            item_id: item_id.to_string(),
        }
    }
}

/// 解析 CIDR 列表；非法项被忽略并输出 warn。
#[must_use]
pub fn parse_cidrs(values: &[String]) -> Vec<(IpAddr, u8)> {
    let mut out = Vec::new();
    for raw in values {
        let Some((addr, len)) = raw.split_once('/') else {
            eprintln!("gse-agent: otlp_allowed_cidrs entry {raw:?} is not a CIDR; ignored");
            continue;
        };
        let (Ok(addr), Ok(len)) = (addr.trim().parse::<IpAddr>(), len.trim().parse::<u8>()) else {
            eprintln!("gse-agent: otlp_allowed_cidrs entry {raw:?} is not a CIDR; ignored");
            continue;
        };
        let max = if addr.is_ipv4() { 32 } else { 128 };
        if len > max {
            eprintln!("gse-agent: otlp_allowed_cidrs entry {raw:?} has prefix > /{max}; ignored");
            continue;
        }
        out.push((addr, len));
    }
    out
}

/// `ip` 是否落在 CIDR 内。
#[must_use]
pub fn cidr_contains(addr: IpAddr, prefix: IpAddr, len: u8) -> bool {
    match (addr, prefix) {
        (IpAddr::V4(addr), IpAddr::V4(prefix)) => {
            if len > 32 {
                return false;
            }
            let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
            u32::from(addr) & mask == u32::from(prefix) & mask
        }
        (IpAddr::V6(addr), IpAddr::V6(prefix)) => {
            if len > 128 {
                return false;
            }
            let mask = if len == 0 {
                0
            } else {
                u128::MAX << (128 - len)
            };
            u128::from(addr) & mask == u128::from(prefix) & mask
        }
        _ => false,
    }
}

/// 一批待上报的 span（同一采集项）。
#[derive(Debug, Clone, PartialEq)]
pub struct SpanBatch {
    pub item_id: String,
    pub records: Vec<Value>,
}

/// 请求处理结果：HTTP 状态码 + 响应体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpOutcome {
    pub status: u16,
    pub body: String,
}

impl HttpOutcome {
    fn ok() -> Self {
        Self {
            status: 200,
            body: r#"{"partialSuccess":{}}"#.to_string(),
        }
    }

    fn error(status: u16, code: &str, message: &str) -> Self {
        Self {
            status,
            body: serde_json::json!({"code": code, "error": message}).to_string(),
        }
    }
}

/// 把一次请求转成 span 记录并送入队列；HTTP 层只依赖这个函数。
///
/// 返回 `HttpOutcome`（状态码与响应体）以及可选的批次。
pub fn handle_request(
    cfg: &OtlpConfig,
    content_type: &str,
    content_encoding: &str,
    authorization: Option<&str>,
    remote: Option<IpAddr>,
    body: &[u8],
    stats: &OtlpStats,
) -> (HttpOutcome, Option<SpanBatch>) {
    stats.requests.fetch_add(1, Ordering::Relaxed);

    if !cfg.token.is_empty() {
        let expected = format!("Bearer {}", cfg.token);
        if authorization != Some(expected.as_str()) {
            stats.denied_auth.fetch_add(1, Ordering::Relaxed);
            return (
                HttpOutcome::error(401, "unauthenticated", "invalid or missing bearer token"),
                None,
            );
        }
    }
    if !cfg.allowed_cidrs.is_empty() {
        let allowed = remote.is_some_and(|addr| {
            cfg.allowed_cidrs
                .iter()
                .any(|(prefix, len)| cidr_contains(addr, *prefix, *len))
        });
        if !allowed {
            stats.denied_cidr.fetch_add(1, Ordering::Relaxed);
            return (
                HttpOutcome::error(403, "permission_denied", "source address not allowed"),
                None,
            );
        }
    }
    if body.len() > cfg.max_body_bytes {
        stats.too_large.fetch_add(1, Ordering::Relaxed);
        return (
            HttpOutcome::error(413, "invalid_argument", "request body too large"),
            None,
        );
    }

    let payload = match super::otlp::decompress(content_encoding, body) {
        Ok(payload) => payload,
        Err(reason) => {
            stats.gzip_errors.fetch_add(1, Ordering::Relaxed);
            return (HttpOutcome::error(400, "invalid_argument", &reason), None);
        }
    };
    if payload.len() > cfg.max_body_bytes {
        stats.too_large.fetch_add(1, Ordering::Relaxed);
        return (
            HttpOutcome::error(413, "invalid_argument", "decompressed body too large"),
            None,
        );
    }

    let request = match decode_request(content_type, &payload) {
        Ok(request) => request,
        Err(reason) => {
            stats.parse_errors.fetch_add(1, Ordering::Relaxed);
            return (HttpOutcome::error(400, "invalid_argument", &reason), None);
        }
    };

    let (records, dropped, invalid) = spans_from_request(&request, cfg);
    stats
        .spans_in
        .fetch_add(records.len() as u64, Ordering::Relaxed);
    stats.spans_dropped.fetch_add(dropped, Ordering::Relaxed);
    stats.spans_invalid.fetch_add(invalid, Ordering::Relaxed);
    if records.is_empty() {
        return (HttpOutcome::ok(), None);
    }
    stats.batches.fetch_add(1, Ordering::Relaxed);
    (
        HttpOutcome::ok(),
        Some(SpanBatch {
            item_id: cfg.item_id.clone(),
            records: records
                .into_iter()
                .filter_map(|record| serde_json::to_value(record).ok())
                .collect(),
        }),
    )
}

/// gzip 解压（`content_encoding` 为空或 `identity` 时原样返回）。
fn decompress(content_encoding: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    let encoding = content_encoding.trim().to_ascii_lowercase();
    if encoding.is_empty() || encoding == "identity" {
        return Ok(body.to_vec());
    }
    if encoding != "gzip" {
        return Err(format!("unsupported content-encoding: {content_encoding}"));
    }
    let mut decoder = flate2::read::GzDecoder::new(body);
    let mut out = Vec::new();
    // 解压后仍受调用方的上限校验约束，这里再留一个硬上限防止 zip bomb。
    let mut limited = decoder.by_ref().take(64 * 1024 * 1024);
    limited
        .read_to_end(&mut out)
        .map_err(|e| format!("gzip decode failed: {e}"))?;
    Ok(out)
}

/// 按 `Content-Type` 解码 OTLP 请求；JSON 与 protobuf 都返回同一类型。
pub fn decode_request(
    content_type: &str,
    body: &[u8],
) -> Result<ExportTraceServiceRequest, String> {
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("json") {
        serde_json::from_slice::<ExportTraceServiceRequest>(body)
            .map_err(|e| format!("invalid OTLP JSON: {e}"))
    } else if ct.contains("protobuf") || ct.contains("octet-stream") || ct.is_empty() {
        ExportTraceServiceRequest::decode(body).map_err(|e| format!("invalid OTLP protobuf: {e}"))
    } else {
        Err(format!("unsupported content-type: {content_type}"))
    }
}

/// OTLP 请求 → `TraceSpanRecord`；返回 `(记录, 被名单过滤数, 非法数)`。
#[must_use]
pub fn spans_from_request(
    request: &ExportTraceServiceRequest,
    cfg: &OtlpConfig,
) -> (Vec<TraceSpanRecord>, u64, u64) {
    let mut records = Vec::new();
    let mut dropped = 0u64;
    let mut invalid = 0u64;
    for ResourceSpans {
        resource,
        scope_spans,
        ..
    } in &request.resource_spans
    {
        let resource_attrs = resource
            .as_ref()
            .map(|r| attributes_to_map(&r.attributes))
            .unwrap_or_default();
        let service = resource_attrs
            .get("service.name")
            .cloned()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| UNKNOWN_SERVICE.to_string());
        if !service_allowed(&service, cfg) {
            dropped += count_spans(scope_spans);
            continue;
        }
        for scope in scope_spans {
            let (scope_name, scope_version) = scope
                .scope
                .as_ref()
                .map(|s| (s.name.clone(), s.version.clone()))
                .unwrap_or_default();
            for span in &scope.spans {
                match span_to_record(
                    span,
                    cfg,
                    &service,
                    &resource_attrs,
                    &scope_name,
                    &scope_version,
                ) {
                    Some(record) => records.push(record),
                    None => invalid += 1,
                }
            }
        }
    }
    (records, dropped, invalid)
}

fn count_spans(scope_spans: &[opentelemetry_proto::tonic::trace::v1::ScopeSpans]) -> u64 {
    scope_spans
        .iter()
        .map(|scope| scope.spans.len() as u64)
        .sum()
}

fn service_allowed(service: &str, cfg: &OtlpConfig) -> bool {
    if cfg.service_denylist.iter().any(|deny| deny == service) {
        return false;
    }
    cfg.service_allowlist.is_empty() || cfg.service_allowlist.iter().any(|allow| allow == service)
}

fn span_to_record(
    span: &Span,
    cfg: &OtlpConfig,
    service: &str,
    resource: &BTreeMap<String, String>,
    scope_name: &str,
    scope_version: &str,
) -> Option<TraceSpanRecord> {
    let trace_id = hex_lower(&span.trace_id, 32)?;
    let span_id = hex_lower(&span.span_id, 16)?;
    // 父 span：全零或长度不符视为根。
    let parent_span_id = hex_lower(&span.parent_span_id, 16).unwrap_or_default();
    let mut attributes = attributes_to_map(&span.attributes);
    if !cfg.attribute_allowlist.is_empty() {
        attributes.retain(|key, _| cfg.attribute_allowlist.iter().any(|allow| allow == key));
    }
    Some(TraceSpanRecord {
        record_id: format!("{trace_id}:{span_id}"),
        timestamp: micros_of(span.start_time_unix_nano),
        trace_id,
        span_id,
        parent_span_id,
        name: span.name.clone(),
        kind: kind_of(span.kind).to_string(),
        start_unix_nano: i64_of(span.start_time_unix_nano),
        end_unix_nano: i64_of(span.end_time_unix_nano),
        status_code: status_of(span.status.as_ref()).to_string(),
        status_message: span
            .status
            .as_ref()
            .map(|s| s.message.clone())
            .unwrap_or_default(),
        trace_flags: u8::try_from(span.flags & 0xff).unwrap_or(0),
        service: service.to_string(),
        resource: resource.clone(),
        scope_name: scope_name.to_string(),
        scope_version: scope_version.to_string(),
        attributes,
        events: span
            .events
            .iter()
            .map(|event| SpanEventRecord {
                name: event.name.clone(),
                time_unix_nano: i64_of(event.time_unix_nano),
                attributes: attributes_to_map(&event.attributes),
            })
            .collect(),
        links: span
            .links
            .iter()
            .map(|link| SpanLinkRecord {
                trace_id: hex_lower(&link.trace_id, 32).unwrap_or_default(),
                span_id: hex_lower(&link.span_id, 16).unwrap_or_default(),
                attributes: attributes_to_map(&link.attributes),
            })
            .collect(),
        dropped_attributes: span.dropped_attributes_count,
        dropped_events: span.dropped_events_count,
        dropped_links: span.dropped_links_count,
        collector: "otlp".to_string(),
        labels: BTreeMap::new(),
    })
}

/// OTLP 的时间字段是无符号纳秒；超过 `i64` 上界（约 2262 年）时按 0 处理。
fn i64_of(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(0)
}

/// 纳秒 → 微秒。
fn micros_of(nanos: u64) -> i64 {
    i64_of(nanos) / 1_000
}

/// 转小写 hex；`hex_len` 是 **hex 字符数**（trace_id 32、span_id 16）。
///
/// 长度不符或全零返回 `None`（由调用方决定是丢弃还是当作空值）。
fn hex_lower(bytes: &[u8], hex_len: usize) -> Option<String> {
    if bytes.len() * 2 != hex_len || bytes.iter().all(|b| *b == 0) {
        return None;
    }
    let mut out = String::with_capacity(hex_len);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    Some(out)
}

fn kind_of(kind: i32) -> &'static str {
    match span::SpanKind::try_from(kind).unwrap_or(span::SpanKind::Unspecified) {
        span::SpanKind::Internal => "internal",
        span::SpanKind::Server => "server",
        span::SpanKind::Client => "client",
        span::SpanKind::Producer => "producer",
        span::SpanKind::Consumer => "consumer",
        span::SpanKind::Unspecified => "internal",
    }
}

fn status_of(status: Option<&opentelemetry_proto::tonic::trace::v1::Status>) -> &'static str {
    match status.map(|s| s.code) {
        Some(code)
            if code == opentelemetry_proto::tonic::trace::v1::status::StatusCode::Error as i32 =>
        {
            "error"
        }
        Some(code)
            if code == opentelemetry_proto::tonic::trace::v1::status::StatusCode::Ok as i32 =>
        {
            "ok"
        }
        _ => "unset",
    }
}

/// AnyValue 统一字符串化：数组转 JSON，其它取字面量。
#[must_use]
pub fn any_value_to_string(value: &AnyValue) -> String {
    match &value.value {
        Some(opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s)) => s.clone(),
        Some(opentelemetry_proto::tonic::common::v1::any_value::Value::BoolValue(b)) => {
            b.to_string()
        }
        Some(opentelemetry_proto::tonic::common::v1::any_value::Value::IntValue(i)) => {
            i.to_string()
        }
        Some(opentelemetry_proto::tonic::common::v1::any_value::Value::DoubleValue(d)) => {
            d.to_string()
        }
        Some(opentelemetry_proto::tonic::common::v1::any_value::Value::BytesValue(b)) => {
            super::otlp::hex_encode(b)
        }
        Some(opentelemetry_proto::tonic::common::v1::any_value::Value::ArrayValue(array)) => {
            let items: Vec<String> = array.values.iter().map(any_value_to_string).collect();
            serde_json::to_string(&items).unwrap_or_default()
        }
        Some(opentelemetry_proto::tonic::common::v1::any_value::Value::KvlistValue(list)) => {
            let map: BTreeMap<String, String> = list
                .values
                .iter()
                .map(|kv| {
                    (
                        kv.key.clone(),
                        kv.value
                            .as_ref()
                            .map(any_value_to_string)
                            .unwrap_or_default(),
                    )
                })
                .collect();
            serde_json::to_string(&map).unwrap_or_default()
        }
        // 0.33 起 AnyValue 还带一个内部用的字符串索引变体，按字符串处理。
        Some(other) => format!("{other:?}"),
        None => String::new(),
    }
}

fn attributes_to_map(attributes: &[KeyValue]) -> BTreeMap<String, String> {
    attributes
        .iter()
        .map(|kv| {
            (
                kv.key.clone(),
                kv.value
                    .as_ref()
                    .map(any_value_to_string)
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// 字节转 hex（小写）。
#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 已启动的接收器：持有服务端与线程，`Drop` 时停止。
pub struct OtlpReceiver {
    server: Arc<tiny_http::Server>,
    stop: Arc<AtomicBool>,
    stats: Arc<OtlpStats>,
    rx: mpsc::UnboundedReceiver<SpanBatch>,
    #[allow(dead_code)]
    thread: Option<std::thread::JoinHandle<()>>,
}

impl OtlpReceiver {
    /// 绑定端口并启动 HTTP 线程；返回接收端（批次由调用方消费）。
    pub fn start(cfg: OtlpConfig) -> Result<Self, String> {
        let server = Arc::new(
            tiny_http::Server::http(&cfg.listen)
                .map_err(|e| format!("otlp listen {} failed: {e}", cfg.listen))?,
        );
        if cfg.token.is_empty() && cfg.allowed_cidrs.is_empty() {
            eprintln!(
                "gse-agent: otlp receiver on {} accepts any source without token; set otlp_token or otlp_allowed_cidrs in production",
                cfg.listen
            );
        }
        let stats = Arc::new(OtlpStats::default());
        let (tx, rx) = mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let server = Arc::clone(&server);
            let stats = Arc::clone(&stats);
            let stop = Arc::clone(&stop);
            let cfg = cfg.clone();
            std::thread::Builder::new()
                .name("otlp-receiver".to_string())
                .spawn(move || serve(server, stats, stop, cfg, tx))
                .map_err(|e| format!("spawn otlp receiver thread failed: {e}"))?
        };
        Ok(Self {
            server,
            stop,
            stats,
            rx,
            thread: Some(thread),
        })
    }

    #[must_use]
    pub fn stats(&self) -> Arc<OtlpStats> {
        Arc::clone(&self.stats)
    }

    /// 实际监听地址（端口为 0 时由系统分配）。
    #[must_use]
    pub fn addr(&self) -> String {
        self.server.server_addr().to_string()
    }

    /// 取一批已解码的 span（测试与 `run` 使用）。
    pub async fn recv(&mut self) -> Option<SpanBatch> {
        self.rx.recv().await
    }
}

impl Drop for OtlpReceiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // `incoming_requests` 阻塞在 accept 上，需要主动唤醒。
        self.server.unblock();
    }
}

/// 阻塞式 HTTP 循环：解析请求 → 转封 → 送入通道。
fn serve(
    server: Arc<tiny_http::Server>,
    stats: Arc<OtlpStats>,
    stop: Arc<AtomicBool>,
    cfg: OtlpConfig,
    tx: mpsc::UnboundedSender<SpanBatch>,
) {
    for mut request in server.incoming_requests() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let method = request.method().as_str().to_string();
        let url = request.url().to_string();
        let content_type = header(&request, "Content-Type").unwrap_or_default();
        let content_encoding = header(&request, "Content-Encoding").unwrap_or_default();
        let authorization = header(&request, "Authorization");
        let remote = request
            .remote_addr()
            .map(|addr| addr.ip())
            .filter(|ip| !ip.is_unspecified());
        // 非 POST 或路径不符：返回 404，不计入 OTLP 统计（计数在 `handle_request` 内）
        if method != "POST" || url != "/v1/traces" {
            let outcome = HttpOutcome::error(404, "not_found", "only POST /v1/traces is supported");
            let response =
                tiny_http::Response::from_string(outcome.body).with_status_code(outcome.status);
            let _ = request.respond(response);
            continue;
        }

        // 先按上限读体，再交给纯函数处理（便于单测覆盖）。
        let limit = cfg.max_body_bytes + 1;
        let mut body = Vec::new();
        let read = request
            .as_reader()
            .take(u64::try_from(limit).unwrap_or(u64::MAX))
            .read_to_end(&mut body);
        if let Err(e) = read {
            stats.parse_errors.fetch_add(1, Ordering::Relaxed);
            let outcome = HttpOutcome::error(400, "invalid_argument", &format!("read body: {e}"));
            let response =
                tiny_http::Response::from_string(outcome.body).with_status_code(outcome.status);
            let _ = request.respond(response);
            continue;
        }

        let (outcome, batch) = handle_request(
            &cfg,
            &content_type,
            &content_encoding,
            authorization.as_deref(),
            remote,
            &body,
            &stats,
        );
        if let Some(batch) = batch {
            if tx.send(batch).is_err() {
                return;
            }
        }
        let response =
            tiny_http::Response::from_string(outcome.body).with_status_code(outcome.status);
        let _ = request.respond(response);
    }
}

fn header(request: &tiny_http::Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

/// 采集项运行时入口：启动接收器并消费批次，交给既有上行通道。
///
/// 结束条件是任务被 abort（采集项被删除/停用）：`OtlpReceiver` 的 `Drop` 会解除
/// HTTP 线程阻塞并让其退出。
pub async fn run(shared: Arc<CollectShared>, cfg: OtlpConfig) {
    let mut receiver = match OtlpReceiver::start(cfg) {
        Ok(receiver) => receiver,
        Err(reason) => {
            eprintln!("gse-agent: {reason}");
            return;
        }
    };
    while let Some(batch) = receiver.recv().await {
        if batch.records.is_empty() {
            continue;
        }
        shared
            .push(DATA_TYPE_TRACES, &batch.item_id, batch.records)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::{any_value, ArrayValue};
    use opentelemetry_proto::tonic::resource::v1::Resource;
    use opentelemetry_proto::tonic::trace::v1::{ScopeSpans, Status};

    fn kv(key: &str, value: any_value::Value) -> KeyValue {
        KeyValue {
            key: key.to_string(),
            value: Some(AnyValue { value: Some(value) }),
            ..Default::default()
        }
    }

    fn sample_request() -> ExportTraceServiceRequest {
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![
                        kv(
                            "service.name",
                            any_value::Value::StringValue("order-api".into()),
                        ),
                        kv(
                            "k8s.pod.name",
                            any_value::Value::StringValue("order-api-1".into()),
                        ),
                        kv(
                            "service.instance.id",
                            any_value::Value::StringValue("order-api-1".into()),
                        ),
                    ],
                    ..Default::default()
                }),
                scope_spans: vec![ScopeSpans {
                    scope: Some(
                        opentelemetry_proto::tonic::common::v1::InstrumentationScope {
                            name: "test-scope".into(),
                            version: "1.2.3".into(),
                            ..Default::default()
                        },
                    ),
                    spans: vec![
                        Span {
                            trace_id: vec![0x4b; 16],
                            span_id: vec![0x01; 8],
                            parent_span_id: Vec::new(),
                            name: "GET /orders".into(),
                            kind: span::SpanKind::Server as i32,
                            start_time_unix_nano: 1_710_000_000_000_000_000,
                            end_time_unix_nano: 1_710_000_000_012_000_000,
                            status: Some(Status {
                                code: opentelemetry_proto::tonic::trace::v1::status::StatusCode::Ok
                                    as i32,
                                message: String::new(),
                            }),
                            attributes: vec![
                                kv(
                                    "http.request.method",
                                    any_value::Value::StringValue("GET".into()),
                                ),
                                kv(
                                    "nest",
                                    any_value::Value::ArrayValue(ArrayValue {
                                        values: vec![
                                            AnyValue {
                                                value: Some(any_value::Value::IntValue(1)),
                                            },
                                            AnyValue {
                                                value: Some(any_value::Value::BoolValue(true)),
                                            },
                                        ],
                                    }),
                                ),
                            ],
                            events: vec![span::Event {
                                name: "exception".into(),
                                time_unix_nano: 1_710_000_000_005_000_000,
                                attributes: vec![kv(
                                    "exception.type",
                                    any_value::Value::StringValue("Timeout".into()),
                                )],
                                ..Default::default()
                            }],
                            links: vec![span::Link {
                                trace_id: vec![0x11; 16],
                                span_id: vec![0x22; 8],
                                ..Default::default()
                            }],
                            dropped_events_count: 2,
                            flags: 1,
                            ..Default::default()
                        },
                        Span {
                            trace_id: vec![0x4b; 16],
                            span_id: vec![0x02; 8],
                            parent_span_id: vec![0x01; 8],
                            name: "INSERT orders".into(),
                            kind: span::SpanKind::Client as i32,
                            start_time_unix_nano: 1_710_000_000_001_000_000,
                            end_time_unix_nano: 1_710_000_000_010_000_000,
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
    }

    #[test]
    fn maps_otlp_span_to_record() {
        let cfg = OtlpConfig::default();
        let (records, dropped, invalid) = spans_from_request(&sample_request(), &cfg);
        assert_eq!(dropped, 0);
        assert_eq!(invalid, 0);
        assert_eq!(records.len(), 2);

        let root = &records[0];
        assert_eq!(root.trace_id, "4b".repeat(16));
        assert_eq!(root.span_id, "01".repeat(8));
        assert_eq!(root.parent_span_id, "", "全零父 span 视为根");
        assert_eq!(
            root.record_id,
            format!("{}:{}", "4b".repeat(16), "01".repeat(8))
        );
        assert_eq!(
            root.timestamp, 1_710_000_000_000_000,
            "timestamp = start/1000"
        );
        assert_eq!(root.name, "GET /orders");
        assert_eq!(root.kind, "server");
        assert_eq!(root.status_code, "ok");
        assert_eq!(root.service, "order-api");
        assert_eq!(root.resource.get("k8s.pod.name").unwrap(), "order-api-1");
        assert_eq!(root.scope_name, "test-scope");
        assert_eq!(root.scope_version, "1.2.3");
        assert_eq!(root.attributes.get("http.request.method").unwrap(), "GET");
        assert_eq!(
            root.attributes.get("nest").unwrap(),
            r#"["1","true"]"#,
            "数组属性转 JSON 字符串"
        );
        assert_eq!(root.events.len(), 1);
        assert_eq!(root.events[0].name, "exception");
        assert_eq!(
            root.events[0].attributes.get("exception.type").unwrap(),
            "Timeout"
        );
        assert_eq!(root.links.len(), 1);
        assert_eq!(root.links[0].trace_id, "11".repeat(16));
        assert_eq!(root.dropped_events, 2);
        assert_eq!(root.trace_flags, 1);
        assert_eq!(root.collector, "otlp");

        assert_eq!(records[1].kind, "client");
        assert_eq!(records[1].status_code, "unset");
    }

    #[test]
    fn service_lists_and_attribute_allowlist_filter() {
        let mut cfg = OtlpConfig {
            service_denylist: vec!["order-api".into()],
            ..OtlpConfig::default()
        };
        let (records, dropped, _) = spans_from_request(&sample_request(), &cfg);
        assert!(records.is_empty());
        assert_eq!(dropped, 2, "denylist 命中的整个 resource 都丢弃");

        cfg.service_denylist.clear();
        cfg.service_allowlist = vec!["other".into()];
        let (records, dropped, _) = spans_from_request(&sample_request(), &cfg);
        assert!(records.is_empty());
        assert_eq!(dropped, 2);

        cfg.service_allowlist.clear();
        cfg.attribute_allowlist = vec!["http.request.method".into()];
        let (records, _, _) = spans_from_request(&sample_request(), &cfg);
        assert_eq!(records[0].attributes.len(), 1, "白名单外的属性被裁剪");
        assert!(records[0].attributes.contains_key("http.request.method"));
    }

    #[test]
    fn rejects_invalid_ids() {
        let mut request = sample_request();
        request.resource_spans[0].scope_spans[0].spans[0].trace_id = vec![0x4b; 8];
        request.resource_spans[0].scope_spans[0].spans[0].span_id = vec![0; 8];
        let (records, _, invalid) = spans_from_request(&request, &OtlpConfig::default());
        assert_eq!(records.len(), 1, "非法 span 被丢弃，其余保留");
        assert_eq!(invalid, 1);
    }

    #[test]
    fn decodes_json_and_protobuf() {
        let request = sample_request();
        let bytes = request.encode_to_vec();
        let decoded = decode_request("application/x-protobuf", &bytes).unwrap();
        assert_eq!(decoded, request);

        let json = serde_json::to_vec(&request).unwrap();
        let decoded = decode_request("application/json", &json).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn json_accepts_camel_case_otlp_payload() {
        // 真实 OTLP/JSON 用 camelCase 键名（`resourceSpans`/`traceId`/`startTimeUnixNano`）。
        let payload = serde_json::json!({
            "resourceSpans": [{
                "resource": {"attributes": [
                    {"key": "service.name", "value": {"stringValue": "order-api"}}
                ]},
                "scopeSpans": [{
                    "scope": {"name": "sdk", "version": "1.0"},
                    "spans": [{
                        "traceId": "4bf92f3577b34da6a3ce929d0e0e4736",
                        "spanId": "00f067aa0ba902b7",
                        "parentSpanId": "",
                        "name": "GET /orders",
                        "kind": 2,
                        "startTimeUnixNano": "1710000000000000000",
                        "endTimeUnixNano": "1710000000012000000",
                        "status": {"code": 2}
                    }]
                }]
            }]
        });
        let decoded = decode_request("application/json", payload.to_string().as_bytes()).unwrap();
        let (records, _, invalid) = spans_from_request(&decoded, &OtlpConfig::default());
        assert_eq!(invalid, 0);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(records[0].kind, "server");
        assert_eq!(records[0].status_code, "error");
        assert_eq!(records[0].timestamp, 1_710_000_000_000_000);
    }

    #[test]
    fn gzip_body_decodes() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let bytes = sample_request().encode_to_vec();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&bytes).unwrap();
        let gzipped = encoder.finish().unwrap();
        assert_ne!(gzipped, bytes);
        let restored = decompress("gzip", &gzipped).unwrap();
        let decoded = decode_request("application/x-protobuf", &restored).unwrap();
        assert_eq!(decoded, sample_request());
    }

    #[test]
    fn handles_auth_cidr_limits_and_bad_bodies() {
        let stats = OtlpStats::default();
        let cfg = OtlpConfig {
            token: "s3cret".into(),
            allowed_cidrs: parse_cidrs(&["10.0.0.0/8".to_string()]),
            max_body_bytes: 64,
            ..OtlpConfig::default()
        };

        // 缺 token → 401
        let (outcome, batch) =
            handle_request(&cfg, "application/json", "", None, None, b"{}", &stats);
        assert_eq!(outcome.status, 401);
        assert!(batch.is_none());
        assert_eq!(stats.snapshot().denied_auth, 1);

        // token 对但来源不允许 → 403
        let (outcome, _) = handle_request(
            &cfg,
            "application/json",
            "",
            Some("Bearer s3cret"),
            Some("192.168.1.1".parse().unwrap()),
            b"{}",
            &stats,
        );
        assert_eq!(outcome.status, 403);
        assert_eq!(stats.snapshot().denied_cidr, 1);

        // 来源允许但 body 超限 → 413
        let big = vec![0u8; 128];
        let (outcome, _) = handle_request(
            &cfg,
            "application/json",
            "",
            Some("Bearer s3cret"),
            Some("10.1.2.3".parse().unwrap()),
            &big,
            &stats,
        );
        assert_eq!(outcome.status, 413);
        assert_eq!(stats.snapshot().too_large, 1);

        // 非法体 → 400
        let (outcome, _) = handle_request(
            &OtlpConfig::default(),
            "application/json",
            "",
            None,
            None,
            b"not-json",
            &stats,
        );
        assert_eq!(outcome.status, 400);
        assert_eq!(stats.snapshot().parse_errors, 1);

        // 不支持的编码 → 400
        let (outcome, _) = handle_request(
            &OtlpConfig::default(),
            "application/json",
            "br",
            None,
            None,
            b"{}",
            &stats,
        );
        assert_eq!(outcome.status, 400);
        assert_eq!(stats.snapshot().gzip_errors, 1);
    }

    #[tokio::test]
    async fn receiver_serves_http_batches_and_stops_on_drop() {
        let cfg = OtlpConfig {
            listen: "127.0.0.1:0".to_string(),
            ..OtlpConfig::default()
        };
        let mut receiver = OtlpReceiver::start(cfg).unwrap();
        let addr = receiver.addr();
        let body = sample_request().encode_to_vec();

        // protobuf 请求
        let response = ureq::post(&format!("http://{addr}/v1/traces"))
            .set("Content-Type", "application/x-protobuf")
            .send_bytes(&body)
            .unwrap();
        assert_eq!(response.status(), 200);
        let batch = receiver.recv().await.expect("应收到一批 span");
        assert_eq!(batch.item_id, "apm-otlp");
        assert_eq!(batch.records.len(), 2, "同一请求内的 span 一起入队");

        // 非 POST /v1/traces → 404
        let error = ureq::get(&format!("http://{addr}/v1/traces"))
            .call()
            .unwrap_err();
        assert!(matches!(error, ureq::Error::Status(404, _)));
        assert_eq!(
            receiver.stats().snapshot().requests,
            1,
            "404 不计入 OTLP 统计"
        );

        // 同地址第二个接收器必须失败（多采集项共用同一监听属配置错误）
        let again = OtlpReceiver::start(OtlpConfig {
            listen: addr.clone(),
            ..OtlpConfig::default()
        });
        assert!(again.is_err(), "重复监听同一地址应失败: {:?}", again.err());

        // Drop 应当让 HTTP 线程退出（否则任务 abort 后端口不释放）
        drop(receiver);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let restarted = OtlpReceiver::start(OtlpConfig {
            listen: addr,
            ..OtlpConfig::default()
        })
        .expect("drop 后端口应可重新绑定");
        drop(restarted);
    }

    #[test]
    fn cidr_matching_covers_families_and_edges() {
        assert!(cidr_contains(
            "10.1.2.3".parse().unwrap(),
            "10.0.0.0".parse().unwrap(),
            8
        ));
        assert!(!cidr_contains(
            "11.1.2.3".parse().unwrap(),
            "10.0.0.0".parse().unwrap(),
            8
        ));
        assert!(cidr_contains(
            "1.2.3.4".parse().unwrap(),
            "0.0.0.0".parse().unwrap(),
            0
        ));
        assert!(cidr_contains(
            "2001:db8::1".parse().unwrap(),
            "2001:db8::".parse().unwrap(),
            32
        ));
        assert!(!cidr_contains(
            "2001:db9::1".parse().unwrap(),
            "2001:db8::".parse().unwrap(),
            32
        ));
        assert!(!cidr_contains(
            "2001:db8::1".parse().unwrap(),
            "10.0.0.0".parse().unwrap(),
            8
        ));
        assert_eq!(
            parse_cidrs(&["bogus".into(), "10.0.0.0/33".into()]).len(),
            0
        );
        assert_eq!(parse_cidrs(&["10.0.0.0/8".into()]).len(), 1);
    }
}
