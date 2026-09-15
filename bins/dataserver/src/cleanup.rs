//! 保存周期清理：live 采集项按 retention_days 删旧日志；已删项读 retain/ 到期后删除。

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dataplane_core::{DataplaneError, ErrorCode};
use dataplane_kv::KvStore;
use dataplane_log::{LogFilter, LogStore};
use serde::Deserialize;
use serde_json::Value;

/// 一天的微秒数。
pub const MICROS_PER_DAY: i64 = 86_400 * 1_000_000;

const RETAIN_PREFIX: &[u8] = b"retain/";

/// 仍在 GSE 列表中的采集项。
#[derive(Debug, Clone)]
pub struct LiveItem {
    pub item_id: String,
    pub retention_days: u32,
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

/// 解析 GSE 采集项列表响应。
pub fn parse_collect_items(body: &str) -> Vec<LiveItem> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let items: Vec<Value> = if let Some(arr) = v.as_array() {
        arr.clone()
    } else if let Some(arr) = v.get("items").and_then(|x| x.as_array()) {
        arr.clone()
    } else if v.get("item_id").is_some() {
        vec![v]
    } else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let item_id = item.get("item_id")?.as_str()?.to_string();
            if item_id.is_empty() {
                return None;
            }
            Some(LiveItem {
                item_id,
                retention_days: retention_days_from_item(item),
            })
        })
        .collect()
}

/// 拉取 live 采集项；GSE 不可达返回错误，调用方继续处理 retain/。
pub async fn fetch_live_items(gse_admin_url: &str) -> Result<Vec<LiveItem>, DataplaneError> {
    let url = join_gse_url(gse_admin_url, "/api/gse/collect-items", None);
    let (status, body) = gse_call("GET", &url, b"").await?;
    if status != 200 {
        return Err(DataplaneError::new(
            ErrorCode::Unavailable,
            format!("gse collect-items HTTP {status}"),
        ));
    }
    Ok(parse_collect_items(&body))
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

/// 按 live 列表与 `retain/` 前缀删除到期日志。
pub async fn apply_retention(
    log: &dyn LogStore,
    kv: &dyn KvStore,
    live: &[LiveItem],
    now_micros: i64,
) -> Result<(), DataplaneError> {
    for item in live {
        let days = clamp_retention_days(item.retention_days) as i64;
        let cutoff = now_micros.saturating_sub(days * MICROS_PER_DAY);
        log.delete_matching(data_id_filter(&item.item_id, Some(cutoff)))
            .await?;
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
        log.delete_matching(data_id_filter(item_id, None)).await?;
        kv.delete(&key).await?;
    }
    Ok(())
}

/// 拉 live 列表（若已配 GSE）并执行清理。
pub async fn run_cleanup(
    log: &dyn LogStore,
    kv: &dyn KvStore,
    gse_admin_url: Option<&str>,
    now_micros: i64,
) -> Result<(), DataplaneError> {
    let live = match gse_admin_url {
        Some(url) => match fetch_live_items(url).await {
            Ok(items) => items,
            Err(e) => {
                eprintln!(
                    "retention cleanup: fetch collect-items failed: {}: {}",
                    e.code.as_str(),
                    e.message
                );
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    apply_retention(log, kv, &live, now_micros).await
}
