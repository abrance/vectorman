//! 加载失败退避（需求 1.7：30 秒起、×2、上限 10 分钟，成功清零）。
//!
//! 纯状态机，不碰时钟：调用方把「本次等待时长」拿去 sleep，因此可以精确单测序列。

use std::time::Duration;

/// 首次等待。
pub const BASE: Duration = Duration::from_secs(30);
/// 上限。
pub const MAX: Duration = Duration::from_secs(600);

/// 指数退避。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backoff {
    next: Duration,
    attempts: u32,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

impl Backoff {
    #[must_use]
    pub fn new() -> Self {
        Self {
            next: BASE,
            attempts: 0,
        }
    }

    /// 连续失败次数。
    #[must_use]
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// 失败一次：返回本次应等待的时长，并把下次时长翻倍（不超过 [`MAX`]）。
    pub fn failed(&mut self) -> Duration {
        let wait = self.next;
        self.attempts = self.attempts.saturating_add(1);
        self.next = (self.next * 2).min(MAX);
        wait
    }

    /// 成功一次：清空退避与计数。
    pub fn succeeded(&mut self) {
        self.next = BASE;
        self.attempts = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_until_cap_then_stays() {
        let mut backoff = Backoff::new();
        let mut waits = Vec::new();
        for _ in 0..8 {
            waits.push(backoff.failed().as_secs());
        }
        assert_eq!(waits, vec![30, 60, 120, 240, 480, 600, 600, 600]);
        assert_eq!(backoff.attempts(), 8);
    }

    #[test]
    fn success_resets() {
        let mut backoff = Backoff::new();
        assert_eq!(backoff.failed().as_secs(), 30);
        assert_eq!(backoff.failed().as_secs(), 60);
        backoff.succeeded();
        assert_eq!(backoff.attempts(), 0);
        assert_eq!(backoff.failed().as_secs(), 30, "成功后从 30 秒重新开始");
    }
}
