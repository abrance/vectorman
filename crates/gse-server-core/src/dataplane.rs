//! 数据面探活：对每条登记记录 `GET {ingest_url}/health`，维护 `online` / `offline`。

use std::sync::Arc;
use std::time::Duration;

use crate::ledger::Ledger;

const PROBE_TIMEOUT_SECS: u64 = 3;

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
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::DataplaneService;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

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
