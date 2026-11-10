//! IT-VM-INGEST-01：`POST /v1/ingest` 的 metrics **真的写进 VictoriaMetrics**。
//!
//! 「落进 VM」只认 VM 原生查询口（`{select}/prometheus/api/v1/query`）：dataserver 的
//! `/api/v1/query` 是代理读，tsink/内存缓存能掩盖「根本没写进 VM」的问题，所以它只作为
//! 「两侧口径一致」的对照，不作为落库证据。
//!
//! 覆盖：批量点落库、标签补全（`agent_id`/`item_id`/`host_id`）、序列数增量、重复投递
//! 不重复入库、重启后 VM 数据不变。
//!
//! 场景说明见 `test_plan/IT-VM-INGEST-01.md`。不用前端、不做静默跳过。

use std::time::Duration;

use vectorman_test_support::proc::{Dataserver, Options};
use vectorman_test_support::{data, prom, util, vm};

const COUNT: usize = 5;
const AGENT_ID: &str = "agent-it-vm-ingest";
const ITEM_ID: &str = "item-it-vm-ingest";
const HOST_ID: &str = "host-it-vm-ingest";
const TIMEOUT: Duration = Duration::from_secs(60);

/// 把响应压成「标签集合 + 值」的排序快照，用来比对两个查询口的结果是否完全一致。
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
            (labels, format!("{:.9}", s.value))
        })
        .collect();
    out.sort();
    out
}

#[test]
fn it_vm_ingest_points_land_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::tempdir().expect("tempdir");
    let prefix = util::unique_prefix("vm-ingest");
    // 指标名不能带 `-`：`unique_prefix()` 里有，VM 会以 invalid metric name 400 拒掉。
    let metric = util::metric_name(&prefix, "gauge");
    let absent = util::metric_name(&prefix, "absent");
    // 写过去 2 分钟的整分点，避免和「此刻」的其它写入撞在同一时刻。
    let ts = util::floor_minute(util::now_micros()) - 120_000_000;
    let query_time = ts as f64 / 1_000_000.0 + 1.0;

    let mut ds = Dataserver::start(
        dir.path(),
        Options::local().with_vm(&vm),
        env!("CARGO_BIN_EXE_dataserver"),
    );

    // ---- 1. 一次写入 5 条（1..5，同一时刻、不同 idx 标签 = 5 条序列）
    let records = data::metric_series(&prefix, &metric, COUNT, 1.0, ts, 0);
    let unique_ids: Vec<String> = records
        .iter()
        .map(|r| r["record_id"].as_str().expect("record_id").to_string())
        .collect();
    let envelope = data::metrics_envelope("batch-1", ITEM_ID, AGENT_ID, HOST_ID, records.clone());

    let reply = ds.ingest(&envelope);
    assert_eq!(
        reply["accepted"].as_u64(),
        Some(COUNT as u64),
        "首轮 ingestion 应全部受理: {reply}"
    );

    // ---- 2. VM 原生口：恰好 5 个样本，值是 1..=5，标签等于信封值
    let got = vm.wait_query(&metric, Some(query_time), TIMEOUT, |r| r.len() == COUNT);
    let mut values: Vec<f64> = got.samples.iter().map(|s| s.value).collect();
    values.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
    assert_eq!(
        values,
        (1..=COUNT).map(|i| i as f64).collect::<Vec<_>>(),
        "VM 上的样本值应为 1..={COUNT}: raw={}",
        got.raw
    );
    for sample in &got.samples {
        assert_eq!(
            sample.metric.get("agent_id"),
            Some(&AGENT_ID.to_string()),
            "标签缺 agent_id: {}",
            got.raw
        );
        assert_eq!(
            sample.metric.get("item_id"),
            Some(&ITEM_ID.to_string()),
            "标签缺 item_id: {}",
            got.raw
        );
        assert_eq!(
            sample.metric.get("host_id"),
            Some(&HOST_ID.to_string()),
            "标签缺 host_id: {}",
            got.raw
        );
        assert!(
            sample.metric.contains_key("idx"),
            "记录自带的 tags 应保留: {}",
            got.raw
        );
    }

    // ---- 3. dataserver 的查询口与 VM **标签 + 值集合完全一致**（排序后比较）
    let proxied = prom::must_query(ds.prom(), &metric, Some(query_time));
    assert_eq!(
        snapshot(&proxied),
        snapshot(&got),
        "dataserver 代理口与 VM 原生口结果不一致（说明代理走了缓存或没真读 VM）"
    );

    // ---- 4. 序列数恰好 5：**只数本用例的 measurement**。
    //   不用 `/v1/ts/stats` 的 `series_count`——它是 VM 全实例的 `totalSeries`
    //   （`status/tsdb` 全局基数），任何并行写这台 VM 的用例都会污染它，
    //   `cargo test --all-features` 下必假红。
    let scoped = vm.wait_query(
        &format!("count by (idx) ({metric})"),
        Some(query_time),
        TIMEOUT,
        |r| r.len() == COUNT,
    );
    assert_eq!(
        scoped.samples.len(),
        COUNT,
        "本用例的 {metric} 应恰好 {COUNT} 条序列（每条记录一个 idx）: raw={}",
        scoped.raw
    );

    // ---- 5. 没写过的指标名查不到
    let none = vm.must_query(&absent, Some(query_time));
    assert!(
        none.is_empty(),
        "未写入的 {absent} 不该有样本: {}",
        none.raw
    );

    // ---- 6. 同批 record_id 再发一次（换 batch_id）→ VM 仍是 5 条
    //    注意：`accepted` 对重复记录**仍然 +1**（`apply_one` 里重复走 `Ok(())`），
    //    所以它不能当作去重证据；去重的硬证据是 VM 样本数不变。
    let replay = data::metrics_envelope("batch-2", ITEM_ID, AGENT_ID, HOST_ID, records.clone());
    let reply = ds.ingest(&replay);
    assert_eq!(
        reply["accepted"].as_u64(),
        Some(COUNT as u64),
        "重复记录仍会被受理（不作为去重证据）: {reply}"
    );
    let after_replay = vm.must_query(&metric, Some(query_time));
    assert_eq!(
        after_replay.len(),
        COUNT,
        "重复投递不应新增样本: {}",
        after_replay.raw
    );
    assert_eq!(
        snapshot(&after_replay),
        snapshot(&got),
        "重复投递不应改变任何样本"
    );
    assert_eq!(
        vm.must_query(&format!("count by (idx) ({metric})"), Some(query_time))
            .samples
            .len(),
        COUNT,
        "重复投递不应新增序列"
    );

    // ---- 7. 真 SIGTERM 重启（同 data_path）→ VM 上的数据不受影响
    ds.restart();
    let after_restart = vm.must_query(&metric, Some(query_time));
    assert_eq!(
        snapshot(&after_restart),
        snapshot(&got),
        "重启后 VM 数据必须与重启前逐点相同"
    );
    assert_eq!(
        after_restart.len(),
        COUNT,
        "重启后样本数应仍为 {COUNT}: {}",
        after_restart.raw
    );

    // 写进断言的是「唯一 record 数」，与 COUNT 一致；上面已断言 5 条序列、5 个值。
    assert_eq!(unique_ids.len(), COUNT);
}
