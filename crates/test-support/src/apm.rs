//! APM / eBPF 聚合的测试夹具。
//!
//! `EbpfMetricsAggregator` 首次运行会把游标（`obs_schema_meta` 表里的
//! `ebpf_metrics_watermark`）对齐到「当前已关闭的分钟」，**不回溯历史**。所以刚起的
//! dataserver 不会聚合任何旧边；测试要验聚合结果，必须要么等一个自然分钟，要么显式把
//! 游标往回拨。这里提供后者：拨游标是测试夹具动作，不改产品代码。
//!
//! 反过来说：这些用例断言的是「边台账 → 分钟指标」这条聚合路径，不是游标策略本身；
//! 游标「不回溯」的行为由 `dataplane-apm` 的单元测试覆盖。

use serde_json::json;

use crate::proc::Dataserver;
use crate::util::wait_until;

/// 游标所在的表与键（与 `dataplane_apm::ebpf_metrics` 一致）。
const META_TABLE: &str = "obs_schema_meta";
const WATERMARK_KEY: &str = "ebpf_metrics_watermark";

/// 等到聚合器写出首个游标（即第一轮 tick 已经跑过）。
pub fn wait_watermark(ds: &Dataserver) {
    wait_until("eBPF 聚合游标初始化", std::time::Duration::from_secs(20), || {
        let rows = ds.sql(
            &format!("SELECT value FROM {META_TABLE} WHERE key = ?1"),
            &[json!(WATERMARK_KEY)],
        );
        rows["rows"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
            .then_some(())
    });
}

/// 把 eBPF 聚合游标拨到 `to_micros`，让 `bucket_start > to_micros` 的边在下一轮被聚合。
pub fn rewind_ebpf_watermark(ds: &Dataserver, to_micros: i64) {
    ds.sql(
        &format!(
            "INSERT INTO {META_TABLE} (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value"
        ),
        &[json!(WATERMARK_KEY), json!(to_micros.to_string())],
    );
}

/// 建一条服务别名（`POST /v1/apm/service-aliases`）。
///
/// `match_kind`：`process_name` | `process_prefix` | `pod_prefix` | `cidr`。
/// 进程指标（`ebpf_process_*` / `ebpf_syscall_*`）的 `service` 维度就是靠它补全的。
pub fn create_alias(ds: &Dataserver, match_kind: &str, match_value: &str, service: &str) {
    let body = serde_json::json!({
        "match_kind": match_kind,
        "match_value": match_value,
        "service": service,
        "enabled": true,
    });
    let (status, resp) = ds.post("/v1/apm/service-aliases", &body.to_string());
    assert_eq!(
        status, 201,
        "建别名失败（{status}）: {}",
        crate::http::tail(&resp)
    );
}
