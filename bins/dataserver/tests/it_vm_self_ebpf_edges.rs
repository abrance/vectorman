//! `IT-VM-SELF-07`：eBPF 边保留期删除的自监控（`dataserver_ebpf_edges_deleted_total`）落 VM。
//!
//! `main.rs` 的全局清理循环只在**真删掉了边记录**时才 `inc_counter` 这条序列（0 值直接
//! `return`，所以它一旦出现就必然 >= 1）。要让它出现，三个条件缺一不可：
//!
//! 1. `gse_admin_url` 配了、GSE 台账里真有 `kind=ebpf_*` 的 live 采集项（边保留只处理 eBPF 项）；
//! 2. 该采集项的保留期已过（eBPF 缺省 3 天，`storage.retention_days` 可覆盖）；
//! 3. 一个 `ts_clean_interval_secs` 周期真的跑完（产品侧下限 60s）。
//!
//! 所以本用例：真起 gse-server → 真下发 `ebpf_network` spec → 真 ingest 一条 4 天前的边 →
//! 真等一个 60s 清理周期 → `dataserver_ebpf_edges_deleted_total{instance=…} >= 1`，且同周期
//! `dataserver_ts_clean_runs_total` 也在涨（证明是清理循环干的，不是别处塞的计数）。
//!
//! 场景定义（唯一事实来源）：`test_plan/IT-VM-SELF-07.md`。

use std::time::Duration;

use serde_json::json;
use vectorman_test_support::{
    data,
    gse::Gse,
    proc::{Dataserver, Options},
    self_metrics, util, vm,
};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");
/// 清理周期下限 60s：启动 tick 时边还没进库，真删发生在一个完整周期之后。
const CLEAN_TIMEOUT: Duration = Duration::from_secs(180);
/// eBPF 边缺省保留 3 天：放到 4 天前一定过期。
const OLD_AGE_SECS: i64 = 4 * 24 * 3600;

fn q(ds: &Dataserver, name: &str) -> String {
    self_metrics::selector(ds, name, &[])
}

/// 只数本用例 `src_service` 下、落在给定时间窗里的 eBPF 边（`/v1/edges/search`）。
///
/// 时间窗必须显式给：接口缺省只查最近 1 小时，而这条边是 4 天前的，用缺省窗口
/// 会把「在库里」误判成「没有」。
fn edge_total(ds: &Dataserver, src: &str, from_ts: i64, to_ts: i64) -> u64 {
    let (status, body) = ds.post(
        "/v1/edges/search",
        &json!({
            "src_service": src,
            "source": "ebpf",
            "from_ts": from_ts,
            "to_ts": to_ts,
        })
        .to_string(),
    );
    assert_eq!(status, 200, "边检索失败: {body}");
    let page: serde_json::Value = serde_json::from_str(&body).expect("边检索返回非 JSON");
    page["total"].as_u64().unwrap_or_default()
}

#[test]
fn ebpf_edges_deleted_total_lands_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::TempDir::new().expect("tempdir");
    let prefix = util::unique_prefix("self-ebpf-edges");
    let agent = format!("{prefix}-agent");
    let host = format!("{prefix}-host");
    let item = format!("{prefix}-item");
    let src = format!("{prefix}-src");
    let dst = format!("{prefix}-dst");

    // 真 gse-server 台账 + 真 dataserver（ts 后端 = VM，自监控与 60s 清理周期都打开）。
    let gse = Gse::start(dir.path().join("gse"));
    let ds = Dataserver::start(
        dir.path().join("ds"),
        Options::local()
            .with_vm(&vm)
            .with_apm(1)
            .with_gse_admin(gse.admin_url())
            .with_self_metrics(1)
            .with_ts_clean(60),
        BIN,
    );

    // 真下发 spec：边保留只处理 `kind` 以 `ebpf` 开头的 live 采集项，且 `item_id` 必须与
    // 边记录的 `data_id` 一致（`delete_edges_before(sql, item_id, cutoff)`）。
    gse.register_agent(&agent, &host, &format!("{prefix}-token"));
    gse.put_spec(
        &agent,
        &json!({
            "params": {"heartbeat_interval_secs": 1},
            "items": [{
                "item_id": item,
                "name": format!("{prefix} ebpf edges"),
                "kind": "ebpf_network",
                "enabled": true,
                "collector": {},
                "storage": {"retention_days": 3},
            }],
        }),
    );

    // 一条 4 天前的边进 sqlite `ebpf_edges`（`data_id = item`）。
    let bucket = util::floor_minute(util::now_micros()) - OLD_AGE_SECS * 1_000_000;
    ds.ingest_ok(&data::ebpf_edges_envelope(
        &format!("{prefix}-b1"),
        &item,
        &agent,
        &host,
        vec![data::ebpf_edge(
            &agent,
            bucket,
            ("10.41.0.1", 41_001, &src),
            ("10.42.0.1", 8_080, &dst),
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

    // 前置：边真的落库了。删 0 行不会 `inc_counter`，不先证明这一点，「计数器没有」就说不清
    // 是清理没跑还是压根没数据。
    let (from_ts, to_ts) = (bucket - 3_600_000_000, bucket + 3_600_000_000);
    assert_eq!(
        edge_total(&ds, &src, from_ts, to_ts),
        1,
        "边必须先真的进库，否则清理删 0 行、计数器永远是 0"
    );

    // 真等一个完整清理周期：删除发生后才 +1，出现即 >= 1。
    self_metrics::wait_at_least(
        &vm,
        &q(&ds, "dataserver_ebpf_edges_deleted_total"),
        1.0,
        CLEAN_TIMEOUT,
    );

    // 同一周期的轮次计数也在涨（>=2 = 启动 tick + 删到边的那一轮）：证明计数来自清理循环本身。
    self_metrics::wait_at_least(
        &vm,
        &q(&ds, "dataserver_ts_clean_runs_total"),
        2.0,
        CLEAN_TIMEOUT,
    );

    // 真删：边检索归零（计数器是「删了几行」的证据，不是「跑过一轮」的证据）。
    assert_eq!(
        edge_total(&ds, &src, from_ts, to_ts),
        0,
        "过了保留期的边必须被真删掉，边检索应归零"
    );

    // 没故障时错误计数器不该存在（懒创建：没 +1 就没有序列）。
    self_metrics::assert_absent(
        &ds,
        "dataserver_ts_clean_errors_total",
        "清理各阶段都没失败时不该 +1",
    );

    let (stdout, stderr) = ds.logs();
    assert!(
        !stderr.contains("retention cleanup: ") && !stderr.contains("failed_phases"),
        "dataserver 侧清理报错：\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains("ebpf_edges_deleted="),
        "清理循环的日志里应出现 ebpf_edges_deleted 字段（证明确实走到边保留分支）:\n{stdout}"
    );
}
