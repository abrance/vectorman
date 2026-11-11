//! `IT-VM-SELF-04`：eBPF 聚合自监控 + 全局清理任务自监控落 VM。
//!
//! 两组序列都只在特定任务里产生：
//! - `dataserver_ebpf_agg_points_total` / `_last_agg_bucket`：`main.rs` 的 eBPF 边聚合任务
//!   （与 APM 聚合共用节拍），只有真聚合出点时才 +1 / 才 set；
//! - `dataserver_ts_clean_runs_total`：`ts_clean_interval_secs` 的全局清理循环（产品侧下限 60s），
//!   每跑完一轮 +1。
//!
//! 触发方式：真 eBPF 边进接入 → 回拨聚合游标（见 `test_support::apm` 的说明）→ 真聚合一轮；
//! 清理任务则真等过第一轮周期（启动 tick 之外还要再看一轮，避免把「刚起进程的那次」当证据）。
//!
//! 场景定义（唯一事实来源）：`test_plan/IT-VM-SELF-04.md`。

use std::time::Duration;

use vectorman_test_support::{
    apm, data,
    proc::{Dataserver, Options},
    self_metrics, util, vm,
};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");
const TIMEOUT: Duration = Duration::from_secs(60);
/// 清理周期下限 60s：要给到「启动 tick + 一个完整周期」两轮。
const CLEAN_TIMEOUT: Duration = Duration::from_secs(180);
const BUCKET_AGE_SECS: i64 = 300;

fn q(ds: &Dataserver, name: &str) -> String {
    self_metrics::selector(ds, name, &[])
}

#[test]
fn ebpf_agg_and_ts_clean_self_metrics_land_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::TempDir::new().expect("tempdir");
    let ds = Dataserver::start(
        dir.path(),
        Options::local()
            .with_vm(&vm)
            .with_apm(1)
            .with_self_metrics(1)
            .with_ts_clean(60),
        BIN,
    );

    let prefix = util::unique_prefix("self-ebpf");
    let agent = format!("{prefix}-agent");
    let src = format!("{prefix}-src");
    let dst = format!("{prefix}-dst");
    let bucket = util::floor_minute(util::now_micros()) - BUCKET_AGE_SECS * 1_000_000;
    // 派生点在 5 分钟前的桶上：求值时刻也放回桶内，避免撞上 VM 的默认回看窗口边界。
    let at_secs = bucket as f64 / 1e6 + 120.0;

    // ---- eBPF 聚合：先等第一轮 tick 把游标写下来，再 ingest，再回拨（顺序反了会漏聚合）。
    apm::wait_watermark(&ds);
    ds.ingest_ok(&data::ebpf_edges_envelope(
        &format!("{prefix}-b1"),
        &format!("{prefix}-item"),
        &agent,
        &format!("{prefix}-host"),
        vec![data::ebpf_edge(
            &agent,
            bucket,
            ("10.31.0.1", 40_001, &src),
            ("10.32.0.1", 8_080, &dst),
            "tcp",
            2,
            300,
            100,
            0,
            "",
            6_000,
            4_000,
            vec![0, 2],
        )],
    ));
    apm::rewind_ebpf_watermark(&ds, bucket - 60_000_000);

    // 先证明边上真派生了分钟指标（自监控计数的来源），只认 VM 原生口。
    let derived = format!(
        "apm_edge_requests_total{{src_service=\"{src}\",dst_service=\"{dst}\",source=\"ebpf\"}}"
    );
    vm.wait_query(&derived, Some(at_secs), TIMEOUT, |r| !r.is_empty());

    self_metrics::wait_at_least(
        &vm,
        &q(&ds, "dataserver_ebpf_agg_points_total"),
        1.0,
        TIMEOUT,
    );
    let got = self_metrics::wait_value(
        &vm,
        &q(&ds, "dataserver_ebpf_last_agg_bucket"),
        bucket as f64,
        TIMEOUT,
    );
    assert_eq!(
        got.first().metric.get("instance"),
        Some(&self_metrics::instance(&ds)),
        "ebpf last_agg_bucket 必须是本进程写入的那条"
    );

    // ---- 全局清理：真要看到两轮（启动 tick 之外再来一轮 60s 周期）。
    self_metrics::wait_at_least(
        &vm,
        &q(&ds, "dataserver_ts_clean_runs_total"),
        2.0,
        CLEAN_TIMEOUT,
    );

    // ---- 没故障时错误计数器不该存在（懒创建：没 +1 就没有序列）。
    self_metrics::assert_absent(
        &ds,
        "dataserver_ebpf_agg_errors_total",
        "eBPF 聚合成功时不该 +1",
    );
    self_metrics::assert_absent(
        &ds,
        "dataserver_ts_clean_errors_total",
        "清理各阶段都没失败时不该 +1",
    );

    let (stdout, stderr) = ds.logs();
    assert!(
        !stderr.contains("ebpf: aggregate failed") && !stderr.contains("failed_phases"),
        "dataserver 侧 eBPF 聚合/清理报错：\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
}
