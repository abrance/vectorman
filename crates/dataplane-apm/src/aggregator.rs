//! 聚合任务：把分钟桶样本与边摘要写成 RED / 边指标（`TimeSeriesStore`）。
//!
//! 数据来源与口径（`/.monkeycode/specs/apm-tracing/design.md`「ApmAggregator」）：
//!
//! - `apm_service_*`：来自 [`RedSamples`] 的 **span 级**样本，只取 `span_kind=server`
//!   （服务侧请求语义）。这样才有 `operation`、`status` 与耗时分位数；trace 摘要表是
//!   trace 粒度（只有根操作与整体耗时），不足以支撑按操作的红指标，因此摘要表只服务
//!   trace 列表/详情与服务清单。
//! - `apm_edge_*`：`calls` / `errors` / `duration_sum` 取 `apm_edge_summary`（已落库、
//!   跨重启不丢），`avg` 由 `sum/calls` 得出，`p95` 取 [`RedSamples`] 的边样本（样本
//!   上限内）。因此边指标没有 `status` 维度——边摘要在 sqlite 里不带状态分组。
//! - 多值指标（耗时的 `avg/p50/p95/p99/max`）用 **label `field`** 区分，`field_name`
//!   固定为 `value`：tsink 里 field 不是序列身份，同 measurement+labels 的不同
//!   `field_name` 会互相覆盖（实测：一次 instant 查询只剩最后写入的那一个）。
//! - 所有点带 `source=otlp`，`timestamp` 为分钟桶起点，空桶不写零值。
//! - 只读 sqlite 与内存，不扫 `LogStore` 明细。
//!
//! 失败取舍：单轮失败记录并跳过该桶，不回填。样本在 `take_closed` 时已被取出，因此
//! 该分钟的**分位数**会丢，计数不受影响（计数来自 sqlite 与样本计数，且样本计数在
//! 取出的同一批里）。

use std::collections::BTreeMap;
use std::sync::Arc;

use dataplane_core::{DataplaneError, SqlValue};
use dataplane_sql::RelationalStore;
use dataplane_ts::{TimeSeriesStore, TsPoint};

use crate::red::{GroupStats, RedSamples};
use crate::tables;

/// 所有观测点固定带 `source=otlp`（eBPF 侧写 `source=ebpf`）。
pub const SOURCE_OTLP: &str = "otlp";

/// 服务侧计数键：`(桶, service, operation, span_kind, status)`。
type ServiceCountKey = (i64, String, String, String, String);
/// 服务侧耗时键：`(桶, service, operation, span_kind)`（耗时按状态合并）。
type ServiceDurationKey = (i64, String, String, String);
/// 合并后的耗时累加值：`(计数, 总和, 最大值, 样本)`。
type DurationAcc = (u64, i64, i64, Vec<i64>);

/// 一轮聚合的产出统计。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AggReport {
    pub service_points: usize,
    pub edge_points: usize,
    /// 本轮涉及的分钟桶（升序）。
    pub buckets: Vec<i64>,
}

/// 聚合任务。
pub struct ApmAggregator {
    sql: Arc<dyn RelationalStore>,
    ts: Arc<dyn TimeSeriesStore>,
    samples: Arc<RedSamples>,
}

impl ApmAggregator {
    #[must_use]
    pub fn new(
        sql: Arc<dyn RelationalStore>,
        ts: Arc<dyn TimeSeriesStore>,
        samples: Arc<RedSamples>,
    ) -> Self {
        Self { sql, ts, samples }
    }

    /// 执行一轮：取出所有已关闭桶的样本，写 RED 与边指标。
    pub async fn run_once(&self, now_ts: i64) -> Result<AggReport, DataplaneError> {
        let closed = self.samples.take_closed(now_ts);
        let mut report = AggReport::default();

        // 1) 服务 RED：只看 server span（服务侧请求语义）。
        //
        //    计数按 status 拆分（`apm_service_requests_total{status}`），但**耗时按
        //    (service, operation, span_kind) 合并**：`apm_service_duration_micros`
        //    的 label 集里没有 status，若按 status 分别写入，同 measurement+labels
        //    的两条点会互相覆盖（谁后写谁生效）。
        let mut counts: BTreeMap<ServiceCountKey, u64> = BTreeMap::new();
        let mut durations: BTreeMap<ServiceDurationKey, DurationAcc> = BTreeMap::new();
        for group in closed.service.iter().filter(|g| g.span_kind == "server") {
            counts
                .entry((
                    group.bucket_start,
                    group.src_or_service.clone(),
                    group.dst_or_operation.clone(),
                    group.span_kind.clone(),
                    group.status.clone(),
                ))
                .and_modify(|c| *c += group.count)
                .or_insert(group.count);
            let merged = durations
                .entry((
                    group.bucket_start,
                    group.src_or_service.clone(),
                    group.dst_or_operation.clone(),
                    group.span_kind.clone(),
                ))
                .or_insert((0, 0, 0, Vec::new()));
            merged.0 += group.count;
            merged.1 += group.duration_sum;
            merged.2 = merged.2.max(group.duration_max);
            merged.3.extend(group.samples());
        }

        for ((bucket, service, operation, span_kind, status), count) in &counts {
            let mut tags = tags_of(&[
                ("service", service),
                ("operation", operation),
                ("span_kind", span_kind),
                ("status", status),
                ("source", SOURCE_OTLP),
            ]);
            self.write(
                "apm_service_requests_total",
                std::mem::take(&mut tags),
                "value",
                *count as f64,
                *bucket,
            )
            .await?;
            report.service_points += 1;
            if status == "error" {
                let err_tags = tags_of(&[
                    ("service", service),
                    ("operation", operation),
                    ("span_kind", span_kind),
                    ("status", status),
                    ("source", SOURCE_OTLP),
                ]);
                self.write(
                    "apm_service_errors_total",
                    err_tags,
                    "value",
                    *count as f64,
                    *bucket,
                )
                .await?;
                report.service_points += 1;
            }
        }

        for ((bucket, service, operation, span_kind), (count, sum, max, samples)) in &durations {
            let mut sorted = samples.clone();
            sorted.sort_unstable();
            let base = [
                ("service", service.as_str()),
                ("operation", operation.as_str()),
                ("span_kind", span_kind.as_str()),
                ("source", SOURCE_OTLP),
            ];
            for (field, value) in [
                (
                    "avg",
                    if *count > 0 {
                        Some(sum / *count as i64)
                    } else {
                        None
                    },
                ),
                ("p50", percentile(&sorted, 50.0)),
                ("p95", percentile(&sorted, 95.0)),
                ("p99", percentile(&sorted, 99.0)),
                ("max", Some(*max)),
            ] {
                if let Some(value) = value {
                    let mut duration_tags = tags_of(&base);
                    duration_tags.insert("field".to_string(), field.to_string());
                    self.write(
                        "apm_service_duration_micros",
                        duration_tags,
                        "value",
                        value as f64,
                        *bucket,
                    )
                    .await?;
                    report.service_points += 1;
                }
            }
        }

        // 2) 边指标：计数/耗时和来自 sqlite（跨重启不丢），p95 来自内存样本。
        //    只有「本轮取到边样本」的桶才会被结算，因此边指标要求聚合任务与 accumulator
        //    在同一进程（设计中的既有前提）。
        let edge_samples = merge_edge_samples(closed.edge.iter());
        let mut buckets: Vec<i64> = edge_samples.keys().map(|(bucket, ..)| *bucket).collect();
        buckets.sort_unstable();
        buckets.dedup();
        for bucket in buckets {
            report.buckets.push(bucket);
            for row in edge_rows(&self.sql, bucket).await? {
                let base = [
                    ("src_service", row.src_service.as_str()),
                    ("dst_service", row.dst_service.as_str()),
                    ("span_kind", row.span_kind.as_str()),
                    ("source", SOURCE_OTLP),
                ];
                self.write(
                    "apm_edge_requests_total",
                    tags_of(&base),
                    "value",
                    row.calls as f64,
                    bucket,
                )
                .await?;
                report.edge_points += 1;

                if row.errors > 0 {
                    self.write(
                        "apm_edge_errors_total",
                        tags_of(&base),
                        "value",
                        row.errors as f64,
                        bucket,
                    )
                    .await?;
                    report.edge_points += 1;
                }

                if row.calls > 0 {
                    let mut avg_tags = tags_of(&base);
                    avg_tags.insert("field".to_string(), "avg".to_string());
                    self.write(
                        "apm_edge_duration_micros",
                        avg_tags,
                        "value",
                        row.duration_sum as f64 / row.calls as f64,
                        bucket,
                    )
                    .await?;
                    report.edge_points += 1;
                }
                if let Some(p95) = edge_samples
                    .get(&(
                        bucket,
                        row.src_service.clone(),
                        row.dst_service.clone(),
                        row.span_kind.clone(),
                    ))
                    .and_then(|samples| percentile(samples, 95.0))
                {
                    let mut p95_tags = tags_of(&base);
                    p95_tags.insert("field".to_string(), "p95".to_string());
                    self.write(
                        "apm_edge_duration_micros",
                        p95_tags,
                        "value",
                        p95 as f64,
                        bucket,
                    )
                    .await?;
                    report.edge_points += 1;
                }
            }
        }
        report.buckets.sort_unstable();
        report.buckets.dedup();
        Ok(report)
    }

    async fn write(
        &self,
        measurement: &str,
        tags: BTreeMap<String, String>,
        field_name: &str,
        value: f64,
        timestamp: i64,
    ) -> Result<(), DataplaneError> {
        self.ts
            .write(TsPoint {
                measurement: measurement.to_string(),
                tags,
                field_name: field_name.to_string(),
                field_value: value,
                timestamp,
            })
            .await
    }
}

fn tags_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// 按 `(桶, src, dst, span_kind)` 合并边样本（跨 status），用于算边耗时分布。
fn merge_edge_samples<'a>(
    groups: impl Iterator<Item = &'a GroupStats>,
) -> BTreeMap<(i64, String, String, String), Vec<i64>> {
    let mut merged: BTreeMap<(i64, String, String, String), Vec<i64>> = BTreeMap::new();
    for group in groups {
        merged
            .entry((
                group.bucket_start,
                group.src_or_service.clone(),
                group.dst_or_operation.clone(),
                group.span_kind.clone(),
            ))
            .or_default()
            .extend(group.samples());
    }
    merged
}

fn percentile(samples: &[i64], p: f64) -> Option<i64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = (p / 100.0 * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted.get(rank - 1).copied()
}

struct EdgeRow {
    src_service: String,
    dst_service: String,
    span_kind: String,
    calls: i64,
    errors: i64,
    duration_sum: i64,
}

async fn edge_rows(
    sql: &Arc<dyn RelationalStore>,
    bucket: i64,
) -> Result<Vec<EdgeRow>, DataplaneError> {
    let result = sql
        .execute(
            &format!(
                "SELECT src_service, dst_service, span_kind, SUM(calls), SUM(errors), SUM(duration_sum)
                 FROM {} WHERE bucket_start = ?1
                 GROUP BY src_service, dst_service, span_kind",
                tables::EDGE_SUMMARY
            ),
            &[SqlValue::Integer(bucket)],
        )
        .await?;
    Ok(result
        .rows
        .iter()
        .filter_map(|row| match row.as_slice() {
            [SqlValue::Text(src), SqlValue::Text(dst), SqlValue::Text(kind), calls, errors, sum] => {
                Some(EdgeRow {
                    src_service: src.clone(),
                    dst_service: dst.clone(),
                    span_kind: kind.clone(),
                    calls: as_i64(calls),
                    errors: as_i64(errors),
                    duration_sum: as_i64(sum),
                })
            }
            _ => None,
        })
        .collect())
}

fn as_i64(value: &SqlValue) -> i64 {
    match value {
        SqlValue::Integer(i) => *i,
        SqlValue::Real(f) => *f as i64,
        _ => 0,
    }
}
