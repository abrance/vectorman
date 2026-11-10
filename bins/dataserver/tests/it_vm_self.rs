//! `IT-VM-SELF-01`：dataserver 自己的 `/metrics` 快照也在 VM 里。
//!
//! `main.rs` 把 `LocalTsSink { ts }` 交给 `vectorman_metrics::flush_loop`：每
//! `self_metrics_interval_secs` 秒 `SelfMetrics::snapshot()`（就是 `/metrics` 文本解析出的
//! 全量样本）逐条写进时序后端。ts 后端换成 VM 后，这些自监控样本必须能在 VM 原生口查到，
//! 且**值与标签**要和 `/metrics` 文本一字不差——否则看板上的自监控数字会悄悄漂。
//!
//! 选 `vectorman_http_requests_total{method,path,status}` 做锚点：它的值完全由我们发起的
//! 请求决定，读文本之后不再打同一路由，就能等到「某个快照的值 == 文本值」，把
//! 「写进去了但值错」和「还没写进去」区分开。
//!
//! 场景定义（唯一事实来源）：`test_plan/IT-VM-SELF-01.md`。

use std::time::Duration;

use vectorman_test_support::{
    http,
    proc::{Dataserver, Options},
    vm,
};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");
const TIMEOUT: Duration = Duration::from_secs(60);
const METRIC: &str = "vectorman_http_requests_total";

/// 从 `/metrics` 文本里取一条 `vectorman_http_requests_total{method,path,status}` 样本：
/// 返回 `(标签, 值)`。找不到直接 panic 并把匹配到的候选行贴出来。
fn find_http_series(
    text: &str,
    method: &str,
    path: &str,
    status: &str,
) -> (Vec<(String, String)>, f64) {
    let prefix = format!("{METRIC}{{");
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with(&prefix) {
            continue;
        }
        let body = &line[prefix.len()..];
        let Some((labels, value)) = body.split_once('}') else {
            continue;
        };
        let mut pairs: Vec<(String, String)> = Vec::new();
        for part in labels.split(',') {
            let Some((k, v)) = part.split_once('=') else {
                continue;
            };
            pairs.push((k.trim().to_string(), v.trim().trim_matches('"').to_string()));
        }
        let has = |name: &str, want: &str| pairs.iter().any(|(k, v)| k == name && v == want);
        if has("method", method) && has("path", path) && has("status", status) {
            let value: f64 = value
                .trim()
                .parse()
                .unwrap_or_else(|e| panic!("/metrics 里的值不是数字（{e}）: {line}"));
            return (pairs, value);
        }
    }
    panic!(
        "/metrics 里没有 {METRIC}{{method=\"{method}\",path=\"{path}\",status=\"{status}\"}}；\
         相关行：\n{}",
        text.lines()
            .filter(|l| l.starts_with(METRIC))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// 标签 → PromQL 选择器（值里没有引号/反斜杠，直接拼）。
fn selector(pairs: &[(String, String)]) -> String {
    let inner: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
    format!("{METRIC}{{{}}}", inner.join(","))
}

fn fetch_metrics(ds: &Dataserver) -> String {
    let url = format!("{}/metrics", ds.metrics());
    let (status, body) = http::get(&url).unwrap_or_else(|e| panic!("GET {url}: {e}"));
    assert_eq!(status, 200, "GET {url} 期望 200，实际 {status}");
    assert!(
        body.contains(METRIC),
        "/metrics 里没有 {METRIC}，自监控没接上？尾部：{}",
        http::tail(&body)
    );
    body
}

#[test]
fn dataserver_self_metrics_land_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::TempDir::new().expect("tempdir");
    // 自监控快照周期 1s：不打开这个开关，`LocalTsSink` 根本不会被调用（配置默认 0）。
    let ds = Dataserver::start(
        dir.path(),
        Options::local().with_vm(&vm).with_self_metrics(1),
        BIN,
    );

    // 先打几个请求把 HTTP 计数顶上去，这样后面读到的值一定 > 0（0 值无法区分「没写」）。
    for _ in 0..3 {
        let (status, _) = ds.get("/health");
        assert_eq!(status, 200, "GET /health 期望 200");
    }

    // 读过文本之后不再打 `/health`：同一个序列的值就固定在 V，等 VM 出现「value == V」即证明
    // 这条序列真的被 flush_loop 写进了 VM（且不是别的快照的旧值）。
    let text = fetch_metrics(&ds);
    let (labels, value) = find_http_series(&text, "GET", "/health", "200");
    assert!(value > 0.0, "GET /health 计数应当 > 0，实际 {value}");

    let promql = selector(&labels);
    let got = vm.wait_query(&promql, None, TIMEOUT, |r| {
        !r.is_empty() && (r.value_sum() - value).abs() < 1e-9
    });
    assert_eq!(
        got.samples.len(),
        1,
        "期望恰好 1 个样本（{promql}），实际 {}: raw={}",
        got.samples.len(),
        got.raw
    );
    let sample = got.first();
    assert!(
        (sample.value - value).abs() < 1e-9,
        "VM 里的值与 /metrics 文本不一致：文本 {value}，VM {}: {}",
        sample.value,
        got.raw
    );

    // 标签名与值都要一致：VM 样本 = `/metrics` 的标签 + `__name__`，多一个少一个都是错。
    let mut want: Vec<(String, String)> = labels.clone();
    want.push(("__name__".to_string(), METRIC.to_string()));
    want.sort();
    let mut have: Vec<(String, String)> = sample
        .metric
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    have.sort();
    assert_eq!(
        have, want,
        "VM 样本的标签集/值必须与 /metrics 文本一致（多标签说明自监控写入口径变了）"
    );

    // 没走本地 tsink：`<data_path>/ts` 不存在或为空（ts_backend=victoriametrics）。
    let tsink_dir = ds.dir().join("data").join("ts");
    let tsink_entries = if tsink_dir.is_dir() {
        std::fs::read_dir(&tsink_dir)
            .unwrap_or_else(|e| panic!("读不了 {}：{e}", tsink_dir.display()))
            .count()
    } else {
        0
    };
    assert_eq!(
        tsink_entries,
        0,
        "ts_backend=victoriametrics 时不该写本地 tsink，但 {} 下有 {tsink_entries} 个条目",
        tsink_dir.display()
    );

    // 用来定位「隔离口径」是否生效：instance 标签是本次 dataserver 的 SQL 口监听地址。
    assert_eq!(
        sample.metric.get("instance").map(String::as_str),
        Some(ds.base().trim_start_matches("http://")),
        "instance 标签应等于 SQL 口监听地址"
    );
    assert_eq!(
        sample.metric.get("component").map(String::as_str),
        Some("dataserver")
    );
    assert_eq!(
        sample.metric.get("data_id").map(String::as_str),
        Some("self")
    );

    // ---- 时序存储状态采样（`main.rs` 里跟着自监控周期跑的 `storage_stats()` 分支）
    //
    // 这一组此前只有「有没有被 flush」的隐含覆盖，一个断言都没有。VM 后端的口径是明确的：
    // `memory_used_bytes` / `memory_budget_bytes` / `wal_size_bytes` 恒 0（内存与 WAL 是本地
    // tsink 的概念），`background_errors_total` / `degraded` 在没故障时恒 0，`series_count`
    // 是 VM 的**全实例** `totalSeries`——只断言「是一个 >= 1 的数字」，不锁绝对值（同一台 VM
    // 上还有别的实例在写，锁死会假红）。
    let text = fetch_metrics(&ds);
    for (name, want) in [
        ("dataserver_ts_series_count", None),
        ("dataserver_ts_memory_used_bytes", Some(0.0)),
        ("dataserver_ts_memory_budget_bytes", Some(0.0)),
        ("dataserver_ts_wal_size_bytes", Some(0.0)),
        ("dataserver_ts_background_errors_total", Some(0.0)),
        ("dataserver_ts_degraded", Some(0.0)),
    ] {
        assert!(
            text.lines().any(|l| l.starts_with(name)),
            "/metrics 里没有 {name}：storage_stats 采样没跑（它跟自监控周期同一个 tick）"
        );
        // VM 侧按本实例过滤：同名序列在别的实例上也有。
        let promql = format!(
            "{name}{{instance=\"{}\"}}",
            ds.base().trim_start_matches("http://")
        );
        let resp = vm.wait_query(&promql, None, TIMEOUT, |r| !r.is_empty());
        let sample = resp.first();
        match want {
            Some(want) => assert!(
                (sample.value - want).abs() < 1e-9,
                "{name} 期望 {want}，VM 实际 {}: {}",
                sample.value,
                resp.raw
            ),
            None => assert!(
                sample.value >= 1.0,
                "{name} 是全实例基数，至少应为 1（本进程刚写过自监控）: {}",
                resp.raw
            ),
        }
    }
    assert_eq!(
        sample.metric.get("data_id").map(String::as_str),
        Some("self")
    );
}
