//! `IT-VM-EBPF-01`：eBPF 边台账 → 分钟派生的 `ebpf_*` / `apm_edge_*{source=ebpf}` → **真假 VM**。
//!
//! 断言口径（与 `dataplane-apm/src/ebpf_metrics.rs` 的 `aggregate()` 一一对应）：
//! - 计数类（连接/字节/失败/请求/错误）按 `(分钟桶, src_service, dst_service, …)` 累加；
//! - `avg = duration_sum / connections`，`max` 取桶内 `duration_micros_max` 的最大值；
//! - `p95` 用 log2 直方图近似：累计占比首次 ≥ 95% 的槽，取槽上界 `2^(slot+1)` 微秒；
//! - 游标 `ebpf_metrics_watermark` 首次运行对齐到当前已关闭的分钟、**不回溯历史**。
//!
//! 顺序陷阱（实测踩过）：必须先 `ingest` 边、再把游标往回调。反过来时，若中间插进一轮 tick，
//! 该轮「扫到 0 行」也会把游标推进到当前上界，我们那批边就永远落在游标后面、再也不会被聚合。
//!
//! 场景定义（唯一事实来源）：`test_plan/IT-VM-EBPF-01.md`。

use std::time::Duration;

use serde_json::json;
use vectorman_test_support::{
    apm, data,
    proc::{Dataserver, Options},
    prom, util, vm,
};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");
const TIMEOUT: Duration = Duration::from_secs(60);
/// 桶往前放 5 分钟：一定「已关闭」，VM 的 instant 查询也还能看到这个点。
const BUCKET_AGE_SECS: i64 = 300;

/// 等到 `promql` 恰好返回一个样本且值等于 `expected`。
fn wait_scalar(vm: &vm::Vm, promql: &str, at_secs: f64, expected: f64) -> prom::Response {
    let resp = vm.wait_query(promql, Some(at_secs), TIMEOUT, |r| {
        !r.is_empty() && (r.value_sum() - expected).abs() < 1e-6
    });
    assert_eq!(
        resp.samples.len(),
        1,
        "期望恰好 1 个样本（{promql}），实际 {}: raw={}",
        resp.samples.len(),
        resp.raw
    );
    resp
}

#[test]
fn ebpf_edge_metrics_land_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::TempDir::new().expect("tempdir");
    let ds = Dataserver::start(dir.path(), Options::local().with_vm(&vm).with_apm(1), BIN);
    // 先等第一轮 tick 把游标写出来（否则后面 rewind 与「首次对齐」会互相覆盖）。
    apm::wait_watermark(&ds);

    let prefix = util::unique_prefix("ebpf");
    let agent = format!("{prefix}-agent");
    let src = format!("{prefix}-src");
    // 两个不同的 `dst_service`：同名标签会在 VM 里互相覆盖，分不开断言。
    let dst_a = format!("{prefix}-dst-a");
    let dst_b = format!("{prefix}-dst-b");
    let bucket = util::floor_minute(util::now_micros()) - BUCKET_AGE_SECS * 1_000_000;
    let at_secs = bucket as f64 / 1e6 + 120.0;

    let edges = vec![
        // A：5 条连接、1 次 refused、耗时和 50000、最大 20000、直方图 [0,0,0,5]
        //    → avg=10000、max=20000、p95=槽 3 的上界 2^4=16。
        data::ebpf_edge(
            &agent,
            bucket,
            ("10.11.0.1", 40_001, &src),
            ("10.22.0.1", 8_080, &dst_a),
            "tcp",
            5,
            1_000,
            500,
            1,
            "refused",
            50_000,
            20_000,
            vec![0, 0, 0, 5],
        ),
        // B：3 条连接、无失败、耗时和 9000、直方图 [0,0,3]
        //    → avg=3000、p95=槽 2 的上界 2^3=8（没有 failures → 不写 failures/errors 点）。
        data::ebpf_edge(
            &agent,
            bucket,
            ("10.11.0.1", 40_001, &src),
            ("10.22.0.2", 9_090, &dst_b),
            "tcp",
            3,
            2_000,
            600,
            0,
            "",
            9_000,
            12_000,
            vec![0, 0, 3],
        ),
    ];
    ds.ingest_ok(&data::ebpf_edges_envelope(
        &format!("{prefix}-b1"),
        &format!("{prefix}-item"),
        &agent,
        &format!("{prefix}-host"),
        edges,
    ));
    // 先 ingest 再回拨游标（见文件头「顺序陷阱」）。
    apm::rewind_ebpf_watermark(&ds, bucket - 60_000_000);

    // 1) 连接数：按 (src, dst, dst_port) 分。
    let conn_a = format!(
        "ebpf_edge_connections_total{{src_service=\"{src}\",dst_service=\"{dst_a}\",dst_port=\"8080\"}}"
    );
    let conn_b = format!(
        "ebpf_edge_connections_total{{src_service=\"{src}\",dst_service=\"{dst_b}\",dst_port=\"9090\"}}"
    );
    wait_scalar(&vm, &conn_a, at_secs, 5.0);
    wait_scalar(&vm, &conn_b, at_secs, 3.0);

    // 2) 字节：方向分开，别把 sent/recv 加错。
    let bytes = format!("ebpf_edge_bytes_total{{src_service=\"{src}\",dst_service=\"{dst_a}\"");
    let sent = wait_scalar(
        &vm,
        &format!("{bytes},direction=\"sent\"}}"),
        at_secs,
        1_000.0,
    );
    assert_eq!(sent.first().metric["direction"], "sent");
    assert_eq!(sent.first().metric["protocol"], "tcp");
    let recv = wait_scalar(
        &vm,
        &format!("{bytes},direction=\"recv\"}}"),
        at_secs,
        500.0,
    );
    assert_eq!(recv.first().metric["direction"], "recv");

    // 3) TCP 失败：只按 failure_reason 有值的行产出。
    let failures = format!(
        "ebpf_tcp_failures_total{{src_service=\"{src}\",dst_service=\"{dst_a}\",reason=\"refused\"}}"
    );
    wait_scalar(&vm, &failures, at_secs, 1.0);

    // 4) APM 侧的边指标用 `source=ebpf` 与 OTLP 侧同名 measurement 合并。
    let req_a = format!(
        "apm_edge_requests_total{{src_service=\"{src}\",dst_service=\"{dst_a}\",source=\"ebpf\"}}"
    );
    let req_b = format!(
        "apm_edge_requests_total{{src_service=\"{src}\",dst_service=\"{dst_b}\",source=\"ebpf\"}}"
    );
    let requests = wait_scalar(&vm, &req_a, at_secs, 5.0);
    assert_eq!(requests.first().metric["source"], "ebpf");
    // VM 不存空标签值，所以 eBPF 边的 `span_kind=""` 回读时**不存在**；OTLP 侧的服务边
    // 一定是 `span_kind=server`，这个「缺标签」正是两路不会混在一起的证据。
    assert!(
        !requests.first().metric.contains_key("span_kind"),
        "eBPF 边的 span_kind 应为空串（VM 不落空标签），实际标签: {:?}",
        requests.first().metric
    );
    wait_scalar(&vm, &req_b, at_secs, 3.0);
    let errors = format!(
        "apm_edge_errors_total{{src_service=\"{src}\",dst_service=\"{dst_a}\",source=\"ebpf\"}}"
    );
    wait_scalar(&vm, &errors, at_secs, 1.0);

    // 5) 边耗时：avg 来自 sum/connections，max 来自 duration_max，p95 来自直方图槽上界。
    let dur = format!(
        "apm_edge_duration_micros{{src_service=\"{src}\",dst_service=\"{dst_a}\",source=\"ebpf\""
    );
    wait_scalar(&vm, &format!("{dur},field=\"avg\"}}"), at_secs, 10_000.0);
    wait_scalar(&vm, &format!("{dur},field=\"max\"}}"), at_secs, 20_000.0);
    wait_scalar(&vm, &format!("{dur},field=\"p95\"}}"), at_secs, 16.0);
    let dur_b = format!(
        "apm_edge_duration_micros{{src_service=\"{src}\",dst_service=\"{dst_b}\",source=\"ebpf\""
    );
    wait_scalar(&vm, &format!("{dur_b},field=\"avg\"}}"), at_secs, 3_000.0);
    wait_scalar(&vm, &format!("{dur_b},field=\"p95\"}}"), at_secs, 8.0);

    // 边界：eBPF 原始事件只进日志引擎，不该派生新边、也不该动已经聚合好的指标。
    let watched = [
        (conn_a.clone(), 5.0),
        (conn_b.clone(), 3.0),
        (format!("{bytes},direction=\"sent\"}}"), 1_000.0),
        (format!("{bytes},direction=\"recv\"}}"), 500.0),
        (failures.clone(), 1.0),
        (req_a.clone(), 5.0),
        (req_b.clone(), 3.0),
        (errors.clone(), 1.0),
        (format!("{dur},field=\"avg\"}}"), 10_000.0),
        (format!("{dur},field=\"max\"}}"), 20_000.0),
        (format!("{dur},field=\"p95\"}}"), 16.0),
        (format!("{dur_b},field=\"avg\"}}"), 3_000.0),
        (format!("{dur_b},field=\"p95\"}}"), 8.0),
    ];
    let before = metric_snapshot(&vm, &watched, at_secs);
    let edges_before = edge_total(&ds, &src);
    assert_eq!(edges_before, 2, "这个 dataserver 里只该有我们那两条边");

    let event_type = format!("{prefix}-event");
    let process_name = format!("{prefix}-proc");
    let event_id = format!("{prefix}-event-1");
    ds.ingest_ok(&data::ebpf_events_envelope(
        &format!("{prefix}-b2"),
        &format!("{prefix}-item"),
        &agent,
        &format!("{prefix}-host"),
        vec![data::ebpf_event(
            &event_id,
            bucket + 5_000_000,
            &event_type,
            4242,
            &process_name,
            "tp probe (IT-VM-EBPF-01)",
        )],
    ));
    let search = ds.post(
        "/v1/ebpf/events/search",
        &json!({ "event_type": event_type }).to_string(),
    );
    assert_eq!(search.0, 200, "事件检索失败: {}", search.1);
    let found: serde_json::Value = serde_json::from_str(&search.1).expect("事件检索返回非 JSON");
    let records = found["records"].as_array().cloned().unwrap_or_default();
    assert!(
        records
            .iter()
            .any(|r| r["id"] == json!(event_id)
                && r["labels"]["process_name"] == json!(process_name)),
        "原始事件必须能按 event_type 检索到: {}",
        search.1
    );

    assert_eq!(
        edge_total(&ds, &src),
        edges_before,
        "原始事件不该派生出新边"
    );
    assert_eq!(
        metric_snapshot(&vm, &watched, at_secs),
        before,
        "原始事件不该改动已聚合的边指标"
    );
}

/// 一条边都查不到时返回 0；只统计本用例 `src_service` 下的边。
fn edge_total(ds: &Dataserver, src: &str) -> u64 {
    let (status, body) = ds.post(
        "/v1/edges/search",
        &json!({ "src_service": src, "source": "ebpf" }).to_string(),
    );
    assert_eq!(status, 200, "边检索失败: {body}");
    let page: serde_json::Value = serde_json::from_str(&body).expect("边检索返回非 JSON");
    page["total"].as_u64().unwrap_or_default()
}

/// 一批查询的完整样本（标签 + 值 + 时间戳），用来断言「数值一字不差」。
///
/// 这里不用「等它变成期望值」：污染是「值变大」而不是「还没写进去」，等下去只会超时。
fn metric_snapshot(
    vm: &vm::Vm,
    queries: &[(String, f64)],
    at_secs: f64,
) -> Vec<(String, Vec<prom::Sample>)> {
    queries
        .iter()
        .map(|(promql, expected)| {
            let resp = vm.must_query(promql, Some(at_secs));
            resp.assert_single_scalar(*expected);
            (promql.clone(), resp.samples)
        })
        .collect()
}
