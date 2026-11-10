//! IT-API-LOGS-01：`POST /v1/logs/search` 的过滤契约（data_type / agent / level / message /
//! trace_id / 时间窗 / labels / limit）与两条空结果边界。
//!
//! 数据全部走真 `/v1/ingest`（`data_type=logs`），两批日志在信封层（agent / host / data_id）
//! 与记录层（level / message / 标签）都刻意区分，因此「某条过滤条件生效」可以用
//! 「返回集合恰好等于预期那几条」来判定，而不是「非空就算过」。
//!
//! 实现口径（`crates/dataplane-log/src/lib.rs::collect_hits_paged` 与
//! `crates/dataplane-ingest/src/lib.rs::search`）：
//! - 结果按 `timestamp` **倒序**返回；
//! - `message_query` 是 jieba 分词后的 **AND 词项**匹配，不是字符串子串匹配
//!   （所以本用例用整词断言，不断言「词的一部分」）；
//! - 响应体只有 `records`，**没有** `total` 字段（边界断言改用 `records` 为空）。
//!
//! 场景说明见 `test_plan/IT-API-LOGS-01.md`。不用前端、不做静默跳过。

use serde_json::{json, Value};

use vectorman_test_support::http;
use vectorman_test_support::proc::{Dataserver, Options};
use vectorman_test_support::{data, util};

struct Fixture {
    /// 持有临时目录：`Dataserver` 只记路径，目录被删掉后日志索引的 lockfile 就没了。
    _dir: tempfile::TempDir,
    ds: Dataserver,
    agent_a: String,
    agent_b: String,
    host_a: String,
    host_b: String,
    data_a: String,
    data_b: String,
    trace_id: String,
    /// 第一批（A）两条的时间戳，从早到晚。
    ts_a1: i64,
    ts_a2: i64,
    /// 第二批（B）一条的时间戳。
    ts_b1: i64,
}

fn start() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let ds = Dataserver::start(
        dir.path(),
        Options::local(),
        env!("CARGO_BIN_EXE_dataserver"),
    );
    let p = util::unique_prefix("api-logs");
    // 日志时间取「10 分钟前」起，避免与「此刻」的其它写入落在同一时刻。
    let base = util::now_micros() - 600_000_000;

    let agent_a = format!("{p}-agent-a");
    let agent_b = format!("{p}-agent-b");
    let host_a = format!("{p}-host-a");
    let host_b = format!("{p}-host-b");
    let data_a = format!("{p}-item-a");
    let data_b = format!("{p}-item-b");
    // 32 位小写 hex：日志聚合（`data_type=apm`）才会写入 trace 标签，这里手动打标，
    // 用来验证 `trace_id` 过滤确实走索引字段。
    let trace_id = format!("{:032x}", std::process::id());

    let ts_a1 = base;
    let ts_a2 = base + 1_000_000;
    let ts_b1 = base + 120_000_000;

    let batch_a = data::logs_envelope(
        &format!("{p}-batch-a"),
        &data_a,
        &agent_a,
        &host_a,
        vec![
            data::log_record_with_labels(
                &format!("{p}-a1"),
                ts_a1,
                "info",
                "checkout started",
                [("trace_id", trace_id.as_str()), ("app", "shop")],
            ),
            data::log_record_with_labels(
                &format!("{p}-a2"),
                ts_a2,
                "error",
                "checkout timeout after 30s",
                [("trace_id", trace_id.as_str()), ("app", "shop")],
            ),
        ],
    );
    let batch_b = data::logs_envelope(
        &format!("{p}-batch-b"),
        &data_b,
        &agent_b,
        &host_b,
        vec![data::log_record_with_labels(
            &format!("{p}-b1"),
            ts_b1,
            "warn",
            "database connected",
            [("app", "db")],
        )],
    );

    ds.ingest_ok(&batch_a);
    ds.ingest_ok(&batch_b);

    Fixture {
        _dir: dir,
        ds,
        agent_a,
        agent_b,
        host_a,
        host_b,
        data_a,
        data_b,
        trace_id,
        ts_a1,
        ts_a2,
        ts_b1,
    }
}

impl Fixture {
    fn search(&self, query: Value) -> Value {
        let (status, body) = self.ds.post("/v1/logs/search", &query.to_string());
        assert_eq!(
            status,
            200,
            "/v1/logs/search 应 200，实际 {status}: {}（query={query}）",
            http::tail(&body)
        );
        serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("/v1/logs/search 应答非 JSON（{e}）: {body}"))
    }

    /// 返回记录 id 列表（响应按 timestamp 倒序）。
    fn ids(&self, query: Value) -> Vec<String> {
        let resp = self.search(query);
        let records = resp["records"]
            .as_array()
            .unwrap_or_else(|| panic!("应答缺 records 数组: {resp}"));
        records
            .iter()
            .map(|r| {
                r["id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("记录缺 id: {r}"))
                    .to_string()
            })
            .collect()
    }
}

#[test]
fn it_api_logs_search_filters_and_boundaries() {
    let f = start();
    let p = util::unique_prefix("api-logs");
    let a1 = format!("{p}-a1");
    let a2 = format!("{p}-a2");
    let b1 = format!("{p}-b1");

    // ---- 0. 基线：全库只有这三条（独立进程 + 独立数据目录）
    let all = f.ids(json!({"limit": 100}));
    assert_eq!(
        all.len(),
        3,
        "独立 dataserver 里应只有本用例写入的 3 条日志: {all:?}"
    );

    // ---- 1. data_type 过滤：三条都是 logs
    assert_eq!(f.ids(json!({"data_type": "logs", "limit": 100})).len(), 3);
    assert!(
        f.ids(json!({"data_type": "metrics"})).is_empty(),
        "data_type=metrics 不应命中 logs 记录"
    );

    // ---- 2. agent_id / host_id / data_id 各自只命中自己那批
    assert_eq!(
        f.ids(json!({"agent_id": f.agent_a})),
        vec![a2.clone(), a1.clone()]
    );
    assert_eq!(f.ids(json!({"agent_id": f.agent_b})), vec![b1.clone()]);
    assert_eq!(
        f.ids(json!({"host_id": f.host_a})),
        vec![a2.clone(), a1.clone()]
    );
    assert_eq!(f.ids(json!({"host_id": f.host_b})), vec![b1.clone()]);
    assert_eq!(
        f.ids(json!({"data_id": f.data_a})),
        vec![a2.clone(), a1.clone()]
    );
    assert_eq!(f.ids(json!({"data_id": f.data_b})), vec![b1.clone()]);

    // ---- 3. level 精确匹配
    assert_eq!(f.ids(json!({"level": "error"})), vec![a2.clone()]);
    assert_eq!(f.ids(json!({"level": "warn"})), vec![b1.clone()]);
    assert!(
        f.ids(json!({"level": "ERROR"})).is_empty(),
        "level 是精确匹配，大小写不同不应命中"
    );

    // ---- 4. message_query：整词命中（jieba 分词 AND，不是子串）
    assert_eq!(f.ids(json!({"message_query": "timeout"})), vec![a2.clone()]);
    assert_eq!(
        f.ids(json!({"message_query": "connected"})),
        vec![b1.clone()]
    );
    // 两个词分别属于两条记录 → AND 后无命中
    let both = f.ids(json!({"message_query": "timeout connected"}));
    assert!(
        both.is_empty(),
        "两个词分属不同记录，AND 语义下应为空: {both:?}"
    );

    // ---- 5. trace_id 过滤：只有第一批两条带该标签
    assert_eq!(
        f.ids(json!({"trace_id": f.trace_id})),
        vec![a2.clone(), a1.clone()]
    );

    // ---- 6. 时间窗：只框住第一批
    let window_a = f.ids(json!({"from_ts": f.ts_a1, "to_ts": f.ts_a2}));
    assert_eq!(
        window_a,
        vec![a2.clone(), a1.clone()],
        "时间窗应恰好等于第一批两条"
    );
    let window_b = f.ids(json!({"from_ts": f.ts_b1, "to_ts": f.ts_b1}));
    assert_eq!(window_b, vec![b1.clone()], "闭区间应命中端点那一秒的记录");

    // ---- 7. labels 组合过滤（`app` 不是提升字段，走 post-filter）
    assert_eq!(
        f.ids(json!({"labels": {"app": "shop"}})),
        vec![a2.clone(), a1.clone()]
    );
    assert_eq!(f.ids(json!({"labels": {"app": "db"}})), vec![b1.clone()]);
    assert!(
        f.ids(json!({"labels": {"app": "shop", "missing": "x"}}))
            .is_empty(),
        "labels 是全部命中的 AND 语义"
    );

    // ---- 8. 多条件组合
    assert_eq!(
        f.ids(json!({
            "agent_id": f.agent_a,
            "level": "error",
            "message_query": "timeout",
            "labels": {"app": "shop"},
        })),
        vec![a2.clone()]
    );

    // ---- 9. limit 精确截断：倒序第一条是最新的那条
    assert_eq!(
        f.ids(json!({"agent_id": f.agent_a, "limit": 1})),
        vec![a2.clone()]
    );
    assert_eq!(
        f.ids(json!({"agent_id": f.agent_a, "limit": 2})),
        vec![a2.clone(), a1.clone()]
    );

    // ---- 10. 边界 1：不存在的 trace_id / 时间窗外 → 200 且结果为空（不是报错）
    let empty = f.search(json!({"trace_id": format!("{:032x}", 0)}));
    let records = empty["records"].as_array().expect("records 数组");
    assert!(records.is_empty(), "不存在的 trace_id 应返回空: {empty}");
    // 该接口没有 total 字段，口径以 records 为准
    assert!(
        empty.get("total").is_none(),
        "/v1/logs/search 响应本来就没有 total 字段: {empty}"
    );

    let outside = f.ids(json!({
        "from_ts": f.ts_b1 + 3_600_000_000i64,
        "to_ts": f.ts_b1 + 7_200_000_000i64,
    }));
    assert!(outside.is_empty(), "时间窗外应返回空: {outside:?}");

    // ---- 11. 边界 2：from_ts > to_ts 是参数错误（400 invalid_argument），不是空结果
    let (status, body) = f.ds.post(
        "/v1/logs/search",
        &json!({"from_ts": f.ts_a2, "to_ts": f.ts_a1}).to_string(),
    );
    assert_eq!(status, 400, "from_ts > to_ts 应 400，实际 {status}: {body}");
    let code: Value = serde_json::from_str(&body).expect("错误应答 JSON");
    assert_eq!(code["code"], "invalid_argument", "错误码口径: {body}");
}
