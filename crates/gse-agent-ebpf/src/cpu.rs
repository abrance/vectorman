//! Agent 自身 CPU 占用采样（`max_cpu_percent` 的落地）。
//!
//! ## 为什么要单独做
//!
//! 需求 12.1 要求 `max_cpu_percent` 限制「CPU 占用」，但**内核态没有 CPU 时间测量手段**
//! （BPF 程序里读不到「本次执行消耗多少 CPU」），所以设计里把这个口径改成两条：
//! 内核态用「每秒事件数」令牌桶（`ebpf_abi::token_bucket_step`），`max_cpu_percent`
//! 退化为**用户态告警阈值**。
//!
//! 但它此前**没有任何作用点** —— 配置能配、代码不读，比「缺功能」更误导：运维以为配了就有保护。
//! 这里把它接上：每轮采集读一次 `/proc/self/stat` 的 `utime+stime`，按时间差算进程 CPU 占比；
//! **连续 [`DEGRADE_WINDOW`] 超限**才标记「降级运行」（设计口径：只告警、不自动停采集）。
//!
//! 边界：读的是**整个 Agent 进程**的 CPU（同一进程内多个 eBPF 采集项会看到同一个值），
//! 且 `USER_HZ` 按 Linux 约定取 100 —— 用 `sysconf` 更严谨，但那要引 libc，收益不抵依赖成本。

use std::time::{Duration, Instant};

/// 采样周期换算用的时钟节拍（Linux 上固定 100）。
pub const USER_HZ: u64 = 100;

/// 连续超限多久算「降级运行」（设计口径：5 分钟）。
pub const DEGRADE_WINDOW: Duration = Duration::from_secs(300);

/// 从 `/proc/self/stat` 内容里取 `utime + stime`（节拍）。
///
/// 格式：`pid (comm) state ...`，其中 `comm` 可能含空格与括号，因此**从最后一个 `)` 之后**开始数字段，
/// 第 11、12 个数字（`utime`、`stime`）相加入和。
#[must_use]
pub fn parse_cpu_ticks(stat: &str) -> Option<u64> {
    let tail = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = tail.split_whitespace().collect();
    // `)` 之后第一个字段是 state（index 0），utime 是第 11 个（index 10），stime 是 index 11。
    let utime: u64 = fields.get(10)?.parse().ok()?;
    let stime: u64 = fields.get(11)?.parse().ok()?;
    Some(utime.saturating_add(stime))
}

/// 读当前进程的 CPU 节拍；读不到返回 `None`（不告警、不 panic）。
#[must_use]
pub fn read_self_cpu_ticks() -> Option<u64> {
    parse_cpu_ticks(&std::fs::read_to_string("/proc/self/stat").ok()?)
}

/// CPU 占比 = 消耗节拍 / (间隔秒数 × USER_HZ) × 100。
#[must_use]
pub fn cpu_percent(prev_ticks: u64, cur_ticks: u64, elapsed: Duration) -> f64 {
    let consumed = cur_ticks.saturating_sub(prev_ticks);
    let seconds = elapsed.as_secs_f64();
    if consumed == 0 || seconds <= 0.0 {
        return 0.0;
    }
    (consumed as f64) / (seconds * USER_HZ as f64) * 100.0
}

/// CPU 采样器：算占比并跟踪「连续超限」窗口。
#[derive(Debug)]
pub struct CpuTracker {
    /// 告警阈值（百分比）。
    max_percent: u32,
    last_ticks: Option<u64>,
    last_at: Option<Instant>,
    /// 本轮连续超限的起点；未超限时为 `None`。
    over_since: Option<Instant>,
    /// 当前占比（上一轮采样值）。
    percent: f64,
    /// 是否处于「降级运行」。
    degraded: bool,
}

impl CpuTracker {
    #[must_use]
    pub fn new(max_percent: u32) -> Self {
        Self {
            max_percent: max_percent.min(100),
            last_ticks: None,
            last_at: None,
            over_since: None,
            percent: 0.0,
            degraded: false,
        }
    }

    /// 当前占比（百分比）。
    #[must_use]
    pub fn percent(&self) -> f64 {
        self.percent
    }

    /// 是否已被标记为「降级运行」。
    #[must_use]
    pub fn degraded(&self) -> bool {
        self.degraded
    }

    /// 用一次采样结果推进状态，返回本次占比。
    ///
    /// 第一次调用只记录基准（没有间隔，算不出占比）。`ticks` 为 `None`（读不到 `/proc`）时
    /// **不重置超限窗口**：读不到不该被当成「恢复正常」。
    pub fn observe(&mut self, now: Instant, ticks: Option<u64>) -> f64 {
        let Some(ticks) = ticks else {
            return self.percent;
        };
        let (Some(prev_ticks), Some(prev_at)) = (self.last_ticks, self.last_at) else {
            self.last_ticks = Some(ticks);
            self.last_at = Some(now);
            return self.percent;
        };
        self.last_ticks = Some(ticks);
        self.last_at = Some(now);
        self.percent = cpu_percent(prev_ticks, ticks, now.saturating_duration_since(prev_at));

        if self.percent > f64::from(self.max_percent) {
            let since = *self.over_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= DEGRADE_WINDOW {
                self.degraded = true;
            }
        } else {
            self.over_since = None;
            self.degraded = false;
        }
        self.percent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ticks_from_proc_stat() {
        // comm 里带空格与括号（真实存在的形态：`a (b) c`）。
        let line = "1234 (weird (name) x) S 1 2 3 4 5 6 7 8 9 4242 42 0 0 20 0 1 0 100 200 300";
        assert_eq!(parse_cpu_ticks(line), Some(4242 + 42));
        // 字段不足 / 不是数字 → None，不 panic。
        assert_eq!(parse_cpu_ticks("1234 (x) S 1 2"), None);
        assert_eq!(parse_cpu_ticks("no parens"), None);
        assert_eq!(parse_cpu_ticks("1 (x) S 1 2 3 4 5 6 7 8 bad 9"), None);
    }

    #[test]
    fn cpu_percent_is_relative_to_elapsed_and_hz() {
        // 10 秒内消耗 50 个节拍 = 0.5 秒 CPU → 5%。
        assert!((cpu_percent(0, 50, Duration::from_secs(10)) - 5.0).abs() < f64::EPSILON);
        // 计数回绕/复位不大于 0。
        assert_eq!(cpu_percent(100, 10, Duration::from_secs(10)), 0.0);
        // 零间隔不除零。
        assert_eq!(cpu_percent(0, 10, Duration::ZERO), 0.0);
    }

    #[test]
    fn tracker_marks_degraded_only_after_window() {
        let start = Instant::now();
        let mut tracker = CpuTracker::new(5);
        // 首次只建基准。
        assert_eq!(tracker.observe(start, Some(0)), 0.0);
        assert!(!tracker.degraded());

        // 1 秒消耗 20 节拍 = 20% > 5%：超限，但未满 5 分钟（窗口从这一刻起算）。
        let p = tracker.observe(start + Duration::from_secs(1), Some(20));
        assert!((p - 20.0).abs() < 0.001, "{p}");
        assert!(!tracker.degraded(), "窗口未满不算降级");
        assert!(!tracker
            .observe(start + Duration::from_secs(300), Some(20 + 20 * 299))
            .is_nan());
        assert!(!tracker.degraded(), "距首次超限只过了 299 秒，还差一点");

        // 持续超限到 5 分钟（首次超限 + 300 秒）→ 降级。
        tracker.observe(start + Duration::from_secs(301), Some(20 + 20 * 300));
        assert!(tracker.degraded());

        // 一旦低于阈值：窗口重置、降级解除（这一秒没有消耗 CPU）。
        tracker.observe(start + Duration::from_secs(302), Some(20 + 20 * 300));
        assert!(!tracker.degraded());
        assert!(tracker.percent() < 5.0);
    }

    #[test]
    fn missing_ticks_do_not_reset_window() {
        let start = Instant::now();
        let mut tracker = CpuTracker::new(1);
        tracker.observe(start, Some(0));
        tracker.observe(start + Duration::from_secs(1), Some(100)); // 100% → 超限
        assert!(tracker.over_since.is_some());

        // 读不到 `/proc/self/stat`：既不算恢复，也不算超限，窗口保持。
        tracker.observe(start + Duration::from_secs(2), None);
        assert!(tracker.over_since.is_some(), "读不到不该被当成恢复正常");

        // 下一次真实采样仍超限（301 秒里一直在用满 CPU）→ 窗口从首次超限起算，跨过 5 分钟即降级。
        tracker.observe(start + Duration::from_secs(302), Some(100 + 301 * 100));
        assert!(tracker.degraded(), "窗口跨过静默期仍然生效");
    }

    #[test]
    fn reads_own_cpu_ticks_on_linux() {
        // 真实读一次：只要不 panic 且能解析即可（值为 0 也可能是刚启动的进程）。
        if let Some(ticks) = read_self_cpu_ticks() {
            assert!(ticks < u64::MAX / 2, "节拍数不合理：{ticks}");
        }
    }
}
