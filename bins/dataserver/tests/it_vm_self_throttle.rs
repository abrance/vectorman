//! `IT-VM-SELF-02`：trace 接入限流的自监控（`dataserver_apm_ingest_throttled_*`）落 VM。
//!
//! `http.rs::ingest` 里，超过 `apm_ingest_max_batches_per_sec` 的 trace 批次直接回 429，并
//! 把「被挡下的批次数 / 记录数」记到两个计数器里。这两个计数器只有在真被限流时才会出现
//! （`SelfMetrics` 的计数器是懒创建的），所以「没触发过」和「没写进 VM」在 `/metrics` 上是
//! 一样的效果——本用例两条都真跑：先让一批进去，再让下一批被 429 挡下。
//!
//! 断言口径：只查本进程（`instance=<SQL 口监听地址>`）的序列，因为同一台 VM 上还有别的
//! dataserver（pit 的并行用例）在写同样名字的指标。
//!
//! 场景定义（唯一事实来源）：`test_plan/IT-VM-SELF-02.md`。

use std::time::Duration;

use vectorman_test_support::{
    data,
    proc::{Dataserver, Options},
    self_metrics, util, vm,
};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");
/// 自监控快照要追上计数并写进 VM，给足两轮以上。
const FLUSH_TIMEOUT: Duration = Duration::from_secs(60);

#[test]
fn apm_ingest_throttle_self_metrics_land_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::TempDir::new().expect("tempdir");
    // APM 打开（限流器只对 trace 批次生效，且 `apm_limiter` 恒存在，但没 APM 的实例
    // 收不到有效 span）；每秒 1 批 → 同一秒的第二批必然是 429。
    let ds = Dataserver::start(
        dir.path(),
        Options::local()
            .with_vm(&vm)
            .with_apm(1)
            .with_self_metrics(1)
            .with_apm_ingest_limit(1),
        BIN,
    );

    let prefix = util::unique_prefix("self-throttle");
    let service = format!("{prefix}-svc");
    let start = util::floor_minute(util::now_micros()) - 300_000_000;
    // 每批只放 1 条 span：这样「被挡下的批次数」与「被挡下的记录数」必然相等，
    // 两个计数器可以互相印证（同一批 1 条的算术关系，不需要猜）。
    let batch = |seq: u64| {
        data::traces_envelope(
            &format!("{prefix}-b{seq}"),
            &format!("{prefix}-item"),
            &format!("{prefix}-agent"),
            &format!("{prefix}-host"),
            vec![data::trace_span(
                seq,
                seq,
                &service,
                "GET /throttle",
                "server",
                "ok",
                start + (seq as i64) * 1_000,
                1_000,
            )],
        )
    };

    // 真触发：一直发单记录批，直到出现第一个 429（跨秒窗口会放行，所以不能只发 2 批就断言）。
    let mut rejected = 0u32;
    let mut allowed = 0u32;
    for seq in 0..60u64 {
        let (status, body) = ds.post("/v1/ingest", &batch(seq));
        match status {
            200 => allowed += 1,
            429 => {
                assert!(
                    body.contains("throttled"),
                    "429 的报文应说明被限流（否则不是限流路径）: {}",
                    body
                );
                rejected += 1;
                break;
            }
            other => panic!(
                "ingest 返回意外状态 {other}: {}",
                vectorman_test_support::http::tail(&body)
            ),
        }
    }
    assert!(
        rejected >= 1,
        "发了 {} 批都没被限流：apm_ingest_max_batches_per_sec=1 应当挡住同一秒的第二批",
        allowed
    );

    // `/metrics` 上的口径与被挡下的批次一致（1 批 1 记录）。
    let text = self_metrics::scrape_text(&ds, "dataserver_apm_ingest_throttled_batches_total");
    let (_, batches) =
        self_metrics::find(&text, "dataserver_apm_ingest_throttled_batches_total", &[]);
    assert!(
        (batches - f64::from(rejected)).abs() < 1e-9,
        "自监控的限流批次数应等于实测 429 次数 {rejected}，实际 {batches}"
    );
    let (_, records) =
        self_metrics::find(&text, "dataserver_apm_ingest_throttled_records_total", &[]);
    assert!(
        (records - f64::from(rejected)).abs() < 1e-9,
        "每批 1 条记录，被挡下的记录数应等于 429 次数 {rejected}，实际 {records}"
    );

    // 落 VM：两个计数器都要能在 VM 原生口按本进程 instance 查到，且值一字不差。
    let batches_q =
        self_metrics::selector(&ds, "dataserver_apm_ingest_throttled_batches_total", &[]);
    let got = self_metrics::wait_value(&vm, &batches_q, f64::from(rejected), FLUSH_TIMEOUT);
    assert_eq!(
        got.first().metric.get("instance"),
        Some(&self_metrics::instance(&ds)),
        "样本的 instance 必须是被测进程的 SQL 口地址"
    );
    assert_eq!(
        got.first().metric.get("component").map(String::as_str),
        Some("dataserver")
    );

    let records_q =
        self_metrics::selector(&ds, "dataserver_apm_ingest_throttled_records_total", &[]);
    self_metrics::wait_value(&vm, &records_q, f64::from(rejected), FLUSH_TIMEOUT);
}
