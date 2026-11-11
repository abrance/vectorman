//! IT-API-TRACES-01：`POST /v1/traces/search` 与 `GET /v1/traces/{trace_id}` 的检索/详情契约。
//!
//! 三条 trace 通过真 `/v1/ingest`（`data_type=traces`）写入，摘要由 APM 后台任务每秒
//! `flush_due` 落进 `apm_trace_summary`，因此所有断言都先轮询到「摘要齐全」再判定。
//!
//! 口径来源（`crates/dataplane-apm/src/query.rs`、`accumulator.rs`）：
//! - `summary.duration_micros = max_end_ts - start_ts`（**整条 trace 的时间跨度**，不是根 span 的耗时）；
//! - `status`：任一 span 为 `error` 即 `error`，否则 `ok`；
//! - `span_count`：累计 span 数；`services` 是去重后的集合；
//! - 详情里不存在的 trace → **404 `not_found`**（`get_trace` 返回 `not_found`，不是 200 空体）；
//! - `trace_id` 不是 32 位 hex → **400 `invalid_argument`**（先校验格式，再查库）。
//!
//! 场景说明见 `test_plan/IT-API-TRACES-01.md`。不用前端、不做静默跳过。

use std::time::Duration;

use serde_json::{json, Value};

use vectorman_test_support::data;
use vectorman_test_support::http;
use vectorman_test_support::proc::{Dataserver, Options};
use vectorman_test_support::util;

const TIMEOUT: Duration = Duration::from_secs(60);

/// 一条 trace 的可断言摘要（顺序与口径固定，便于整表比较）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    trace_id: String,
    start_ts: i64,
    duration_micros: i64,
    status: String,
    span_count: i64,
}

struct Fixture {
    /// 持有临时目录：`Dataserver` 只记路径，目录被删掉后 sqlite 文件就没了。
    _dir: tempfile::TempDir,
    ds: Dataserver,
    svc_a: String,
    svc_c: String,
    trace_a: String,
    trace_b: String,
    trace_c: String,
    span_a_root: String,
    span_a_child: String,
    ts_a: i64,
    ts_b: i64,
    ts_c: i64,
    from_ts: i64,
    to_ts: i64,
}

fn search(ds: &Dataserver, query: &Value) -> Value {
    let (status, body) = ds.post("/v1/traces/search", &query.to_string());
    assert_eq!(
        status,
        200,
        "/v1/traces/search 应 200，实际 {status}: {}（query={query}）",
        http::tail(&body)
    );
    serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("/v1/traces/search 应答非 JSON（{e}）: {body}"))
}

fn rows(page: &Value) -> Vec<Row> {
    page["traces"]
        .as_array()
        .unwrap_or_else(|| panic!("应答缺 traces 数组: {page}"))
        .iter()
        .map(|t| Row {
            trace_id: t["trace_id"].as_str().expect("trace_id").to_string(),
            start_ts: t["start_ts"].as_i64().expect("start_ts"),
            duration_micros: t["duration_micros"].as_i64().expect("duration_micros"),
            status: t["status"].as_str().expect("status").to_string(),
            span_count: t["span_count"].as_i64().expect("span_count"),
        })
        .collect()
}

fn total(page: &Value) -> i64 {
    page["total"]
        .as_i64()
        .unwrap_or_else(|| panic!("应答缺 total: {page}"))
}

impl Fixture {
    fn page(&self, query: Value) -> Value {
        search(&self.ds, &query)
    }

    fn in_window(&self, extra: Value) -> Value {
        let mut q = json!({"from_ts": self.from_ts, "to_ts": self.to_ts, "limit": 100});
        for (k, v) in extra.as_object().expect("extra 必须是对象") {
            q[k] = v.clone();
        }
        q
    }

    /// 轮询到「三条摘要都已落库且 A 的两个 span 都计进去」，返回全量页。
    fn wait_all(&self) -> Value {
        util::wait_until(
            "三条 trace 摘要落库（A 的 span_count=2）",
            TIMEOUT,
            || {
                let page = self.page(self.in_window(json!({})));
                let rows = rows(&page);
                let ok = total(&page) == 3
                    && rows.iter().any(|r| {
                        r.trace_id == self.trace_a
                            && r.span_count == 2
                            && r.duration_micros == 500_000
                    });
                ok.then_some(page)
            },
        )
    }
}

fn trace_id(n: u64) -> String {
    data::hex_id(n, 32)
}

fn start() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    // traces 检索 / 详情在 apm_enabled=false 时直接 503，所以必须打开 APM；
    // `apm_min_duration_micros_for_detail` 由夹具固定为 0（全部 span 写明细）。
    let ds = Dataserver::start(
        dir.path(),
        Options::local().with_apm(1),
        env!("CARGO_BIN_EXE_dataserver"),
    );
    let p = util::unique_prefix("api-traces");
    let base = util::now_micros() - 600_000_000;

    // 32 位 hex：pid 做种子，避免与本进程里其它用例的 id 撞（各自独立库，只为可读性）
    let seq = u64::from(std::process::id());
    let trace_a = trace_id(seq * 16 + 1);
    let trace_b = trace_id(seq * 16 + 2);
    let trace_c = trace_id(seq * 16 + 3);
    let span_a_root = data::hex_id(0x101, 16);
    let span_a_child = data::hex_id(0x102, 16);

    let svc_a = format!("{p}-svc-a");
    let svc_b = format!("{p}-svc-b");
    let svc_c = format!("{p}-svc-c");

    let ts_a = base;
    let ts_b = base + 60_000_000;
    let ts_c = base + 120_000_000;

    let root_a = data::trace_span(
        seq * 16 + 1,
        0x101,
        &svc_a,
        "GET /a",
        "server",
        "ok",
        ts_a,
        500_000,
    );
    assert_eq!(root_a["span_id"], json!(span_a_root));
    let mut child_a = data::trace_span(
        seq * 16 + 1,
        0x102,
        &svc_a,
        "db query",
        "client",
        "ok",
        ts_a + 10_000,
        100_000,
    );
    child_a["parent_span_id"] = json!(span_a_root);
    let span_b = data::trace_span(
        seq * 16 + 2,
        0x201,
        &svc_b,
        "POST /b",
        "server",
        "error",
        ts_b,
        2_000_000,
    );
    let span_c = data::trace_span(
        seq * 16 + 3,
        0x301,
        &svc_c,
        "GET /c",
        "server",
        "ok",
        ts_c,
        100_000,
    );

    let envelope = data::traces_envelope(
        &format!("{p}-batch"),
        &format!("{p}-item"),
        &format!("{p}-agent"),
        &format!("{p}-host"),
        vec![root_a, child_a, span_b, span_c],
    );
    ds.ingest_ok(&envelope);

    Fixture {
        _dir: dir,
        ds,
        svc_a,
        svc_c,
        trace_a,
        trace_b,
        trace_c,
        span_a_root,
        span_a_child,
        ts_a,
        ts_b,
        ts_c,
        from_ts: base - 1_000_000,
        to_ts: base + 300_000_000,
    }
}

#[test]
fn it_api_traces_search_and_detail() {
    let f = start();
    f.wait_all();

    // ---- 1. 全量：三条摘要的每个字段都精确
    let all = f.wait_all();
    assert_eq!(total(&all), 3, "全量 total 应为 3: {all}");
    let got = rows(&all);
    assert_eq!(
        got,
        vec![
            Row {
                trace_id: f.trace_c.clone(),
                start_ts: f.ts_c,
                duration_micros: 100_000,
                status: "ok".into(),
                span_count: 1,
            },
            Row {
                trace_id: f.trace_b.clone(),
                start_ts: f.ts_b,
                duration_micros: 2_000_000,
                status: "error".into(),
                span_count: 1,
            },
            Row {
                trace_id: f.trace_a.clone(),
                start_ts: f.ts_a,
                duration_micros: 500_000,
                status: "ok".into(),
                span_count: 2,
            },
        ],
        "默认排序（start_ts DESC）下三条摘要应逐字段相等: {all}"
    );

    // 摘要里的 root_service / root_operation / services 也一并核对（取 A）
    let a_summary = all["traces"]
        .as_array()
        .expect("traces")
        .iter()
        .find(|t| t["trace_id"] == json!(f.trace_a))
        .expect("缺 trace A");
    assert_eq!(a_summary["root_service"], json!(f.svc_a));
    assert_eq!(a_summary["root_operation"], json!("GET /a"));
    let mut services: Vec<String> = a_summary["services"]
        .as_array()
        .expect("services")
        .iter()
        .map(|s| s.as_str().expect("service 名").to_string())
        .collect();
    services.sort();
    assert_eq!(
        services,
        {
            let mut want = vec![f.svc_a.clone()];
            want.sort();
            want
        },
        "A 的 services 应只有 svc-a: {a_summary}"
    );

    // ---- 2. service / status / min_duration_micros 过滤各自生效
    let by_service = f.page(f.in_window(json!({"service": f.svc_a})));
    assert_eq!(
        total(&by_service),
        1,
        "service=svc-a 应只命中 1 条: {by_service}"
    );
    assert_eq!(rows(&by_service)[0].trace_id, f.trace_a);

    let by_status = f.page(f.in_window(json!({"status": "error"})));
    assert_eq!(
        total(&by_status),
        1,
        "status=error 应只命中 1 条: {by_status}"
    );
    assert_eq!(rows(&by_status)[0].trace_id, f.trace_b);

    let by_min = f.page(f.in_window(json!({"min_duration_micros": 1_000_000i64})));
    assert_eq!(
        total(&by_min),
        1,
        "min_duration_micros=1s 应只命中 2s 那条（A=0.5s / C=0.1s 被排除）: {by_min}"
    );
    assert_eq!(rows(&by_min)[0].trace_id, f.trace_b);

    let not_hit = f.page(f.in_window(json!({"service": f.svc_c, "status": "error"})));
    assert_eq!(total(&not_hit), 0, "组合条件互相排除时应为 0: {not_hit}");
    assert!(rows(&not_hit).is_empty());

    // ---- 3. 排序：duration_micros asc → C(0.1s) < A(0.5s) < B(2s)
    let sorted = f.page(f.in_window(json!({"sort": "duration_micros", "order": "asc"})));
    let order: Vec<String> = rows(&sorted).into_iter().map(|r| r.trace_id).collect();
    assert_eq!(
        order,
        vec![f.trace_c.clone(), f.trace_a.clone(), f.trace_b.clone()],
        "duration_micros asc 顺序应为 C → A → B: {sorted}"
    );
    assert_eq!(total(&sorted), 3);

    // ---- 4. 分页：limit=1&offset=1 必须等于全量结果的第 2 条（不重不漏）
    let asc_all = f.page(f.in_window(json!({"sort": "start_ts", "order": "asc"})));
    let asc_rows = rows(&asc_all);
    assert_eq!(
        asc_rows
            .iter()
            .map(|r| r.trace_id.clone())
            .collect::<Vec<_>>(),
        vec![f.trace_a.clone(), f.trace_b.clone(), f.trace_c.clone()],
        "start_ts asc 顺序应为 A → B → C: {asc_all}"
    );
    let page_mid =
        f.page(f.in_window(json!({"sort": "start_ts", "order": "asc", "limit": 1, "offset": 1})));
    assert_eq!(total(&page_mid), 3, "分页不影响 total: {page_mid}");
    assert_eq!(
        rows(&page_mid),
        vec![asc_rows[1].clone()],
        "limit=1&offset=1 应等于全量第 2 条: {page_mid}"
    );

    // ---- 5. 详情：存在的 trace 返回全部 span（含子 span、父子关系与时间顺序）
    let (status, body) = f.ds.get(&format!("/v1/traces/{}", f.trace_a));
    assert_eq!(status, 200, "详情应 200，实际 {status}: {body}");
    let detail: Value = serde_json::from_str(&body).expect("详情 JSON");
    assert_eq!(detail["summary"]["trace_id"], json!(f.trace_a));
    assert_eq!(detail["summary"]["span_count"], json!(2));
    assert_eq!(detail["expected_span_count"], json!(2));
    assert_eq!(
        detail["partial"],
        json!(false),
        "全部 span 都在，不该 partial: {detail}"
    );
    assert_eq!(detail["reason"], json!(null));

    let spans = detail["spans"].as_array().expect("spans 数组");
    assert_eq!(spans.len(), 2, "应返回 2 个 span: {detail}");
    assert_eq!(
        spans[0]["span_id"],
        json!(f.span_a_root),
        "升序第一个是根 span"
    );
    assert_eq!(
        spans[0]["parent_span_id"],
        json!(""),
        "根 span 的 parent 为空"
    );
    assert_eq!(spans[0]["name"], json!("GET /a"));
    assert_eq!(spans[0]["duration_micros"], json!(500_000));
    assert_eq!(
        spans[1]["span_id"],
        json!(f.span_a_child),
        "第二个是子 span"
    );
    assert_eq!(
        spans[1]["parent_span_id"],
        json!(f.span_a_root),
        "子 span 的 parent 必须指向根 span: {detail}"
    );
    assert_eq!(spans[1]["name"], json!("db query"));
    assert_eq!(spans[1]["duration_micros"], json!(100_000));

    // ---- 6. 详情：不存在的 32 位 hex id → 404 not_found（真实状态码，不是 200 空体）
    let absent = trace_id(0xdead_beef);
    let (status, body) = f.ds.get(&format!("/v1/traces/{absent}"));
    assert_eq!(status, 404, "不存在的 trace 应 404，实际 {status}: {body}");
    let err: Value = serde_json::from_str(&body).expect("错误应答 JSON");
    assert_eq!(err["code"], "not_found", "错误码口径: {body}");
    assert!(
        err["error"].as_str().is_some_and(|m| m.contains(&absent)),
        "错误信息应点名 trace_id: {body}"
    );

    // ---- 7. 详情：非 32 位 hex id → 400 invalid_argument（先校验格式）
    let (status, body) = f.ds.get("/v1/traces/not-a-trace-id");
    assert_eq!(status, 400, "非法 trace_id 应 400，实际 {status}: {body}");
    let err: Value = serde_json::from_str(&body).expect("错误应答 JSON");
    assert_eq!(err["code"], "invalid_argument", "错误码口径: {body}");

    // ---- 8. 反证锚点：窗口挪到三条之外必须一条都没有（否则上面的过滤都是恒真）
    let outside = f.page(json!({
        "from_ts": f.to_ts + 1_000_000,
        "to_ts": f.to_ts + 60_000_000,
    }));
    assert_eq!(total(&outside), 0, "窗口外应 0 条: {outside}");
    assert!(rows(&outside).is_empty());
}
