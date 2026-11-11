//! `IT-VM-EBPF-SYSCALL-01`：`ebpf_syscall_duration_micros{field="avg"|"p95"}` 与
//! `ebpf_syscall_failures_total{op,errno}` → dataserver → **真假 VM**。
//!
//! 产物来自 `gse-agent-ebpf::syscall::duration_metrics` / `failure_metrics`（Agent 直接产出这两类
//! 指标，dataserver 只补 `service`），本用例调的是这两个产品函数，所以 `p95` 的「直方图槽上界」
//! 口径、`field` 维度、`errno` 十进制字符串都是线上真实形态。
//!
//! 已覆盖过的只有 `avg`（`IT-VM-PROCESS-01` 里那条只为验 `service` 维度），两个维度的
//! 取值从没断言过：`field`/`op` 一旦写反，前端「慢调用 P95」图会静默显示成均值。
//!
//! 为什么不起真 eBPF：syscall 采集项同样要 root + `CAP_BPF` + BTF。TP 覆盖
//! 「Agent 产出 → `/v1/ingest` → VM」，内核态 per-CPU 聚合由 crate 单元测试覆盖。
//!
//! 场景定义见 `test_plan/IT-VM-EBPF-SYSCALL-01.md`。不用前端、不做静默跳过。

use std::time::Duration;

use vectorman_test_support::{
    agent, data,
    proc::{Dataserver, Options},
    prom, util, vm,
};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");
const TIMEOUT: Duration = Duration::from_secs(60);
const DURATION: &str = "ebpf_syscall_duration_micros";
const FAILURES: &str = "ebpf_syscall_failures_total";

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
fn ebpf_syscall_metrics_land_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::TempDir::new().expect("tempdir");
    // APM 关闭：本用例只验 Agent 自产的 op/field/errno 维度（`service` 补全与它无关）。
    let ds = Dataserver::start(dir.path(), Options::local().with_vm(&vm), BIN);

    let prefix = util::unique_prefix("es");
    let agent_id = format!("{prefix}-agent");
    let item_id = format!("{prefix}-item");
    let host_id = format!("{prefix}-host");
    // `process_name` 走内核 `comm`（16 字节上限）。
    let process = format!("{prefix}-1");
    assert!(process.len() < 16, "comm 不能超过 15 字节");
    let ts = util::now_micros();

    let mut records = agent::ebpf_syscall_duration_records(
        &agent_id,
        &item_id,
        4242,
        7,
        &process,
        agent::SYSCALL_OP_OPENAT,
        5,
        500,
        // 5 次全落在槽 3（`[8,16)` 微秒）→ p95 = 槽上界 16；avg = 500/5 = 100。
        3,
        ts,
    );
    records.extend(agent::ebpf_syscall_duration_records(
        &agent_id,
        &item_id,
        4243,
        8,
        &format!("{prefix}-2"),
        agent::SYSCALL_OP_READ,
        2,
        900,
        // 槽 1 → p95 = 4；avg = 900/2 = 450（两条 op 的四个值互不相同，串了就红）。
        1,
        ts,
    ));
    // 同一个 op 两个 errno，且 errno 以十进制字符串落库。
    for (errno, count) in [(13u32, 2u64), (111, 1)] {
        records.push(
            agent::ebpf_syscall_failure_record(
                &agent_id,
                &item_id,
                agent::SYSCALL_OP_OPENAT,
                errno,
                count,
                ts,
            )
            .unwrap_or_else(|| panic!("count={count} 必须产出点")),
        );
    }
    assert_eq!(records.len(), 6, "4 条耗时 + 2 条失败：{records:?}");
    ds.ingest_ok(&data::metrics_envelope(
        &format!("{prefix}-b1"),
        &item_id,
        &agent_id,
        &host_id,
        records,
    ));

    // 1) `field` 维度：同一个 op 的 p95 与 avg 是两条序列，值各自正确。
    for (op, field, expected) in [
        (agent::SYSCALL_OP_OPENAT, "p95", 16.0),
        (agent::SYSCALL_OP_OPENAT, "avg", 100.0),
        (agent::SYSCALL_OP_READ, "p95", 4.0),
        (agent::SYSCALL_OP_READ, "avg", 450.0),
    ] {
        let promql = format!(
            "{DURATION}{{agent_id=\"{agent_id}\",op=\"{}\",field=\"{field}\"}}",
            if op == agent::SYSCALL_OP_OPENAT {
                "openat"
            } else {
                "read"
            }
        );
        let resp = vm.wait_query(&promql, None, TIMEOUT, |r| !r.is_empty());
        assert_eq!(scalar(&resp, &promql), expected, "raw={}", resp.raw);
    }

    // 2) 两条 op × 两个 field = 4 条序列，没有多出来的（例如 `field` 写成同一个值）。
    let all = vm.must_query(&format!("{DURATION}{{agent_id=\"{agent_id}\"}}"), None);
    assert_eq!(all.samples.len(), 4, "raw={}", all.raw);

    // 3) `op` + `errno` 维度：同一 op 的不同 errno 分开累加。
    for (errno, expected) in [("13", 2.0), ("111", 1.0)] {
        let promql =
            format!("{FAILURES}{{agent_id=\"{agent_id}\",op=\"openat\",errno=\"{errno}\"}}");
        let resp = vm.wait_query(&promql, None, TIMEOUT, |r| !r.is_empty());
        assert_eq!(scalar(&resp, &promql), expected, "raw={}", resp.raw);
    }
    let by_op = vm.must_query(
        &format!("sum({FAILURES}{{agent_id=\"{agent_id}\",op=\"openat\"}})"),
        None,
    );
    assert_eq!(scalar(&by_op, "openat 失败合计"), 3.0, "raw={}", by_op.raw);
    let series = vm.must_query(&format!("{FAILURES}{{agent_id=\"{agent_id}\"}}"), None);
    assert_eq!(
        series.samples.len(),
        2,
        "两个 errno 两条序列: raw={}",
        series.raw
    );
    // 没失败过的 op 不该有序列（`count == 0` 不产出点）。
    let read_failures = vm.must_query(
        &format!("{FAILURES}{{agent_id=\"{agent_id}\",op=\"read\"}}"),
        None,
    );
    assert!(
        read_failures.is_empty(),
        "read 没有失败，不该有序列: raw={}",
        read_failures.raw
    );
}
