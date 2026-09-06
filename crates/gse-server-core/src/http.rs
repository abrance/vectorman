//! HTTP 管理端口：以 axum 暴露台账四表的增删改查，供运维预登记与查询。
//!
//! 该模块仅操作 `Ledger` 与可选的会话注册表；v1 管理接口不鉴权，
//! 默认仅监听回环地址。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use gse_proto::GseError;
use serde_json::json;

use crate::ledger::{ledger_stamp, AccessPoint, Agent, AgentConfig, Host, Ledger};
use crate::session::SessionRegistry;

/// HTTP 管理端口共享状态：台账 + 可选的会话注册表。
#[derive(Clone)]
pub struct AdminState {
    pub ledger: Arc<Ledger>,
    /// 删除 Agent 时联动清理活跃会话；独立部署管理端口时为 None。
    pub registry: Option<Arc<SessionRegistry>>,
}

fn err_json(status: StatusCode, e: GseError) -> Response {
    (status, Json(json!({"error": e.message, "code": e.code}))).into_response()
}

fn created<T: serde::Serialize>(value: &T) -> Response {
    (StatusCode::CREATED, Json(value)).into_response()
}

fn ok<T: serde::Serialize>(value: &T) -> Response {
    Json(value).into_response()
}

fn require(present: bool, field: &str) -> Option<Response> {
    if present {
        None
    } else {
        Some(err_json(
            StatusCode::BAD_REQUEST,
            GseError::new(
                "invalid_argument",
                format!("missing required field: {field}"),
            ),
        ))
    }
}

fn from_json_err(e: axum::extract::rejection::JsonRejection) -> Response {
    err_json(
        StatusCode::BAD_REQUEST,
        GseError::new("invalid_argument", format!("invalid JSON body: {e}")),
    )
}

/// 构造台账管理路由，状态为 `AdminState`。
pub fn router(admin: AdminState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/hosts", get(list_hosts).post(create_host))
        .route("/hosts/{host_id}", get(get_host).delete(delete_host))
        .route(
            "/access-points",
            get(list_access_points).post(create_access_point),
        )
        .route(
            "/access-points/{id}",
            get(get_access_point).delete(delete_access_point),
        )
        .route("/agents", get(list_agents).post(create_agent))
        .route("/agents/{agent_id}", get(get_agent).delete(delete_agent))
        .route(
            "/agent-configs",
            get(list_agent_configs).post(create_agent_config),
        )
        .route("/agent-configs/{agent_id}", get(get_agent_config))
        .with_state(admin)
}

/// 绑定并托管 HTTP 管理端口；成功后持续运行直至底层错误。
pub async fn serve(admin: AdminState, listen: &str) -> Result<(), GseError> {
    let app = router(admin);
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| GseError::new("query_failed", format!("bind http {listen}: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| GseError::new("query_failed", e.to_string()))?;
    println!("gse-server: http management listening on {addr}");
    axum::serve(listener, app)
        .await
        .map_err(|e| GseError::new("query_failed", e.to_string()))
}

async fn health() -> Response {
    Json(json!({"status": "ok"})).into_response()
}

// ---- hosts ----

async fn list_hosts(State(admin): State<AdminState>) -> Response {
    match admin.ledger.list_hosts().await {
        Ok(v) => ok(&v),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn create_host(
    State(admin): State<AdminState>,
    body: Result<Json<Host>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(host) = match body {
        Ok(b) => b,
        Err(e) => return from_json_err(e),
    };
    if let Some(resp) = require(!host.host_id.trim().is_empty(), "host_id") {
        return resp;
    }
    if let Some(resp) = require(!host.inner_ip.trim().is_empty(), "inner_ip") {
        return resp;
    }
    let mut h = host;
    if h.created_at.is_empty() {
        h.created_at = ledger_stamp();
    }
    match admin.ledger.upsert_host(&h).await {
        Ok(()) => created(&h),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn get_host(State(admin): State<AdminState>, Path(id): Path<String>) -> Response {
    match admin.ledger.get_host(&id).await {
        Ok(Some(h)) => ok(&h),
        Ok(None) => err_json(
            StatusCode::NOT_FOUND,
            GseError::new("not_found", format!("host {id} not found")),
        ),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn delete_host(State(admin): State<AdminState>, Path(id): Path<String>) -> Response {
    match admin.ledger.remove_host(&id).await {
        Ok(()) => Json(json!({"deleted": id})).into_response(),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

// ---- access_points ----

async fn list_access_points(State(admin): State<AdminState>) -> Response {
    match admin.ledger.list_access_points().await {
        Ok(v) => ok(&v),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn create_access_point(
    State(admin): State<AdminState>,
    body: Result<Json<AccessPoint>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(ap) = match body {
        Ok(b) => b,
        Err(e) => return from_json_err(e),
    };
    if let Some(resp) = require(!ap.id.trim().is_empty(), "id") {
        return resp;
    }
    if let Some(resp) = require(!ap.name.trim().is_empty(), "name") {
        return resp;
    }
    if let Some(resp) = require(!ap.server_ip.trim().is_empty(), "server_ip") {
        return resp;
    }
    let mut ap = ap;
    if ap.created_at.is_empty() {
        ap.created_at = ledger_stamp();
    }
    match admin.ledger.upsert_access_point(&ap).await {
        Ok(()) => created(&ap),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn get_access_point(State(admin): State<AdminState>, Path(id): Path<String>) -> Response {
    match admin.ledger.get_access_point(&id).await {
        Ok(Some(ap)) => ok(&ap),
        Ok(None) => err_json(
            StatusCode::NOT_FOUND,
            GseError::new("not_found", format!("access point {id} not found")),
        ),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn delete_access_point(State(admin): State<AdminState>, Path(id): Path<String>) -> Response {
    match admin.ledger.remove_access_point(&id).await {
        Ok(()) => Json(json!({"deleted": id})).into_response(),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

// ---- agents ----

async fn list_agents(State(admin): State<AdminState>) -> Response {
    match admin.ledger.list_agents().await {
        Ok(v) => ok(&v),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn create_agent(
    State(admin): State<AdminState>,
    body: Result<Json<Agent>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(agent) = match body {
        Ok(b) => b,
        Err(e) => return from_json_err(e),
    };
    if let Some(resp) = require(!agent.agent_id.trim().is_empty(), "agent_id") {
        return resp;
    }
    if let Some(resp) = require(!agent.host_id.trim().is_empty(), "host_id") {
        return resp;
    }
    if let Some(resp) = require(!agent.token.trim().is_empty(), "token") {
        return resp;
    }
    let mut a = agent;
    a.status = "unknown".to_string();
    a.last_heartbeat_at = None;
    if a.registered_at.is_empty() {
        a.registered_at = ledger_stamp();
    }
    match admin.ledger.upsert_agent(&a).await {
        Ok(()) => created(&a),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn get_agent(State(admin): State<AdminState>, Path(id): Path<String>) -> Response {
    match admin.ledger.get_agent(&id).await {
        Ok(Some(a)) => ok(&a),
        Ok(None) => err_json(
            StatusCode::NOT_FOUND,
            GseError::new("not_found", format!("agent {id} not found")),
        ),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn delete_agent(State(admin): State<AdminState>, Path(id): Path<String>) -> Response {
    let ledger = &admin.ledger;
    if let Err(e) = ledger.remove_agent(&id).await {
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    // 级联清 Agent 运行时配置与活跃会话，保证删除后节点不可再被操作。
    if let Err(e) = ledger.remove_agent_config(&id).await {
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    if let Some(registry) = &admin.registry {
        if let Some(session) = registry.remove(&id).await {
            let _ = session.end.close().await;
        }
    }
    Json(json!({"deleted": id})).into_response()
}

// ---- agent_configs ----

async fn list_agent_configs(State(admin): State<AdminState>) -> Response {
    match admin.ledger.list_agent_configs().await {
        Ok(v) => ok(&v),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn create_agent_config(
    State(admin): State<AdminState>,
    body: Result<Json<AgentConfig>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(cfg) = match body {
        Ok(b) => b,
        Err(e) => return from_json_err(e),
    };
    if let Some(resp) = require(!cfg.agent_id.trim().is_empty(), "agent_id") {
        return resp;
    }
    if let Some(resp) = require(!cfg.host_id.trim().is_empty(), "host_id") {
        return resp;
    }
    let mut cfg = cfg;
    if cfg.log_level.is_empty() {
        cfg.log_level = "info".to_string();
    }
    cfg.updated_at = ledger_stamp();
    match admin.ledger.upsert_agent_config(&cfg).await {
        Ok(()) => created(&cfg),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn get_agent_config(
    State(admin): State<AdminState>,
    Path(agent_id): Path<String>,
) -> Response {
    match admin.ledger.get_agent_config(&agent_id).await {
        Ok(Some(c)) => ok(&c),
        Ok(None) => err_json(
            StatusCode::NOT_FOUND,
            GseError::new("not_found", format!("agent config {agent_id} not found")),
        ),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    fn test_db(name: &str) -> String {
        std::env::temp_dir()
            .join(format!("gse-http-{}-{name}.db", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    async fn app_ledger(name: &str) -> (Router, Arc<Ledger>) {
        let db = test_db(name);
        let ledger = Arc::new(Ledger::new(&db).expect("open"));
        ledger.init().await.expect("init");
        let app = router(AdminState {
            ledger: ledger.clone(),
            registry: None,
        });
        (app, ledger)
    }

    async fn send(app: &mut Router, req: Request<Body>) -> (StatusCode, String) {
        let resp = app.clone().oneshot(req).await.expect("oneshot response");
        let status = resp.status();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("collect")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn req(method: &str, uri: &str, body: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(b) = body {
            builder = builder.header("content-type", "application/json");
            return builder.body(Body::from(b.to_string())).expect("request");
        }
        builder.body(Body::empty()).expect("request")
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let (mut app, _ledger) = app_ledger("health").await;
        let (status, body) = send(&mut app, req("GET", "/health", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"status\":\"ok\""), "{body}");
    }

    #[tokio::test]
    async fn hosts_crud_and_validation() {
        let (mut app, _ledger) = app_ledger("hosts").await;

        // 缺 inner_ip -> 400
        let (status, body) = send(
            &mut app,
            req("POST", "/hosts", Some(r#"{"host_id":"h-1"}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("inner_ip"), "{body}");

        // 创建 -> 201
        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/hosts",
                Some(r#"{"host_id":"h-1","inner_ip":"10.0.0.1","hostname":"web-1"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(body.contains("10.0.0.1"), "{body}");

        // 列表 -> 200
        let (status, body) = send(&mut app, req("GET", "/hosts", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("h-1"), "{body}");

        // 查询单个 -> 200，缺失 -> 404
        let (status, body) = send(&mut app, req("GET", "/hosts/h-1", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("web-1"), "{body}");
        let (status, _) = send(&mut app, req("GET", "/hosts/ghost", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // 删除 -> 200，删除后再查询 404
        let (status, _) = send(&mut app, req("DELETE", "/hosts/h-1", None)).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = send(&mut app, req("GET", "/hosts/h-1", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn agents_crud_and_validation() {
        let (mut app, _ledger) = app_ledger("agents").await;

        // 缺 token -> 400
        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/agents",
                Some(r#"{"agent_id":"a-1","host_id":"h-1"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("token"), "{body}");

        // 创建 -> 201，状态归为 unknown
        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/agents",
                Some(r#"{"agent_id":"a-1","host_id":"h-1","token":"tok-a","version":"0.1.0"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(body.contains("\"token\":\"tok-a\""), "{body}");
        assert!(body.contains("\"status\":\"unknown\""), "{body}");

        // 列表 -> 200 含 token
        let (status, body) = send(&mut app, req("GET", "/agents", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("tok-a"), "{body}");

        // 查询 -> 200；缺失 -> 404
        let (status, body) = send(&mut app, req("GET", "/agents/a-1", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("tok-a"), "{body}");
        let (status, _) = send(&mut app, req("GET", "/agents/ghost", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = send(&mut app, req("DELETE", "/agents/a-1", None)).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = send(&mut app, req("GET", "/agents/a-1", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn access_points_and_agent_configs_crud() {
        let (mut app, _ledger) = app_ledger("misc").await;

        let (status, _) = send(
            &mut app,
            req(
                "POST",
                "/access-points",
                Some(r#"{"id":"ap-1","name":"main","server_ip":"192.168.1.1","rpc_port":7100}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, body) = send(&mut app, req("GET", "/access-points", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("ap-1"), "{body}");
        let (status, _) = send(&mut app, req("GET", "/access-points/ghost", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/agent-configs",
                Some(r#"{"agent_id":"a-1","host_id":"h-1","cpu_limit_percent":50}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(body.contains("\"log_level\":\"info\""), "{body}");
        let (status, body) = send(&mut app, req("GET", "/agent-configs/a-1", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("50"), "{body}");
        let (status, body) = send(
            &mut app,
            req("POST", "/agent-configs", Some(r#"{"host_id":"h-1"}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    #[tokio::test]
    async fn delete_agent_cascades_config_but_keeps_host() {
        let (mut app, ledger) = app_ledger("cascade").await;

        send(
            &mut app,
            req(
                "POST",
                "/hosts",
                Some(r#"{"host_id":"h-1","inner_ip":"10.0.0.1"}"#),
            ),
        )
        .await;
        send(
            &mut app,
            req(
                "POST",
                "/agents",
                Some(r#"{"agent_id":"a-1","host_id":"h-1","token":"tok-a"}"#),
            ),
        )
        .await;
        send(
            &mut app,
            req(
                "POST",
                "/agent-configs",
                Some(r#"{"agent_id":"a-1","host_id":"h-1","cpu_limit_percent":50}"#),
            ),
        )
        .await;

        let (status, _) = send(&mut app, req("DELETE", "/agents/a-1", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(ledger.get_agent("a-1").await.expect("get").is_none());
        assert!(
            ledger.get_agent_config("a-1").await.expect("get").is_none(),
            "agent config should be cascaded away"
        );
        // host 不随 agent 删除而消失。
        assert!(ledger.get_host("h-1").await.expect("get").is_some());
    }
}
