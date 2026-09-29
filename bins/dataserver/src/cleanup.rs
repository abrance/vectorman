//! 保存周期清理：live 采集项按 retention_days 删旧日志与旧聚合指标；已删项读
//! `retain/` 到期后删除。
//!
//! 顺序要求：同一个清理周期内必须先跑 [`apply_ts_retention`]（读取 `retain/` 但不删
//! 键），再跑 [`apply_retention`]（删日志并在最后删除 `retain/` 键）。反过来会让
//! 时序侧永远看不到已删项的到期时间。

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dataplane_core::{DataplaneError, ErrorCode};
use dataplane_kv::KvStore;
use dataplane_log::{LogFilter, LogStore};
use dataplane_sql::RelationalStore;
use dataplane_ts::{TimeSeriesStore, TsMatcher, TsMatcherOp, TsSeriesSelection};
use serde::Deserialize;
use serde_json::{json, Value};

/// 一天的微秒数。
pub const MICROS_PER_DAY: i64 = 86_400 * 1_000_000;

const RETAIN_PREFIX: &[u8] = b"retain/";

/// 上一轮 live 采集项集合的 KV 前缀：`spec-live/{item_id}` → `{"retention_days":N}`。
///
/// 为什么要记：采集项现在是 per-Agent spec 里的一段，**没有 per-item 删除事件** ——
/// 运维把它从 spec 里去掉、或把它 `enabled=false`，服务端只会少返回一条，
/// 不会告诉任何人「这条被删了」。要按它原来的保留期清理数据，就只能自己记住上一轮有哪些。
const LIVE_PREFIX: &[u8] = b"spec-live/";

/// `spec-live/{item_id}` 键。
pub fn live_key(item_id: &str) -> String {
    format!("spec-live/{item_id}")
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct LiveValue {
    retention_days: u32,
}

/// 仍在 GSE 列表中的采集项。
#[derive(Debug, Clone)]
pub struct LiveItem {
    pub item_id: String,
    pub retention_days: u32,
    /// 采集项类型：eBPF 系列的保留期缺省是 3 天（其余保持 1 天）。
    pub kind: String,
}

impl LiveItem {
    /// 是否 eBPF 采集项（`ebpf_network` / `ebpf_tcp` / `ebpf_process` / …）。
    #[must_use]
    pub fn is_ebpf(&self) -> bool {
        self.kind.starts_with("ebpf")
    }
}

#[derive(Debug, Deserialize)]
struct RetainValue {
    until_micros: i64,
}

/// 当前 Unix 微秒。
pub fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// `retain/{item_id}` 键。
pub fn retain_key(item_id: &str) -> String {
    format!("retain/{item_id}")
}

/// 规范化保存天数，缺省 1。
pub fn clamp_retention_days(days: u32) -> u32 {
    if days == 0 {
        1
    } else {
        days
    }
}

/// 拼接 GSE 管理口 URL。
pub fn join_gse_url(base: &str, path: &str, query: Option<&str>) -> String {
    let base = base.trim_end_matches('/');
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    match query {
        Some(q) if !q.is_empty() => format!("{base}{path}?{q}"),
        _ => format!("{base}{path}"),
    }
}

/// 同步调用 GSE HTTP；连接失败返回 `unavailable`。
pub fn gse_http_sync(
    method: &str,
    url: &str,
    body: &[u8],
) -> Result<(u16, String), DataplaneError> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();
    let req = agent.request(method, url);
    let result = if matches!(method, "GET" | "HEAD") || (method == "DELETE" && body.is_empty()) {
        req.call()
    } else {
        req.set("Content-Type", "application/json").send_bytes(body)
    };
    match result {
        Ok(resp) => {
            let status = resp.status();
            let text = resp.into_string().unwrap_or_default();
            Ok((status, text))
        }
        Err(ureq::Error::Status(code, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            Ok((code, text))
        }
        Err(e) => Err(DataplaneError::new(ErrorCode::Unavailable, e.to_string())),
    }
}

/// 异步包装 `gse_http_sync`。
pub async fn gse_call(
    method: &str,
    url: &str,
    body: &[u8],
) -> Result<(u16, String), DataplaneError> {
    let method = method.to_string();
    let url = url.to_string();
    let body = body.to_vec();
    tokio::task::spawn_blocking(move || gse_http_sync(&method, &url, &body))
        .await
        .map_err(|e| DataplaneError::new(ErrorCode::Unavailable, e.to_string()))?
}

/// 从采集项 JSON 读 `retention_days`，缺省 1。
pub fn retention_days_from_item(v: &Value) -> u32 {
    if let Some(d) = v
        .pointer("/storage/retention_days")
        .and_then(|x| x.as_u64())
    {
        return clamp_retention_days(d as u32);
    }
    if let Some(s) = v.get("storage_json").and_then(|x| x.as_str()) {
        if let Ok(j) = serde_json::from_str::<Value>(s) {
            if let Some(d) = j.get("retention_days").and_then(|x| x.as_u64()) {
                return clamp_retention_days(d as u32);
            }
        }
    }
    1
}

/// 解析 GSE `/api/gse/agent-specs` 响应为 live 采集项。
///
/// 响应是「每台 Agent 一条视图」，采集项在 `desired.spec.items` 里。
/// 同一条 `item_id` 可能出现在多台 Agent 的 spec 里（迁移会把全局采集项展开成多份拷贝，
/// 之后各自可改），因此按 `item_id` 合并：`kind` 取首个非空，`retention_days` **取最大值**
/// —— 口径冲突时宁可不提早删数据。
///
/// `enabled = false` 的项**不算 live**：它已经停止采集，数据按保留期清理。
pub fn parse_agent_specs(body: &str) -> Vec<LiveItem> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let Some(views) = v.as_array() else {
        return Vec::new();
    };
    let mut by_id: BTreeMap<String, LiveItem> = BTreeMap::new();
    for view in views {
        let Some(items) = view
            .pointer("/desired/spec/items")
            .and_then(|x| x.as_array())
        else {
            continue;
        };
        for item in items {
            let Some(item_id) = item.get("item_id").and_then(|x| x.as_str()) else {
                continue;
            };
            if item_id.is_empty() {
                continue;
            }
            if !item
                .get("enabled")
                .and_then(|x| x.as_bool())
                .unwrap_or(true)
            {
                continue;
            }
            let kind = item
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            // eBPF 侧的边聚合体积远小于日志明细、排障窗口更长，缺省 3 天（其余缺省 1 天）。
            let days = if kind.starts_with("ebpf") {
                dataplane_apm::ebpf_retention::retention_days(item)
            } else {
                retention_days_from_item(item)
            };
            let entry = by_id
                .entry(item_id.to_string())
                .or_insert_with(|| LiveItem {
                    item_id: item_id.to_string(),
                    retention_days: days,
                    kind: kind.clone(),
                });
            entry.retention_days = entry.retention_days.max(days);
            if entry.kind.is_empty() {
                entry.kind = kind;
            }
        }
    }
    by_id.into_values().collect()
}

/// 拉取 live 采集项；GSE 不可达返回错误，调用方继续处理 retain/。
pub async fn fetch_live_items(gse_admin_url: &str) -> Result<Vec<LiveItem>, DataplaneError> {
    let url = join_gse_url(gse_admin_url, "/api/gse/agent-specs", None);
    let (status, body) = gse_call("GET", &url, b"").await?;
    if status != 200 {
        return Err(DataplaneError::new(
            ErrorCode::Unavailable,
            format!("gse agent-specs HTTP {status}"),
        ));
    }
    Ok(parse_agent_specs(&body))
}

/// 与上一轮 live 集合比对，处理「消失」与「回来」两种变化。
///
/// - **消失**（从 spec 里删掉，或 `enabled=false`）：写 `retain/{item_id}`，按它上次的
///   保留天数排一次清理；这是新模型下已删采集项数据的唯一清理入口。
/// - **回来**（重新启用/加回）：删掉 `retain/{item_id}`，撤销之前的删除计划 ——
///   否则一次临时停用就会在保留期到点时把数据删掉。
///
/// 调用方必须先确认「拿到了 live 列表」：拿不到时调用本函数会把所有项误判成已删除。
pub async fn mark_removed_items(
    kv: &dyn KvStore,
    live: &[LiveItem],
    now_micros: i64,
) -> Result<usize, DataplaneError> {
    let live_ids: Vec<&str> = live.iter().map(|i| i.item_id.as_str()).collect();
    let previous = kv.scan_prefix(LIVE_PREFIX).await?;
    let mut marked = 0;
    for (key, value) in previous {
        let key_s = String::from_utf8_lossy(&key);
        let Some(item_id) = key_s.strip_prefix("spec-live/") else {
            continue;
        };
        if item_id.is_empty() || live_ids.contains(&item_id) {
            continue;
        }
        let days = serde_json::from_slice::<LiveValue>(&value)
            .map(|v| v.retention_days)
            .unwrap_or(1);
        let days = clamp_retention_days(days);
        let until = now_micros + days as i64 * MICROS_PER_DAY;
        let payload = json!({"until_micros": until});
        kv.set(
            retain_key(item_id).as_bytes(),
            payload.to_string().as_bytes(),
        )
        .await?;
        kv.delete(&key).await?;
        marked += 1;
        println!("retention: item {item_id} left the specs, retain for {days} day(s)");
    }
    for item in live {
        let retain = retain_key(&item.item_id);
        if kv.exists(retain.as_bytes()).await.unwrap_or(false) {
            kv.delete(retain.as_bytes()).await?;
            println!(
                "retention: item {} came back, cancelled pending delete",
                item.item_id
            );
        }
        let value = serde_json::to_vec(&LiveValue {
            retention_days: item.retention_days,
        })
        .map_err(|e| DataplaneError::new(ErrorCode::QueryFailed, e.to_string()))?;
        kv.set(live_key(&item.item_id).as_bytes(), &value).await?;
    }
    Ok(marked)
}

fn data_id_filter(item_id: &str, to_ts: Option<i64>) -> LogFilter {
    let mut labels = BTreeMap::new();
    labels.insert("data_id".to_string(), item_id.to_string());
    LogFilter {
        from_ts: None,
        to_ts,
        level: None,
        message_query: None,
        labels,
        limit: 0,
    }
}

/// 时序聚合指标清理结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[must_use]
pub struct TsCleanReport {
    /// 实际执行删除的采集项数。
    pub items: u64,
    pub matched_series: u64,
    pub tombstones_applied: u64,
}

/// `apply_retention` 的结果（时序部分由 `run_cleanup` 合并）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionReport {
    pub log_deleted: u64,
    pub ebpf_edges_deleted: u64,
}

/// 一次清理周期的汇总。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[must_use]
pub struct CleanupReport {
    pub log_deleted: u64,
    pub ts: TsCleanReport,
    /// 删除的 `ebpf_edges` 行数。
    pub ebpf_edges_deleted: u64,
}

/// 连续零命中多少轮后提示一次。
const ZERO_HIT_WARN_ROUNDS: u32 = 3;

/// 记录每个采集项的“连续零命中”轮数，用于提示 matcher 写错。
///
/// 由常驻清理循环持有并跨轮传递；进程重启后归零，不影响正确性。
#[derive(Debug, Default)]
pub struct TsCleanTracker {
    zero_hits: BTreeMap<String, u32>,
}

impl TsCleanTracker {
    /// 记录一轮结果；连续 [`ZERO_HIT_WARN_ROUNDS`] 轮零命中时返回 `true`。
    fn observe(&mut self, item_id: &str, matched_series: u64) -> bool {
        if matched_series > 0 {
            self.zero_hits.remove(item_id);
            return false;
        }
        let rounds = self.zero_hits.entry(item_id.to_string()).or_insert(0);
        *rounds += 1;
        *rounds == ZERO_HIT_WARN_ROUNDS
    }
}

/// 按采集项保留期删除时序聚合点。
///
/// tsink 的保留窗口是全局的（见 `ts_retention_enforced`），因此这里只处理两类
/// 全局窗口盖不住的情况：
///
/// 1. 采集项自己的 `retention_days` 比全局窗口短；
/// 2. 已删除的采集项（`retain/{item_id}`）已到 `until_micros`。
///
/// **不删除 `retain/` 键**：键的删除由 [`apply_retention`] 在日志清理后统一完成，
/// 否则下一轮就看不到已删项的到期时间。
///
/// 单个采集项删除失败不阻断其余项：错误直接返回给调用方，已完成的部分保留
/// （删除是幂等的，下一轮继续）。
pub async fn apply_ts_retention(
    ts: &dyn TimeSeriesStore,
    kv: &dyn KvStore,
    live: &[LiveItem],
    global_days: u32,
    now_micros: i64,
    tracker: &mut TsCleanTracker,
) -> Result<TsCleanReport, DataplaneError> {
    let mut report = TsCleanReport::default();
    let global_days = global_days.max(1);

    for item in live {
        let days = clamp_retention_days(item.retention_days);
        // 全局窗口已经覆盖的采集项：无需单独写墓矴。
        if days >= global_days {
            continue;
        }
        let cutoff = now_micros.saturating_sub(i64::from(days) * MICROS_PER_DAY);
        if cutoff <= 0 {
            continue;
        }
        let started = std::time::Instant::now();
        let r = ts
            .delete_series(item_selection(&item.item_id, cutoff))
            .await?;
        report.items += 1;
        report.matched_series += r.matched_series;
        report.tombstones_applied += r.tombstones_applied;
        println!(
            "ts retention: item_id={} retention_days={} matched_series={} tombstones_applied={} elapsed_ms={}",
            item.item_id,
            days,
            r.matched_series,
            r.tombstones_applied,
            started.elapsed().as_millis()
        );
        if tracker.observe(&item.item_id, r.matched_series) {
            eprintln!(
                "ts retention: item_id={} matched 0 series for {} rounds; check the item_id label or retention config",
                item.item_id, ZERO_HIT_WARN_ROUNDS
            );
        }
    }

    let rows = kv.scan_prefix(RETAIN_PREFIX).await?;
    for (key, value) in rows {
        let key_s = String::from_utf8_lossy(&key);
        let Some(item_id) = key_s.strip_prefix("retain/") else {
            continue;
        };
        if item_id.is_empty() {
            continue;
        }
        let Ok(meta) = serde_json::from_slice::<RetainValue>(&value) else {
            continue;
        };
        if now_micros < meta.until_micros || meta.until_micros <= 0 {
            continue;
        }
        let r = ts
            .delete_series(item_selection(item_id, meta.until_micros))
            .await?;
        report.items += 1;
        report.matched_series += r.matched_series;
        report.tombstones_applied += r.tombstones_applied;
    }
    Ok(report)
}

/// 按 `item_id` matcher 选一个采集项的 `[0, to_ts)`。
fn item_selection(item_id: &str, to_ts: i64) -> TsSeriesSelection {
    TsSeriesSelection {
        measurement: None,
        matchers: vec![TsMatcher {
            name: "item_id".to_string(),
            op: TsMatcherOp::Equal,
            value: item_id.to_string(),
        }],
        from_ts: 0,
        to_ts,
    }
}

/// 按 live 列表与 `retain/` 前缀删除到期日志，返回删除条数。
pub async fn apply_retention(
    sql: &dyn RelationalStore,
    log: &dyn LogStore,
    kv: &dyn KvStore,
    live: &[LiveItem],
    now_micros: i64,
) -> Result<RetentionReport, DataplaneError> {
    let mut deleted = 0u64;
    let mut ebpf_edges_deleted = 0u64;
    for item in live {
        let days = clamp_retention_days(item.retention_days) as i64;
        let cutoff = now_micros.saturating_sub(days * MICROS_PER_DAY);
        deleted += log
            .delete_matching(data_id_filter(&item.item_id, Some(cutoff)))
            .await?;
        // eBPF 边聚合走 sqlite：分批删（见 `ebpf_retention`），只处理 eBPF 采集项。
        if item.is_ebpf() {
            let report =
                dataplane_apm::ebpf_retention::delete_edges_before(sql, &item.item_id, cutoff)
                    .await?;
            ebpf_edges_deleted += report.deleted;
        }
    }

    let rows = kv.scan_prefix(RETAIN_PREFIX).await?;
    for (key, value) in rows {
        let key_s = String::from_utf8_lossy(&key);
        let Some(item_id) = key_s.strip_prefix("retain/") else {
            continue;
        };
        if item_id.is_empty() {
            continue;
        }
        let Ok(meta) = serde_json::from_slice::<RetainValue>(&value) else {
            continue;
        };
        if now_micros < meta.until_micros {
            continue;
        }
        deleted += log.delete_matching(data_id_filter(item_id, None)).await?;
        // 采集项已从 GSE 删除：边记录一次性清空（与日志同一个 `retain/` 到期口径）。
        let report =
            dataplane_apm::ebpf_retention::delete_edges_before(sql, item_id, now_micros).await?;
        ebpf_edges_deleted += report.deleted;
        kv.delete(&key).await?;
    }
    Ok(RetentionReport {
        log_deleted: deleted,
        ebpf_edges_deleted,
    })
}

/// 拉 live 列表（若已配 GSE）并执行清理。
// 参数是「一路存储一个」的独立依赖，包成结构体只会多一层壳（调用点只有 main 与测试）。
#[allow(clippy::too_many_arguments)]
pub async fn run_cleanup(
    sql: &dyn RelationalStore,
    log: &dyn LogStore,
    ts: &dyn TimeSeriesStore,
    kv: &dyn KvStore,
    gse_admin_url: Option<&str>,
    global_ts_days: u32,
    now_micros: i64,
    tracker: &mut TsCleanTracker,
) -> Result<CleanupReport, DataplaneError> {
    // `None` = 「没拿到 live 列表」（GSE 不可达 / 未配置），与「列表为空」必须在语义上分开：
    // 把前者当成空列表会让 `mark_removed_items` 把**所有**采集项误判成已删除、
    // 进而删掉全部历史数据。
    let live = match gse_admin_url {
        Some(url) => match fetch_live_items(url).await {
            Ok(items) => Some(items),
            Err(e) => {
                eprintln!(
                    "retention cleanup: fetch agent-specs failed: {}: {}",
                    e.code.as_str(),
                    e.message
                );
                None
            }
        },
        None => None,
    };
    if let Some(items) = live.as_ref() {
        if let Err(e) = mark_removed_items(kv, items, now_micros).await {
            eprintln!(
                "retention cleanup: mark removed items failed: {}",
                e.message
            );
        }
    }
    let live = live.unwrap_or_default();
    // 时序先于日志：`apply_retention` 会删除 `retain/` 键。
    let ts_report = apply_ts_retention(ts, kv, &live, global_ts_days, now_micros, tracker).await?;
    let retention = apply_retention(sql, log, kv, &live, now_micros).await?;
    Ok(CleanupReport {
        log_deleted: retention.log_deleted,
        ts: ts_report,
        ebpf_edges_deleted: retention.ebpf_edges_deleted,
    })
}

#[cfg(test)]
mod ebpf_retention_tests {
    use super::*;
    use dataplane_apm::ebpf_retention;
    use dataplane_core::SqlValue;
    use dataplane_sql::{RelationalStore, SqliteRelationalStore};

    fn store(dir: &std::path::Path) -> std::sync::Arc<dyn RelationalStore> {
        std::sync::Arc::new(SqliteRelationalStore::new(dir.join("sql.db")).unwrap())
    }

    async fn insert_edge(sql: &dyn RelationalStore, record_id: &str, bucket: i64, item_id: &str) {
        sql.execute(
                &format!(
                    "INSERT OR REPLACE INTO {} (record_id, bucket_start, bucket_micros, protocol,
                        src_ip, src_port, dst_ip, dst_port, src_service, dst_service,
                        connections, bytes_sent, bytes_recv, duration_sum, duration_max,
                        tcp_retrans, tcp_resets, failures, failure_reason, latency_hist,
                        agent_id, data_id)
                     VALUES (?1,?2,10000000,'tcp','10.0.0.5',1,'10.0.0.9',2,'a','b',1,0,0,0,0,0,0,0,'','[]','agent-1',?3)",
                    dataplane_apm::tables::EBPF_EDGES
                ),
                &[
                    SqlValue::Text(record_id.to_string()),
                    SqlValue::Integer(bucket),
                    SqlValue::Text(item_id.to_string()),
                ],
            )
            .await
            .unwrap();
    }

    async fn count(sql: &dyn RelationalStore) -> i64 {
        let result = sql
            .execute(
                &format!("SELECT COUNT(*) FROM {}", dataplane_apm::tables::EBPF_EDGES),
                &[],
            )
            .await
            .unwrap();
        match result.rows.first().and_then(|row| row.first()) {
            Some(SqlValue::Integer(i)) => *i,
            _ => 0,
        }
    }

    #[tokio::test]
    async fn deletes_in_batches_until_empty() {
        let dir = tempfile::tempdir().unwrap();
        let sql = store(dir.path());
        dataplane_apm::bootstrap(sql.as_ref()).await.unwrap();

        // 7 条过期 + 3 条仍然有效；批大小 2 → 至少 4 批。
        for index in 0..7 {
            insert_edge(sql.as_ref(), &format!("old-{index}"), 1_000, "item-ebpf").await;
        }
        for index in 0..3 {
            insert_edge(
                sql.as_ref(),
                &format!("new-{index}"),
                9_000_000,
                "item-ebpf",
            )
            .await;
        }
        let report =
            ebpf_retention::delete_edges_before_batched(sql.as_ref(), "item-ebpf", 5_000, 2)
                .await
                .unwrap();
        assert_eq!(report.deleted, 7);
        assert!(report.batches >= 4, "分批删除: {report:?}");
        assert_eq!(count(sql.as_ref()).await, 3, "有效数据不动");

        // 再跑一次：没有可删的行（说明循环能正常收敛）。
        let again =
            ebpf_retention::delete_edges_before_batched(sql.as_ref(), "item-ebpf", 5_000, 2)
                .await
                .unwrap();
        assert_eq!(again.deleted, 0);
    }

    #[tokio::test]
    async fn only_touches_the_given_item() {
        let dir = tempfile::tempdir().unwrap();
        let sql = store(dir.path());
        dataplane_apm::bootstrap(sql.as_ref()).await.unwrap();
        insert_edge(sql.as_ref(), "a", 1_000, "item-a").await;
        insert_edge(sql.as_ref(), "b", 1_000, "item-b").await;
        let report = ebpf_retention::delete_edges_before(sql.as_ref(), "item-a", 5_000)
            .await
            .unwrap();
        assert_eq!(report.deleted, 1);
        assert_eq!(count(sql.as_ref()).await, 1, "别的采集项不受影响");
    }

    #[tokio::test]
    async fn apply_retention_handles_live_and_expired_items() {
        let dir = tempfile::tempdir().unwrap();
        let sql = store(dir.path());
        let kv = dataplane_kv::RedbKvStore::new(dir.path().join("kv")).unwrap();
        let log = dataplane_log::TantivyLogStore::new(dir.path().join("logs")).unwrap();
        dataplane_apm::bootstrap(sql.as_ref()).await.unwrap();

        let now = 10 * MICROS_PER_DAY;
        // live 的 eBPF 采集项：保留 3 天 → 5 天前的边被删，1 天前的不动。
        insert_edge(sql.as_ref(), "old", now - 5 * MICROS_PER_DAY, "item-ebpf").await;
        insert_edge(sql.as_ref(), "fresh", now - MICROS_PER_DAY, "item-ebpf").await;
        let live = vec![LiveItem {
            item_id: "item-ebpf".to_string(),
            retention_days: 3,
            kind: "ebpf_network".to_string(),
        }];
        let report = apply_retention(sql.as_ref(), &log, &kv, &live, now)
            .await
            .unwrap();
        assert_eq!(report.ebpf_edges_deleted, 1);
        assert_eq!(count(sql.as_ref()).await, 1);

        // 已删除的采集项：`retain/` 到期 → 边记录清空。
        // 用一个严格早于 `now` 的桶：删除条件是 `bucket_start < cutoff`。
        insert_edge(sql.as_ref(), "leftover", now - 1, "item-gone").await;
        kv.set(retain_key("item-gone").as_bytes(), br#"{"until_micros":1}"#)
            .await
            .unwrap();
        let report = apply_retention(sql.as_ref(), &log, &kv, &[], now)
            .await
            .unwrap();
        assert_eq!(report.ebpf_edges_deleted, 1);
        assert_eq!(count(sql.as_ref()).await, 1, "只剩 live 项那一条");
        assert!(
            kv.get(retain_key("item-gone").as_bytes()).await.is_err(),
            "到期后删除 retain 键（再次读取应为 not_found）"
        );
    }

    #[test]
    fn parse_agent_specs_marks_ebpf_kind_and_default_days() {
        let body = r#"[
          {"agent_id":"a-1","desired":{"spec":{"items":[
            {"item_id":"e1","kind":"ebpf_network","enabled":true},
            {"item_id":"e2","kind":"ebpf_tcp","enabled":true,"storage":{"retention_days":7}},
            {"item_id":"l1","kind":"log_file","enabled":true},
            {"item_id":"off","kind":"log_file","enabled":false}
          ]}}}
        ]"#;
        let mut items = parse_agent_specs(body);
        items.sort_by(|a, b| a.item_id.cmp(&b.item_id));
        // 停用的项不算 live：它已停止采集，数据按保留期清理。
        assert_eq!(items.len(), 3, "{items:?}");
        assert_eq!(items[0].item_id, "e1");
        assert!(items[0].is_ebpf());
        assert_eq!(
            items[0].retention_days, 3,
            "eBPF 采集项缺省 3 天（需求 14.1）"
        );
        assert_eq!(items[1].item_id, "e2");
        assert_eq!(items[1].retention_days, 7, "显式配置优先");
        assert_eq!(items[2].item_id, "l1");
        assert!(!items[2].is_ebpf());
        assert_eq!(items[2].retention_days, 1, "日志类仍缺省 1 天");
    }

    /// 同一条 `item_id` 出现在多台 Agent 的 spec 里时按最大保留期合并：
    /// 取小了会提早删数据，取大了只是多留几天。
    #[test]
    fn parse_agent_specs_dedups_by_item_id_taking_max_retention() {
        let body = r#"[
          {"agent_id":"a-1","desired":{"spec":{"items":[
            {"item_id":"same","kind":"log_file","enabled":true,"storage":{"retention_days":1}}
          ]}}},
          {"agent_id":"a-2","desired":{"spec":{"items":[
            {"item_id":"same","kind":"log_file","enabled":true,"storage":{"retention_days":9}}
          ]}}},
          {"agent_id":"a-3","applied":{"spec":{"items":[
            {"item_id":"not-desired","kind":"log_file","enabled":true}
          ]}}}
        ]"#;
        let items = parse_agent_specs(body);
        assert_eq!(items.len(), 1, "按 item_id 合并，且只看期望值: {items:?}");
        assert_eq!(items[0].item_id, "same");
        assert_eq!(items[0].retention_days, 9, "冲突取最大值");
    }

    #[test]
    fn parse_agent_specs_tolerates_garbage() {
        assert!(parse_agent_specs("not json").is_empty());
        assert!(parse_agent_specs("{}").is_empty());
        assert!(parse_agent_specs(r#"[{"agent_id":"a-1"}]"#).is_empty());
    }

    fn live(item_id: &str, days: u32) -> LiveItem {
        LiveItem {
            item_id: item_id.to_string(),
            retention_days: days,
            kind: "log_file".to_string(),
        }
    }

    /// 采集项从 spec 里消失 → 按上次的保留期排一次清理；重新出现 → 撤销删除计划。
    ///
    /// 这是新模型下「已删采集项的数据」唯一的清理入口：per-Agent spec 没有删除事件。
    #[tokio::test]
    async fn mark_removed_items_schedules_delete_and_cancels_on_return() {
        let dir = tempfile::tempdir().unwrap();
        let kv = dataplane_kv::RedbKvStore::new(dir.path().join("kv")).unwrap();
        let now = 1_000_000_000_000i64;

        // 第一轮：两条 live。
        let marked = mark_removed_items(&kv, &[live("i1", 7), live("i2", 1)], now)
            .await
            .expect("mark");
        assert_eq!(marked, 0, "首轮没有任何东西消失");
        assert!(kv.exists(live_key("i1").as_bytes()).await.unwrap());
        assert!(
            !kv.exists(retain_key("i1").as_bytes()).await.unwrap(),
            "live 项不应有删除计划"
        );

        // 第二轮：i2 消失、i1 保留 → 只给 i2 排删除，按它自己的 1 天。
        let marked = mark_removed_items(&kv, &[live("i1", 7)], now)
            .await
            .expect("mark");
        assert_eq!(marked, 1);
        let raw = kv.get(retain_key("i2").as_bytes()).await.expect("retain");
        let meta: Value = serde_json::from_slice(&raw).expect("json");
        assert_eq!(
            meta["until_micros"].as_i64(),
            Some(now + MICROS_PER_DAY),
            "按消失前记录的保留天数排期"
        );
        assert!(!kv.exists(live_key("i2").as_bytes()).await.unwrap());

        // 第三轮：i2 回来了 → 撤销删除计划，否则一次临时停用就会在到期时删掉数据。
        let marked = mark_removed_items(&kv, &[live("i1", 7), live("i2", 1)], now)
            .await
            .expect("mark");
        assert_eq!(marked, 0);
        assert!(
            !kv.exists(retain_key("i2").as_bytes()).await.unwrap(),
            "回来的采集项必须撤销删除计划"
        );
        assert!(kv.exists(live_key("i2").as_bytes()).await.unwrap());
    }

    /// 同一条 `item_id` 的保留期在多台 Agent 的 spec 里不一致时，按最大的记录：
    /// 取小了会提早删数据。
    #[tokio::test]
    async fn mark_removed_items_uses_recorded_retention_days() {
        let dir = tempfile::tempdir().unwrap();
        let kv = dataplane_kv::RedbKvStore::new(dir.path().join("kv")).unwrap();
        let now = 5_000_000i64;

        mark_removed_items(&kv, &[live("i1", 9)], now)
            .await
            .expect("mark");
        mark_removed_items(&kv, &[], now).await.expect("mark");

        let raw = kv.get(retain_key("i1").as_bytes()).await.expect("retain");
        let meta: Value = serde_json::from_slice(&raw).expect("json");
        assert_eq!(
            meta["until_micros"].as_i64(),
            Some(now + 9 * MICROS_PER_DAY)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dataplane_ts::{TimeSeriesStore, TsPoint, TsRetentionConfig, TsinkTimeSeriesStore};

    fn store(dir: &std::path::Path) -> TsinkTimeSeriesStore {
        TsinkTimeSeriesStore::new(
            dir.join("ts"),
            TsRetentionConfig {
                enforced: false,
                ..TsRetentionConfig::default()
            },
        )
        .unwrap()
    }

    async fn instant(ts: &dyn TimeSeriesStore, ts_micros: i64) -> usize {
        ts.query_instant("m", Some(ts_micros))
            .await
            .unwrap()
            .result
            .into_iter()
            .filter(|s| s.value.is_some())
            .count()
    }

    #[tokio::test]
    async fn shorter_item_window_is_deleted_global_window_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let ts = store(dir.path());
        let kv = dataplane_kv::RedbKvStore::new(dir.path().join("kv.redb")).unwrap();
        let now = now_micros();

        let mut tags = BTreeMap::new();
        tags.insert("item_id".to_string(), "item-short".to_string());
        ts.write(TsPoint {
            measurement: "m".to_string(),
            tags: tags.clone(),
            field_name: "value".to_string(),
            field_value: 1.0,
            timestamp: now - 2 * MICROS_PER_DAY,
        })
        .await
        .unwrap();
        ts.write(TsPoint {
            measurement: "m".to_string(),
            tags: tags.clone(),
            field_name: "value".to_string(),
            field_value: 2.0,
            timestamp: now - 60_000_000,
        })
        .await
        .unwrap();
        let mut long_tags = BTreeMap::new();
        long_tags.insert("item_id".to_string(), "item-long".to_string());
        ts.write(TsPoint {
            measurement: "m".to_string(),
            tags: long_tags,
            field_name: "value".to_string(),
            field_value: 3.0,
            timestamp: now - 2 * MICROS_PER_DAY,
        })
        .await
        .unwrap();

        let live = vec![
            LiveItem {
                item_id: "item-short".to_string(),
                retention_days: 1,
                kind: "log_file".to_string(),
            },
            LiveItem {
                item_id: "item-long".to_string(),
                retention_days: 30,
                kind: "log_file".to_string(),
            },
        ];
        let report = apply_ts_retention(&ts, &kv, &live, 30, now, &mut TsCleanTracker::default())
            .await
            .unwrap();
        assert_eq!(report.items, 1, "只有短保留期的采集项参与删除");
        assert!(report.tombstones_applied >= 1);

        assert_eq!(
            instant(&ts, now - 2 * MICROS_PER_DAY).await,
            1,
            "长保留期的旧点应保留（只剩 item-long 那条）"
        );
        assert!(
            ts.query_instant("m{item_id=\"item-short\"}", Some(now - 2 * MICROS_PER_DAY))
                .await
                .unwrap()
                .result
                .is_empty(),
            "短保留期的旧点应被删除"
        );
        assert_eq!(
            instant(&ts, now - 60_000_000).await,
            1,
            "窗口内的新点应保留"
        );
    }

    #[tokio::test]
    async fn retained_deleted_item_is_cleaned_without_removing_key() {
        let dir = tempfile::tempdir().unwrap();
        let ts = store(dir.path());
        let kv = dataplane_kv::RedbKvStore::new(dir.path().join("kv.redb")).unwrap();
        let now = now_micros();
        let mut tags = BTreeMap::new();
        tags.insert("item_id".to_string(), "item-gone".to_string());
        ts.write(TsPoint {
            measurement: "m".to_string(),
            tags,
            field_name: "value".to_string(),
            field_value: 1.0,
            timestamp: now - 10 * MICROS_PER_DAY,
        })
        .await
        .unwrap();
        let until = now - MICROS_PER_DAY;
        kv.set(
            retain_key("item-gone").as_bytes(),
            format!("{{\"until_micros\":{until}}}").as_bytes(),
        )
        .await
        .unwrap();

        let report = apply_ts_retention(&ts, &kv, &[], 30, now, &mut TsCleanTracker::default())
            .await
            .unwrap();
        assert_eq!(report.items, 1);
        assert!(report.tombstones_applied >= 1);
        assert!(
            !kv.get(retain_key("item-gone").as_bytes())
                .await
                .unwrap()
                .is_empty(),
            "apply_ts_retention 不应删除 retain/ 键（由 apply_retention 负责）"
        );

        // 未到期的 retain/ 不做处理。
        // 先模拟 `apply_retention` 已删除到期键，否则 item-gone 会被幂等地再处理一次。
        kv.delete(retain_key("item-gone").as_bytes()).await.unwrap();
        kv.set(
            retain_key("item-later").as_bytes(),
            format!("{{\"until_micros\":{}}}", now + MICROS_PER_DAY).as_bytes(),
        )
        .await
        .unwrap();
        let report = apply_ts_retention(&ts, &kv, &[], 30, now, &mut TsCleanTracker::default())
            .await
            .unwrap();
        assert_eq!(report.items, 0, "未到期的已删项不参与删除");
    }

    #[test]
    fn tracker_warns_once_after_three_zero_hit_rounds() {
        let mut tracker = TsCleanTracker::default();
        assert!(!tracker.observe("item", 0));
        assert!(!tracker.observe("item", 0));
        assert!(tracker.observe("item", 0), "第 3 轮零命中应提示");
        assert!(!tracker.observe("item", 0), "同一段零命中只提示一次");
        assert!(!tracker.observe("item", 5), "有命中后清零");
        assert!(!tracker.observe("item", 0), "清零后重新计数");
    }
}
