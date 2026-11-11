//! `IT-VM-SELF-03`：APM 聚合 / 刷盘的自监控（`dataserver_apm_agg_*`、`dataserver_apm_*_flushed_*`、
//! 四个 sink gauge）落 VM。
//!
//! 这些序列全在 `main.rs` 的聚合任务（`apm_agg_interval_secs > 0`）与刷盘任务（`apm` sink 存在时
//! 每秒一轮）里产生，而 `IT-VM-SELF-01` 的实例 APM 关闭、也没有聚合任务，所以此前一条都没被触发。
//!
//! 触发方式是真的走一遍链路：真 span 进接入 → 真聚合周期把 RED / 边点写进 VM → 真刷盘周期
//! 把 trace 摘要 / endpoint / 边落库。断言只取本进程 `instance`，避免与并行实例的同名序列混在一起。
//!
//! 场景定义（唯一事实来源）：`test_plan/IT-VM-SELF-03.md`。

use std::time::Duration;

use vectorman_test_support::{
    data,
    proc::{Dataserver, Options},
    self_metrics, util, vm,
};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");
const TIMEOUT: Duration = Duration::from_secs(60);
/// 桶往前放 5 分钟：一定「已关闭」，聚合任务才会把它写出来。
const BUCKET_AGE_SECS: i64 = 300;

/// 名字 → 本进程 selector；`extra` 是标签过滤。
fn q(ds: &Dataserver, name: &str) -> String {
    self_metrics::selector(ds, name, &[])
}

#[test]
fn apm_agg_and_flush_self_metrics_land_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::TempDir::new().expect("tempdir");
    // 聚合 1s 一轮 + 自监控 1s 一轮：两者都在起进程时就挂上，不需要额外开关。
    let ds = Dataserver::start(
        dir.path(),
        Options::local()
            .with_vm(&vm)
            .with_apm(1)
            .with_self_metrics(1),
        BIN,
    );

    let prefix = util::unique_prefix("self-apm");
    let service = format!("{prefix}-svc");
    let bucket = util::floor_minute(util::now_micros()) - BUCKET_AGE_SECS * 1_000_000;

    // 一条 server span（RED 服务侧指标）+ 一条 client span（派生 OTLP 边，刷盘时进边摘要）。
    // 两条都在同一个已关闭的分钟桶里，聚合只写一个桶。
    let spans = vec![
        data::trace_span(
            1,
            1,
            &service,
            "GET /agg",
            "server",
            "ok",
            bucket + 1_000_000,
            2_000,
        ),
        data::trace_span(
            1,
            2,
            &service,
            "POST /downstream",
            "client",
            "error",
            bucket + 2_000_000,
            4_000,
        ),
    ];
    ds.ingest_ok(&data::traces_envelope(
        &format!("{prefix}-b1"),
        &format!("{prefix}-item"),
        &format!("{prefix}-agent"),
        &format!("{prefix}-host"),
        spans,
    ));

    // 先证明「聚合真的写出点了」（这是自监控数值的来源，不能只看计数器）。
    // 点在 5 分钟前的分钟桶上，instant 查询的求值时刻必须也放回桶内：用「现在」求值会撞上
    // VM 的默认回看窗口（约 5 分钟）边界，属于会偶发假红的写法。
    let at_secs = bucket as f64 / 1e6 + 120.0;
    let service_points = format!(
        "apm_service_requests_total{{service=\"{service}\",operation=\"GET /agg\",span_kind=\"server\",source=\"otlp\"}}"
    );
    vm.wait_query(&service_points, Some(at_secs), TIMEOUT, |r| !r.is_empty());
    let edge_points =
        format!("apm_edge_requests_total{{src_service=\"{service}\",source=\"otlp\"}}");
    vm.wait_query(&edge_points, Some(at_secs), TIMEOUT, |r| !r.is_empty());

    // ---- 聚合任务的自监控
    self_metrics::wait_at_least(&vm, &q(&ds, "dataserver_apm_agg_runs_total"), 1.0, TIMEOUT);
    // `agg_points_total` 累加的是「本次写出的服务点 + 边点」，只有真写了点才会 +。
    self_metrics::wait_at_least(
        &vm,
        &q(&ds, "dataserver_apm_agg_points_total"),
        2.0,
        TIMEOUT,
    );
    // 最后一轮聚合的桶 == 我们那条 span 所在的分钟桶（微秒，f64 在 2^53 内可精确表示）。
    let got = self_metrics::wait_value(
        &vm,
        &q(&ds, "dataserver_apm_last_agg_bucket"),
        bucket as f64,
        TIMEOUT,
    );
    assert_eq!(
        got.first().metric.get("instance"),
        Some(&self_metrics::instance(&ds)),
        "last_agg_bucket 必须是本进程写入的那条"
    );

    // 两个 RedSamples gauge：每轮聚合后无条件 set，所以它们的存在本身就是「聚合循环在跑」。
    for name in [
        "dataserver_apm_open_groups",
        "dataserver_apm_dropped_samples",
    ] {
        let resp = self_metrics::wait_present(&vm, &q(&ds, name), TIMEOUT);
        assert!(
            resp.first().value >= 0.0,
            "{name} 应是非负 gauge: {}",
            resp.raw
        );
    }

    // ---- 刷盘任务的自监控：三个 flushed 计数器都必须来自真实的 flush 报告
    for name in [
        "dataserver_apm_trace_summaries_flushed_total",
        "dataserver_apm_endpoints_flushed_total",
        "dataserver_apm_edges_flushed_total",
    ] {
        self_metrics::wait_at_least(&vm, &q(&ds, name), 1.0, TIMEOUT);
    }

    // ---- 四个 sink gauge（live / dirty / paired / pending）：每轮 flush 后无条件 set
    for name in [
        "dataserver_apm_live_traces",
        "dataserver_apm_dirty_traces",
        "dataserver_apm_paired_edges",
        "dataserver_apm_pending_spans",
    ] {
        let resp = self_metrics::wait_present(&vm, &q(&ds, name), TIMEOUT);
        assert!(
            resp.first().value >= 0.0,
            "{name} 应是非负 gauge: {}",
            resp.raw
        );
    }

    // ---- 边界：没有故障时错误计数器不该出现（懒创建的负向证据）
    self_metrics::assert_absent(&ds, "dataserver_apm_agg_errors_total", "聚合成功时不该 +1");
    self_metrics::assert_absent(
        &ds,
        "dataserver_apm_flush_errors_total",
        "刷盘成功时不该 +1",
    );

    // 聚合/刷盘没有把错误打到日志里（与上面的错误计数器互为印证）。
    let (stdout, stderr) = ds.logs();
    assert!(
        !stderr.contains("aggregate failed") && !stderr.contains("flush failed"),
        "dataserver 侧聚合/刷盘报错：\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
}
