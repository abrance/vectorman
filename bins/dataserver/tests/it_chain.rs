//! IT-CHAIN-01：gse-server → dataserver → gse-agent **三个真进程**的端到端链路。
//!
//! 断言顺序与真实链路一致：agent 认证后上线 → 解析并生效 GSE 下发的采集项 →
//! 按采集项把 metrics 上报到 dataserver `/v1/ingest` → Prom 即时查询回读带
//! `agent_id` / `item_id` 标签的点。
//!
//! 场景说明（前置数据、步骤、期望、清理）见 `test_plan/IT-CHAIN-01.md`。
//!
//! 编排（写配置、起进程、台账登记、等 online/synced）在
//! `vectorman_test_support::gse`，与 IT-VM-AGENT-01 共用；本文件只写这条链路的断言。
//!
//! 设计约束：
//! - 不依赖固定端口（全部 `127.0.0.1:0` 现取现用）与外部中间件（redb + tsink 本地后端）。
//! - 不用 `#[ignore]`、不做「缺东西就 return」的静默跳过：缺二进制或启动失败直接 panic 并带上
//!   子进程的 stdout/stderr，避免「假绿」。
//! - 子进程由夹具的 `Drop` 回收（kill + wait）。

use std::time::Duration;

use serde_json::Value;
use vectorman_test_support::gse::Gse;
use vectorman_test_support::http;
use vectorman_test_support::proc::{Dataserver, Options};
use vectorman_test_support::util;

const AGENT_ID: &str = "agent-it-chain";
const TOKEN: &str = "it-chain-token";
const ITEM_ID: &str = "item-it-chain";
const METRIC: &str = "cpu_usage";

// ---------------------------------------------------------------- 用例

#[test]
fn it_chain_agent_online_ingest_and_query_with_labels() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    // ---- 1. gse-server 真进程（临时台账 sqlite），起好即等到 /health
    let mut gse = Gse::start(root.join("gse"));

    // ---- 2. dataserver 真进程（redb + tsink，本地目录；管理口反代到 gse-server）
    let ds = Dataserver::start(
        root.join("ds"),
        Options::local().with_gse_admin(gse.admin_url()),
        env!("CARGO_BIN_EXE_dataserver"),
    );
    let ds_prom_url = ds.prom().to_string();

    // ---- 3. 台账登记 agent（认证前必须存在：auth 只 UPDATE status）
    gse.register_agent(AGENT_ID, "host-it-chain", TOKEN);

    // ---- 4. 登记 dataserver 为数据面，等探活 online（agent 才会拿到 ingest_url）
    gse.register_dataplane("ds-it-chain", ds.base(), ds.prom());

    // ---- 5. 写该 Agent 的期望 spec：1s 一轮的主机指标采集项
    gse.put_host_metrics_spec(AGENT_ID, ITEM_ID, "it-chain host metrics", 1);

    // ---- 6. gse-agent 真进程，并等它上线、生效下发 spec
    gse.start_agent(AGENT_ID, TOKEN);
    gse.wait_agent_online(AGENT_ID);
    gse.wait_spec_synced(AGENT_ID, ITEM_ID);

    // ---- 7. 数据落到时序库：Prom 即时查询能回读到带标签的点
    let (value, labels) = gse.wait_for("Prom 查到 cpu_usage 数据点", |_| {
        let url = format!(
            "{ds_prom_url}/api/v1/query?query={METRIC}&time={}",
            util::now_secs()
        );
        let (status, body) = http::get(&url)?;
        let value: Value = serde_json::from_str(&body)
            .map_err(|e| format!("Prom 查询返回非 JSON: {e}: {}", http::tail(&body)))?;
        if status != 200 {
            return Err(format!("Prom 查询返回 {status}: {value}"));
        }
        assert_eq!(value["status"], "success", "Prom 查询失败: {value}");
        let Some(result) = value["data"]["result"].as_array() else {
            return Err(format!("data.result 不是数组: {value}"));
        };
        let Some(first) = result.first() else {
            return Err("data.result 为空（尚未入库）".to_string());
        };
        let metric = first["metric"].clone();
        if metric["agent_id"] != AGENT_ID || metric["item_id"] != ITEM_ID {
            return Err(format!("标签不匹配: {metric}"));
        }
        let Some(sample) = first["value"].as_array().and_then(|v| v.get(1)) else {
            return Err(format!("value 结构异常: {first}"));
        };
        // Prom 的 `value[1]` 是 JSON 数字（字符串形式也一并接受，避免不同实现差异）。
        let parsed = sample
            .as_f64()
            .or_else(|| sample.as_str().and_then(|s| s.parse::<f64>().ok()));
        if parsed.is_none() {
            return Err(format!("value 不是数字: {sample}"));
        }
        Ok((parsed.expect("checked"), metric))
    });
    assert!(
        (0.0..=100.0).contains(&value),
        "cpu_usage 超出 0..=100: {value}"
    );
    assert_eq!(labels["agent_id"], AGENT_ID);
    assert_eq!(labels["item_id"], ITEM_ID);

    // ---- 8. 全程三个进程都没有异常退出
    gse.check_alive();
}
