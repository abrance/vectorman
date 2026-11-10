//! IT-API-APM-01：`GET /v1/apm/services` 与 `/v1/apm/service-aliases` 的 CRUD 契约。
//!
//! 两个接口各自对应一条真实依赖：
//! - 服务清单读 `apm_service_endpoint`（span 接入时登记，APM 后台任务每秒 flush），
//!   所以必须先经真 `/v1/ingest` 写入带唯一服务名的 span，再轮询到它出现；
//! - 别名是新键派生 upsert（`alias_id = "{match_kind}:{match_value}"`），
//!   **重复 POST 是覆盖而不是 409**，PUT 只允许改 `service`/`enabled`/`note`，
//!   改 `match_kind`/`match_value` 直接 400（`bins/dataserver/src/http.rs::alias_update`）。
//!
//! 每条断言都带本用例唯一前缀过滤，避免被同库其它数据干扰；场景说明见
//! `test_plan/IT-API-APM-01.md`。不用前端、不做静默跳过。

use std::time::Duration;

use serde_json::{json, Value};

use vectorman_test_support::data;
use vectorman_test_support::http;
use vectorman_test_support::proc::{Dataserver, Options};
use vectorman_test_support::util;

const TIMEOUT: Duration = Duration::from_secs(30);

struct Fixture {
    /// 持有临时目录：`Dataserver` 只记路径，目录被删掉后 sqlite 文件就没了。
    _dir: tempfile::TempDir,
    ds: Dataserver,
    service: String,
    match_value: String,
    alias_id: String,
}

fn get_services(ds: &Dataserver) -> Value {
    http::get_json_ok(&format!("{}/v1/apm/services", ds.base()))
}

fn service_names(page: &Value) -> Vec<String> {
    page["services"]
        .as_array()
        .unwrap_or_else(|| panic!("应答缺 services 数组: {page}"))
        .iter()
        .map(|row| {
            row["service"]
                .as_str()
                .unwrap_or_else(|| panic!("服务行缺 service: {row}"))
                .to_string()
        })
        .collect()
}

/// 别名列表（只取 `process_prefix` 这一种，减少无关数据）。
fn get_aliases(ds: &Dataserver) -> Value {
    http::get_json_ok(&format!(
        "{}/v1/apm/service-aliases?match_kind=process_prefix",
        ds.base()
    ))
}

fn alias_rows(page: &Value) -> Vec<Value> {
    page["aliases"]
        .as_array()
        .unwrap_or_else(|| panic!("应答缺 aliases 数组: {page}"))
        .clone()
}

fn status_and_code(ds: &Dataserver, method: &str, path: &str, body: &Value) -> (u16, String) {
    let url = format!("{}{path}", ds.base());
    let (status, resp) = match method {
        "POST" => http::post_json(&url, &body.to_string()),
        "PUT" => http::put_json(&url, &body.to_string()),
        other => panic!("不支持的方法 {other}"),
    }
    .unwrap_or_else(|e| panic!("{method} {path}: {e}"));
    let code = serde_json::from_str::<Value>(&resp)
        .ok()
        .and_then(|v| v["code"].as_str().map(str::to_string))
        .unwrap_or_default();
    (status, code)
}

fn start() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    // `apm_enabled=false` 时这两个接口都直接 503，所以必须打开 APM。
    let ds = Dataserver::start(
        dir.path(),
        Options::local().with_apm(1),
        env!("CARGO_BIN_EXE_dataserver"),
    );
    let p = util::unique_prefix("api-apm");
    let service = format!("{p}-svc");
    let base = util::now_micros() - 300_000_000;

    // 一条带唯一 service 的 span：端点表由 APM 后台任务 flush 出来
    let seq = u64::from(std::process::id());
    let span = data::trace_span(
        seq * 8 + 1,
        0x401,
        &service,
        "GET /healthz",
        "server",
        "ok",
        base,
        1_000,
    );
    let envelope = data::traces_envelope(
        &format!("{p}-batch"),
        &format!("{p}-item"),
        &format!("{p}-agent"),
        &format!("{p}-host"),
        vec![span],
    );
    ds.ingest_ok(&envelope);

    let match_value = format!("{p}-proc");
    let alias_id = format!("process_prefix:{match_value}");
    Fixture {
        _dir: dir,
        ds,
        service,
        match_value,
        alias_id,
    }
}

#[test]
fn it_api_apm_services_and_alias_crud() {
    let f = start();

    // ---- 1. 服务清单：轮询到本用例服务出现（端点是每秒 flush 的派生数据）
    let page = util::wait_until("服务清单出现本用例服务", TIMEOUT, || {
        let page = get_services(&f.ds);
        service_names(&page)
            .iter()
            .any(|s| *s == f.service)
            .then_some(page)
    });
    let row = page["services"]
        .as_array()
        .expect("services")
        .iter()
        .find(|r| r["service"] == json!(f.service))
        .expect("刚轮询到的服务行不该消失");
    assert_eq!(
        row["instance_count"],
        json!(1),
        "单 span 应只有一个实例: {row}"
    );
    let instances = row["instances"].as_array().expect("instances 数组");
    assert_eq!(
        instances.len(),
        1,
        "实例数组长度应等于 instance_count: {row}"
    );
    assert_eq!(instances[0]["collector"], json!("otlp"));
    assert!(
        instances[0]["last_seen_ts"].as_i64().unwrap_or(0) > 0,
        "实例应有 last_seen_ts: {row}"
    );

    // ---- 2. 重复请求结果稳定（同一份数据两次读取逐字段相同）
    let again = get_services(&f.ds);
    let row_a = page["services"]
        .as_array()
        .expect("services")
        .iter()
        .find(|r| r["service"] == json!(f.service))
        .expect("服务行");
    let row_b = again["services"]
        .as_array()
        .expect("services")
        .iter()
        .find(|r| r["service"] == json!(f.service))
        .expect("服务行");
    assert_eq!(row_a, row_b, "同一数据连续两次请求应完全一致");

    // ---- 3. 别名：POST 建 → 列表含它
    let body = json!({
        "match_kind": "process_prefix",
        "match_value": f.match_value,
        "service": "svc-alias-one",
        "enabled": true,
        "note": "it-api-apm",
    });
    let (status, resp) = http::post_json(
        &format!("{}/v1/apm/service-aliases", f.ds.base()),
        &body.to_string(),
    )
    .expect("POST 别名");
    assert_eq!(status, 201, "新建别名应 201，实际 {status}: {resp}");
    let created: Value = serde_json::from_str(&resp).expect("别名 JSON");
    assert_eq!(created["alias_id"], json!(f.alias_id));
    assert_eq!(created["service"], json!("svc-alias-one"));
    assert_eq!(created["enabled"], json!(true));
    assert_eq!(created["note"], json!("it-api-apm"));

    let listed = get_aliases(&f.ds);
    let rows = alias_rows(&listed);
    let mine: Vec<&Value> = rows
        .iter()
        .filter(|r| r["alias_id"] == json!(f.alias_id))
        .collect();
    assert_eq!(mine.len(), 1, "列表里本用例别名应恰好 1 条: {listed}");
    assert_eq!(mine[0]["service"], json!("svc-alias-one"));

    // ---- 4. PUT 改 service → 列表反映新值（匹配条件不变，alias_id 不变）
    let (status, code) = status_and_code(
        &f.ds,
        "PUT",
        &format!("/v1/apm/service-aliases/{}", f.alias_id),
        &json!({
            "match_kind": "process_prefix",
            "match_value": f.match_value,
            "service": "svc-alias-two",
            "enabled": false,
            "note": "renamed",
        }),
    );
    assert_eq!(status, 200, "PUT 应 200，实际 {status}: code={code}");
    let listed = get_aliases(&f.ds);
    let mine = alias_rows(&listed)
        .into_iter()
        .find(|r| r["alias_id"] == json!(f.alias_id))
        .unwrap_or_else(|| panic!("PUT 后别名应仍在列表里: {listed}"));
    assert_eq!(
        mine["service"],
        json!("svc-alias-two"),
        "PUT 应改掉 service"
    );
    assert_eq!(mine["enabled"], json!(false), "PUT 应改掉 enabled");
    assert_eq!(mine["note"], json!("renamed"));

    // ---- 5. PUT 试图改 match_value → 400 invalid_argument（匹配条件是主键的一部分）
    let (status, code) = status_and_code(
        &f.ds,
        "PUT",
        &format!("/v1/apm/service-aliases/{}", f.alias_id),
        &json!({
            "match_kind": "process_prefix",
            "match_value": format!("{}-other", f.match_value),
            "service": "svc-alias-x",
        }),
    );
    assert_eq!(status, 400, "改 match_value 应 400，实际 {status}");
    assert_eq!(code, "invalid_argument", "错误码口径: {code}");

    // ---- 6. 重复 POST 同一匹配条件：**覆盖**（201），不是 409；列表仍只有 1 条
    let (status, resp) = http::post_json(
        &format!("{}/v1/apm/service-aliases", f.ds.base()),
        &json!({
            "match_kind": "process_prefix",
            "match_value": f.match_value,
            "service": "svc-alias-three",
        })
        .to_string(),
    )
    .expect("重复 POST 别名");
    assert_eq!(
        status, 201,
        "重复 POST 的既有行为是 upsert 覆盖，应仍 201，实际 {status}: {resp}"
    );
    let created: Value = serde_json::from_str(&resp).expect("别名 JSON");
    assert_eq!(created["alias_id"], json!(f.alias_id), "主键不变");
    assert_eq!(
        created["service"],
        json!("svc-alias-three"),
        "覆盖为新 service"
    );
    assert_eq!(created["enabled"], json!(true), "未传 enabled 时缺省 true");

    let listed = get_aliases(&f.ds);
    let rows = alias_rows(&listed);
    let count = rows
        .iter()
        .filter(|r| r["alias_id"] == json!(f.alias_id))
        .count();
    assert_eq!(count, 1, "覆盖后列表里仍应只有 1 条: {listed}");
    let row = rows
        .iter()
        .find(|r| r["alias_id"] == json!(f.alias_id))
        .expect("别名行");
    assert_eq!(row["service"], json!("svc-alias-three"));

    // ---- 7. DELETE → 列表不含；再删一次 → 404 not_found
    let url = format!("{}/v1/apm/service-aliases/{}", f.ds.base(), f.alias_id);
    let (status, resp) = http::delete(&url).unwrap_or_else(|e| panic!("DELETE 别名: {e}"));
    assert_eq!(status, 200, "DELETE 应 200，实际 {status}: {resp}");
    let deleted: Value = serde_json::from_str(&resp).expect("删除应答 JSON");
    assert_eq!(deleted["deleted"], json!(true));
    assert_eq!(deleted["alias_id"], json!(f.alias_id));

    let listed = get_aliases(&f.ds);
    assert!(
        alias_rows(&listed)
            .iter()
            .all(|r| r["alias_id"] != json!(f.alias_id)),
        "删除后列表不应再有该别名: {listed}"
    );

    let (status, resp) = http::delete(&url).unwrap_or_else(|e| panic!("DELETE 别名: {e}"));
    assert_eq!(status, 404, "重复删除应 404，实际 {status}: {resp}");
    let err: Value = serde_json::from_str(&resp).expect("错误应答 JSON");
    assert_eq!(err["code"], "not_found", "错误码口径: {resp}");

    // ---- 8. 非法 match_kind → 400 invalid_argument
    let (status, code) = status_and_code(
        &f.ds,
        "POST",
        "/v1/apm/service-aliases",
        &json!({"match_kind": "nope", "match_value": f.match_value, "service": "svc"}),
    );
    assert_eq!(status, 400, "非法 match_kind 应 400，实际 {status}");
    assert_eq!(code, "invalid_argument", "错误码口径: {code}");
}
