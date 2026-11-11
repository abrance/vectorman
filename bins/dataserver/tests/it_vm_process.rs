//! `IT-VM-PROCESS-01`：进程指标（`ebpf_process_*`）的 `service` 维度补全 → **真假 VM**。
//!
//! Agent 只上报 `process_name`（进程名）；`service` 是 dataserver 侧按静态映射
//! （`apm_service_alias`，只对 `ebpf_process_*` / `ebpf_syscall_*` 生效）补出来的。
//! 这条验的正是「补出来的维度真的跟着点落进 VM」，而不是只在进程内存里出现过：
//! VM 的 series 身份包含全部标签，补不上就是另一条序列，后面按服务聚合的图会全空。
//!
//! 未命中时按约定补 `unknown-<process_name>`（与边记录的 `unknown-<ip>` 同口径），
//! 空服务维度 `service=""` 必须**查不到任何序列**。
//!
//! 场景定义（唯一事实来源）：`test_plan/IT-VM-PROCESS-01.md`。

use std::time::Duration;

use vectorman_test_support::{
    apm, data,
    proc::{Dataserver, Options},
    prom, util, vm,
};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");
const TIMEOUT: Duration = Duration::from_secs(60);
const MEASUREMENT: &str = "ebpf_process_cpu_usage";
const SYSCALL_MEASUREMENT: &str = "ebpf_syscall_duration_micros";
/// 往前放 2 分钟：点在 VM instant 查询的回看窗口里，且不与别的用例抢同一个分钟。
const AGE_SECS: i64 = 120;

fn wait_scalar(vm: &vm::Vm, promql: &str, expected: f64) -> prom::Response {
    let resp = vm.wait_query(promql, None, TIMEOUT, |r| {
        !r.is_empty() && (r.value_sum() - expected).abs() < 1e-6
    });
    assert_eq!(
        resp.samples.len(),
        1,
        "期望恰好 1 个样本（{promql}），实际 {}: raw={}",
        resp.samples.len(),
        resp.raw
    );
    resp
}

#[test]
fn process_metric_service_dimension_lands_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::TempDir::new().expect("tempdir");
    let ds = Dataserver::start(dir.path(), Options::local().with_vm(&vm).with_apm(1), BIN);

    let prefix = util::unique_prefix("proc");
    let process = format!("{prefix}-proc");
    let service = format!("{prefix}-svc");
    let unmapped = format!("{prefix}-unmapped");
    apm::create_alias(&ds, "process_name", &process, &service);

    let ts = util::floor_minute(util::now_micros()) - AGE_SECS * 1_000_000;
    // 两条同 measurement 的记录：一条进程名建过别名（42.0），一条没建（7.0）。
    let records = vec![
        data::metric_record(
            &format!("{prefix}-r1"),
            MEASUREMENT,
            [("process_name", process.as_str()), ("pid", "4242")],
            "value",
            42.0,
            ts,
        ),
        data::metric_record(
            &format!("{prefix}-r2"),
            MEASUREMENT,
            [("process_name", unmapped.as_str()), ("pid", "4343")],
            "value",
            7.0,
            ts,
        ),
        // 第三个前缀 `ebpf_syscall_*` 与 `ebpf_process_*` 共用同一个判断
        // （`SERVICE_MEASUREMENT_PREFIXES`），但只测前者的话数组里漏一个也发现不了。
        data::metric_record(
            &format!("{prefix}-r3"),
            SYSCALL_MEASUREMENT,
            [("process_name", process.as_str()), ("syscall", "openat")],
            "value",
            3.5,
            ts,
        ),
    ];
    ds.ingest_ok(&data::metrics_envelope(
        &format!("{prefix}-b1"),
        &format!("{prefix}-item"),
        &format!("{prefix}-agent"),
        &format!("{prefix}-host"),
        records,
    ));

    // 1) 命中别名：`service` 由服务端补出来，并且真的成了 VM 的序列标签。
    let mapped = format!("{MEASUREMENT}{{service=\"{service}\",process_name=\"{process}\"}}");
    let hit = wait_scalar(&vm, &mapped, 42.0);
    assert_eq!(hit.first().metric["service"], service);
    assert_eq!(hit.first().metric["process_name"], process);
    assert_eq!(
        hit.first().metric["pid"],
        "4242",
        "原始标签不能被补维度挤掉"
    );

    // 2) 未命中别名：按约定补 `unknown-<process_name>`，值原样落库。
    let unknown_service = format!("unknown-{unmapped}");
    let miss =
        format!("{MEASUREMENT}{{service=\"{unknown_service}\",process_name=\"{unmapped}\"}}");
    let miss_resp = wait_scalar(&vm, &miss, 7.0);
    assert_eq!(miss_resp.first().metric["service"], unknown_service);

    // 2b) `ebpf_syscall_*` 走同一套补维度规则，不能被漏在数组外。
    let syscall =
        format!("{SYSCALL_MEASUREMENT}{{service=\"{service}\",process_name=\"{process}\"}}");
    let sc = wait_scalar(&vm, &syscall, 3.5);
    assert_eq!(
        sc.first().metric["syscall"],
        "openat",
        "原始标签不能被补维度挤掉"
    );

    // 3) 空服务维度：一条都查不到（补维度不能落成空串，否则前端「按服务聚合」会多一条空组）。
    let empty = vm.must_query(&format!("{MEASUREMENT}{{service=\"\"}}"), None);
    assert!(
        empty.is_empty(),
        "service=\"\" 不该有任何序列：raw={}",
        empty.raw
    );

    // 4) 两条记录各一条序列（没有互相覆盖），按 measurement 全量数一遍。
    let all = vm.must_query(
        &format!("{MEASUREMENT}{{process_name=~\"{prefix}-.*\"}}"),
        None,
    );
    assert_eq!(
        all.samples.len(),
        2,
        "别名/未命中各一条序列: raw={}",
        all.raw
    );
}
