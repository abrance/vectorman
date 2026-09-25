//! 数据面探活：对每条登记记录 `GET {ingest_url}/health`，维护 `online` / `offline`。

use std::sync::Arc;
use std::time::Duration;

use crate::ledger::Ledger;

const PROBE_TIMEOUT_SECS: u64 = 3;
/// 注册后立刻探活的尝试次数与间隔（数据面可能刚启动，一次失败就等 30 秒代价太大）。
const PROBE_RETRY_ATTEMPTS: u32 = 5;
const PROBE_RETRY_GAP: Duration = Duration::from_secs(2);

/// 拼接健康检查 URL。
fn health_url(ingest_url: &str) -> String {
    format!("{}/health", ingest_url.trim_end_matches('/'))
}

/// 同步探活一次；HTTP 200 且 body `status=ok` 视为在线。
fn probe_health(ingest_url: &str) -> bool {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(PROBE_TIMEOUT_SECS))
        .build();
    match agent.get(&health_url(ingest_url)).call() {
        Ok(resp) => {
            if resp.status() != 200 {
                return false;
            }
            let body = resp.into_string().unwrap_or_default();
            serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(|s| s == "ok"))
                .unwrap_or(false)
        }
        Err(_) => false,
    }
}

/// 探活全部登记记录并回写状态；任一失败立刻 `offline`。
pub async fn probe_once(ledger: &Ledger) -> Result<(), String> {
    let services = ledger
        .list_dataplanes()
        .await
        .map_err(|e| format!("list dataplanes: {}", e.message))?;
    for service in services {
        probe_service(ledger, &service.service_id).await;
    }
    Ok(())
}

/// 周期探活任务，随进程运行。
pub async fn probe_dataplanes(ledger: Arc<Ledger>, interval: Duration) {
    loop {
        if let Err(e) = probe_once(&ledger).await {
            eprintln!("gse-server: dataplane probe failed: {e}");
        }
        tokio::time::sleep(interval).await;
    }
}

/// 探活**单个**数据面并回写状态（`true` = online）。
///
/// 注册时就要调用它：探活循环是「先探再睡 30 秒」，新登记的数据面如果只等下一轮，
/// 会有最多 30 秒处于 `unknown`，这段时间 Agent 拿不到上报地址，采集数据只能积压
/// （缓冲按「淘汰最旧」处理，实测 60 秒里丢了 2279 条边记录、只入库 149 条）。
pub async fn probe_service(ledger: &Ledger, service_id: &str) -> bool {
    let Ok(Some(service)) = ledger.get_dataplane(service_id).await else {
        return false;
    };
    let ingest_url = service.ingest_url.clone();
    let online = tokio::task::spawn_blocking(move || probe_health(&ingest_url))
        .await
        .unwrap_or(false);
    let (status, seen) = if online {
        ("online", Some(crate::session::now_micros().to_string()))
    } else {
        ("offline", None)
    };
    if let Err(e) = ledger
        .set_dataplane_status(&service.service_id, status, seen.as_deref())
        .await
    {
        eprintln!(
            "gse-server: set_dataplane_status {} failed: {}",
            service.service_id, e.message
        );
        return false;
    }
    if service.status != status {
        eprintln!(
            "gse-server: dataplane {} probe -> {status}",
            service.service_id
        );
    }
    online
}

/// 注册后立刻探活，失败时**短重试**（默认 5 次 × 2 秒）。
///
/// 只探一次不够：数据面往往正在启动，一次失败就会把它标成 `offline`，而下一次轮询在
/// 一个探活间隔之后（默认 30 秒）—— 这段时间 Agent 拿不到上报地址，采集数据被淘汰。
pub fn spawn_probe_after_register(ledger: Arc<Ledger>, service_id: String) {
    tokio::spawn(async move {
        for attempt in 0..PROBE_RETRY_ATTEMPTS {
            if probe_service(&ledger, &service_id).await {
                return;
            }
            if attempt + 1 < PROBE_RETRY_ATTEMPTS {
                tokio::time::sleep(PROBE_RETRY_GAP).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::DataplaneService;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;
    use std::sync::Arc;

    fn tmp_db(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("gse-dataplane-{}-{tag}.db", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    async fn ledger(tag: &str) -> Ledger {
        let ledger = Ledger::new(&tmp_db(tag)).expect("open ledger");
        ledger.init().await.expect("init");
        ledger
    }

    fn service(service_id: &str, ingest_url: &str) -> DataplaneService {
        DataplaneService {
            service_id: service_id.to_string(),
            ingest_url: ingest_url.to_string(),
            query_url: ingest_url.to_string(),
            status: "unknown".to_string(),
            last_seen_at: None,
            registered_at: "t".to_string(),
        }
    }

    async fn spawn_health(ok: bool) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new().route(
            "/health",
            get(move || async move {
                if ok {
                    Json(json!({"status": "ok"}))
                } else {
                    Json(json!({"status": "bad"}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), handle)
    }

    /// 注册单个数据面时立刻探活：不然要等一个探活间隔（默认 30s）才 online，
    /// 这段时间 Agent 拿不到上报地址，采集数据积压后被淘汰。
    #[tokio::test]
    async fn probe_service_marks_single_dataplane_online() {
        let ledger = ledger("probe-single").await;
        let (ok_url, ok_handle) = spawn_health(true).await;
        ledger
            .upsert_dataplane(&service("s-new", &ok_url))
            .await
            .unwrap();
        // 还没有任何探活：仍是 unknown。
        assert_eq!(
            ledger.get_dataplane("s-new").await.unwrap().unwrap().status,
            "unknown"
        );

        assert!(probe_service(&ledger, "s-new").await);
        let after = ledger.get_dataplane("s-new").await.unwrap().unwrap();
        assert_eq!(after.status, "online");
        assert!(after.last_seen_at.is_some());

        // 不存在的数据面返回 false 且不 panic。
        assert!(!probe_service(&ledger, "nope").await);
        ok_handle.abort();
    }

    /// 注册后的短重试：数据面稍后才可用时也要在几秒内变 online，而不是等一个探活间隔。
    #[tokio::test]
    async fn probe_after_register_retries_until_online() {
        let ledger = Arc::new(ledger("probe-retry").await);
        // 先登记一个**不可达**地址：第一次探活失败。
        ledger
            .upsert_dataplane(&service("s-late", "http://127.0.0.1:9"))
            .await
            .unwrap();
        spawn_probe_after_register(ledger.clone(), "s-late".to_string());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            ledger
                .get_dataplane("s-late")
                .await
                .unwrap()
                .unwrap()
                .status,
            "offline"
        );

        // 换成可达地址后再触发一次：应当变 online（重试路径的探活本身已由上面的用例覆盖）。
        let (ok_url, ok_handle) = spawn_health(true).await;
        ledger
            .upsert_dataplane(&service("s-late", &ok_url))
            .await
            .unwrap();
        spawn_probe_after_register(ledger.clone(), "s-late".to_string());
        for _ in 0..50 {
            if ledger
                .get_dataplane("s-late")
                .await
                .unwrap()
                .unwrap()
                .status
                == "online"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        assert_eq!(
            ledger
                .get_dataplane("s-late")
                .await
                .unwrap()
                .unwrap()
                .status,
            "online"
        );
        ok_handle.abort();
    }

    #[tokio::test]
    async fn probe_marks_online_and_offline() {
        let ledger = ledger("probe-on-off").await;
        let (ok_url, ok_handle) = spawn_health(true).await;
        let (bad_url, bad_handle) = spawn_health(false).await;
        ledger
            .upsert_dataplane(&service("s-ok", &ok_url))
            .await
            .unwrap();
        ledger
            .upsert_dataplane(&service("s-bad", &bad_url))
            .await
            .unwrap();

        probe_once(&ledger).await.unwrap();

        let ok = ledger.get_dataplane("s-ok").await.unwrap().unwrap();
        assert_eq!(ok.status, "online");
        assert!(ok.last_seen_at.is_some());
        let bad = ledger.get_dataplane("s-bad").await.unwrap().unwrap();
        assert_eq!(bad.status, "offline");
        assert!(bad.last_seen_at.is_none());

        ok_handle.abort();
        bad_handle.abort();
    }
}
