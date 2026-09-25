//! 从 `ebpf_edges` 派生 eBPF 边指标（`ebpf_*` 与 `apm_edge_*{source=ebpf}`）。
//!
//! 为什么放在 dataserver 而不是 Agent（实现期口径修正）：
//!
//! - 这些指标的维度里有 `src_service` / `dst_service`，只有 dataserver 能做服务名反查；
//! - Agent 侧如果也发同名点，会因为缺服务维度形成**第二套序列**，前端无法合并。
//!
//! 因此 Agent 只发能力状态、进程指标与原始事件；边指标全部在这里按分钟派生。
//!
//! ## 幂等与游标
//!
//! 游标 `ebpf_metrics_watermark`（`obs_schema_meta`）记录**已处理到的分钟桶上界**。每轮：
//!
//! 1. 只处理 `watermark < bucket_start <= floor(now - lag)` 的行（`lag` 缺省 60 秒，
//!    给 Agent 的 10 秒桶 + 上报延迟留出迟到余量，避免漏掉迟到的边）；
//! 2. 写完指标点后推进游标。
//!
//! 若在「写完点」与「推进游标」之间崩溃，下一轮会重写同一批点：`TimeSeriesStore` 对
//! 相同 measurement+labels+timestamp 是覆盖语义（`ebpf_metrics::tests::rewriting_same_point_is_idempotent`
//! 锁住这个前提），因此重放不会翻倍。

use std::collections::BTreeMap;
use std::sync::Arc;

use dataplane_core::{DataplaneError, ErrorCode, SqlValue};
use dataplane_sql::RelationalStore;
use dataplane_ts::{TimeSeriesStore, TsPoint};

use crate::tables;

/// 聚合滞后：边记录到达可能晚于它所属桶（Agent 每 `flush_interval_secs` 读一次快照并上报）。
pub const DEFAULT_LAG_SECS: i64 = 60;

/// 一分钟（微秒）。
const MINUTE_MICROS: i64 = 60_000_000;

/// 游标键。
pub const WATERMARK_KEY: &str = "ebpf_metrics_watermark";

/// 一次聚合的结果（自监控用）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EbpfAggReport {
    /// 处理的分钟桶。
    pub buckets: Vec<i64>,
    /// 读入的边记录数。
    pub rows: usize,
    /// 写出的指标点数。
    pub points: usize,
}

/// 从 `ebpf_edges` 派生指标的聚合器。
pub struct EbpfMetricsAggregator {
    sql: Arc<dyn RelationalStore>,
    ts: Arc<dyn TimeSeriesStore>,
    lag_secs: i64,
}

impl EbpfMetricsAggregator {
    #[must_use]
    pub fn new(sql: Arc<dyn RelationalStore>, ts: Arc<dyn TimeSeriesStore>, lag_secs: i64) -> Self {
        Self {
            sql,
            ts,
            lag_secs: lag_secs.max(0),
        }
    }

    /// 处理一轮；没有新桶时返回空报告。
    pub async fn run_once(&self, now_ts: i64) -> Result<EbpfAggReport, DataplaneError> {
        let upper = floor_minute(now_ts - self.lag_secs * 1_000_000);
        let watermark = read_watermark(self.sql.as_ref()).await?;
        let Some(watermark) = watermark else {
            // 首次运行：把游标对齐到当前已关闭的分钟，不回溯历史（避免刚上线就扫全表）。
            write_watermark(self.sql.as_ref(), upper).await?;
            return Ok(EbpfAggReport::default());
        };
        if upper <= watermark {
            return Ok(EbpfAggReport::default());
        }

        let rows = self.load_rows(watermark, upper).await?;
        let mut report = EbpfAggReport {
            rows: rows.len(),
            ..EbpfAggReport::default()
        };
        if rows.is_empty() {
            write_watermark(self.sql.as_ref(), upper).await?;
            return Ok(report);
        }

        for point in aggregate(&rows) {
            report.buckets.push(point.timestamp);
            self.ts.write(point).await?;
            report.points += 1;
        }
        report.buckets.sort_unstable();
        report.buckets.dedup();
        write_watermark(self.sql.as_ref(), upper).await?;
        Ok(report)
    }

    async fn load_rows(
        &self,
        from_exclusive: i64,
        to_inclusive: i64,
    ) -> Result<Vec<EdgeRow>, DataplaneError> {
        let result = self
            .sql
            .execute(
                &format!(
                    "SELECT bucket_start, src_service, dst_service, src_ip, src_port,
                            dst_ip, dst_port, protocol, connections, bytes_sent, bytes_recv,
                            duration_sum, duration_max, tcp_retrans, tcp_resets, failures,
                            failure_reason, latency_hist
                     FROM {} WHERE bucket_start > ?1 AND bucket_start <= ?2",
                    tables::EBPF_EDGES
                ),
                &[
                    SqlValue::Integer(from_exclusive),
                    SqlValue::Integer(to_inclusive),
                ],
            )
            .await?;
        Ok(result
            .rows
            .iter()
            .map(|row| EdgeRow {
                bucket_start: as_i64(row.first()),
                src_service: as_text(row.get(1)),
                dst_service: as_text(row.get(2)),
                src_ip: as_text(row.get(3)),
                src_port: as_i64(row.get(4)),
                dst_ip: as_text(row.get(5)),
                dst_port: as_i64(row.get(6)),
                protocol: as_text(row.get(7)),
                connections: as_i64(row.get(8)),
                bytes_sent: as_i64(row.get(9)),
                bytes_recv: as_i64(row.get(10)),
                duration_sum: as_i64(row.get(11)),
                duration_max: as_i64(row.get(12)),
                tcp_retrans: as_i64(row.get(13)),
                tcp_resets: as_i64(row.get(14)),
                failures: as_i64(row.get(15)),
                failure_reason: as_text(row.get(16)),
                latency_hist: parse_hist(&as_text(row.get(17))),
            })
            .collect())
    }
}

/// 一行边记录（只保留聚合需要的字段）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EdgeRow {
    pub bucket_start: i64,
    pub src_service: String,
    pub dst_service: String,
    pub src_ip: String,
    pub src_port: i64,
    pub dst_ip: String,
    pub dst_port: i64,
    pub protocol: String,
    pub connections: i64,
    pub bytes_sent: i64,
    pub bytes_recv: i64,
    pub duration_sum: i64,
    /// 该边的最大耗时（同分钟多条边取 max，见 `aggregate`）。
    pub duration_max: i64,
    pub tcp_retrans: i64,
    pub tcp_resets: i64,
    pub failures: i64,
    pub failure_reason: String,
    pub latency_hist: Vec<u64>,
}

/// 单条边的耗时累计：`(耗时和, 耗时最大值, 耗时直方图)`。
type DurationAgg = (i64, i64, Vec<u64>);

/// 字节计数的聚合键：`(分钟桶, 源服务, 目标服务, 源 IP, 目标 IP, 目标端口, 方向:协议)`。
type BytesKey = (i64, String, String, String, String, i64, String);

/// 把边记录聚合成指标点（纯函数，便于单测）。
#[must_use]
pub fn aggregate(rows: &[EdgeRow]) -> Vec<TsPoint> {
    // 计数类：按 (分钟, src, dst, ...) 累加。
    let mut connections: BTreeMap<(i64, String, String, i64), f64> = BTreeMap::new();
    let mut retrans: BTreeMap<(i64, String, String), f64> = BTreeMap::new();
    let mut resets: BTreeMap<(i64, String, String), f64> = BTreeMap::new();
    let mut failures: BTreeMap<(i64, String, String, String), f64> = BTreeMap::new();
    let mut bytes: BTreeMap<BytesKey, f64> = BTreeMap::new();
    // 耗时：均值直接累加，p95 用直方图槽近似。
    let mut duration: BTreeMap<(i64, String, String), DurationAgg> = BTreeMap::new();

    for row in rows {
        let bucket = floor_minute(row.bucket_start);
        let src = row.src_service.clone();
        let dst = row.dst_service.clone();
        *connections
            .entry((bucket, src.clone(), dst.clone(), row.dst_port))
            .or_default() += row.connections as f64;
        if row.tcp_retrans > 0 {
            *retrans
                .entry((bucket, src.clone(), dst.clone()))
                .or_default() += row.tcp_retrans as f64;
        }
        if row.tcp_resets > 0 {
            *resets
                .entry((bucket, src.clone(), dst.clone()))
                .or_default() += row.tcp_resets as f64;
        }
        if row.failures > 0 {
            let reason = if row.failure_reason.is_empty() {
                "other".to_string()
            } else {
                row.failure_reason.clone()
            };
            *failures
                .entry((bucket, src.clone(), dst.clone(), reason))
                .or_default() += row.failures as f64;
        }
        if row.bytes_sent > 0 {
            *bytes
                .entry((
                    bucket,
                    src.clone(),
                    dst.clone(),
                    row.src_ip.clone(),
                    row.dst_ip.clone(),
                    row.dst_port,
                    format!("sent:{}", row.protocol),
                ))
                .or_default() += row.bytes_sent as f64;
        }
        if row.bytes_recv > 0 {
            *bytes
                .entry((
                    bucket,
                    src.clone(),
                    dst.clone(),
                    row.src_ip.clone(),
                    row.dst_ip.clone(),
                    row.dst_port,
                    format!("recv:{}", row.protocol),
                ))
                .or_default() += row.bytes_recv as f64;
        }
        let entry = duration
            .entry((bucket, src, dst))
            .or_insert_with(|| (0, 0, vec![0; row.latency_hist.len()]));
        entry.0 += row.duration_sum;
        // `max` 取桶内各边的最大值（同分钟多条边的 duration_max 取 max 才是真的最大值）。
        entry.1 = entry.1.max(row.duration_max);
        if entry.2.len() < row.latency_hist.len() {
            entry.2.resize(row.latency_hist.len(), 0);
        }
        for (index, count) in row.latency_hist.iter().enumerate() {
            entry.2[index] = entry.2[index].saturating_add(*count);
        }
    }

    let mut out = Vec::new();
    for ((bucket, src, dst, port), value) in connections {
        out.push(point(
            "ebpf_edge_connections_total",
            &[
                ("src_service", &src),
                ("dst_service", &dst),
                ("dst_port", &port.to_string()),
            ],
            "value",
            value,
            bucket,
        ));
        // 与 APM 侧同名 measurement 合并：请求数/错误数/耗时都带 `source=ebpf`。
        out.push(point(
            "apm_edge_requests_total",
            &[
                ("src_service", &src),
                ("dst_service", &dst),
                ("span_kind", ""),
                ("status", ""),
                ("source", "ebpf"),
            ],
            "value",
            value,
            bucket,
        ));
    }
    for ((bucket, src, dst), value) in retrans {
        out.push(point(
            "ebpf_tcp_retrans_total",
            &[("src_service", &src), ("dst_service", &dst)],
            "value",
            value,
            bucket,
        ));
    }
    for ((bucket, src, dst), value) in resets {
        out.push(point(
            "ebpf_tcp_resets_total",
            &[("src_service", &src), ("dst_service", &dst)],
            "value",
            value,
            bucket,
        ));
    }
    for ((bucket, src, dst, reason), value) in failures {
        out.push(point(
            "ebpf_tcp_failures_total",
            &[
                ("src_service", &src),
                ("dst_service", &dst),
                ("reason", &reason),
            ],
            "value",
            value,
            bucket,
        ));
        out.push(point(
            "apm_edge_errors_total",
            &[
                ("src_service", &src),
                ("dst_service", &dst),
                ("span_kind", ""),
                ("source", "ebpf"),
            ],
            "value",
            value,
            bucket,
        ));
    }
    for ((bucket, src, dst, src_ip, dst_ip, port, direction), value) in bytes {
        let (direction, protocol) = direction
            .split_once(':')
            .map_or((direction.clone(), String::new()), |(d, p)| {
                (d.to_string(), p.to_string())
            });
        out.push(point(
            "ebpf_edge_bytes_total",
            &[
                ("src_service", &src),
                ("dst_service", &dst),
                ("src_ip", &src_ip),
                ("dst_ip", &dst_ip),
                ("dst_port", &port.to_string()),
                ("protocol", &protocol),
                ("direction", &direction),
            ],
            "value",
            value,
            bucket,
        ));
    }
    for ((bucket, src, dst), (duration_sum, duration_max, hist)) in duration {
        let connections: f64 = connections_total(rows, &src, &dst, bucket);
        if connections > 0.0 {
            out.push(point(
                "apm_edge_duration_micros",
                &[
                    ("src_service", &src),
                    ("dst_service", &dst),
                    ("span_kind", ""),
                    ("field", "avg"),
                    ("source", "ebpf"),
                ],
                "value",
                duration_sum as f64 / connections,
                bucket,
            ));
        }
        if duration_max > 0 {
            out.push(point(
                "apm_edge_duration_micros",
                &[
                    ("src_service", &src),
                    ("dst_service", &dst),
                    ("span_kind", ""),
                    ("field", "max"),
                    ("source", "ebpf"),
                ],
                "value",
                duration_max as f64,
                bucket,
            ));
        }
        if let Some(p95) = histogram_p95(&hist) {
            out.push(point(
                "apm_edge_duration_micros",
                &[
                    ("src_service", &src),
                    ("dst_service", &dst),
                    ("span_kind", ""),
                    ("field", "p95"),
                    ("source", "ebpf"),
                ],
                "value",
                p95 as f64,
                bucket,
            ));
        }
    }
    out
}

/// 某条边在该分钟的连接总数（算平均耗时用）。
fn connections_total(rows: &[EdgeRow], src: &str, dst: &str, bucket: i64) -> f64 {
    rows.iter()
        .filter(|row| {
            row.src_service == src
                && row.dst_service == dst
                && floor_minute(row.bucket_start) == bucket
        })
        .map(|row| row.connections as f64)
        .sum()
}

/// 直方图近似 P95：取累计占比首次 ≥ 95% 的槽上界。
///
/// 这是**近似值**（槽上界会低估长尾，最后一槽无上界时按前一槽的 2 倍报），
/// 前端 tooltip 必须标注，不能与 OTLP 侧的精确 P95 相加。
#[must_use]
pub fn histogram_p95(hist: &[u64]) -> Option<u64> {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return None;
    }
    let target = total as f64 * 0.95;
    let mut cumulative = 0u64;
    for (slot, count) in hist.iter().enumerate() {
        cumulative = cumulative.saturating_add(*count);
        if cumulative as f64 >= target {
            return Some(slot_upper_micros(slot));
        }
    }
    Some(slot_upper_micros(hist.len().saturating_sub(1)))
}

/// 槽上界：槽 `i` 的标称上界是 `2^(i+1)` 微秒。
///
/// 直方图最后一槽是「溢出槽」（Agent 侧把 ≥ 2^31 微秒也就是 ≥ 35 分钟的都塞进去），
/// 它严格来说是开区间，这里仍按标称上界报，避免出现「上界小于下界」的荒谬值。
fn slot_upper_micros(slot: usize) -> u64 {
    1u64 << (slot + 1).min(62)
}

fn point(
    measurement: &str,
    tags: &[(&str, &str)],
    field_name: &str,
    value: f64,
    timestamp: i64,
) -> TsPoint {
    TsPoint {
        measurement: measurement.to_string(),
        tags: tags
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
        field_name: field_name.to_string(),
        field_value: value,
        timestamp,
    }
}

fn floor_minute(ts: i64) -> i64 {
    ts - ts.rem_euclid(MINUTE_MICROS)
}

fn parse_hist(text: &str) -> Vec<u64> {
    serde_json::from_str(text).unwrap_or_default()
}

fn as_i64(value: Option<&SqlValue>) -> i64 {
    match value {
        Some(SqlValue::Integer(i)) => *i,
        Some(SqlValue::Real(f)) => *f as i64,
        _ => 0,
    }
}

fn as_text(value: Option<&SqlValue>) -> String {
    match value {
        Some(SqlValue::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

/// 读游标。
pub async fn read_watermark(sql: &dyn RelationalStore) -> Result<Option<i64>, DataplaneError> {
    let result = sql
        .execute(
            &format!("SELECT value FROM {} WHERE key = ?1", tables::META),
            &[SqlValue::Text(WATERMARK_KEY.to_string())],
        )
        .await?;
    Ok(result
        .rows
        .first()
        .and_then(|row| row.first())
        .and_then(|value| match value {
            SqlValue::Text(s) => s.parse::<i64>().ok(),
            _ => None,
        }))
}

/// 写游标（不存在则插入）。
pub async fn write_watermark(sql: &dyn RelationalStore, value: i64) -> Result<(), DataplaneError> {
    sql.execute(
        &format!(
            "INSERT INTO {} (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            tables::META
        ),
        &[
            SqlValue::Text(WATERMARK_KEY.to_string()),
            SqlValue::Text(value.to_string()),
        ],
    )
    .await
    .map_err(|e| DataplaneError::new(ErrorCode::QueryFailed, e.message))?;
    Ok(())
}

/// 一个 Agent 的 eBPF 能力状态（`GET /v1/ebpf/capability`）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CapabilityEntry {
    pub agent_id: String,
    pub item_id: String,
    pub available: bool,
    pub kernel_ok: bool,
    pub btf_ok: bool,
    pub capability_ok: bool,
    pub kernel_release: String,
    /// 不可用时的原因（取自指标标签 `reason`）。
    pub reason: String,
}

/// 能力状态报告。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct CapabilityReport {
    pub agents: Vec<CapabilityEntry>,
    /// 有多少 Agent 报过能力状态（用来区分「没上报」与「上报了不可用」）。
    pub reported: usize,
}

/// 能力状态的回看窗口：Agent 只在采集项启动/状态变化时上报一次，所以要能查到很久之前的点。
pub const CAPABILITY_LOOKBACK_MICROS: i64 = 7 * 24 * 3600 * 1_000_000;

/// 回看窗口内的查询步长（秒）。
pub const CAPABILITY_STEP_SECS: i64 = 3_600;

/// 读 `agent_ebpf_capability` 指标的最新值。
///
/// 指标由 Agent 在每个采集项启动时上报一次（`field_value=1/0`，标签带
/// `agent_id`/`item_id`/`kernel_ok`/`btf_ok`/`capability_ok`/`kernel_release`/`reason`）。
///
/// **不能用 instant 查询 + 当前时间**：Prom 的 instant 查询只回看几分钟，而能力点是**状态**，
/// 可能几小时前才上报过一次；那样会把「早就上报过不可用」显示成「没有上报」。这里改用
/// 7 天范围查询并取每条序列的最后一个样本。
pub async fn capability_report(
    ts: &dyn TimeSeriesStore,
) -> Result<CapabilityReport, DataplaneError> {
    let now = crate::now_micros();
    let result = ts
        .query_range(
            "agent_ebpf_capability",
            now - CAPABILITY_LOOKBACK_MICROS,
            now,
            CAPABILITY_STEP_SECS,
        )
        .await?;
    let mut agents: Vec<CapabilityEntry> = Vec::new();
    for series in result.result {
        // 取最后一个样本：同标签的后续上报覆盖旧值。
        let value = series
            .values
            .as_ref()
            .and_then(|values| values.last().map(|(_, v)| *v))
            .or(series.value.map(|(_, v)| v))
            .unwrap_or_default();
        let tag = |key: &str| series.metric.get(key).cloned().unwrap_or_default();
        agents.push(CapabilityEntry {
            agent_id: tag("agent_id"),
            item_id: tag("item_id"),
            available: value >= 1.0,
            kernel_ok: tag("kernel_ok") == "true",
            btf_ok: tag("btf_ok") == "true",
            capability_ok: tag("capability_ok") == "true",
            kernel_release: tag("kernel_release"),
            reason: tag("reason"),
        });
    }
    agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id).then(a.item_id.cmp(&b.item_id)));
    Ok(CapabilityReport {
        reported: agents.len(),
        agents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(bucket: i64, connections: i64, failures: i64, hist: Vec<u64>) -> EdgeRow {
        EdgeRow {
            bucket_start: bucket,
            src_service: "order-api".into(),
            dst_service: "pay-api".into(),
            src_ip: "10.0.0.5".into(),
            dst_ip: "10.0.0.9".into(),
            src_port: 40_000,
            dst_port: 8_080,
            protocol: "tcp".into(),
            connections,
            bytes_sent: 100,
            bytes_recv: 200,
            duration_sum: 1_000,
            duration_max: 1_500,
            tcp_retrans: 1,
            tcp_resets: 0,
            failures,
            failure_reason: if failures > 0 {
                "refused".into()
            } else {
                String::new()
            },
            latency_hist: hist,
        }
    }

    #[test]
    fn aggregates_same_minute_across_buckets() {
        // 两个 10 秒桶落在同一分钟：计数相加，点只有一个时间戳。
        let base = 1_020_000_000i64;
        let rows = vec![
            row(base, 2, 1, vec![1, 0, 0]),
            row(base + 10_000_000, 3, 0, vec![0, 2, 0]),
        ];
        let points = aggregate(&rows);
        let connections = points
            .iter()
            .find(|p| p.measurement == "ebpf_edge_connections_total")
            .expect("connections point");
        assert_eq!(connections.field_value, 5.0);
        assert_eq!(connections.timestamp, base);
        assert_eq!(connections.tags["dst_port"], "8080");

        let requests = points
            .iter()
            .find(|p| p.measurement == "apm_edge_requests_total")
            .unwrap();
        assert_eq!(requests.field_value, 5.0);
        assert_eq!(requests.tags["source"], "ebpf");
        assert_eq!(requests.tags["span_kind"], "");

        let errors = points
            .iter()
            .find(|p| p.measurement == "apm_edge_errors_total")
            .unwrap();
        assert_eq!(errors.field_value, 1.0);
        assert_eq!(errors.tags["source"], "ebpf");

        let failures = points
            .iter()
            .find(|p| p.measurement == "ebpf_tcp_failures_total")
            .unwrap();
        assert_eq!(failures.tags["reason"], "refused");

        let bytes: Vec<_> = points
            .iter()
            .filter(|p| p.measurement == "ebpf_edge_bytes_total")
            .collect();
        assert_eq!(bytes.len(), 2, "发送/接收各一条");
        let sent = bytes
            .iter()
            .find(|p| p.tags["direction"] == "sent")
            .unwrap();
        assert_eq!(sent.field_value, 200.0, "两个桶的 bytes_sent 相加");
        assert_eq!(sent.tags["protocol"], "tcp");

        let avg = points
            .iter()
            .find(|p| p.measurement == "apm_edge_duration_micros" && p.tags["field"] == "avg")
            .unwrap();
        assert_eq!(avg.field_value, 2_000.0 / 5.0, "总耗时 / 连接数");
        let p95 = points
            .iter()
            .find(|p| p.measurement == "apm_edge_duration_micros" && p.tags["field"] == "p95")
            .unwrap();
        // 直方图 [1,2,0]：累计到槽 1 已达 95%，槽 1 的上界是 4 微秒。
        assert_eq!(p95.field_value, 4.0);
        // `max` 是桶内各边的最大值（边表里有 duration_max，但此前没有对应指标）。
        let max = points
            .iter()
            .find(|p| p.measurement == "apm_edge_duration_micros" && p.tags["field"] == "max")
            .unwrap();
        assert!(max.field_value >= avg.field_value, "max 不应小于 avg");
        assert_eq!(max.tags["source"], "ebpf");
    }

    #[test]
    fn empty_and_failure_fields_are_skipped() {
        let base = 1_020_000_000i64;
        let mut only_ok = row(base, 1, 0, vec![]);
        only_ok.bytes_sent = 0;
        only_ok.bytes_recv = 0;
        only_ok.tcp_retrans = 0;
        only_ok.tcp_resets = 0;
        let points = aggregate(&[only_ok]);
        assert!(points
            .iter()
            .all(|p| p.measurement != "ebpf_tcp_failures_total"
                && p.measurement != "ebpf_edge_bytes_total"
                && p.measurement != "ebpf_tcp_retrans_total"));
        assert!(points
            .iter()
            .all(|p| p.measurement != "apm_edge_errors_total"));
    }

    #[test]
    fn histogram_p95_edges() {
        assert_eq!(histogram_p95(&[]), None);
        assert_eq!(histogram_p95(&[0, 0]), None);
        // 槽 i 覆盖 [2^i, 2^(i+1)) 微秒，上界即 2^(i+1)。
        assert_eq!(histogram_p95(&[10, 0]), Some(2));
        assert_eq!(histogram_p95(&[0, 10]), Some(4), "溢出槽也按标称上界报");
        assert_eq!(histogram_p95(&[10]), Some(2));
        assert_eq!(
            histogram_p95(&[95, 5, 0, 0]),
            Some(2),
            "累计到 95% 即取该槽上界"
        );
    }
}
