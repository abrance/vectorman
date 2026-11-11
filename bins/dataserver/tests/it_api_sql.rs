//! IT-API-SQL-01：`POST /v1/sql` 的建表 / 占位参数插入 / 回读契约，以及三条错误路径。
//!
//! 此前 tp 只覆盖「写入 → 落进 VM」，dataserver 自己的 SQL 口没有任何端到端断言：
//! 列名与顺序、类型往返（int 不变成字符串、null 保持 null）、错误码口径全是空白。
//!
//! 本用例只走 HTTP：一个独立 dataserver（`Options::local()` + 临时目录），表名带 pid，
//! 与同库其它数据不共享。场景说明见 `test_plan/IT-API-SQL-01.md`。
//!
//! 错误码按实现口径断言（`bins/dataserver/src/http.rs::sql_exec`）：
//! - 缺 `sql` 字段 / JSON 语法错 → 400 `invalid_argument`（axum `JsonRejection` 在这里被显式映射）；
//! - SQL 执行错误（含语法错） → **422** `query_failed`（不是 400）；
//! - 参数是 object/array → 400 `invalid_argument`（`json_params_to_sql_values` 只放行
//!   `{"b64": "..."}` 形式的 blob，其它 object/array 一律拒绝）。
//!
//! 不用前端、不做静默跳过。

use serde_json::{json, Value};

use vectorman_test_support::proc::{Dataserver, Options};

/// 表名不能带 `-`：`unique_prefix()` 里带（`tp-api-sql-<pid>`），SQLite 会把 `-` 当减号。
fn table_name() -> String {
    format!("tp_api_sql_{}", std::process::id())
}

/// `POST /v1/sql` 原样返回（不 panic，供错误路径断言）。
fn post_sql(ds: &Dataserver, body: &Value) -> (u16, String) {
    ds.post("/v1/sql", &body.to_string())
}

/// 错误应答里的稳定错误码（`{"error": ..., "code": ...}`）。
fn code_of(body: &str) -> String {
    let parsed: Value =
        serde_json::from_str(body).unwrap_or_else(|e| panic!("错误应答非 JSON（{e}）: {body}"));
    parsed["code"]
        .as_str()
        .unwrap_or_else(|| panic!("错误应答缺 code 字段: {body}"))
        .to_string()
}

#[test]
fn it_api_sql_create_insert_select_and_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ds = Dataserver::start(
        dir.path(),
        Options::local(),
        env!("CARGO_BIN_EXE_dataserver"),
    );
    let table = table_name();

    // ---- 1. 建表：成功时 columns/rows 都为空数组
    let created = ds.sql(
        &format!(
            "CREATE TABLE {table} (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL, note TEXT)"
        ),
        &[],
    );
    assert_eq!(
        created,
        json!({"columns": [], "rows": []}),
        "CREATE TABLE 的成功应答应为空 columns/rows"
    );

    // ---- 2. 占位参数插入：int / real / string / null 各至少一次
    ds.sql(
        &format!("INSERT INTO {table} (id, name, score, note) VALUES (?1, ?2, ?3, ?4)"),
        &[json!(1), json!("alpha"), json!(1.5), json!(null)],
    );
    ds.sql(
        &format!("INSERT INTO {table} (id, name, score, note) VALUES (?1, ?2, ?3, ?4)"),
        &[json!(2), json!("beta"), json!(0.25), json!("note-b")],
    );

    // ---- 3. 回读：列名与顺序精确、行逐行逐值相等
    let rows = ds.sql(
        &format!("SELECT id, name, score, note FROM {table} ORDER BY id"),
        &[],
    );
    assert_eq!(
        rows["columns"],
        json!(["id", "name", "score", "note"]),
        "列名与顺序应等于 SELECT 列表"
    );
    assert_eq!(
        rows["rows"],
        json!([[1, "alpha", 1.5, null], [2, "beta", 0.25, "note-b"]]),
        "回读结果应逐行逐值等于写入值"
    );

    // 类型往返：int 仍是整型数（不是 "1"），real 是浮点，null 仍是 null
    let got = rows["rows"].as_array().expect("rows 必须是数组");
    assert_eq!(got.len(), 2, "应回读 2 行: {rows}");
    assert_eq!(
        got[0][0].as_i64(),
        Some(1),
        "INTEGER 引应回读为整数: {rows}"
    );
    assert!(
        got[0][0].as_str().is_none(),
        "INTEGER 不应变成字符串: {rows}"
    );
    assert_eq!(got[0][2].as_f64(), Some(1.5), "REAL 应回读为浮点: {rows}");
    assert!(got[0][3].is_null(), "NULL 应保持 null: {rows}");
    assert_eq!(got[1][1].as_str(), Some("beta"), "TEXT 应逐字相等: {rows}");

    // ---- 4. 错误路径 1：缺 `sql` 字段 → 400 invalid_argument
    let (status, body) = post_sql(&ds, &json!({"params": []}));
    assert_eq!(status, 400, "缺 sql 字段应 400，实际 {status}: {body}");
    assert_eq!(code_of(&body), "invalid_argument", "错误码口径: {body}");

    // ---- 5. 错误路径 2：SQL 语法错 → 422 query_failed（不是 400）
    let (status, body) = post_sql(&ds, &json!({"sql": "SELEC 1"}));
    assert_eq!(status, 422, "SQL 语法错应 422，实际 {status}: {body}");
    assert_eq!(code_of(&body), "query_failed", "错误码口径: {body}");

    // ---- 6. 错误路径 3：参数放 object / array → 400 invalid_argument
    for bad in [json!({"foo": "bar"}), json!([1, 2, 3])] {
        let (status, body) = post_sql(&ds, &json!({"sql": "SELECT ?1", "params": [bad]}));
        assert_eq!(
            status, 400,
            "非法参数类型 {bad} 应 400，实际 {status}: {body}"
        );
        assert_eq!(code_of(&body), "invalid_argument", "错误码口径: {body}");
    }

    // 反面对照：`{"b64": ...}` 是唯一被放行的 object 形式，说明上面的拒绝是有针对性的
    let blob = ds.sql("SELECT ?1", &[json!({"b64": "aGk="})]);
    assert_eq!(
        blob["rows"],
        json!([[{"b64": "aGk="}]]),
        "blob 参数应原样往返: {blob}"
    );

    // ---- 7. 清理：DROP TABLE 后同一张表必须查不到（422），证明表真的没了
    ds.sql(&format!("DROP TABLE {table}"), &[]);
    let (status, body) = post_sql(&ds, &json!({"sql": format!("SELECT id FROM {table}")}));
    assert_eq!(
        status, 422,
        "DROP TABLE 后查询应 422，实际 {status}: {body}"
    );
    assert_eq!(code_of(&body), "query_failed", "错误码口径: {body}");

    // 表名带 pid，删除后不影响同库其它表；这里顺带确认 sqlite 元数据里没有残留
    let leftovers = ds.sql(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
        &[json!(table)],
    );
    assert_eq!(
        leftovers["rows"],
        json!([]),
        "DROP TABLE 后 sqlite_master 不应再有这张表: {leftovers}"
    );
}
