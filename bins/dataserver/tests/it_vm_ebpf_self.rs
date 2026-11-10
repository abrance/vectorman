//! `IT-VM-EBPF-SELF-01`：Agent eBPF 自监控（`agent_ebpf_*` 12 条 + `agent_ebpf_capability`）
//! → dataserver → **真假 VM**。
//!
//! 产物来自 `gse-agent-ebpf::interval_records`（三个采集循环共用的「每轮必报」），所以点数、
//! `record_id` 规则、标签集合与线上一致；计数器值由夹具填成 `1..=10`，行串位就会红。
//!
//! 两个必须按**真实落库形态**断言的口径（写错就会「断言一个 VM 里不存在的东西」）：
//!
//! - `agent_ebpf_capability` 的 `field_name` 是 `available`，但 VM 后端只写
//!   `measurement{tags} value ts`（`encode_point`），`field_name` **在 VM 里会丢**。所以断言点是
//!   指标名 + `value`（0/1），不是 `available` 序列，也不能断言 `field_name` 标签。
//! - TP 环境没有 root/`CAP_BPF`，capability 的真实形态就是「不满足」→ `value=0` 且 `reason`
//!   有值。这不是伪造：Agent 起不来 eBPF 时上报的就是这条降级路径。
//!
//! 场景定义见 `test_plan/IT-VM-EBPF-SELF-01.md`。不用前端、不做静默跳过。

use std::time::Duration;

use vectorman_test_support::{
    agent, data,
    proc::{Dataserver, Options},
    prom, util, vm,
};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");
const TIMEOUT: Duration = Duration::from_secs(60);
const CAPABILITY: &str = "agent_ebpf_capability";

/// `stats_metrics` 的产出顺序 + 夹具填的值（`agent::agent_ebpf_interval_records` 注释）。
const COUNTERS: [(&str, f64); 10] = [
    ("agent_ebpf_flushes_total", 1.0),
    ("agent_ebpf_edges_total", 2.0),
    ("agent_ebpf_metric_points_total", 3.0),
    ("agent_ebpf_filtered_total", 4.0),
    ("agent_ebpf_idle_keys_total", 5.0),
    ("agent_ebpf_read_errors_total", 6.0),
    ("agent_ebpf_map_overflow_dropped_total", 7.0),
    ("agent_ebpf_rate_limited_total", 8.0),
    ("agent_ebpf_buffer_dropped_total", 9.0),
    ("agent_ebpf_edges_skipped_total", 10.0),
];
/// 期望值：`set_cpu` 按 ×100 存整数再除回来，12.34 能原样回来（12.345 会被截断）。
const CPU_PERCENT: f64 = 12.34;

fn scalar(resp: &prom::Response, what: &str) -> f64 {
    assert_eq!(
        resp.samples.len(),
        1,
        "{what} 期望恰好 1 个样本，实际 {}: raw={}",
        resp.samples.len(),
        resp.raw
    );
    resp.first().value
}

#[test]
fn agent_ebpf_self_metrics_land_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::TempDir::new().expect("tempdir");
    let ds = Dataserver::start(dir.path(), Options::local().with_vm(&vm), BIN);

    let prefix = util::unique_prefix("eself");
    let agent_id = format!("{prefix}-agent");
    let item_id = format!("{prefix}-item");
    let host_id = format!("{prefix}-host");
    let kernel_release = format!("{prefix}-kernel");
    let reason = format!("{prefix}-no-cap-bpf");

    let report = agent::unavailable_report(&kernel_release, &reason);
    // 采集侧采样值是 12.34（`set_cpu` 内部存 1234，再除回 12.34）。
    let records = agent::agent_ebpf_interval_records(&agent_id, &item_id, 12.34, true, &report);
    assert_eq!(
        records.len(),
        13,
        "10 条计数器 + cpu_percent + degraded + capability = 13: {records:?}"
    );
    ds.ingest_ok(&data::metrics_envelope(
        &format!("{prefix}-b1"),
        &item_id,
        &agent_id,
        &host_id,
        records,
    ));

    // 1) 10 条计数器逐条断言（值 1..=10 互不相同，行串位必红）。
    for (measurement, expected) in COUNTERS {
        let promql = format!("{measurement}{{agent_id=\"{agent_id}\"}}");
        let resp = vm.wait_query(&promql, None, TIMEOUT, |r| !r.is_empty());
        assert_eq!(scalar(&resp, measurement), expected, "raw={}", resp.raw);
    }

    // 2) 两个 gauge：CPU 百分比（×100 存整数后还原）与降级标记。
    let cpu = vm.wait_query(
        &format!("agent_ebpf_cpu_percent{{agent_id=\"{agent_id}\"}}"),
        None,
        TIMEOUT,
        |r| !r.is_empty(),
    );
    assert!((scalar(&cpu, "agent_ebpf_cpu_percent") - CPU_PERCENT).abs() < 1e-9);
    let degraded = vm.must_query(
        &format!("agent_ebpf_degraded{{agent_id=\"{agent_id}\"}}"),
        None,
    );
    assert_eq!(scalar(&degraded, "agent_ebpf_degraded"), 1.0);

    // 3) capability：按 VM 真实形态断言（指标名 + value + 维度），`field_name` 已丢。
    let promql = format!(
        "{CAPABILITY}{{agent_id=\"{agent_id}\",item_id=\"{item_id}\",kernel_release=\"{kernel_release}\",kernel_ok=\"true\",btf_ok=\"true\",capability_ok=\"false\",reason=\"{reason}\"}}"
    );
    let cap = vm.wait_query(&promql, None, TIMEOUT, |r| !r.is_empty());
    assert_eq!(scalar(&cap, CAPABILITY), 0.0, "raw={}", cap.raw);
    let sample = cap.first();
    assert!(
        !sample.metric.contains_key("field_name"),
        "VM 只写 measurement{{tags}} value ts，不该有 field_name 标签: {:?}",
        sample.metric
    );
    // `field_name="available"` 不能变成另一条指标名（后端不落 field_name）。
    let as_name = vm.must_query(&format!("available{{agent_id=\"{agent_id}\"}}"), None);
    assert!(
        as_name.is_empty(),
        "不该存在 available 序列: raw={}",
        as_name.raw
    );

    // 4) 这个 agent 名下正好 13 条序列：少了说明有点没落库，多了说明重复上报。
    let all = vm.must_query(&format!("count({{agent_id=\"{agent_id}\"}})"), None);
    assert_eq!(scalar(&all, "序列数"), 13.0, "raw={}", all.raw);
}
