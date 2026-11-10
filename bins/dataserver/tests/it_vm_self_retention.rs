//! `IT-VM-SELF-05`：APM 保留期任务的自监控（`dataserver_apm_retention_*` / `_details|summaries|edges_deleted_total` /
//! `_data_bytes`）落 VM。
//!
//! 任务只在 `apm_enabled && apm_clean_interval_secs > 0` 时挂上，产品侧周期下限 60s，所以此前
//! 一条都没被触发过。这里真造「过期数据」让保留任务干活：把 span 放在 4 天前（缺省保留 3 天），
//! 于是
//! - span 明细（日志引擎里 `data_type=traces` 的行）被删 → `_details_deleted_total`；
//! - 刷盘写下的 trace 摘要（`apm_trace_summary.start_ts`）被删 → `_summaries_deleted_total`；
//! - 刷盘写下的边摘要（`apm_edge_summary.bucket_start`）被删 → `_edges_deleted_total`。
//!
//! 注意 ts 侧的按时间窗删除在这条路径上是**跳过**的：VM 后端 `supports_time_window_delete() = false`
//! （VM 删除接口不吃 start/end），所以 `dataserver_apm_*` 的墓碑计数不在这里验。
//!
//! 场景定义（唯一事实来源）：`test_plan/IT-VM-SELF-05.md`。

use std::time::Duration;

use vectorman_test_support::{
    data,
    proc::{Dataserver, Options},
    self_metrics, util, vm,
};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");
const TIMEOUT: Duration = Duration::from_secs(60);
/// 保留任务周期下限 60s：启动 tick 看不到数据，真删发生在一个完整周期之后。
const RETENTION_TIMEOUT: Duration = Duration::from_secs(180);
/// 缺省 APM 保留 3 天：放到 4 天前一定过期。
const OLD_AGE_SECS: i64 = 4 * 24 * 3600;

fn q(ds: &Dataserver, name: &str) -> String {
    self_metrics::selector(ds, name, &[])
}

#[test]
fn apm_retention_self_metrics_land_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::TempDir::new().expect("tempdir");
    let ds = Dataserver::start(
        dir.path(),
        Options::local()
            .with_vm(&vm)
            .with_apm(1)
            .with_self_metrics(1)
            .with_apm_clean(60),
        BIN,
    );

    let prefix = util::unique_prefix("self-retention");
    let service = format!("{prefix}-svc");
    let bucket = util::floor_minute(util::now_micros()) - OLD_AGE_SECS * 1_000_000;

    // 一条 server span + 一条 client span：前者进 trace 明细/摘要，后者派生 OTLP 边进边摘要。
    ds.ingest_ok(&data::traces_envelope(
        &format!("{prefix}-b1"),
        &format!("{prefix}-item"),
        &format!("{prefix}-agent"),
        &format!("{prefix}-host"),
        vec![
            data::trace_span(
                1,
                1,
                &service,
                "GET /expired",
                "server",
                "ok",
                bucket + 1_000_000,
                1_000,
            ),
            data::trace_span(
                1,
                2,
                &service,
                "POST /expired-downstream",
                "client",
                "error",
                bucket + 2_000_000,
                2_000,
            ),
        ],
    ));

    // 先证明聚合真写出了点（否则保留任务没有可删的行，计数器就永远是 0）。
    // 点在 4 天前的分钟桶上，instant 查询必须把求值时刻也放回那个桶：默认回看窗口只有几分钟，
    // 用「现在」求值只会查到空。
    let at_secs = bucket as f64 / 1e6 + 120.0;
    vm.wait_query(
        &format!(
            "apm_service_requests_total{{service=\"{service}\",operation=\"GET /expired\",span_kind=\"server\",source=\"otlp\"}}"
        ),
        Some(at_secs),
        TIMEOUT,
        |r| !r.is_empty(),
    );

    // 三个删除计数器都必须来自真删除（0 值 `inc_counter` 会提前返回，所以出现即非零）。
    for name in [
        "dataserver_apm_details_deleted_total",
        "dataserver_apm_summaries_deleted_total",
        "dataserver_apm_edges_deleted_total",
    ] {
        self_metrics::wait_at_least(&vm, &q(&ds, name), 1.0, RETENTION_TIMEOUT);
    }

    // 轮次计数与容量 gauge：每轮无条件 +1 / set，出现即说明任务在周期性跑。
    self_metrics::wait_at_least(
        &vm,
        &q(&ds, "dataserver_apm_retention_runs_total"),
        2.0,
        RETENTION_TIMEOUT,
    );
    let bytes = self_metrics::wait_present(&vm, &q(&ds, "dataserver_apm_data_bytes"), TIMEOUT);
    assert!(
        bytes.first().value > 0.0,
        "data_bytes 应是 data_path 的实际体积（子目录已建好，不该是 0）: {}",
        bytes.raw
    );

    // 没故障时错误计数器不该存在。
    self_metrics::assert_absent(
        &ds,
        "dataserver_apm_retention_errors_total",
        "保留任务成功时不该 +1",
    );

    let (stdout, stderr) = ds.logs();
    assert!(
        !stderr.contains("apm retention failed"),
        "dataserver 侧保留任务报错：\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
}
