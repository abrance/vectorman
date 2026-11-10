//! 时间、端口、轮询与隔离前缀。

use std::net::TcpListener;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 现取一个回环空闲端口。
///
/// 拿到的端口在 `TcpListener` 释放后存在被别的进程抢走的窗口，所以调用方要
/// 尽快把它绑给被测进程；测试并行度不高时这个窗口可以忽略（现有用例都这么做）。
pub fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local_addr").port()
}

pub fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_micros() as i64
}

pub fn now_secs() -> i64 {
    now_micros() / 1_000_000
}

/// 分钟桶起点（Unix 微秒），与 `dataplane-apm` 的桶口径一致。
pub fn floor_minute(micros: i64) -> i64 {
    micros - micros.rem_euclid(60 * 1_000_000)
}

/// 轮询直到 `f` 返回 `Some`，超时 panic 并带上 `what`。
///
/// `what` 要写「在等什么 + 期望值」，超时信息是排查的唯一线索。
pub fn wait_until<T>(what: &str, timeout: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = f() {
            return value;
        }
        if started.elapsed() >= timeout {
            panic!("等待 {what} 超时（{:?}）", timeout);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 合法的 Prometheus/VictoriaMetrics 指标名：只保留字母、数字、下划线与冒号，首字符不能是数字。
///
/// `unique_prefix()` 里带 `-`（"tp-vm-1234"），直接当 measurement 会被 VM 后端以
/// `invalid metric name` 400 拒掉——这是共用生成器最容易踩的坑。
pub fn metric_name(prefix: &str, suffix: &str) -> String {
    let mut name: String = format!("{prefix}_{suffix}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == ':' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert(0, 'm');
    }
    name
}

/// 按进程隔离的数据前缀：同一台机器上并行跑多条测试（或 CI 复用同一个 VM）时，
/// 各自的序列名/标签互不相交。
pub fn unique_prefix(tag: &str) -> String {
    format!("tp-{tag}-{}", std::process::id())
}
