//! IT-VM-AGENT-01：真 `gse-agent` 采到的主机指标**经真 dataserver 落进 VictoriaMetrics**。
//!
//! 这是「采集 → 接入 → 外置时序库」的最短真链路：gse-server 下发 `metrics_host` 采集项，
//! agent 每 1s 读 `/proc` 产出 `cpu_usage` / `mem_usage`，上报到 dataserver `/v1/ingest`，
//! dataserver 以 VM 为 ts 后端写出。
//!
//! 三个断言各自防一种真实故障：
//! 1. VM 原生口能查到 `cpu_usage{agent_id,item_id}` 且值在 `0..=100` —— 采集与落库都通（值域错
//!    说明 `/proc` 解析或百分比换算坏了）。
//! 2. dataserver 代理口能看到同一条序列 —— 读路径与写路径口径一致。
//! 3. `<data_path>/ts` 不存在或为空 —— 证明这一轮**没走本地 tsink**，不是「两边都写」把
//!    「VM 没写成功」掩盖掉。
//!
//! 场景说明见 `test_plan/IT-VM-AGENT-01.md`。不用前端、不做静默跳过。

use std::time::Duration;

use vectorman_test_support::gse::Gse;
use vectorman_test_support::proc::{Dataserver, Options};
use vectorman_test_support::{prom, vm};

const METRIC: &str = "cpu_usage";
const TIMEOUT: Duration = Duration::from_secs(120);

/// 指标名是产品固定的 `cpu_usage`，隔离只能落在 `agent_id` / `item_id` 上：
/// 带上 pid，同一台机器上前后两次运行（或 CI 复用同一个 VM）不会互相看到对方的序列。
fn agent_id() -> String {
    format!("agent-it-vm-agent-{}", std::process::id())
}

fn item_id() -> String {
    format!("item-it-vm-agent-{}", std::process::id())
}

fn promql() -> String {
    format!(
        "{METRIC}{{agent_id=\"{}\",item_id=\"{}\"}}",
        agent_id(),
        item_id()
    )
}

/// 标签集合 + 值 的排序快照（不含时间戳，两个查询口取点时刻不同）。
fn snapshot(resp: &prom::Response) -> Vec<(Vec<(String, String)>, String)> {
    let mut out: Vec<(Vec<(String, String)>, String)> = resp
        .samples
        .iter()
        .map(|s| {
            let labels: Vec<(String, String)> = s
                .metric
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            (labels, format!("{:.6}", s.value))
        })
        .collect();
    out.sort();
    out
}

#[test]
fn it_vm_agent_host_metrics_land_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    // ---- 1. 真 gse-server（台账）+ 真 dataserver（ts 后端 = VM，管理口反代到 gse）
    let agent_id = agent_id();
    let item_id = item_id();
    let token = format!("it-vm-agent-token-{}", std::process::id());

    let mut gse = Gse::start(root.join("gse"));
    let ds = Dataserver::start(
        root.join("ds"),
        Options::local()
            .with_vm(&vm)
            .with_gse_admin(gse.admin_url()),
        env!("CARGO_BIN_EXE_dataserver"),
    );

    // ---- 2. 台账：唯一 agent、数据面指向 dataserver、1s 一轮的主机指标采集项
    gse.register_agent(&agent_id, "host-it-vm-agent", &token);
    gse.register_dataplane("ds-it-vm-agent", ds.base(), ds.prom());
    gse.put_host_metrics_spec(&agent_id, &item_id, "it-vm-agent host metrics", 1);

    // ---- 3. 真 gse-agent：上线 → 生效 spec → 开始按采集项上报
    gse.start_agent(&agent_id, &token);
    gse.wait_agent_online(&agent_id);
    gse.wait_spec_synced(&agent_id, &item_id);

    // ---- 4. VM 原生口能看到这条序列，且值落在 0..=100
    let query = promql();
    let got = vm.wait_query(&query, None, TIMEOUT, |r| !r.is_empty());
    for sample in &got.samples {
        assert_eq!(
            sample.metric.get("agent_id"),
            Some(&agent_id),
            "agent_id 标签必须等于台账里的 agent: {}",
            got.raw
        );
        assert_eq!(
            sample.metric.get("item_id"),
            Some(&item_id),
            "item_id 标签必须等于采集项: {}",
            got.raw
        );
        assert!(
            (0.0..=100.0).contains(&sample.value),
            "{METRIC} 是百分比，必须在 0..=100，实际 {}: {}",
            sample.value,
            got.raw
        );
    }

    // ---- 5. dataserver 代理口能看到同一条序列（标签与值一致）
    let proxied = prom::must_query(ds.prom(), &query, None);
    assert_eq!(
        snapshot(&proxied),
        snapshot(&got),
        "dataserver 代理口与 VM 原生口的序列应一致（说明读路径没走缓存或本地 tsink）"
    );

    // ---- 6. 没走本地 tsink：`<data_path>/ts` 不存在或为空
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

    // ---- 7. 三个进程全程没有异常退出
    gse.check_alive();
}
