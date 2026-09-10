//! HTTP 端口：以 axum 暴露台账四表的增删改查，供运维预登记与查询。
//!
//! 台账 API 统一挂在 `/api/gse` 前缀下（与前端 `@vectorman/*` 的
//! `GseAdminAdapter` 前缀一致）；根路径仅保留 `/health`。可选的
//! `web_dir` 使同一端口同时托管前端 dist：`ServeDir` 找不到文件时回退
//! `index.html`，满足 SPA 客户端路由。
//!
//! 该模块仅操作 `Ledger` 与可选的会话注册表；v1 管理接口不鉴权，
//! 默认仅监听回环地址。

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use gse_proto::GseError;
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};

use crate::config::ServerConfig;
use crate::ledger::{ledger_stamp, AccessPoint, Agent, AgentConfig, Host, JobTemplate, Ledger};
use crate::rerun::RerunRequest;
use crate::server::{submit_job, submit_job_with_template, submit_rerun, JobSubmit};
use crate::session::SessionRegistry;
use crate::template::{new_template_id, TemplateInput};

/// HTTP 管理端口共享状态：台账 + 可选的会话注册表 + 可选的 server 配置。
#[derive(Clone)]
pub struct AdminState {
    pub ledger: Arc<Ledger>,
    /// 删除 Agent 时联动清理活跃会话；独立部署管理端口时为 None。
    pub registry: Option<Arc<SessionRegistry>>,
    /// 作业提交校验所需的 server 配置；独立部署管理端口时为 None，作业接口不可用。
    pub cfg: Option<Arc<ServerConfig>>,
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

/// 台账 CRUD 子路由（相对路径），统一挂到 `/api/gse` 前缀下。
fn ledger_routes(admin: AdminState) -> Router {
    Router::new()
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
        .route("/jobs", get(list_jobs).post(create_job))
        .route("/jobs/{job_id}", get(get_job))
        .route("/jobs/{job_id}/rerun", post(rerun_job))
        .route(
            "/jobs/{job_id}/save-as-template",
            post(save_job_as_template),
        )
        .route(
            "/job-templates",
            get(list_job_templates).post(create_job_template),
        )
        .route(
            "/job-templates/{template_id}",
            get(get_job_template)
                .put(update_job_template)
                .delete(delete_job_template),
        )
        .route(
            "/job-templates/{template_id}/submit",
            post(submit_job_template),
        )
        .with_state(admin)
}

/// 构造 HTTP 服务路由：台账 API 挂在 `/api/gse` 前缀，根路径保留 `/health`。
/// 提供 `web_dir` 时同一端口托管该目录下的前端 dist，未命中的路径回退
/// `index.html`（SPA 客户端路由），已存在的静态资源（JS/CSS/字体）正常返回。
pub fn router(admin: AdminState, web_dir: Option<&std::path::Path>) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .nest("/api/gse", ledger_routes(admin));
    match web_dir {
        Some(dir) => {
            let index = dir.join("index.html");
            api.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)))
        }
        None => api,
    }
}

/// 绑定并托管 HTTP 管理端口；成功后持续运行直至底层错误。
pub async fn serve(
    admin: AdminState,
    listen: &str,
    web_dir: Option<String>,
) -> Result<(), GseError> {
    let app = router(admin, web_dir.as_deref().map(std::path::Path::new));
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| GseError::new("query_failed", format!("bind http {listen}: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| GseError::new("query_failed", e.to_string()))?;
    println!("gse-server: http management listening on {addr}");
    if let Some(dir) = web_dir {
        if std::path::Path::new(&dir).join("index.html").exists() {
            println!("gse-server: serving web dist from {dir} on {addr}");
        } else {
            eprintln!("gse-server: web_dir {dir} missing index.html; static UI disabled");
        }
    }
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

// ---- jobs ----

#[derive(serde::Deserialize)]
struct JobListQuery {
    agent_id: Option<String>,
    status: Option<String>,
    limit: Option<i64>,
}

fn job_status(code: &str) -> StatusCode {
    match code {
        "invalid_argument" => StatusCode::BAD_REQUEST,
        "not_found" => StatusCode::NOT_FOUND,
        "already_exists" => StatusCode::CONFLICT,
        "unavailable" => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn list_jobs(State(admin): State<AdminState>, Query(q): Query<JobListQuery>) -> Response {
    match admin
        .ledger
        .list_jobs(q.agent_id.as_deref(), q.status.as_deref(), q.limit)
        .await
    {
        Ok(v) => ok(&v),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn create_job(
    State(admin): State<AdminState>,
    body: Result<Json<JobSubmit>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => return from_json_err(e),
    };
    let (Some(registry), Some(cfg)) = (admin.registry.as_ref(), admin.cfg.as_ref()) else {
        return err_json(
            StatusCode::SERVICE_UNAVAILABLE,
            GseError::new("unavailable", "job dispatch requires the in-process server"),
        );
    };
    match submit_job(&admin.ledger, registry, cfg, req).await {
        Ok(record) => created(&record),
        Err(e) => err_json(job_status(&e.code), e),
    }
}

async fn get_job(State(admin): State<AdminState>, Path(job_id): Path<String>) -> Response {
    match admin.ledger.get_job(&job_id).await {
        Ok(Some(j)) => ok(&j),
        Ok(None) => err_json(
            StatusCode::NOT_FOUND,
            GseError::new("not_found", format!("job {job_id} not found")),
        ),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// 重做历史作业：以来源作业为默认值，按可选请求体覆盖后提交新作业。
/// 请求体可省略（等价于空对象，原样重做）。
async fn rerun_job(
    State(admin): State<AdminState>,
    Path(job_id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let source = match admin.ledger.get_job(&job_id).await {
        Ok(Some(j)) => j,
        Ok(None) => {
            return err_json(
                StatusCode::NOT_FOUND,
                GseError::new("not_found", format!("job {job_id} not found")),
            )
        }
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let req = if body.is_empty() {
        RerunRequest::default()
    } else {
        match serde_json::from_slice::<RerunRequest>(&body) {
            Ok(r) => r,
            Err(e) => {
                return err_json(
                    StatusCode::BAD_REQUEST,
                    GseError::new("invalid_argument", e.to_string()),
                )
            }
        }
    };
    let (Some(registry), Some(cfg)) = (admin.registry.as_ref(), admin.cfg.as_ref()) else {
        return err_json(
            StatusCode::SERVICE_UNAVAILABLE,
            GseError::new("unavailable", "job dispatch requires the in-process server"),
        );
    };
    match submit_rerun(&admin.ledger, registry, cfg, &source, req).await {
        Ok(record) => created(&record),
        Err(e) => err_json(job_status(&e.code), e),
    }
}

// ---- 作业模板 ----

#[derive(serde::Deserialize)]
struct TemplateListQuery {
    name: Option<String>,
    limit: Option<i64>,
}

#[derive(serde::Deserialize)]
struct TemplateSubmitRequest {
    #[serde(default)]
    agent_id: String,
    #[serde(default)]
    vars: BTreeMap<String, String>,
}

#[derive(serde::Deserialize)]
struct SaveAsTemplateRequest {
    #[serde(default)]
    name: String,
}

/// 模板接口在管理端口独立部署时回退默认配置（仅影响默认超时/上限）。
fn template_cfg(admin: &AdminState) -> Arc<ServerConfig> {
    admin
        .cfg
        .clone()
        .unwrap_or_else(|| Arc::new(ServerConfig::default()))
}

/// 校验并写入模板，返回落库后的完整记录。
async fn write_template(
    admin: &AdminState,
    input: &TemplateInput,
    cfg: &ServerConfig,
) -> Result<JobTemplate, GseError> {
    input.validate(cfg)?;
    let template_id = new_template_id();
    let now = ledger_stamp();
    admin
        .ledger
        .insert_template(&template_id, &input.to_new_template(cfg), &now)
        .await?;
    admin
        .ledger
        .get_template(&template_id)
        .await?
        .ok_or_else(|| GseError::new("query_failed", "template missing after insert"))
}

async fn list_job_templates(
    State(admin): State<AdminState>,
    Query(q): Query<TemplateListQuery>,
) -> Response {
    match admin
        .ledger
        .list_templates(q.name.as_deref(), q.limit)
        .await
    {
        Ok(v) => ok(&v),
        Err(e) => err_json(job_status(&e.code), e),
    }
}

async fn create_job_template(
    State(admin): State<AdminState>,
    body: Result<Json<TemplateInput>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(input) = match body {
        Ok(b) => b,
        Err(e) => return from_json_err(e),
    };
    let cfg = template_cfg(&admin);
    match write_template(&admin, &input, &cfg).await {
        Ok(t) => created(&t),
        Err(e) => err_json(job_status(&e.code), e),
    }
}

async fn get_job_template(
    State(admin): State<AdminState>,
    Path(template_id): Path<String>,
) -> Response {
    match admin.ledger.get_template(&template_id).await {
        Ok(Some(t)) => ok(&t),
        Ok(None) => err_json(
            StatusCode::NOT_FOUND,
            GseError::new("not_found", format!("template {template_id} not found")),
        ),
        Err(e) => err_json(job_status(&e.code), e),
    }
}

async fn update_job_template(
    State(admin): State<AdminState>,
    Path(template_id): Path<String>,
    body: Result<Json<TemplateInput>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(input) = match body {
        Ok(b) => b,
        Err(e) => return from_json_err(e),
    };
    match admin.ledger.get_template(&template_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return err_json(
                StatusCode::NOT_FOUND,
                GseError::new("not_found", format!("template {template_id} not found")),
            )
        }
        Err(e) => return err_json(job_status(&e.code), e),
    }
    let cfg = template_cfg(&admin);
    if let Err(e) = input.validate(&cfg) {
        return err_json(job_status(&e.code), e);
    }
    if let Err(e) = admin
        .ledger
        .update_template(&template_id, &input.to_new_template(&cfg), &ledger_stamp())
        .await
    {
        return err_json(job_status(&e.code), e);
    }
    match admin.ledger.get_template(&template_id).await {
        Ok(Some(t)) => ok(&t),
        Ok(None) => err_json(
            StatusCode::NOT_FOUND,
            GseError::new("not_found", format!("template {template_id} not found")),
        ),
        Err(e) => err_json(job_status(&e.code), e),
    }
}

async fn delete_job_template(
    State(admin): State<AdminState>,
    Path(template_id): Path<String>,
) -> Response {
    match admin.ledger.delete_template(&template_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => err_json(
            StatusCode::NOT_FOUND,
            GseError::new("not_found", format!("template {template_id} not found")),
        ),
        Err(e) => err_json(job_status(&e.code), e),
    }
}

async fn submit_job_template(
    State(admin): State<AdminState>,
    Path(template_id): Path<String>,
    body: Result<Json<TemplateSubmitRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => return from_json_err(e),
    };
    if req.agent_id.trim().is_empty() {
        return err_json(
            StatusCode::BAD_REQUEST,
            GseError::new("invalid_argument", "agent_id required"),
        );
    }
    let template = match admin.ledger.get_template(&template_id).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return err_json(
                StatusCode::NOT_FOUND,
                GseError::new("not_found", format!("template {template_id} not found")),
            )
        }
        Err(e) => return err_json(job_status(&e.code), e),
    };
    let (Some(registry), Some(cfg)) = (admin.registry.as_ref(), admin.cfg.as_ref()) else {
        return err_json(
            StatusCode::SERVICE_UNAVAILABLE,
            GseError::new("unavailable", "job dispatch requires the in-process server"),
        );
    };
    let input = TemplateInput::from_template(&template);
    let expanded = match crate::template::expand(&input, &req.vars, cfg) {
        Ok(e) => e,
        Err(e) => return err_json(job_status(&e.code), e),
    };
    let submit = expanded.into_submit(req.agent_id);
    match submit_job_with_template(&admin.ledger, registry, cfg, submit, Some(template_id)).await {
        Ok(record) => created(&record),
        Err(e) => err_json(job_status(&e.code), e),
    }
}

async fn save_job_as_template(
    State(admin): State<AdminState>,
    Path(job_id): Path<String>,
    body: Result<Json<SaveAsTemplateRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => return from_json_err(e),
    };
    let job = match admin.ledger.get_job(&job_id).await {
        Ok(Some(j)) => j,
        Ok(None) => {
            return err_json(
                StatusCode::NOT_FOUND,
                GseError::new("not_found", format!("job {job_id} not found")),
            )
        }
        Err(e) => return err_json(job_status(&e.code), e),
    };
    let input = TemplateInput {
        name: req.name,
        description: None,
        interpreter: Some(job.interpreter),
        script: job.script,
        args: job.args,
        env: job.env,
        working_dir: job.working_dir,
        timeout_secs: Some(job.timeout_secs),
    };
    let cfg = template_cfg(&admin);
    match write_template(&admin, &input, &cfg).await {
        Ok(t) => created(&t),
        Err(e) => err_json(job_status(&e.code), e),
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
        let app = router(
            AdminState {
                ledger: ledger.clone(),
                registry: None,
                cfg: None,
            },
            None,
        );
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
            req("POST", "/api/gse/hosts", Some(r#"{"host_id":"h-1"}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("inner_ip"), "{body}");

        // 创建 -> 201
        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/hosts",
                Some(r#"{"host_id":"h-1","inner_ip":"10.0.0.1","hostname":"web-1"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(body.contains("10.0.0.1"), "{body}");

        // 列表 -> 200
        let (status, body) = send(&mut app, req("GET", "/api/gse/hosts", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("h-1"), "{body}");

        // 查询单个 -> 200，缺失 -> 404
        let (status, body) = send(&mut app, req("GET", "/api/gse/hosts/h-1", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("web-1"), "{body}");
        let (status, _) = send(&mut app, req("GET", "/api/gse/hosts/ghost", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // 删除 -> 200，删除后再查询 404
        let (status, _) = send(&mut app, req("DELETE", "/api/gse/hosts/h-1", None)).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = send(&mut app, req("GET", "/api/gse/hosts/h-1", None)).await;
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
                "/api/gse/agents",
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
                "/api/gse/agents",
                Some(r#"{"agent_id":"a-1","host_id":"h-1","token":"tok-a","version":"0.1.0"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(body.contains("\"token\":\"tok-a\""), "{body}");
        assert!(body.contains("\"status\":\"unknown\""), "{body}");

        // 列表 -> 200 含 token
        let (status, body) = send(&mut app, req("GET", "/api/gse/agents", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("tok-a"), "{body}");

        // 查询 -> 200；缺失 -> 404
        let (status, body) = send(&mut app, req("GET", "/api/gse/agents/a-1", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("tok-a"), "{body}");
        let (status, _) = send(&mut app, req("GET", "/api/gse/agents/ghost", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = send(&mut app, req("DELETE", "/api/gse/agents/a-1", None)).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = send(&mut app, req("GET", "/api/gse/agents/a-1", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn access_points_and_agent_configs_crud() {
        let (mut app, _ledger) = app_ledger("misc").await;

        let (status, _) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/access-points",
                Some(r#"{"id":"ap-1","name":"main","server_ip":"192.168.1.1","rpc_port":7100}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, body) = send(&mut app, req("GET", "/api/gse/access-points", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("ap-1"), "{body}");
        let (status, _) = send(&mut app, req("GET", "/api/gse/access-points/ghost", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/agent-configs",
                Some(r#"{"agent_id":"a-1","host_id":"h-1","cpu_limit_percent":50}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(body.contains("\"log_level\":\"info\""), "{body}");
        let (status, body) = send(&mut app, req("GET", "/api/gse/agent-configs/a-1", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("50"), "{body}");
        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/agent-configs",
                Some(r#"{"host_id":"h-1"}"#),
            ),
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
                "/api/gse/hosts",
                Some(r#"{"host_id":"h-1","inner_ip":"10.0.0.1"}"#),
            ),
        )
        .await;
        send(
            &mut app,
            req(
                "POST",
                "/api/gse/agents",
                Some(r#"{"agent_id":"a-1","host_id":"h-1","token":"tok-a"}"#),
            ),
        )
        .await;
        send(
            &mut app,
            req(
                "POST",
                "/api/gse/agent-configs",
                Some(r#"{"agent_id":"a-1","host_id":"h-1","cpu_limit_percent":50}"#),
            ),
        )
        .await;

        let (status, _) = send(&mut app, req("DELETE", "/api/gse/agents/a-1", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(ledger.get_agent("a-1").await.expect("get").is_none());
        assert!(
            ledger.get_agent_config("a-1").await.expect("get").is_none(),
            "agent config should be cascaded away"
        );
        // host 不随 agent 删除而消失。
        assert!(ledger.get_host("h-1").await.expect("get").is_some());
    }

    #[tokio::test]
    async fn web_dir_serves_static_and_spa_fallback() {
        let dir = std::env::temp_dir().join(format!("gse-http-web-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("index.html"), "<html>spa-root</html>").unwrap();
        std::fs::write(dir.join("assets/app.js"), "console.log(1)").unwrap();

        let db = test_db("web-dir");
        let ledger = Arc::new(Ledger::new(&db).expect("open"));
        ledger.init().await.expect("init");
        let mut app = router(
            AdminState {
                ledger: ledger.clone(),
                registry: None,
                cfg: None,
            },
            Some(&dir),
        );

        // 静态资源按原路径命中。
        let (status, body) = send(&mut app, req("GET", "/assets/app.js", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, "console.log(1)");

        // SPA 客户端路由（如 /hosts 页面）回退 index.html。
        let (status, body) = send(&mut app, req("GET", "/hosts", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("spa-root"), "{body}");

        // API 前缀仍优先于静态回退。
        let (status, body) = send(&mut app, req("GET", "/api/gse/agents", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("["), "{body}");

        std::fs::remove_dir_all(&dir).ok();
    }

    async fn app_with_jobs(name: &str) -> (Router, Arc<Ledger>) {
        let db = test_db(name);
        let ledger = Arc::new(Ledger::new(&db).expect("open"));
        ledger.init().await.expect("init");
        let app = router(
            AdminState {
                ledger: ledger.clone(),
                registry: Some(Arc::new(SessionRegistry::new())),
                cfg: Some(Arc::new(ServerConfig::default())),
            },
            None,
        );
        (app, ledger)
    }

    #[tokio::test]
    async fn jobs_list_empty_and_unknown_get_404() {
        let (mut app, _ledger) = app_with_jobs("jobs-empty").await;
        let (status, body) = send(&mut app, req("GET", "/api/gse/jobs", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, "[]");

        let (status, _) = send(&mut app, req("GET", "/api/gse/jobs/nope", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn jobs_create_validation_and_offline_agent() {
        let (mut app, _ledger) = app_with_jobs("jobs-create").await;

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/jobs",
                Some(r#"{"agent_id":"a-1","script":"   "}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("script"), "{body}");

        // 校验通过但 agent 无在线会话 -> 409。
        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/jobs",
                Some(r#"{"agent_id":"a-1","script":"echo hi"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("unavailable"), "{body}");
    }

    #[tokio::test]
    async fn jobs_without_in_process_server_return_503() {
        let (mut app, _ledger) = app_ledger("jobs-no-server").await;
        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/jobs",
                Some(r#"{"agent_id":"a-1","script":"echo hi"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    }

    #[tokio::test]
    async fn job_rerun_source_missing_is_404() {
        let (mut app, _ledger) = app_with_jobs("rerun-404").await;
        let (status, body) = send(&mut app, req("POST", "/api/gse/jobs/ghost/rerun", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    }

    #[tokio::test]
    async fn job_rerun_requires_in_process_server() {
        let (mut app, ledger) = app_ledger("rerun-503").await;
        insert_job(&ledger, "job-src").await;
        let (status, body) = send(
            &mut app,
            req("POST", "/api/gse/jobs/job-src/rerun", None),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    }

    #[tokio::test]
    async fn job_rerun_merges_and_validates() {
        let (mut app, ledger) = app_with_jobs("rerun-merge").await;
        insert_job(&ledger, "job-src").await;

        // 空请求体：以来源作业参数提交，目标 Agent 离线 -> 409。
        let (status, body) = send(
            &mut app,
            req("POST", "/api/gse/jobs/job-src/rerun", None),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("unavailable"), "{body}");

        // 显式覆盖脚本且超出上限 -> 400。
        let long = "a".repeat(ServerConfig::default().job_max_script_bytes + 1);
        let payload = format!(r#"{{"script":"{long}"}}"#);
        let (status, body) = send(
            &mut app,
            req("POST", "/api/gse/jobs/job-src/rerun", Some(payload.as_str())),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        // 非法 JSON -> 400。
        let (status, _) = send(
            &mut app,
            req("POST", "/api/gse/jobs/job-src/rerun", Some("{")),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn jobs_list_filters_by_agent_and_status() {
        let (mut app, ledger) = app_with_jobs("jobs-filter").await;
        ledger
            .insert_job(&crate::ledger::NewJob {
                job_id: "job-a".to_string(),
                agent_id: "a-1".to_string(),
                interpreter: "bash".to_string(),
                script: "echo hi".to_string(),
                args: vec![],
                env: std::collections::BTreeMap::new(),
                working_dir: None,
                template_id: None,
                rerun_of: None,
                timeout_secs: 30,
                created_at: ledger_stamp(),
            })
            .await
            .expect("insert");

        let (status, body) = send(
            &mut app,
            req("GET", "/api/gse/jobs?agent_id=a-1&status=pending", None),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("job-a"), "{body}");

        let (status, body) = send(&mut app, req("GET", "/api/gse/jobs?agent_id=other", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, "[]");
    }

    const TEMPLATE_BODY: &str = r#"{"name":"collect","description":"logs","interpreter":"bash","script":"echo ${svc}","args":["--tag=${svc}"],"env":{"LANG":"C"},"working_dir":"/tmp","timeout_secs":60}"#;

    async fn insert_job(ledger: &Ledger, job_id: &str) {
        ledger
            .insert_job(&crate::ledger::NewJob {
                job_id: job_id.to_string(),
                agent_id: "a-1".to_string(),
                interpreter: "bash".to_string(),
                script: "echo hi".to_string(),
                args: vec![],
                env: std::collections::BTreeMap::new(),
                working_dir: None,
                template_id: None,
                rerun_of: None,
                timeout_secs: 30,
                created_at: ledger_stamp(),
            })
            .await
            .expect("insert job");
    }

    #[tokio::test]
    async fn job_templates_crud() {
        let (mut app, _ledger) = app_ledger("tpl-crud-http").await;

        let (status, body) = send(
            &mut app,
            req("POST", "/api/gse/job-templates", Some(TEMPLATE_BODY)),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(body.contains("\"name\":\"collect\""), "{body}");
        assert!(body.contains("tpl-"), "{body}");
        let template_id = body
            .split("\"template_id\":\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .expect("template id")
            .to_string();

        let (status, body) = send(&mut app, req("GET", "/api/gse/job-templates", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("collect"), "{body}");

        let (status, body) = send(&mut app, req("GET", "/api/gse/job-templates?name=coll", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("collect"), "{body}");
        let (status, body) = send(&mut app, req("GET", "/api/gse/job-templates?name=nomatch", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, "[]");

        let (status, body) = send(
            &mut app,
            req("GET", &format!("/api/gse/job-templates/{template_id}"), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("\"timeout_secs\":60"), "{body}");

        let (status, body) = send(
            &mut app,
            req(
                "PUT",
                &format!("/api/gse/job-templates/{template_id}"),
                Some(r#"{"name":"collect-v2","script":"echo hi"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("collect-v2"), "{body}");
        assert!(body.contains("\"timeout_secs\":300"), "{body}");

        let (status, _) = send(
            &mut app,
            req("DELETE", &format!("/api/gse/job-templates/{template_id}"), None),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = send(
            &mut app,
            req("GET", &format!("/api/gse/job-templates/{template_id}"), None),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = send(
            &mut app,
            req("GET", "/api/gse/job-templates/ghost", None),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn job_templates_validation_and_conflict() {
        let (mut app, _ledger) = app_ledger("tpl-validation").await;

        let (status, body) = send(
            &mut app,
            req("POST", "/api/gse/job-templates", Some(r#"{"script":"echo hi"}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("name"), "{body}");

        let (status, body) = send(
            &mut app,
            req("POST", "/api/gse/job-templates", Some(r#"{"name":"x","script":""}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("script"), "{body}");

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/job-templates",
                Some(r#"{"name":"bad","script":"echo ${1x}"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        let (status, body) = send(
            &mut app,
            req("POST", "/api/gse/job-templates", Some(TEMPLATE_BODY)),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let (status, body) = send(
            &mut app,
            req("POST", "/api/gse/job-templates", Some(TEMPLATE_BODY)),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("already_exists"), "{body}");
    }

    #[tokio::test]
    async fn job_template_submit_requires_server_and_vars() {
        // 无进程内 Server -> 503。
        let (mut app, _ledger) = app_ledger("tpl-submit-503").await;
        let tpl_body = TEMPLATE_BODY;
        let (_, created_body) =
            send(&mut app, req("POST", "/api/gse/job-templates", Some(tpl_body))).await;
        let template_id = created_body
            .split("\"template_id\":\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .expect("template id")
            .to_string();
        let (status, body) = send(
            &mut app,
            req(
                "POST",
                &format!("/api/gse/job-templates/{template_id}/submit"),
                Some(r#"{"agent_id":"a-1","vars":{"svc":"nginx"}}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");

        // 有 Server 但缺 agent_id / 缺变量 -> 400；变量齐全但 agent 离线 -> 409。
        let (mut app, _ledger) = app_with_jobs("tpl-submit-vars").await;
        let (_, created_body) =
            send(&mut app, req("POST", "/api/gse/job-templates", Some(tpl_body))).await;
        let template_id = created_body
            .split("\"template_id\":\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .expect("template id")
            .to_string();

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                &format!("/api/gse/job-templates/{template_id}/submit"),
                Some(r#"{"agent_id":"  ","vars":{"svc":"nginx"}}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("agent_id"), "{body}");

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                &format!("/api/gse/job-templates/{template_id}/submit"),
                Some(r#"{"agent_id":"a-1","vars":{}}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("missing variable"), "{body}");

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                &format!("/api/gse/job-templates/{template_id}/submit"),
                Some(r#"{"agent_id":"a-1","vars":{"svc":"nginx"}}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("unavailable"), "{body}");

        let (status, _) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/job-templates/ghost/submit",
                Some(r#"{"agent_id":"a-1"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn save_job_as_template() {
        let (mut app, ledger) = app_with_jobs("tpl-save").await;
        insert_job(&ledger, "job-src").await;

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/jobs/job-src/save-as-template",
                Some(r#"{"name":"from-job"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(body.contains("from-job"), "{body}");
        assert!(body.contains("\"interpreter\":\"bash\""), "{body}");

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/jobs/job-src/save-as-template",
                Some(r#"{"name":""}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        let (status, _) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/jobs/ghost/save-as-template",
                Some(r#"{"name":"x"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
