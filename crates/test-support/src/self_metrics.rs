//! 自监控快照断言夹具：`/metrics` 文本 ↔ VictoriaMetrics 里的同一条序列。
//!
//! 自监控（`SelfMetrics`）的序列名是**全局固定**的（`dataserver_apm_*` / `dataserver_ts_*` /
//! `vectorman_ingest_*`），同一台机器上并行跑的第二个 dataserver、以及 pit 同时往同一台 VM
//! 写的实例，都会带上相同的指标名。所以这里所有查询都必须再挂一个
//! `instance="<SQL 口监听地址>"`——每个进程的端口是随机的，这就是天然的唯一隔离口径。
//!
//! 另一个坑：计数器和 gauge 都是**懒创建**的（`SelfMetrics::inc_counter` / `set_gauge` 第一次
//! 被调用时才注册进 registry）。没被真实触发过的名字在 `/metrics` 里根本不存在，所以
//! 「名字查不到」既可能是没写进 VM，也可能是压根没触发——断言必须先真跑出那条路径。

use std::time::Duration;

use crate::http;
use crate::proc::Dataserver;
use crate::prom::Response;
use crate::util::wait_until;
use crate::vm::Vm;

/// `instance` 标签值 = 被测进程 SQL 口的监听地址（`SelfMetrics::new("dataserver", listen)`）。
pub fn instance(ds: &Dataserver) -> String {
    ds.base().trim_start_matches("http://").to_string()
}

/// 只属于本进程的 selector：`name{instance="…", k="v", …}`。
pub fn selector(ds: &Dataserver, name: &str, extra: &[(&str, &str)]) -> String {
    let mut parts = vec![format!("instance=\"{}\"", instance(ds))];
    parts.extend(extra.iter().map(|(k, v)| format!("{k}=\"{v}\"")));
    format!("{name}{{{}}}", parts.join(","))
}

/// `GET <metrics 口>/metrics`，顺带断言文本里有 `name`（没有说明那条路径根本没触发）。
pub fn scrape_text(ds: &Dataserver, name: &str) -> String {
    let url = format!("{}/metrics", ds.metrics());
    let (status, body) = http::get(&url).unwrap_or_else(|e| panic!("GET {url}: {e}"));
    assert_eq!(status, 200, "GET {url} 期望 200，实际 {status}");
    assert!(
        has(&body, name),
        "/metrics 里没有 {name}：这条自监控路径没被触发过（懒创建），别把「没触发」当成「没写进 VM」；尾部：{}",
        http::tail(&body)
    );
    body
}

/// 文本里有没有这个指标名（含「没触发过就不存在」的负向断言）。
pub fn has(text: &str, name: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with(name)
            && !line[name.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// 从 `/metrics` 文本里取一条样本：`extra` 里的标签必须全部命中。
///
/// 返回 `(标签, 值)`；找不到时把同名样本行整段贴出来，便于区分「名字没有」和「标签不对」。
pub fn find(text: &str, name: &str, extra: &[(&str, &str)]) -> (Vec<(String, String)>, f64) {
    let prefix = format!("{name}{{");
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with(&prefix) {
            continue;
        }
        let Some((labels, value)) = line[prefix.len()..].split_once('}') else {
            continue;
        };
        let pairs = parse_labels(labels);
        let hit = extra
            .iter()
            .all(|(k, v)| pairs.iter().any(|(pk, pv)| pk == k && pv == v));
        if !hit {
            continue;
        }
        let value: f64 = value
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("{name} 的值不是数字（{e}）: {line}"));
        return (pairs, value);
    }
    panic!(
        "/metrics 里没有 {name}{{{:?}}} 的样本；同名行：\n{}",
        extra,
        text.lines()
            .filter(|l| has(l, name))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// 标签串 `k1="v1",k2="v2"` → 有序对（保持文本顺序，比较时调用方自行排序）。
fn parse_labels(labels: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for part in labels.split(',') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        pairs.push((k.trim().to_string(), v.trim().trim_matches('"').to_string()));
    }
    pairs
}

/// 等 VM 上该 selector **恰好一个**样本且值等于 `want`。
///
/// 值相等才收工：只等「查得到」会把「写进去了但值错/截断」一起放绿。
pub fn wait_value(vm: &Vm, promql: &str, want: f64, timeout: Duration) -> Response {
    let got = vm.wait_query(promql, None, timeout, |r| {
        r.samples.len() == 1 && (r.samples[0].value - want).abs() < 1e-9
    });
    assert_eq!(
        got.samples.len(),
        1,
        "期望恰好 1 个样本（{promql}），实际 {}: raw={}",
        got.samples.len(),
        got.raw
    );
    got
}

/// 等 VM 上的值 `>= want`（单调计数器在并行期间只会变大，用下界避免与 flush 竞速）。
pub fn wait_at_least(vm: &Vm, promql: &str, want: f64, timeout: Duration) -> Response {
    let got = vm.wait_query(promql, None, timeout, |r| {
        !r.is_empty() && r.value_sum() >= want
    });
    let sample = got.first();
    assert!(
        sample.value >= want,
        "VM 上的 {promql} 期望 >= {want}，实际 {}: {}",
        sample.value,
        got.raw
    );
    got
}

/// 等 VM 上出现该 selector 的样本（gauge 的值本身不锁，只锁「写进去了」）。
pub fn wait_present(vm: &Vm, promql: &str, timeout: Duration) -> Response {
    vm.wait_query(promql, None, timeout, |r| !r.is_empty())
}

/// `prom::Response` 里唯一的样本值（多个样本直接报错，避免「取第一个」掩盖多序列）。
pub fn single_value(resp: &Response) -> f64 {
    assert_eq!(
        resp.samples.len(),
        1,
        "期望恰好 1 个样本，实际 {}: raw={}",
        resp.samples.len(),
        resp.raw
    );
    resp.samples[0].value
}

/// 在 `/metrics` 文本里找不到的名字：断言它确实不存在（错误计数器「没故障就没触发」的证据）。
pub fn assert_absent(ds: &Dataserver, name: &str, why: &str) {
    let url = format!("{}/metrics", ds.metrics());
    let (status, body) = http::get(&url).unwrap_or_else(|e| panic!("GET {url}: {e}"));
    assert_eq!(status, 200, "GET {url} 期望 200，实际 {status}");
    assert!(
        !has(&body, name),
        "{name} 本不该出现（{why}），但 /metrics 里出现了：{}",
        body.lines()
            .filter(|l| has(l, name))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// 轮询直到 `/metrics` 里 `name` 的值满足断言（触发后等计数器变到期望值）。
pub fn wait_text_at_least(
    ds: &Dataserver,
    name: &str,
    extra: &[(&str, &str)],
    want: f64,
    timeout: Duration,
) {
    wait_until(&format!("/metrics 里 {name} >= {want}"), timeout, || {
        let url = format!("{}/metrics", ds.metrics());
        let (status, body) = http::get(&url).unwrap_or_else(|e| panic!("GET {url}: {e}"));
        assert_eq!(status, 200, "GET {url} 期望 200，实际 {status}");
        let (_, value) = find(&body, name, extra);
        (value >= want).then_some(value)
    });
}
