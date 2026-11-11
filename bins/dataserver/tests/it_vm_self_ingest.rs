//! `IT-VM-SELF-06`：接入计数的自监控（`vectorman_ingest_*` / `dataserver_ingest_*`）落 VM。
//!
//! `http.rs::ingest` 每受理一批就记四条：收到的记录数、被拒的记录数，以及按
//! `data_type` / `status`、`data_type` / `result` 拆分的带标签计数器（需求 12.4 的 eBPF 批次口径
//! 就落在这两个带标签序列上）。它们此前只在 `/metrics` 上存在，没人验证过是否也进了 VM。
//!
//! 触发方式是两批真接入：一批全合法（`status=ok`），一批混一条非法记录
//! （`record_id` 为空 → `invalid_record`，整批回 200 但 `status=partial`、`failures` 一条）。
//! 带标签序列的**标签值**同样要断言，否则「按 data_type 拆分」这件事本身没被验证。
//!
//! 场景定义（唯一事实来源）：`test_plan/IT-VM-SELF-06.md`。

use std::time::Duration;

use vectorman_test_support::{
    data,
    proc::{Dataserver, Options},
    self_metrics, util, vm,
};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");
const TIMEOUT: Duration = Duration::from_secs(60);

#[test]
fn ingest_counters_self_metrics_land_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::TempDir::new().expect("tempdir");
    let ds = Dataserver::start(
        dir.path(),
        Options::local().with_vm(&vm).with_self_metrics(1),
        BIN,
    );

    let prefix = util::unique_prefix("self-ingest");
    let metric = util::metric_name(&prefix, "gauge");
    let ts = util::now_micros() - 5_000_000;
    let agent = format!("{prefix}-agent");
    let data_id = format!("{prefix}-item");
    let host = format!("{prefix}-host");

    // ---- 第 1 批：2 条合法记录 → ok
    let good = data::metric_series(&prefix, &metric, 2, 1.0, ts, 0);
    let reply = ds.ingest(&data::metrics_envelope(
        &format!("{prefix}-b1"),
        &data_id,
        &agent,
        &host,
        good,
    ));
    assert_eq!(
        reply["accepted"].as_u64(),
        Some(2),
        "第 1 批应 2 条全收: {reply}"
    );
    assert_eq!(
        reply["status"].as_str(),
        Some("ok"),
        "全合法的批次 status 应为 ok: {reply}"
    );

    // ---- 第 2 批：1 条合法 + 1 条 record_id 为空（invalid_record）→ partial
    let mut mixed = data::metric_series(
        &prefix,
        &util::metric_name(&prefix, "gauge2"),
        1,
        9.0,
        ts,
        0,
    );
    mixed.push(data::metric_record(
        "",
        &metric,
        [("series", prefix.as_str())],
        "value",
        1.0,
        ts,
    ));
    let reply = ds.ingest(&data::metrics_envelope(
        &format!("{prefix}-b2"),
        &data_id,
        &agent,
        &host,
        mixed,
    ));
    assert_eq!(
        reply["accepted"].as_u64(),
        Some(1),
        "第 2 批应只收 1 条: {reply}"
    );
    assert_eq!(
        reply["status"].as_str(),
        Some("partial"),
        "有非法记录时 status 应为 partial: {reply}"
    );
    assert_eq!(
        reply["failures"].as_array().map(Vec::len),
        Some(1),
        "应报出 1 条失败记录: {reply}"
    );

    // ---- `/metrics` 上的口径：3 收 / 1 拒，批次与记录按 data_type+status/result 拆分
    let text = self_metrics::scrape_text(&ds, "vectorman_ingest_records_accepted_total");
    for (name, extra, want) in [
        ("vectorman_ingest_records_accepted_total", vec![], 3.0),
        ("vectorman_ingest_records_failed_total", vec![], 1.0),
        (
            "dataserver_ingest_batches_total",
            vec![("data_type", "metrics"), ("status", "ok")],
            1.0,
        ),
        (
            "dataserver_ingest_batches_total",
            vec![("data_type", "metrics"), ("status", "partial")],
            1.0,
        ),
        (
            "dataserver_ingest_records_total",
            vec![("data_type", "metrics"), ("result", "accepted")],
            3.0,
        ),
        (
            "dataserver_ingest_records_total",
            vec![("data_type", "metrics"), ("result", "invalid")],
            1.0,
        ),
    ] {
        let (_, got) = self_metrics::find(&text, name, &extra);
        assert!(
            (got - want).abs() < 1e-9,
            "/metrics 上 {name}{extra:?} 期望 {want}，实际 {got}"
        );
    }

    // ---- 落 VM：同六条序列，值一字不差，且带标签的序列必须保留标签值
    for (name, extra, want) in [
        ("vectorman_ingest_records_accepted_total", vec![], 3.0),
        ("vectorman_ingest_records_failed_total", vec![], 1.0),
        (
            "dataserver_ingest_batches_total",
            vec![("data_type", "metrics"), ("status", "ok")],
            1.0,
        ),
        (
            "dataserver_ingest_batches_total",
            vec![("data_type", "metrics"), ("status", "partial")],
            1.0,
        ),
        (
            "dataserver_ingest_records_total",
            vec![("data_type", "metrics"), ("result", "accepted")],
            3.0,
        ),
        (
            "dataserver_ingest_records_total",
            vec![("data_type", "metrics"), ("result", "invalid")],
            1.0,
        ),
    ] {
        let promql = self_metrics::selector(&ds, name, &extra);
        let got = self_metrics::wait_value(&vm, &promql, want, TIMEOUT);
        let sample = got.first();
        for (k, v) in &extra {
            assert_eq!(
                sample.metric.get(*k).map(String::as_str),
                Some(*v),
                "VM 样本丢了标签 {k}={v}（按 data_type 拆分就是靠它）: {}",
                got.raw
            );
        }
    }
}
