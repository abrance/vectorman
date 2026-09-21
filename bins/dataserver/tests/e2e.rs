//! 集成 e2e：登记 dataserver → 探活 online → 新建 metrics 采集项 → Agent 上报 →
//! Prom 查到带 `agent_id` / `item_id` 标签的 `cpu_usage`。

use std::sync::Arc;
use std::time::Duration;

use dataplane_core::{resolve_data_paths, NoopAuth};
use dataplane_file::{DirFileStore, FileStore};
use dataplane_kv::{KvStore, RedbKvStore};
use dataplane_log::{LogStore, TantivyLogStore};
use dataplane_sql::{RelationalStore, SqliteRelationalStore};
use dataplane_ts::{TimeSeriesStore, TsinkTimeSeriesStore};
use dataserver::{prom_router, sql_router, AppState};
use gse_server_core::{http_router, probe_once, AdminState, DataplaneService, Ledger};

/// 与测试内上报一致的时间戳（微秒），保证 Prom 即时查询能命中。
const TS: i64 = 1_710_000_000_000_000;

fn tmp_db(tag: &str) -> String {
    std::env::temp_dir()
        .join(format!("dataserver-e2e-{}-{tag}.db", std::process::id()))
        .to_string_lossy()
        .into_owned()
}

/// 在随机端口托管路由，返回 `http://host:port`。
async fn spawn(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// 阻塞 HTTP 调用放到 blocking 线程，避免在 current-thread runtime 上死锁。
async fn http(method: &str, url: &str, body: Option<String>) -> (u16, String) {
    let url = url.to_string();
    let method = method.to_string();
    tokio::task::spawn_blocking(move || {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .build();
        let req = match method.as_str() {
            "GET" => agent.get(&url),
            "POST" => agent.post(&url),
            "PUT" => agent.put(&url),
            "DELETE" => agent.delete(&url),
            other => panic!("unsupported method {other}"),
        };
        let result = match body {
            Some(b) => req.set("Content-Type", "application/json").send_string(&b),
            None => req.call(),
        };
        match result {
            Ok(resp) => (resp.status(), resp.into_string().unwrap_or_default()),
            Err(ureq::Error::Status(code, resp)) => (code, resp.into_string().unwrap_or_default()),
            Err(e) => panic!("transport error on {method} {url}: {e}"),
        }
    })
    .await
    .expect("join")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn register_probe_collect_ingest_and_query() {
    // 1. dataserver 存储后端。
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("data");
    std::fs::create_dir_all(&root).expect("mkdir");
    let paths = resolve_data_paths(root.to_str().unwrap()).expect("paths");
    paths.ensure_dirs().expect("ensure dirs");
    let file: Arc<dyn FileStore> = Arc::new(DirFileStore::new(paths.files.clone()));
    let kv: Arc<dyn KvStore> = Arc::new(RedbKvStore::new(&paths.kv).expect("kv"));
    let sql: Arc<dyn RelationalStore> =
        Arc::new(SqliteRelationalStore::new(&paths.sql).expect("sql"));
    let ts: Arc<dyn TimeSeriesStore> = Arc::new(TsinkTimeSeriesStore::new(&paths.ts).expect("ts"));
    let log: Arc<dyn LogStore> = Arc::new(TantivyLogStore::new(&paths.logs).expect("log"));

    // 2. GSE 控制面：台账 + HTTP 管理端口。
    let ledger = Arc::new(Ledger::new(&tmp_db("chain")).expect("ledger"));
    ledger.init().await.expect("init");
    let gse_url = spawn(http_router(
        AdminState {
            ledger: ledger.clone(),
            registry: None,
            cfg: None,
        },
        None,
    ))
    .await;

    // 3. dataserver：SQL 口反代 GSE，Prom 口独立托管。
    let state = AppState {
        file,
        kv,
        sql,
        ts,
        log,
        auth: Arc::new(NoopAuth),
        gse_admin_url: Some(gse_url.clone()),
        metrics: None,
    };
    let ds_url = spawn(sql_router(state.clone(), None)).await;
    let prom_url = spawn(prom_router(state)).await;

    // 4. 登记 dataserver 并探活 → online，选路命中。
    ledger
        .upsert_dataplane(&DataplaneService {
            service_id: "ds-1".to_string(),
            ingest_url: ds_url.clone(),
            query_url: prom_url.clone(),
            status: "unknown".to_string(),
            last_seen_at: None,
            registered_at: "t".to_string(),
        })
        .await
        .expect("upsert dataplane");
    probe_once(&ledger).await.expect("probe");
    assert_eq!(
        ledger.get_dataplane("ds-1").await.unwrap().unwrap().status,
        "online"
    );
    assert_eq!(
        ledger.pick_ingest_url("agent-1").await.unwrap().as_deref(),
        Some(ds_url.as_str())
    );

    // 5. 经 dataserver 反代在 GSE 新建 metrics 采集项。
    let create = serde_json::json!({
        "name": "cpu metrics",
        "agent_ids": ["agent-1"],
        "kind": "metrics_host",
        "enabled": true,
        "collector": {"interval_secs": 15},
        "storage": {"retention_days": 1},
    })
    .to_string();
    let (status, body) = http("POST", &format!("{ds_url}/v1/collect-items"), Some(create)).await;
    assert_eq!(status, 201, "{body}");
    let item: serde_json::Value = serde_json::from_str(&body).expect("item json");
    let item_id = item["item_id"].as_str().expect("item_id").to_string();
    assert_eq!(item["agent_ids"], serde_json::json!(["agent-1"]));

    let (status, body) = http(
        "GET",
        &format!("{ds_url}/v1/collect-items?agent_id=agent-1"),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(&item_id), "{body}");

    // 6. Agent 上报 metrics（与 gse-agent-core 构造的信封一致）。
    let envelope = serde_json::json!({
        "batch_id": "b-metrics",
        "data_type": "metrics",
        "data_id": item_id,
        "agent_id": "agent-1",
        "host_id": "host-1",
        "sent_at_micros": TS,
        "records": [{
            "record_id": "m-1",
            "timestamp": TS,
            "measurement": "cpu_usage",
            "tags": {},
            "field_name": "value",
            "field_value": 12.5
        }]
    })
    .to_string();
    let (status, body) = http("POST", &format!("{ds_url}/v1/ingest"), Some(envelope)).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"status\":\"ok\""), "{body}");

    // 7. Prom 即时查询命中，标签含 agent_id 与 item_id。
    let time = TS / 1_000_000;
    let (status, body) = http(
        "GET",
        &format!("{prom_url}/api/v1/query?query=cpu_usage&time={time}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).expect("prom json");
    assert_eq!(v["status"], "success", "{body}");
    let result = v["data"]["result"]
        .as_array()
        .expect("result array")
        .clone();
    assert!(!result.is_empty(), "{body}");
    assert_eq!(result[0]["metric"]["agent_id"], "agent-1", "{body}");
    assert_eq!(result[0]["metric"]["item_id"], item_id.as_str(), "{body}");

    // 8. 流索引出现该 Agent 与采集项。
    let (status, body) = http("GET", &format!("{ds_url}/v1/streams"), None).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("agent-1"), "{body}");
    assert!(body.contains(&item_id), "{body}");
}
