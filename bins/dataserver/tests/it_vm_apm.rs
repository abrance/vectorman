//! `IT-VM-APM-01`：OTLP span → `apm_service_*` 分钟指标 → **真假 VictoriaMetrics**。
//!
//! 「落进 VM」只认 VM 原生查询口（`{select}/prometheus/api/v1/query`），dataserver 自己的
//! `/api/v1/query` 是代理读，会掩盖「写没进 VM」的问题（见 `test_plan/README.md` 第 6 条）。
//!
//! 断言口径（与 `dataplane-apm/src/aggregator.rs` 的实现一一对应）：
//! 1. 计数按 `status` 拆分，**只看 `span_kind=server`**（client span 不进 `apm_service_*`）；
//! 2. 耗时按 `(service, operation, span_kind)` 合并（status 不参与），`avg = sum/count`，
//!    `max` 取桶内最大，分位数用最近秩 `rank = ceil(p/100 * n)`；
//! 3. 点标签带 `source=otlp`、`span_kind=server`，时间戳是分钟桶起点。
//!
//! 场景定义（唯一事实来源）：`test_plan/IT-VM-APM-01.md`。

use std::time::Duration;

use vectorman_test_support::{
    data,
    proc::{Dataserver, Options},
    prom, util, vm,
};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");
const TIMEOUT: Duration = Duration::from_secs(60);
/// 桶往前放 5 分钟：桶一定「已关闭」，且 VM 的 instant 查询回看 5 分钟仍能看到这个点。
const BUCKET_AGE_SECS: i64 = 300;

/// 最小百分号编码：只保留 RFC 3986 unreserved，其余逐字节编码（避免为一个查询引入依赖）。
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// 求值点与时间戳都固定的范围查询：`start == end` 时只求值一次，返回的样本时间戳就是求值
/// 时刻，所以**能取到点**等价于「库里有不晚于该时刻的样本」；再配合「桶之前没有任何写入」，
/// 这就是「点写在桶起点」的判定方式。
fn range_timestamps(vm: &vm::Vm, promql: &str, start: f64, end: f64, step: f64) -> Vec<f64> {
    let base = vm.select_url.trim_end_matches('/');
    let prefix = if vm.single_node {
        format!("{base}/prometheus")
    } else {
        format!("{base}/select/{}/prometheus", vm.tenant)
    };
    let url = format!(
        "{prefix}/api/v1/query_range?query={}&start={start}&end={end}&step={step}",
        percent_encode(promql)
    );
    let (status, body) = vectorman_test_support::http::get(&url).unwrap_or_else(|e| panic!("{e}"));
    assert!((200..300).contains(&status), "范围查询 {status}: {body}");
    let value: serde_json::Value = serde_json::from_str(&body).expect("范围查询返回非 JSON");
    let mut stamps = Vec::new();
    for series in value["data"]["result"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        for point in series["values"].as_array().cloned().unwrap_or_default() {
            stamps.push(point[0].as_f64().unwrap_or_default());
        }
    }
    stamps
}

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
fn apm_service_red_lands_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::TempDir::new().expect("tempdir");
    let ds = Dataserver::start(dir.path(), Options::local().with_vm(&vm).with_apm(1), BIN);

    let prefix = util::unique_prefix("apm");
    let service = format!("{prefix}-svc");
    let bucket = util::floor_minute(util::now_micros()) - BUCKET_AGE_SECS * 1_000_000;
    // 断言时刻放在桶内第 120 秒：点在桶起点，instant 查询仍落在回看窗口里。
    let at_secs = bucket as f64 / 1e6 + 120.0;

    // 4 条 span：3 条 server `GET /orders`（ok/ok/error，1000/3000/5000 微秒），
    // 1 条 client `POST /downstream`（9000 微秒）。start 落在桶内 1/2/3/3.5 秒。
    let spans = vec![
        data::trace_span(
            1,
            1,
            &service,
            "GET /orders",
            "server",
            "ok",
            bucket + 1_000_000,
            1_000,
        ),
        data::trace_span(
            1,
            2,
            &service,
            "GET /orders",
            "server",
            "ok",
            bucket + 2_000_000,
            3_000,
        ),
        data::trace_span(
            1,
            3,
            &service,
            "GET /orders",
            "server",
            "error",
            bucket + 3_000_000,
            5_000,
        ),
        data::trace_span(
            1,
            4,
            &service,
            "POST /downstream",
            "client",
            "ok",
            bucket + 3_500_000,
            9_000,
        ),
    ];
    ds.ingest_ok(&data::traces_envelope(
        &format!("{prefix}-b1"),
        &format!("{prefix}-item"),
        &format!("{prefix}-agent"),
        &format!("{prefix}-host"),
        spans,
    ));

    // 1) 计数按 status 拆分：ok == 2、error == 1。
    let base = format!(
        "apm_service_requests_total{{service=\"{service}\",operation=\"GET /orders\",span_kind=\"server\",source=\"otlp\""
    );
    let ok = wait_scalar(&vm, &format!("{base},status=\"ok\"}}"), at_secs, 2.0);
    assert_eq!(ok.first().metric["source"], "otlp", "点必须带 source=otlp");
    assert_eq!(
        ok.first().metric["span_kind"],
        "server",
        "点必须带 span_kind=server"
    );
    let err = wait_scalar(&vm, &format!("{base},status=\"error\"}}"), at_secs, 1.0);
    assert_eq!(err.first().metric["status"], "error");

    // 2) 耗时：avg = (1000+3000+5000)/3、max = 5000、
    //    p95 = 最近秩 ceil(0.95*3)=3 → 排序后的第 3 个 = 5000（与 aggregator 的 percentile() 同口径）。
    let dur = format!(
        "apm_service_duration_micros{{service=\"{service}\",operation=\"GET /orders\",span_kind=\"server\",source=\"otlp\""
    );
    let avg = wait_scalar(&vm, &format!("{dur},field=\"avg\"}}"), at_secs, 3_000.0);
    assert_eq!(avg.first().metric["field"], "avg");
    let max = wait_scalar(&vm, &format!("{dur},field=\"max\"}}"), at_secs, 5_000.0);
    assert_eq!(max.first().metric["field"], "max");
    let p95 = wait_scalar(&vm, &format!("{dur},field=\"p95\"}}"), at_secs, 5_000.0);
    assert_eq!(p95.first().metric["field"], "p95");

    // 3) client span 不进 `apm_service_*`：同一 service 下按操作查为空。
    let client = format!(
        "apm_service_requests_total{{service=\"{service}\",operation=\"POST /downstream\"}}"
    );
    let got = vm.must_query(&client, Some(at_secs));
    assert!(
        got.is_empty(),
        "client span 不该出现在 apm_service_requests_total：raw={}",
        got.raw
    );

    // 4) 顺带核对「分钟桶」这个口径：只在桶起点上求值一次；能取到点就说明点写在桶起点，
    //    而不是每个 span 一个点（instant 查询回的是求值时刻，误差看不出这一层）。
    let dur_p95 = format!("{dur},field=\"p95\"}}");
    let bucket_secs = bucket as f64 / 1e6;
    let stamps = range_timestamps(&vm, &dur_p95, bucket_secs, bucket_secs, 60.0);
    assert_eq!(
        stamps,
        vec![bucket_secs],
        "同桶的三个 server span 必须合并成一个点，时间戳 = 桶起点"
    );

    let (stdout, stderr) = ds.logs();
    assert!(
        !stderr.contains("aggregate failed") && !stderr.contains("observe_span failed"),
        "dataserver 侧聚合/观测报错：\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
}
