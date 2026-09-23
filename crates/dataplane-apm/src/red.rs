//! 分钟桶内的 span/边延迟样本：RED 的计数与分位数来源。
//!
//! 为什么需要它：`TimeSeriesStore` 一个点只有一个数值 field（没有 histogram），
//! 分位数必须在写入前算好；而 `apm_trace_summary` 是 trace 粒度（只有根操作与整体
//! 耗时），拿不到「按操作/span_kind 的 span 耗时分布」。因此累加器在观察 span 时把
//! `(桶, service, operation, kind, status) → 延迟样本` 累积在内存里，桶关闭后交给
//! 聚合任务算 `avg/p50/p95/p99/max`，随后丢弃。
//!
//! 取舍：样本按组设上限（默认 20_000），超限**丢弃新样本并计数**（不做蓄水池采样——
//! 采样会让分位数无偏但计数与 sqlite 不一致，这里选择让计数保持诚实、分位数略偏高）。
//! 进程重启会丢当前未关闭桶的样本，只影响该分钟的分位数，不影响计数（计数走 sqlite）。

use std::collections::BTreeMap;
use std::sync::Mutex;

/// 每秒微秒数。
const MICROS_PER_SEC: i64 = 1_000_000;

/// 每组样本上限。
pub const DEFAULT_GROUP_SAMPLE_CAP: usize = 20_000;

/// 分组键：`(桶起点, 源服务, 目标/操作, span_kind, status)`。
type GroupKey = (i64, String, String, String, String);

/// 已关闭桶的定义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bucket {
    pub bucket_start: i64,
}

/// 一个分组的统计结果。
#[derive(Debug, Clone, PartialEq)]
pub struct GroupStats {
    pub bucket_start: i64,
    pub src_or_service: String,
    pub dst_or_operation: String,
    pub span_kind: String,
    pub status: String,
    pub count: u64,
    pub errors: u64,
    pub duration_sum: i64,
    pub duration_max: i64,
    /// 排序后的样本，用于算分位数。
    samples: Vec<i64>,
}

impl GroupStats {
    /// 排序后的样本切片。
    #[must_use]
    pub fn samples(&self) -> &[i64] {
        &self.samples
    }
}

impl GroupStats {
    /// 最近秩（nearest-rank）分位数；无样本返回 `None`。
    #[must_use]
    pub fn percentile(&self, p: f64) -> Option<i64> {
        if self.samples.is_empty() {
            return None;
        }
        let rank = (p / 100.0 * self.samples.len() as f64).ceil().max(1.0) as usize;
        self.samples.get(rank - 1).copied()
    }

    /// 平均值（微秒，四舍五入）。
    #[must_use]
    pub fn avg(&self) -> Option<i64> {
        if self.count == 0 {
            return None;
        }
        Some(self.duration_sum / self.count as i64)
    }
}

/// 一组样本的累积值：`(样本数, 总和, 最大值, 样本列表)`。
type GroupAcc = (u64, i64, i64, Vec<i64>);

#[derive(Debug, Default)]
struct Inner {
    /// 服务维度（span 级）样本。
    service: BTreeMap<GroupKey, GroupAcc>,
    /// 边维度样本。
    edge: BTreeMap<GroupKey, GroupAcc>,
    dropped_samples: u64,
}

/// 已关闭桶的样本：服务侧与边侧分开返回，避免靠 `span_kind` 反推来源。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ClosedSamples {
    pub service: Vec<GroupStats>,
    pub edge: Vec<GroupStats>,
}

/// span 与边的分钟样本累加器。
#[derive(Debug)]
pub struct RedSamples {
    group_cap: usize,
    inner: Mutex<Inner>,
}

impl RedSamples {
    #[must_use]
    pub fn new(group_cap: usize) -> Self {
        Self {
            group_cap: group_cap.max(1),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// 记录一个 span 级样本（service 维度 RED）。
    pub fn observe_span(
        &self,
        bucket_start: i64,
        service: &str,
        operation: &str,
        span_kind: &str,
        status: &str,
        duration_micros: i64,
    ) {
        self.observe(
            SampleSide::Service,
            bucket_start,
            service,
            operation,
            span_kind,
            status,
            duration_micros,
        );
    }

    /// 记录一个边级样本（`src` 为源服务，`dst` 为目标服务）。
    pub fn observe_edge(
        &self,
        bucket_start: i64,
        src_service: &str,
        dst_service: &str,
        span_kind: &str,
        status: &str,
        duration_micros: i64,
    ) {
        self.observe(
            SampleSide::Edge,
            bucket_start,
            src_service,
            dst_service,
            span_kind,
            status,
            duration_micros,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn observe(
        &self,
        side: SampleSide,
        bucket_start: i64,
        first: &str,
        second: &str,
        span_kind: &str,
        status: &str,
        duration_micros: i64,
    ) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let key = (
            bucket_start,
            first.to_string(),
            second.to_string(),
            span_kind.to_string(),
            status.to_string(),
        );
        let table = match side {
            SampleSide::Service => &mut inner.service,
            SampleSide::Edge => &mut inner.edge,
        };
        let entry = table.entry(key).or_insert_with(|| (0, 0, 0, Vec::new()));
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry.1.saturating_add(duration_micros);
        entry.2 = entry.2.max(duration_micros);
        if entry.3.len() < self.group_cap {
            entry.3.push(duration_micros);
        } else {
            inner.dropped_samples = inner.dropped_samples.saturating_add(1);
        }
    }

    /// 取出并移除所有已关闭的桶（`bucket_start + 60s <= now_ts`）。
    pub fn take_closed(&self, now_ts: i64) -> ClosedSamples {
        let Ok(mut inner) = self.inner.lock() else {
            return ClosedSamples::default();
        };
        let service = drain_closed(&mut inner.service, now_ts);
        let edge = drain_closed(&mut inner.edge, now_ts);
        ClosedSamples { service, edge }
    }

    /// 当前登记的组数（自监控用）。
    #[must_use]
    pub fn groups(&self) -> usize {
        self.inner
            .lock()
            .map(|i| i.service.len() + i.edge.len())
            .unwrap_or(0)
    }

    /// 因超出每组上限被丢弃的样本数（自监控用）。
    #[must_use]
    pub fn dropped_samples(&self) -> u64 {
        self.inner.lock().map(|i| i.dropped_samples).unwrap_or(0)
    }
}

/// 样本来自哪一侧。
#[derive(Debug, Clone, Copy)]
enum SampleSide {
    Service,
    Edge,
}

fn drain_closed(table: &mut BTreeMap<GroupKey, GroupAcc>, now_ts: i64) -> Vec<GroupStats> {
    let closed: Vec<GroupKey> = table
        .keys()
        .filter(|(bucket_start, ..)| *bucket_start + 60 * MICROS_PER_SEC <= now_ts)
        .cloned()
        .collect();
    let mut out = Vec::with_capacity(closed.len());
    for key in closed {
        let Some((count, sum, max, mut samples)) = table.remove(&key) else {
            continue;
        };
        samples.sort_unstable();
        out.push(GroupStats {
            bucket_start: key.0,
            src_or_service: key.1,
            dst_or_operation: key.2,
            span_kind: key.3,
            status: key.4,
            count,
            errors: 0,
            duration_sum: sum,
            duration_max: max,
            samples,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_nearest_rank() {
        let stats = GroupStats {
            bucket_start: 0,
            src_or_service: "svc".into(),
            dst_or_operation: "op".into(),
            span_kind: "server".into(),
            status: "ok".into(),
            count: 4,
            errors: 0,
            duration_sum: 400,
            duration_max: 400,
            samples: vec![100, 200, 300, 400],
        };
        assert_eq!(stats.percentile(50.0), Some(200), "p50 = 第 2 个");
        assert_eq!(stats.percentile(95.0), Some(400), "ceil(0.95*4)=4");
        assert_eq!(stats.percentile(99.0), Some(400));
        assert_eq!(stats.avg(), Some(100));

        let empty = GroupStats {
            samples: Vec::new(),
            count: 0,
            ..stats
        };
        assert_eq!(empty.percentile(50.0), None);
        assert_eq!(empty.avg(), None);
    }

    #[test]
    fn take_closed_only_returns_closed_buckets() {
        let red = RedSamples::new(10);
        red.observe_span(0, "svc", "op", "server", "ok", 10);
        red.observe_span(60 * MICROS_PER_SEC, "svc", "op", "server", "ok", 20);
        assert!(
            red.take_closed(30 * MICROS_PER_SEC).service.is_empty(),
            "桶未关闭"
        );
        let closed = red.take_closed(120 * MICROS_PER_SEC);
        assert_eq!(closed.service.len(), 2);
        assert!(
            red.take_closed(120 * MICROS_PER_SEC).service.is_empty(),
            "取出后清空"
        );
    }

    #[test]
    fn group_cap_drops_samples_and_counts() {
        let red = RedSamples::new(2);
        for d in 0..5 {
            red.observe_span(0, "svc", "op", "server", "ok", d);
        }
        let closed = red.take_closed(120 * MICROS_PER_SEC);
        assert_eq!(closed.service[0].count, 5, "计数不因样本上限而丢");
        assert_eq!(closed.service[0].samples().len(), 2, "样本被截断到上限");
        assert_eq!(red.dropped_samples(), 3);
    }
}
