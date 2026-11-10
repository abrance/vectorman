//! `IT-VM-EBPF-TCP-01`：eBPF 边的 TCP 重传/重置计数 → 分钟派生 → **真假 VM**。
//!
//! 断言口径以 `crates/dataplane-apm/src/ebpf_metrics.rs` 的 `aggregate()` 为准（先读代码：
//! - `if row.tcp_retrans > 0 { … or_default() += … }`：**0 值不产出点**，VM 里连序列都没有；
//! - 聚合键是 `(分钟桶, src_service, dst_service)`：同桶多条边**累加**，跨桶**各算各的**
//!   （不是跨桶累加的 counter），所以两桶的值分别是自己的和。
//!
//! 这两个 measurement 的标签只有 `src_service` / `dst_service`（没有 `status` / `source` /
//! `reason`），多一个标签就是产品口径变了，断言里直接锁死标签集合。
//!
//! 场景定义（唯一事实来源）：`test_plan/IT-VM-EBPF-TCP-01.md`。

use std::time::Duration;

use vectorman_test_support::{
    apm, data,
    proc::{Dataserver, Options},
    prom, util, vm,
};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");
const TIMEOUT: Duration = Duration::from_secs(60);
/// 桶往前放 5 分钟：一定「已关闭」，VM 的 instant 查询也还能看到这个点。
const BUCKET_AGE_SECS: i64 = 300;
const MINUTE_MICROS: i64 = 60_000_000;

fn wait_scalar(vm: &vm::Vm, promql: &str, at_secs: f64, expected: f64) -> prom::Response {
    let resp = vm.wait_query(promql, Some(at_secs), TIMEOUT, |r| {
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

/// 标签集合（排序后）——用来锁死「只有 src_service / dst_service」。
fn label_keys(resp: &prom::Response) -> Vec<String> {
    let mut keys: Vec<String> = resp.first().metric.keys().cloned().collect();
    keys.sort();
    keys
}

#[test]
fn ebpf_tcp_retrans_resets_land_in_victoriametrics() {
    let vm = vm::Vm::require();
    vm.wait_ready(Duration::from_secs(20));

    let dir = tempfile::TempDir::new().expect("tempdir");
    let ds = Dataserver::start(dir.path(), Options::local().with_vm(&vm).with_apm(1), BIN);
    // 先等第一轮 tick 把游标写出来（否则后面 rewind 与「首次对齐」会互相覆盖）。
    apm::wait_watermark(&ds);

    let prefix = util::unique_prefix("ebpf-tcp");
    let agent = format!("{prefix}-agent");
    let src = format!("{prefix}-src");
    // 有重传的边与全 0 的边必须是不同的 `dst_service`，否则同名序列互相覆盖。
    let dst = format!("{prefix}-dst");
    let dst_zero = format!("{prefix}-dst-zero");
    let bucket = util::floor_minute(util::now_micros()) - BUCKET_AGE_SECS * 1_000_000;
    let bucket_next = bucket + MINUTE_MICROS;
    // 每桶的查询时刻必须落在**本桶内**：instant 查询返回「<= time 的最新样本」，
    // 用 `bucket + 120s` 会读到下一桶的点（实测踩过：断言 10 却拿到 4）。
    let at = bucket as f64 / 1e6 + 30.0;
    let at_next = bucket_next as f64 / 1e6 + 30.0;

    let edges = vec![
        // 桶 1 第一条：retrans 7 / resets 2。
        data::ebpf_edge_tcp(
            &agent,
            bucket,
            ("10.33.0.1", 41_001, &src),
            ("10.44.0.1", 8_080, &dst),
            "tcp",
            5,
            0,
            0,
            0,
            "",
            0,
            0,
            vec![0, 0, 5],
            7,
            2,
        ),
        // 桶 1 第二条：同一对 (src, dst)、不同 src_port（record_id 不同才会被各自受理），
        // retrans 3 / resets 0 → 同桶累加，期望 retrans 10、resets 2。
        data::ebpf_edge_tcp(
            &agent,
            bucket,
            ("10.33.0.1", 41_002, &src),
            ("10.44.0.1", 8_080, &dst),
            "tcp",
            4,
            0,
            0,
            0,
            "",
            0,
            0,
            vec![0, 0, 4],
            3,
            0,
        ),
        // 桶 2 只投一条：retrans 4 / resets 1 → 若产品把这里当成跨桶累加的 counter，
        // 桶 2 会读成 14/3；按代码它是**每桶独立的和**，所以断言 4/1。
        data::ebpf_edge_tcp(
            &agent,
            bucket_next,
            ("10.33.0.1", 41_001, &src),
            ("10.44.0.1", 8_080, &dst),
            "tcp",
            2,
            0,
            0,
            0,
            "",
            0,
            0,
            vec![0, 0, 2],
            4,
            1,
        ),
        // 全 0 的边：聚合侧 `if > 0` 直接跳过 → VM 里不该有这条序列。
        data::ebpf_edge_tcp(
            &agent,
            bucket,
            ("10.33.0.1", 41_001, &src),
            ("10.44.0.2", 9_090, &dst_zero),
            "tcp",
            2,
            0,
            0,
            0,
            "",
            0,
            0,
            vec![0, 0, 2],
            0,
            0,
        ),
    ];
    ds.ingest_ok(&data::ebpf_edges_envelope(
        &format!("{prefix}-b1"),
        &format!("{prefix}-item"),
        &agent,
        &format!("{prefix}-host"),
        edges,
    ));
    // 先 ingest 再回拨游标（顺序反了会因中间插进一轮 tick 把边永久落在游标后面）。
    apm::rewind_ebpf_watermark(&ds, bucket - MINUTE_MICROS);

    // 1) 桶 1：同桶两条边累加 → 10 / 2。
    let retrans = format!("ebpf_tcp_retrans_total{{src_service=\"{src}\",dst_service=\"{dst}\"}}");
    let resets = format!("ebpf_tcp_resets_total{{src_service=\"{src}\",dst_service=\"{dst}\"}}");
    let retrans_first = wait_scalar(&vm, &retrans, at, 10.0);
    assert_eq!(
        label_keys(&retrans_first),
        vec![
            "__name__".to_string(),
            "dst_service".to_string(),
            "src_service".to_string()
        ],
        "`ebpf_tcp_retrans_total` 的标签集只该有 src_service/dst_service，实际: {:?}",
        retrans_first.first().metric
    );
    let resets_first = wait_scalar(&vm, &resets, at, 2.0);
    assert_eq!(
        label_keys(&resets_first),
        vec![
            "__name__".to_string(),
            "dst_service".to_string(),
            "src_service".to_string()
        ],
        "`ebpf_tcp_resets_total` 的标签集只该有 src_service/dst_service，实际: {:?}",
        resets_first.first().metric
    );
    assert_eq!(retrans_first.first().metric["src_service"], src);
    assert_eq!(retrans_first.first().metric["dst_service"], dst);

    // 2) 桶 2：每桶独立，值就是本桶之和（4 / 1），不是 10+4 / 2+1。
    wait_scalar(&vm, &retrans, at_next, 4.0);
    wait_scalar(&vm, &resets, at_next, 1.0);

    // 3) 0 值边：聚合侧跳过，VM 里查不到任何点（不是「查到 0」）。
    //    此刻正样本已经落库，说明聚合确实跑过了，这里空就是真空。
    for (name, query) in [
        ("ebpf_tcp_retrans_total", &retrans),
        ("ebpf_tcp_resets_total", &resets),
    ] {
        let zero = format!(
            "{name}{{src_service=\"{src}\",dst_service=\"{dst_zero}\"}}",
            name = name,
            src = src,
            dst_zero = dst_zero
        );
        let resp = vm.must_query(&zero, Some(at));
        assert!(
            resp.is_empty(),
            "0 值的 `{name}` 不该产出点（聚合侧 `if > 0` 跳过），实际: {}（{query} 是同桶正样本）",
            resp.raw
        );
    }
}
