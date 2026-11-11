//! `IT-VM-EBPF-PROC-01`：eBPF 进程分钟聚合（`ebpf_process_*_total`）→ dataserver → **真假 VM**。
//!
//! 产物来自 `gse-agent-ebpf` 的 `ProcessMinuteAccumulator::drain_closed`（Agent 侧唯一的进程
//! 指标出口，见 `crates/gse-agent-ebpf/src/lib.rs`），本用例调的就是这个产品函数，**手搓 JSON
//! 不算**：`process_name` 截断、`cgroup_id`/`pid` 标签、`value` 为 0 不产出点这些口径都在里面。
//!
//! 为什么不起真 eBPF：`ebpf_process_*` 需要 Agent 先用 `aya` 加载内核态程序（root + `CAP_BPF`
//! + BTF）。TP 只覆盖「Agent 产出 → `/v1/ingest` → VM」这段，内核态聚合由 crate 单元测试覆盖。
//!
//! 已有一条 `IT-VM-PROCESS-01` 验的是 `service` 维度补全，它用的 `ebpf_process_cpu_usage`
//! 是**合成名**（产品不产出这个 measurement），因此那条例证不能当作这三个真名的覆盖。
//!
//! 场景定义见 `test_plan/IT-VM-EBPF-PROC-01.md`。不用前端、不做静默跳过。

use std::time::Duration;

use vectorman_test_support::{
    agent, data,
    proc::{Dataserver, Options},
    prom, util, vm,
};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");
const TIMEOUT: Duration = Duration::from_secs(60);
/// 分钟桶在 `now` 之前一分钟，`time = None` 的 instant 查询回看窗口够用。
const EXEC: &str = "ebpf_process_exec_total";
const EXIT: &str = "ebpf_process_exit_total";
const FORK: &str = "ebpf_process_fork_total";

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
fn ebpf_process_totals_land_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::TempDir::new().expect("tempdir");
    // APM 关闭：本用例只验 Agent 自产的指标，`service` 补全由 IT-VM-PROCESS-01 负责。
    let ds = Dataserver::start(dir.path(), Options::local().with_vm(&vm), BIN);

    let prefix = util::unique_prefix("ep");
    let agent_id = format!("{prefix}-agent");
    let item_id = format!("{prefix}-item");
    let host_id = format!("{prefix}-host");
    // `process_name` 走内核 `comm`（`TASK_COMM_LEN` = 16 字节），超过就被截断成另一个标签值。
    let busy = format!("{prefix}-1");
    let quiet = format!("{prefix}-2");
    assert!(busy.len() < 16 && quiet.len() < 16, "comm 不能超过 15 字节");

    let now = util::now_micros();
    let records = agent::ebpf_process_records(
        &agent_id,
        now,
        &[
            // exec/exit/fork 全非零：三条指标都必须产出点。
            (4242, 7, busy.as_str(), (3, 1, 2)),
            // exit 为 0：产品侧跳过该条，VM 里**不该**有这条序列。
            (4343, 8, quiet.as_str(), (5, 0, 0)),
        ],
    );
    assert_eq!(
        records.len(),
        4,
        "busy 三条 + quiet 只有 exec（exit/fork 为 0 跳过）= 4 条点：{records:?}"
    );
    ds.ingest_ok(&data::metrics_envelope(
        &format!("{prefix}-b1"),
        &item_id,
        &agent_id,
        &host_id,
        records,
    ));

    let q = |measurement: &str, process: &str, extra: &str| {
        format!("{measurement}{{agent_id=\"{agent_id}\",process_name=\"{process}\"{extra}}}")
    };

    // 1) 三个真名都落库，值与原样一致（不是 0、也没串行）。
    for (measurement, expected) in [(EXEC, 3.0), (EXIT, 1.0), (FORK, 2.0)] {
        let promql = q(measurement, &busy, "");
        let resp = vm.wait_query(&promql, None, TIMEOUT, |r| !r.is_empty());
        assert_eq!(scalar(&resp, measurement), expected, "raw={}", resp.raw);
    }

    // 2) `/proc` 之外的维度：pid / cgroup_id 必须真的成了 VM 的序列标签。
    let exec = vm.must_query(&q(EXEC, &busy, ""), None);
    let sample = exec.first();
    assert_eq!(sample.metric.get("pid").map(String::as_str), Some("4242"));
    assert_eq!(
        sample.metric.get("cgroup_id").map(String::as_str),
        Some("7")
    );
    assert_eq!(
        sample.metric.get("item_id").map(String::as_str),
        Some(item_id.as_str()),
        "item_id 由信封补，不能被记录里的旧值盖掉"
    );

    // 3) `value == 0` 的计数不产出点：quiet 进程只有 exec=5，exit/fork 都查不到序列。
    assert_eq!(
        scalar(
            &vm.wait_query(&q(EXEC, &quiet, ""), None, TIMEOUT, |r| !r.is_empty()),
            EXEC,
        ),
        5.0
    );
    for measurement in [EXIT, FORK] {
        let empty = vm.must_query(&q(measurement, &quiet, ""), None);
        assert!(
            empty.is_empty(),
            "{measurement} 在 quiet 进程上是 0，不该有序列，实际 raw={}",
            empty.raw
        );
    }

    // 4) 两条进程各一条 exec 序列，没有互相覆盖、也没有多出来的。
    let all = vm.must_query(
        &format!("{EXEC}{{agent_id=\"{agent_id}\",process_name=~\"{prefix}-.*\"}}"),
        None,
    );
    assert_eq!(all.samples.len(), 2, "raw={}", all.raw);

    // 5) 没走本地 tsink（与 IT-VM-AGENT-01 同口径：写了本地就说明 VM 那条是假的）。
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
        "不该写本地 tsink：{}",
        tsink_dir.display()
    );
}
