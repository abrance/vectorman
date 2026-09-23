//! 接入限流：按「每秒批次数」保护 trace 写入。
//!
//! 设计（`/.monkeycode/specs/apm-tracing/requirements.md` Requirement 10）：采样决策在
//! 应用侧，dataserver 不做二次采样，只做写入保护；超限返回 HTTP 429 + `unavailable`，
//! Agent 按既有退避重试处理（不丢批）。计数按固定秒窗统计，窗口切换即清零。

use std::sync::Mutex;

/// 每秒批次上限的限流器；`max_per_sec = 0` 表示不限。
#[derive(Debug)]
pub struct BatchLimiter {
    max_per_sec: u64,
    state: Mutex<Window>,
}

#[derive(Debug, Default, Clone, Copy)]
struct Window {
    /// 当前窗口的秒起点。
    second: i64,
    /// 本窗口已放行的批次数。
    used: u64,
}

impl BatchLimiter {
    #[must_use]
    pub fn new(max_per_sec: u64) -> Self {
        Self {
            max_per_sec,
            state: Mutex::new(Window::default()),
        }
    }

    /// 是否放行本批；超限返回 `false`（调用方回 429）。
    pub fn allow(&self, now_ts: i64) -> bool {
        if self.max_per_sec == 0 {
            return true;
        }
        let second = now_ts.div_euclid(1_000_000);
        let Ok(mut window) = self.state.lock() else {
            // 锁中毒时保守放行：宁可写入也不静默丢弃观测数据。
            return true;
        };
        if window.second != second {
            window.second = second;
            window.used = 0;
        }
        if window.used >= self.max_per_sec {
            return false;
        }
        window.used += 1;
        true
    }

    #[must_use]
    pub fn max_per_sec(&self) -> u64 {
        self.max_per_sec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_means_unlimited() {
        let limiter = BatchLimiter::new(0);
        for _ in 0..1_000 {
            assert!(limiter.allow(1_000_000));
        }
    }

    #[test]
    fn allows_up_to_limit_then_resets_next_second() {
        let limiter = BatchLimiter::new(2);
        assert!(limiter.allow(1_500_000), "第 1 批");
        assert!(limiter.allow(1_900_000), "第 2 批（同一秒）");
        assert!(!limiter.allow(1_999_999), "第 3 批超限");
        assert!(limiter.allow(2_000_000), "下一秒窗口重置");
    }
}
