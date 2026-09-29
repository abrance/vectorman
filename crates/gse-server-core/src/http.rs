//! HTTP 端口：以 axum 暴露台账四表的增删改查，供运维预登记与查询。
//!
//! 台账 API 统一挂在 `/api/gse` 前缀下（与前端 `@vectorman/*` 的
//! `GseAdminAdapter` 前缀一致）；根路径仅保留 `/health`。可选的
//! `web_dir` 使同一端口同时托管前端 dist：`ServeDir` 找不到文件时回退
//! `index.html`，满足 SPA 客户端路由。
//!
//! 该模块仅操作 `Ledger` 与可选的会话注册表。
//!
//! 管理接口默认不鉴权、仅监听回环地址；配置 `admin_password`（建议用环境变量
//! `GSE_SERVER_ADMIN_PASSWORD`）后 `/api/gse/*` 需要密码，`/health` 与静态前端目录保持开放。

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::extract::Multipart;
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use gse_proto::GseError;
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};
use vectorman_metrics::SelfMetrics;

use crate::config::ServerConfig;
use crate::file_transfer::{submit_file_job, submit_file_rerun, FileJobSubmit};
use crate::job_file_store::JobFileStore;
use crate::ledger::{
    ledger_stamp, AccessPoint, Agent, DataplaneService, Host, JobTemplate, Ledger,
};
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
    /// 作业临时文件；独立部署管理端口时为 None，文件接口不可用。
    pub file_store: Option<Arc<JobFileStore>>,
    /// 管理端口密码；**空串 = 不认证**。
    pub admin_password: String,
}

pub(crate) struct GseScrapeHook {
    pub ledger: Arc<Ledger>,
    pub registry: Option<Arc<SessionRegistry>>,
}

#[async_trait::async_trait]
impl vectorman_metrics::ScrapeHook for GseScrapeHook {
    async fn on_scrape(&self, metrics: &SelfMetrics) {
        if let Ok(agents) = self.ledger.list_agents().await {
            let n = agents.iter().filter(|a| a.status == "online").count();
            metrics.set_gauge("vectorman_gse_agents_online", n as f64);
        }
        let n = match &self.registry {
            Some(r) => r.list().await.len(),
            None => 0,
        };
        metrics.set_gauge("vectorman_gse_sessions", n as f64);
    }
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
        .route("/dataplanes", get(list_dataplanes).post(create_dataplane))
        .route(
            "/dataplanes/{service_id}",
            get(get_dataplane).delete(delete_dataplane),
        )
        .route("/agent-specs", get(list_agent_specs))
        .route(
            "/agents/{agent_id}/spec",
            get(get_agent_spec).put(put_agent_spec),
        )
        .route("/agents/{agent_id}/spec/apply", post(apply_agent_spec))
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
        // 作业文件上传走 multipart，body 上限提高到配置里的 `job_max_file_bytes`
        // （默认 64MB）。axum 的 `DefaultBodyLimit` 默认只有 2MB，不覆盖它会让
        // 上传在 2MB 处被截断，且报错是「multipart 解析失败」，看不出是限额。
        .route(
            "/job-files",
            get(list_job_files)
                .post(upload_job_file)
                .layer(DefaultBodyLimit::max(job_file_body_limit(&admin))),
        )
        .route(
            "/job-files/{file_id}",
            get(download_job_file).delete(delete_job_file),
        )
        // **未匹配的 `/api/gse/*` 必须 404**：不挂这条 fallback，请求会落到
        // `web_dir` 的静态回退上，拿到 `200 + text/html` —— API 路径打错一个字、
        // 或者删掉旧路由后，调用方（脚本/CLI）都会把它当成功。
        .fallback(api_not_found)
        // 认证只罩住 `/api/gse/*`（即这个嵌套 router）：`/health` 与静态前端目录不经过这里，
        // 否则「要登录才能加载登录页」。
        .layer(middleware::from_fn_with_state(
            admin.clone(),
            require_admin_password,
        ))
        .with_state(admin)
}

/// 未匹配的 API 路径：JSON 404（而不是静态回退的 `200 + HTML`）。
async fn api_not_found(req: Request) -> Response {
    err_json(
        StatusCode::NOT_FOUND,
        GseError::new(
            "not_found",
            format!("no such API route: {}", req.uri().path()),
        ),
    )
}

/// 管理端口密码认证。
///
/// - 密码为空 → 直接放行（默认行为，向后兼容）；
/// - 否则接受 `Authorization: Bearer <密码>`（CLI/脚本）或 HTTP Basic（浏览器原生弹窗，
///   密码部分匹配即可，用户名忽略 —— 这里只有一个共享密码，没有用户体系）；
/// - 拒绝时带 `WWW-Authenticate: Basic`，浏览器会自己弹登录框，前端无需任何改动。
async fn require_admin_password(
    State(admin): State<AdminState>,
    req: Request,
    next: Next,
) -> Response {
    if admin.admin_password.is_empty() || password_matches(req.headers(), &admin.admin_password) {
        return next.run(req).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        [("WWW-Authenticate", "Basic realm=\"vectorman\"")],
        Json(json!({"error": "admin password required", "code": "unauthorized"})),
    )
        .into_response()
}

/// 从 `Authorization` 头里取出候选密码。
fn candidate_password(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    if let Some(token) = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
    {
        return Some(token.to_string());
    }
    let b64 = raw
        .strip_prefix("Basic ")
        .or_else(|| raw.strip_prefix("basic "))?;
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    // Basic 是 `user:password`；用户名忽略（没有用户体系，只有一个共享密码）。
    let (_, password) = text.split_once(':')?;
    Some(password.to_string())
}

fn password_matches(headers: &axum::http::HeaderMap, expected: &str) -> bool {
    candidate_password(headers)
        .map(|candidate| constant_time_eq(candidate.as_bytes(), expected.as_bytes()))
        .unwrap_or(false)
}

/// 定长比较，避免按字节提前返回泄漏前缀信息（长度不等直接返回 False —— 长度不属于秘密）。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 作业文件上传的 body 上限：取 `job_max_file_bytes`，再留出 multipart 边框余量。
/// 管理端口独立部署时 `cfg` 为 None（文件接口本就不可用），退回 axum 默认值。
fn job_file_body_limit(admin: &AdminState) -> usize {
    admin
        .cfg
        .as_ref()
        .map(|c| (c.job_max_file_bytes.saturating_add(4096)).min(usize::MAX as u64) as usize)
        // ponytail: 走 axum 默认 2MB；cfg 缺失时该接口不可用，无需更宽
        .unwrap_or(2 * 1024 * 1024)
}

/// 构造 HTTP 服务路由：台账 API 挂在 `/api/gse` 前缀，根路径保留 `/health`。
/// 提供 `web_dir` 时同一端口托管该目录下的前端 dist，未命中的路径回退
/// `index.html`（SPA 客户端路由），已存在的静态资源（JS/CSS/字体）正常返回。
pub fn router(admin: AdminState, web_dir: Option<&std::path::Path>) -> Router {
    build_router(admin, web_dir, None)
}

fn build_router(
    admin: AdminState,
    web_dir: Option<&std::path::Path>,
    metrics: Option<Arc<SelfMetrics>>,
) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .nest("/api/gse", ledger_routes(admin));
    let app = match web_dir {
        Some(dir) => {
            let index = dir.join("index.html");
            api.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)))
        }
        None => api,
    };
    vectorman_metrics::apply_http_metrics(app, metrics)
}

/// 绑定并托管 HTTP 管理端口；成功后持续运行直至底层错误。
pub async fn serve(
    admin: AdminState,
    listen: &str,
    web_dir: Option<String>,
    metrics: Option<Arc<SelfMetrics>>,
) -> Result<(), GseError> {
    let app = build_router(admin, web_dir.as_deref().map(std::path::Path::new), metrics);
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

/// Agent 台账列表，附**会话口径**的字段。
///
/// `status` 是心跳口径（窗口内有心跳即 online），而作业下发用的是内存会话
/// （`SessionRegistry`）。两者可以不一致 —— 这正是「心跳在线但作业通道已死」
/// 这一故障的形态。`session_state` / `job_channel_available` 让这种差异可被查询，
/// 不必翻日志。
#[derive(serde::Serialize)]
struct AgentView {
    #[serde(flatten)]
    agent: crate::ledger::Agent,
    /// 会话状态：online | checking | offline | closed | absent（无会话）。
    session_state: String,
    /// 作业通道是否可用 —— 仅当会话为 Online 时为 true。
    job_channel_available: bool,
}

fn session_state_name(state: crate::session::SessionState) -> &'static str {
    use crate::session::SessionState;
    match state {
        SessionState::Online => "online",
        SessionState::Checking => "checking",
        SessionState::Offline => "offline",
        SessionState::Closed => "closed",
    }
}

async fn list_agents(State(admin): State<AdminState>) -> Response {
    let agents = match admin.ledger.list_agents().await {
        Ok(v) => v,
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let sessions = match admin.registry.as_ref() {
        Some(r) => r.list().await,
        None => Vec::new(),
    };
    let views: Vec<AgentView> = agents
        .into_iter()
        .map(|agent| {
            let session = sessions.iter().find(|s| s.agent_id == agent.agent_id);
            let (state_name, available) = match session {
                Some(s) => (
                    session_state_name(s.state),
                    s.state == crate::session::SessionState::Online,
                ),
                None => ("absent", false),
            };
            AgentView {
                agent,
                session_state: state_name.to_string(),
                job_channel_available: available,
            }
        })
        .collect();
    ok(&views)
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
    // 级联清 Agent 的期望 spec / 生效状态与活跃会话，保证删除后节点不可再被操作。
    if let Err(e) = ledger.remove_agent_spec(&id).await {
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    if let Err(e) = ledger.remove_agent_spec_state(&id).await {
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    if let Some(registry) = &admin.registry {
        if let Some(session) = registry.remove(&id).await {
            let _ = session.end.close().await;
        }
    }
    Json(json!({"deleted": id})).into_response()
}

// ---- dataplanes ----

async fn list_dataplanes(State(admin): State<AdminState>) -> Response {
    match admin.ledger.list_dataplanes().await {
        Ok(v) => ok(&v),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn create_dataplane(
    State(admin): State<AdminState>,
    body: Result<Json<DataplaneService>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(d) = match body {
        Ok(b) => b,
        Err(e) => return from_json_err(e),
    };
    if let Some(resp) = require(!d.service_id.trim().is_empty(), "service_id") {
        return resp;
    }
    if let Some(resp) = require(!d.ingest_url.trim().is_empty(), "ingest_url") {
        return resp;
    }
    if let Some(resp) = require(!d.query_url.trim().is_empty(), "query_url") {
        return resp;
    }
    let mut d = d;
    if d.registered_at.is_empty() {
        d.registered_at = ledger_stamp();
    }
    match admin.ledger.upsert_dataplane(&d).await {
        Ok(()) => {
            // 立刻探一次：否则要等最多一个探活间隔（默认 30s）才会变 online，
            // 这段时间 Agent 拿不到上报地址，采集数据积压后被淘汰。
            crate::dataplane::spawn_probe_after_register(
                admin.ledger.clone(),
                d.service_id.clone(),
            );
            created(&d)
        }
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn get_dataplane(
    State(admin): State<AdminState>,
    Path(service_id): Path<String>,
) -> Response {
    match admin.ledger.get_dataplane(&service_id).await {
        Ok(Some(d)) => ok(&d),
        Ok(None) => err_json(
            StatusCode::NOT_FOUND,
            GseError::new("not_found", format!("dataplane {service_id} not found")),
        ),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn delete_dataplane(
    State(admin): State<AdminState>,
    Path(service_id): Path<String>,
) -> Response {
    match admin.ledger.delete_dataplane(&service_id).await {
        Ok(()) => Json(json!({"deleted": service_id})).into_response(),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

// ---- agent spec ----

/// 采集项类型白名单。服务端校验与前端下拉共用同一份清单。
fn is_item_kind(kind: &str) -> bool {
    matches!(
        kind,
        "metrics_host"
            | "log_file"
            | "log_k8s_stdout"
            | "apm_otlp"
            | "ebpf_network"
            | "ebpf_tcp"
            | "ebpf_process"
            | "ebpf_syscall"
    )
}

/// 列表与详情共用的视图。
///
/// 合并返回（期望 + 生效 + diff）是刻意的：列表页与采集链路总览都要这些字段，
/// 分开三个接口会让前端对 N 台 Agent 打 3N 次请求。
#[derive(serde::Serialize)]
struct AgentSpecView {
    agent_id: String,
    host_id: String,
    /// 会话口径（内存会话注册表），与台账 `status`（心跳口径）可能不一致。
    session_state: String,
    /// synced | stale | rejected | unspecified | unknown。
    sync_status: &'static str,
    /// 期望 spec 的更新时间；无期望时为空。
    updated_at: Option<String>,
    /// 生效快照的落库时间；无上报时为空。
    reported_at: Option<String>,
    desired: Option<SpecRevisionView>,
    applied: Option<AppliedView>,
    diff: Option<crate::spec::SpecDiff>,
}

#[derive(serde::Serialize)]
struct SpecRevisionView {
    revision: String,
    spec: gse_proto::AgentSpecWire,
}

#[derive(serde::Serialize)]
struct AppliedView {
    revision: String,
    outcome: String,
    spec: gse_proto::AgentSpecWire,
    not_enforced: Vec<String>,
    detail: String,
}

/// 脱敏哨兵：读出去是它，写回来表示「保持原值」。
const MASK: &str = "***";

/// 敏感字段的对外表示：已设置 → `***`，未设置 → 空串（两者必须可区分，
/// 否则前端无法判断「留空」到底意味着什么）。
fn mask_secret(value: Option<&String>) -> Option<String> {
    Some(match value {
        Some(v) if !v.is_empty() => MASK.to_string(),
        _ => String::new(),
    })
}

fn mask_params(params: &gse_proto::SpecParams) -> gse_proto::SpecParams {
    let mut masked = params.clone();
    masked.token = mask_secret(params.token.as_ref());
    masked.otlp_token = mask_secret(params.otlp_token.as_ref());
    masked
}

fn mask_spec(spec: &gse_proto::AgentSpecWire) -> gse_proto::AgentSpecWire {
    gse_proto::AgentSpecWire {
        params: mask_params(&spec.params),
        items: spec.items.clone(),
    }
}

/// `null` 与「字段缺失」在 `Option<T>` 里都会解析成 `None`，而敏感字段必须区分：
/// 缺失/空串 = 保持原值（前端只拿得到脱敏占位），`null` = 清空。
fn double_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    <Option<T> as serde::Deserialize>::deserialize(de).map(Some)
}

/// 敏感字段的写回语义。`field` 只用于错误信息。
fn merge_secret(
    incoming: Option<Option<&str>>,
    existing: Option<&str>,
    field: &str,
) -> Result<Option<String>, GseError> {
    match incoming {
        // 字段缺失：保持原值。
        None => Ok(existing.map(|s| s.to_string())),
        // 显式 null：清空。
        Some(None) => Ok(None),
        Some(Some(v)) if v == MASK || v.is_empty() => {
            // 哨兵是「保持原值」的意思，原值本来就没有说明前端在发一份它自己造出来的占位值 ——
            // 静默当成空会把「没设置」与「设置过再被抹掉」混为一谈。
            if v == MASK && existing.is_none_or(|e| e.is_empty()) {
                return Err(GseError::new(
                    "invalid_argument",
                    format!("{field}: 传了脱敏占位值但当前并没有已设置的值"),
                ));
            }
            Ok(existing.map(|s| s.to_string()))
        }
        Some(Some(v)) => Ok(Some(v.to_string())),
    }
}

/// 单条采集项输入。`item_id` 缺省由服务端生成；已有项回传原 id 以保持稳定。
#[derive(serde::Deserialize)]
struct SpecItemInput {
    #[serde(default)]
    item_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    collector: serde_json::Value,
    #[serde(default)]
    storage: serde_json::Value,
}

fn default_enabled() -> bool {
    true
}

static ITEM_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 生成采集项 ID：`item-{unix_micros}-{seq}`。
fn new_collect_item_id() -> String {
    let seq = ITEM_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("item-{}-{}", crate::session::now_micros(), seq)
}

fn json_non_empty_str(v: &serde_json::Value, key: &str) -> bool {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

fn json_non_empty_str_array(v: &serde_json::Value, key: &str) -> bool {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .any(|s| s.as_str().map(|x| !x.trim().is_empty()).unwrap_or(false))
        })
        .unwrap_or(false)
}

/// 入库配置标准化：`retention_days` 缺省或非正数时回落到 1 天。
fn normalize_storage(v: &serde_json::Value) -> serde_json::Value {
    let days = v
        .get("retention_days")
        .and_then(|x| x.as_u64())
        .filter(|d| *d > 0)
        .unwrap_or(1);
    json!({"retention_days": days})
}

/// 校验输入并构造采集项：名称非空、类型合法、日志类含匹配模式、eBPF 参数有界。
fn build_spec_item(input: &SpecItemInput) -> Result<gse_proto::SpecItem, GseError> {
    if input.name.trim().is_empty() {
        return Err(GseError::new(
            "invalid_argument",
            "missing required field: name",
        ));
    }
    if !is_item_kind(&input.kind) {
        return Err(GseError::new(
            "invalid_argument",
            format!("unsupported kind: {}", input.kind),
        ));
    }
    match input.kind.as_str() {
        "log_file" if !json_non_empty_str_array(&input.collector, "path_patterns") => {
            return Err(GseError::new(
                "invalid_argument",
                "log_file requires non-empty path_patterns",
            ));
        }
        "apm_otlp" => {
            // OTLP 接收器：名单可选，但上限必须有界（Agent 侧同样夹取，这里是第一道闸）。
            for key in [
                "service_allowlist",
                "service_denylist",
                "attribute_allowlist",
            ] {
                if let Some(value) = input.collector.get(key) {
                    if !value.is_array() {
                        return Err(GseError::new(
                            "invalid_argument",
                            format!("apm_otlp {key} must be an array of strings"),
                        ));
                    }
                    if value
                        .as_array()
                        .is_some_and(|items| items.iter().any(|item| !item.is_string()))
                    {
                        return Err(GseError::new(
                            "invalid_argument",
                            format!("apm_otlp {key} must contain only strings"),
                        ));
                    }
                }
            }
            if let Some(batch) = input
                .collector
                .get("batch_max_records")
                .and_then(|v| v.as_u64())
            {
                if batch == 0 || batch > 5_000 {
                    return Err(GseError::new(
                        "invalid_argument",
                        "apm_otlp batch_max_records must be within 1..=5000",
                    ));
                }
            }
            if let Some(flush) = input
                .collector
                .get("flush_interval_secs")
                .and_then(|v| v.as_u64())
            {
                if flush == 0 || flush > 60 {
                    return Err(GseError::new(
                        "invalid_argument",
                        "apm_otlp flush_interval_secs must be within 1..=60",
                    ));
                }
            }
        }
        "ebpf_network" | "ebpf_tcp" | "ebpf_process" | "ebpf_syscall" => {
            // eBPF 采集项：Agent 侧会夹取（`EbpfConfig::from_value`），这里是第一道闸；
            // 只校验「明显非法」的值，避免把 Agent 的夹取逻辑抄两遍产生口径分叉。
            for key in [
                "cgroup_include",
                "cgroup_exclude",
                "process_include",
                "process_exclude",
            ] {
                if let Some(value) = input.collector.get(key) {
                    if !value.is_array() {
                        return Err(GseError::new(
                            "invalid_argument",
                            format!(
                                "{kind} {key} must be an array of strings",
                                kind = input.kind
                            ),
                        ));
                    }
                    if value
                        .as_array()
                        .is_some_and(|items| items.iter().any(|item| !item.is_string()))
                    {
                        return Err(GseError::new(
                            "invalid_argument",
                            format!("{kind} {key} must contain only strings", kind = input.kind),
                        ));
                    }
                }
            }
            for key in ["port_include", "port_exclude"] {
                if let Some(value) = input.collector.get(key) {
                    let items = value.as_array().ok_or_else(|| {
                        GseError::new(
                            "invalid_argument",
                            format!("{kind} {key} must be an array of ports", kind = input.kind),
                        )
                    })?;
                    if items
                        .iter()
                        .any(|item| item.as_u64().is_none_or(|port| port > 65_535))
                    {
                        return Err(GseError::new(
                            "invalid_argument",
                            format!(
                                "{kind} {key} must contain ports within 0..=65535",
                                kind = input.kind
                            ),
                        ));
                    }
                }
            }
            for (key, min, max) in [("bucket_secs", 1u64, 60u64), ("flush_interval_secs", 1, 60)] {
                if let Some(value) = input.collector.get(key).and_then(|v| v.as_u64()) {
                    if value < min || value > max {
                        return Err(GseError::new(
                            "invalid_argument",
                            format!(
                                "{kind} {key} must be within {min}..={max}",
                                kind = input.kind
                            ),
                        ));
                    }
                }
            }
            if let Some(ratio) = input
                .collector
                .get("raw_events_sample_ratio")
                .and_then(|v| v.as_f64())
            {
                if !(0.0..=1.0).contains(&ratio) {
                    return Err(GseError::new(
                        "invalid_argument",
                        format!(
                            "{kind} raw_events_sample_ratio must be within 0..=1",
                            kind = input.kind
                        ),
                    ));
                }
            }
        }
        "log_k8s_stdout" => {
            if !json_non_empty_str(&input.collector, "namespace") {
                return Err(GseError::new(
                    "invalid_argument",
                    "log_k8s_stdout requires namespace",
                ));
            }
            if !json_non_empty_str(&input.collector, "pod_name_pattern") {
                return Err(GseError::new(
                    "invalid_argument",
                    "log_k8s_stdout requires pod_name_pattern",
                ));
            }
        }
        _ => {}
    }
    Ok(gse_proto::SpecItem {
        item_id: if input.item_id.trim().is_empty() {
            new_collect_item_id()
        } else {
            input.item_id.trim().to_string()
        },
        name: input.name.trim().to_string(),
        kind: input.kind.clone(),
        enabled: input.enabled,
        collector: input.collector.clone(),
        storage: normalize_storage(&input.storage),
    })
}

/// 允许下发的心跳周期上限（判活窗口的 1/3）。
///
/// 超过它 Agent 会在两次心跳之间被判离线，表现成周期性「离线→上线」闪断，
/// 采集器也会被反复重对齐 —— 这类配置错误必须在写库前拦住。
fn heartbeat_limit_secs(admin: &AdminState) -> u64 {
    admin
        .cfg
        .as_ref()
        .map(|c| c.max_agent_heartbeat_interval_secs())
        .unwrap_or(crate::config::DEFAULT_MAX_AGENT_HEARTBEAT_INTERVAL_SECS)
}

/// spec 的结构性校验（全部拦在写库前）。
fn validate_spec(admin: &AdminState, spec: &gse_proto::AgentSpecWire) -> Result<(), GseError> {
    let limit = heartbeat_limit_secs(admin);
    if spec.params.heartbeat_interval_secs == 0 {
        return Err(GseError::new(
            "invalid_argument",
            "heartbeat_interval_secs 必须大于 0",
        ));
    }
    if spec.params.heartbeat_interval_secs > limit {
        return Err(GseError::new(
            "invalid_argument",
            format!(
                "heartbeat_interval_secs 不得大于 {limit}（判活窗口 heartbeat_timeout_secs/3 秒）"
            ),
        ));
    }
    let mut seen: Vec<&str> = Vec::new();
    for item in &spec.items {
        if item.item_id.trim().is_empty() {
            return Err(GseError::new("invalid_argument", "item_id 不得为空"));
        }
        if seen.contains(&item.item_id.as_str()) {
            return Err(GseError::new(
                "invalid_argument",
                format!("item_id 重复: {}", item.item_id),
            ));
        }
        seen.push(item.item_id.as_str());
        if !is_item_kind(&item.kind) {
            return Err(GseError::new(
                "invalid_argument",
                format!("unsupported kind: {}", item.kind),
            ));
        }
    }
    Ok(())
}

/// PUT 的请求体。
///
/// 非敏感字段是**整体覆盖**（缺失回落到内置默认值），只有 `token` / `otlp_token`
/// 是「保持原值」语义 —— 因为前端读回来的永远是脱敏占位值。
#[derive(serde::Deserialize)]
#[serde(default)]
struct SpecParamsInput {
    heartbeat_interval_secs: u64,
    allowed_interpreters: Vec<String>,
    job_default_interpreter: String,
    max_concurrent_jobs: usize,
    job_work_dir: Option<String>,
    otlp_enabled: bool,
    otlp_listen: String,
    otlp_max_body_bytes: usize,
    #[serde(deserialize_with = "double_option")]
    otlp_token: Option<Option<String>>,
    otlp_allowed_cidrs: Vec<String>,
    #[serde(deserialize_with = "double_option")]
    token: Option<Option<String>>,
    cpu_limit_percent: Option<i64>,
    mem_limit_percent: Option<i64>,
    log_level: String,
}

impl Default for SpecParamsInput {
    /// 缺省值对齐 `SpecParams::default()`，敏感字段缺省为「不修改」。
    fn default() -> Self {
        let d = gse_proto::SpecParams::default();
        Self {
            heartbeat_interval_secs: d.heartbeat_interval_secs,
            allowed_interpreters: d.allowed_interpreters,
            job_default_interpreter: d.job_default_interpreter,
            max_concurrent_jobs: d.max_concurrent_jobs,
            job_work_dir: d.job_work_dir,
            otlp_enabled: d.otlp_enabled,
            otlp_listen: d.otlp_listen,
            otlp_max_body_bytes: d.otlp_max_body_bytes,
            otlp_token: None,
            otlp_allowed_cidrs: d.otlp_allowed_cidrs,
            token: None,
            cpu_limit_percent: d.cpu_limit_percent,
            mem_limit_percent: d.mem_limit_percent,
            log_level: d.log_level,
        }
    }
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct SpecPutBody {
    params: SpecParamsInput,
    items: Vec<SpecItemInput>,
}

/// 同步状态：描述「期望值与生效值是否一致」。
fn sync_status(
    desired: Option<&crate::ledger::AgentSpec>,
    state: Option<&crate::ledger::AgentSpecState>,
) -> &'static str {
    let Some(state) = state else {
        return "unknown";
    };
    if state.outcome == gse_proto::spec_outcome::REJECTED {
        return "rejected";
    }
    match desired {
        // 没有期望 spec：Agent 跑的是本地文件基线，无从谈「同步」。
        None => "unspecified",
        Some(d) if d.revision == state.revision => "synced",
        Some(_) => "stale",
    }
}

/// 组装一台 Agent 的视图。`session_state` 由调用方给出（列表一次取全量注册表）。
fn spec_view(
    agent_id: &str,
    host_id: String,
    session_state: String,
    desired: Option<&crate::ledger::AgentSpec>,
    state: Option<&crate::ledger::AgentSpecState>,
) -> AgentSpecView {
    AgentSpecView {
        agent_id: agent_id.to_string(),
        host_id,
        session_state,
        sync_status: sync_status(desired, state),
        updated_at: desired.map(|d| d.updated_at.clone()),
        reported_at: state.map(|s| s.reported_at.clone()),
        desired: desired.map(|d| SpecRevisionView {
            revision: d.revision.clone(),
            spec: mask_spec(&d.spec),
        }),
        applied: state.map(|s| AppliedView {
            revision: s.revision.clone(),
            outcome: s.outcome.clone(),
            spec: mask_spec(&s.applied),
            not_enforced: s.not_enforced.clone(),
            detail: s.detail.clone(),
        }),
        diff: state.map(|s| s.diff.clone()),
    }
}

/// 某台 Agent 的会话口径；无注册表（管理端口独立部署）时恒为 `absent`。
async fn session_state_of(admin: &AdminState, agent_id: &str) -> String {
    let Some(registry) = admin.registry.as_ref() else {
        return "absent".to_string();
    };
    match registry.get(agent_id).await {
        Some(s) => format!("{:?}", s.state).to_lowercase(),
        None => "absent".to_string(),
    }
}

/// 列表：台账里的每台 Agent 一行（含期望、生效与 diff）。
async fn list_agent_specs(State(admin): State<AdminState>) -> Response {
    let agents = match admin.ledger.list_agents().await {
        Ok(v) => v,
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let desired = match admin.ledger.list_agent_specs().await {
        Ok(v) => v,
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let states = match admin.ledger.list_agent_spec_states().await {
        Ok(v) => v,
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let mut views: Vec<AgentSpecView> = agents
        .iter()
        .map(|a| {
            let d = desired.iter().find(|d| d.agent_id == a.agent_id);
            let s = states.iter().find(|s| s.agent_id == a.agent_id);
            // 占位：session_state 需要 await，下面统一填。
            spec_view(&a.agent_id, a.host_id.clone(), "absent".to_string(), d, s)
        })
        .collect();
    for (view, agent) in views.iter_mut().zip(agents.iter()) {
        view.session_state = session_state_of(&admin, &agent.agent_id).await;
    }
    // 有期望 spec 但台账里没有对应 Agent 的行也一并列出（数据不一致时要看得见，而不是消失）。
    for d in &desired {
        if agents.iter().any(|a| a.agent_id == d.agent_id) {
            continue;
        }
        let s = states.iter().find(|s| s.agent_id == d.agent_id);
        views.push(spec_view(
            &d.agent_id,
            String::new(),
            session_state_of(&admin, &d.agent_id).await,
            Some(d),
            s,
        ));
    }
    views.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    ok(&views)
}

async fn get_agent_spec(State(admin): State<AdminState>, Path(agent_id): Path<String>) -> Response {
    let desired = match admin.ledger.get_agent_spec(&agent_id).await {
        Ok(v) => v,
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let state = match admin.ledger.get_agent_spec_state(&agent_id).await {
        Ok(v) => v,
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let host_id = match admin.ledger.get_agent(&agent_id).await {
        Ok(Some(a)) => a.host_id,
        Ok(None) => String::new(),
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    // 从未保存过、也从未上报过：确实是「没有这个东西」。
    if desired.is_none() && state.is_none() {
        return err_json(
            StatusCode::NOT_FOUND,
            GseError::new("not_found", format!("agent {agent_id} 没有 spec")),
        );
    }
    let session_state = session_state_of(&admin, &agent_id).await;
    ok(&spec_view(
        &agent_id,
        host_id,
        session_state,
        desired.as_ref(),
        state.as_ref(),
    ))
}

/// 保存期望 spec（**只写台账，不推送**）。
async fn put_agent_spec(
    State(admin): State<AdminState>,
    Path(agent_id): Path<String>,
    body: Result<Json<SpecPutBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return from_json_err(e),
    };
    // `apply` 是 `/{agent_id}/spec/apply` 的静态段，被路由吃掉会表现为
    // 「保存成功但下发打到了别的 Agent」。
    if agent_id == "apply" {
        return err_json(
            StatusCode::BAD_REQUEST,
            GseError::new("invalid_argument", "agent_id \"apply\" 是保留字"),
        );
    }
    let existing_agent = match admin.ledger.get_agent(&agent_id).await {
        Ok(v) => v,
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    if existing_agent.is_none() {
        return err_json(
            StatusCode::NOT_FOUND,
            GseError::new("not_found", format!("agent {agent_id} 未登记")),
        );
    }
    let existing = match admin.ledger.get_agent_spec(&agent_id).await {
        Ok(v) => v,
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    };

    let params = match body
        .params
        .into_params(existing.as_ref().map(|e| &e.spec.params))
    {
        Ok(p) => p,
        Err(e) => return err_json(StatusCode::BAD_REQUEST, e),
    };
    let mut items = Vec::with_capacity(body.items.len());
    for input in &body.items {
        match build_spec_item(input) {
            Ok(i) => items.push(i),
            Err(e) => return err_json(StatusCode::BAD_REQUEST, e),
        }
    }
    let spec = gse_proto::AgentSpecWire { params, items };
    if let Err(e) = validate_spec(&admin, &spec) {
        return err_json(StatusCode::BAD_REQUEST, e);
    }
    let revision = match crate::spec::revision(&spec) {
        Ok(r) => r,
        Err(e) => {
            return err_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                GseError::new("internal", e),
            )
        }
    };
    let desired = crate::ledger::AgentSpec {
        agent_id: agent_id.clone(),
        revision,
        spec,
        updated_at: ledger_stamp(),
    };
    if let Err(e) = admin.ledger.upsert_agent_spec(&desired).await {
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, e);
    }

    // token 轮换：`agents.token` 才是认证用的真相源，必须在这里跟上，
    // 否则 Agent 收到新 token 重连会被拒。旧值进 `prev_token` 做宽限
    // （下发是手动的，agent 可能在这之前就重连过）。
    let new_token = desired.spec.params.token.clone();
    if let Some(new_token) = new_token {
        match admin.ledger.agent_tokens(&agent_id).await {
            Ok(Some((current, _))) if current == new_token => {}
            Ok(Some(_)) => {
                if let Err(e) = admin.ledger.rotate_agent_token(&agent_id, &new_token).await {
                    return err_json(StatusCode::INTERNAL_SERVER_ERROR, e);
                }
            }
            Ok(None) => {}
            Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
        }
    }

    ok(&spec_view(
        &agent_id,
        existing_agent.map(|a| a.host_id).unwrap_or_default(),
        session_state_of(&admin, &agent_id).await,
        Some(&desired),
        admin
            .ledger
            .get_agent_spec_state(&agent_id)
            .await
            .ok()
            .flatten()
            .as_ref(),
    ))
}

/// 下发该 Agent 的期望 spec 并取回执。
async fn apply_agent_spec(
    State(admin): State<AdminState>,
    Path(agent_id): Path<String>,
) -> Response {
    let Some(registry) = admin.registry.as_ref() else {
        return err_json(
            StatusCode::SERVICE_UNAVAILABLE,
            GseError::new("unavailable", "管理端口独立部署，无会话注册表，无法下发"),
        );
    };
    match crate::server::push_agent_spec(&admin.ledger, registry, &agent_id).await {
        Ok(ack) => ok(&json!({"ok": true, "ack": ack})),
        Err(e) => {
            let status = match e.code.as_str() {
                "not_found" => StatusCode::NOT_FOUND,
                "agent_offline" => StatusCode::CONFLICT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            err_json(status, e)
        }
    }
}

/// 把请求体里的参数合并成完整的 `SpecParams`（敏感字段按「保持原值」语义）。
impl SpecParamsInput {
    fn into_params(
        self,
        existing: Option<&gse_proto::SpecParams>,
    ) -> Result<gse_proto::SpecParams, GseError> {
        Ok(gse_proto::SpecParams {
            heartbeat_interval_secs: self.heartbeat_interval_secs,
            allowed_interpreters: self.allowed_interpreters,
            job_default_interpreter: self.job_default_interpreter,
            max_concurrent_jobs: self.max_concurrent_jobs,
            job_work_dir: self.job_work_dir,
            otlp_enabled: self.otlp_enabled,
            otlp_listen: self.otlp_listen,
            otlp_max_body_bytes: self.otlp_max_body_bytes,
            otlp_token: merge_secret(
                self.otlp_token.as_ref().map(|v| v.as_deref()),
                existing.and_then(|e| e.otlp_token.as_deref()),
                "otlp_token",
            )?,
            otlp_allowed_cidrs: self.otlp_allowed_cidrs,
            token: merge_secret(
                self.token.as_ref().map(|v| v.as_deref()),
                existing.and_then(|e| e.token.as_deref()),
                "token",
            )?,
            cpu_limit_percent: self.cpu_limit_percent,
            mem_limit_percent: self.mem_limit_percent,
            log_level: self.log_level,
        })
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
    body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
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
    let kind = req.get("kind").and_then(|v| v.as_str()).unwrap_or("script");
    if kind == "file_transfer" {
        let Some(store) = admin.file_store.as_ref() else {
            return err_json(
                StatusCode::SERVICE_UNAVAILABLE,
                GseError::new("unavailable", "job file store unavailable"),
            );
        };
        let parsed: FileJobSubmit = match serde_json::from_value(req) {
            Ok(v) => v,
            Err(e) => {
                return err_json(
                    StatusCode::BAD_REQUEST,
                    GseError::new("invalid_argument", e.to_string()),
                )
            }
        };
        match submit_file_job(
            admin.ledger.clone(),
            registry.clone(),
            cfg.clone(),
            store.clone(),
            parsed,
            None,
        )
        .await
        {
            Ok(record) => created(&record),
            Err(e) => err_json(job_status(&e.code), e),
        }
    } else {
        let parsed: JobSubmit = match serde_json::from_value(req) {
            Ok(v) => v,
            Err(e) => {
                return err_json(
                    StatusCode::BAD_REQUEST,
                    GseError::new("invalid_argument", e.to_string()),
                )
            }
        };
        match submit_job(&admin.ledger, registry, cfg, parsed).await {
            Ok(record) => created(&record),
            Err(e) => err_json(job_status(&e.code), e),
        }
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
    if source.kind == "file_transfer" {
        let Some(store) = admin.file_store.as_ref() else {
            return err_json(
                StatusCode::SERVICE_UNAVAILABLE,
                GseError::new("unavailable", "job file store unavailable"),
            );
        };
        match submit_file_rerun(
            admin.ledger.clone(),
            registry.clone(),
            cfg.clone(),
            store.clone(),
            &source,
            req.timeout_secs,
            req.agent_id,
            req.dest_path,
        )
        .await
        {
            Ok(record) => created(&record),
            Err(e) => err_json(job_status(&e.code), e),
        }
    } else {
        match submit_rerun(&admin.ledger, registry, cfg, &source, req).await {
            Ok(record) => created(&record),
            Err(e) => err_json(job_status(&e.code), e),
        }
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
    if job.kind == "file_transfer" {
        return err_json(
            StatusCode::BAD_REQUEST,
            GseError::new(
                "invalid_argument",
                "file transfer jobs cannot be saved as templates",
            ),
        );
    }
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

fn require_file_store(admin: &AdminState) -> Option<(&Arc<JobFileStore>, &ServerConfig)> {
    Some((admin.file_store.as_ref()?, admin.cfg.as_ref()?))
}

fn file_store_unavailable() -> Response {
    err_json(
        StatusCode::SERVICE_UNAVAILABLE,
        GseError::new("unavailable", "job file store unavailable"),
    )
}

async fn list_job_files(State(admin): State<AdminState>) -> Response {
    let Some((store, cfg)) = require_file_store(&admin) else {
        return file_store_unavailable();
    };
    match store.list_unexpired(crate::session::now_micros(), cfg.job_file_retain_secs) {
        Ok(v) => ok(&v),
        Err(e) => err_json(job_status(&e.code), e),
    }
}

async fn upload_job_file(
    State(admin): State<AdminState>,
    headers: axum::http::HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let Some((store, cfg)) = require_file_store(&admin) else {
        return file_store_unavailable();
    };
    let max = cfg.job_max_file_bytes;
    if let Some(len) = headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
    {
        if len > max.saturating_add(4096) {
            return err_json(
                StatusCode::BAD_REQUEST,
                GseError::new("invalid_argument", format!("upload exceeds {max} bytes")),
            );
        }
    }
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                return err_json(
                    StatusCode::BAD_REQUEST,
                    GseError::new("invalid_argument", e.to_string()),
                )
            }
        };
        if field.name() != Some("file") {
            continue;
        }
        let file_name = field
            .file_name()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "upload.bin".to_string());
        let file_id = JobFileStore::new_file_id();
        let mut writer = match store.begin_write(&file_id, &file_name) {
            Ok(w) => w,
            Err(e) => return err_json(job_status(&e.code), e),
        };
        let mut size = 0u64;
        let mut field = field;
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    size += chunk.len() as u64;
                    if size > max {
                        writer.abort();
                        return err_json(
                            StatusCode::BAD_REQUEST,
                            GseError::new(
                                "invalid_argument",
                                format!("upload exceeds {max} bytes"),
                            ),
                        );
                    }
                    if let Err(e) = writer.write_all(&chunk) {
                        writer.abort();
                        return err_json(job_status(&e.code), e);
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    writer.abort();
                    return err_json(
                        StatusCode::BAD_REQUEST,
                        GseError::new("invalid_argument", e.to_string()),
                    );
                }
            }
        }
        match writer.finish() {
            Ok(meta) => return created(&meta),
            Err(e) => return err_json(job_status(&e.code), e),
        }
    }
    err_json(
        StatusCode::BAD_REQUEST,
        GseError::new("invalid_argument", "missing required field: file"),
    )
}

async fn download_job_file(
    State(admin): State<AdminState>,
    Path(file_id): Path<String>,
) -> Response {
    let Some((store, _)) = require_file_store(&admin) else {
        return file_store_unavailable();
    };
    match store.get(&file_id) {
        Ok((meta, data)) => {
            let disp = format!(
                "attachment; filename=\"{}\"",
                meta.file_name.replace(['\\', '"'], "_")
            );
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/octet-stream"),
            );
            if let Ok(v) = axum::http::HeaderValue::from_str(&disp) {
                headers.insert(axum::http::header::CONTENT_DISPOSITION, v);
            }
            (StatusCode::OK, headers, data).into_response()
        }
        Err(e) => err_json(job_status(&e.code), e),
    }
}

async fn delete_job_file(State(admin): State<AdminState>, Path(file_id): Path<String>) -> Response {
    let Some((store, _)) = require_file_store(&admin) else {
        return file_store_unavailable();
    };
    match store.delete(&file_id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
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

    /// 带密码的管理端口（其余字段与 `app_ledger` 一致）。
    async fn app_with_password(name: &str, password: &str) -> (Router, Arc<Ledger>) {
        let db = test_db(name);
        let ledger = Arc::new(Ledger::new(&db).expect("open"));
        ledger.init().await.expect("init");
        let app = router(
            AdminState {
                ledger: ledger.clone(),
                registry: None,
                cfg: None,
                file_store: None,
                admin_password: password.to_string(),
            },
            None,
        );
        (app, ledger)
    }

    fn req_with_auth(method: &str, uri: &str, auth: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", auth)
            .body(Body::empty())
            .expect("request")
    }

    /// 管理端口的密码认证（`/api/gse/*` 需要密码，`/health` 不需要）。
    #[tokio::test]
    async fn admin_password_protects_api_but_not_health() {
        let (mut app, _ledger) = app_with_password("admin-pw", "s3cret").await;

        // 无凭据 → 401，且必须带 WWW-Authenticate（浏览器据此弹登录框）。
        let resp = app
            .clone()
            .oneshot(req("GET", "/api/gse/agents", None))
            .await
            .expect("oneshot");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(
            resp.headers().contains_key("www-authenticate"),
            "缺 WWW-Authenticate 时浏览器不会弹登录框"
        );

        // 错密码 → 401。
        let (status, _) = send(
            &mut app,
            req_with_auth("GET", "/api/gse/agents", "Bearer wrong"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // Bearer（CLI / 脚本）。
        let (status, body) = send(
            &mut app,
            req_with_auth("GET", "/api/gse/agents", "Bearer s3cret"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        // HTTP Basic（浏览器）：用户名忽略，只看密码。
        use base64::Engine;
        let basic = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("any:s3cret")
        );
        let (status, body) = send(&mut app, req_with_auth("GET", "/api/gse/agents", &basic)).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        // /health 不认证：存活探测与反代不该被密码挡住。
        let (status, body) = send(&mut app, req("GET", "/health", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        // 写接口同样受保护（配置下发是敏感操作）。
        let (status, _) = send(
            &mut app,
            req(
                "PUT",
                "/api/gse/agents/a-1/spec",
                Some(r#"{"params":{"heartbeat_interval_secs":30},"items":[]}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn empty_password_keeps_api_open() {
        let (mut app, _ledger) = app_ledger("admin-pw-empty").await;
        let (status, body) = send(&mut app, req("GET", "/api/gse/agents", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    #[tokio::test]
    async fn static_web_dir_stays_open_so_the_spa_can_load() {
        let dir = std::env::temp_dir().join(format!("gse-http-pw-web-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "<html>spa-root</html>").unwrap();
        let db = test_db("admin-pw-web");
        let ledger = Arc::new(Ledger::new(&db).expect("open"));
        ledger.init().await.expect("init");
        let mut app = router(
            AdminState {
                ledger: ledger.clone(),
                registry: None,
                cfg: None,
                file_store: None,
                admin_password: "s3cret".to_string(),
            },
            Some(&dir),
        );
        // 静态页面不认证：否则「要登录才能看到登录页」。
        let (status, body) = send(&mut app, req("GET", "/", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("spa-root"), "{body}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 未匹配的 API 路径必须 JSON 404 —— 不能落到静态回退上变成 `200 + HTML`。
    ///
    /// 这条是实测踩出来的：删掉旧路由后，`curl /api/gse/collect-items` 返回 200
    /// 且正文是 index.html，用状态码根本判断不出「路由没了」，脚本还会当成功。
    #[tokio::test]
    async fn unknown_api_path_returns_json_404_even_with_spa_enabled() {
        let dir = std::env::temp_dir().join(format!("gse-http-api404-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "<html>spa-root</html>").unwrap();
        let db = test_db("api-404");
        let ledger = Arc::new(Ledger::new(&db).expect("open"));
        ledger.init().await.expect("init");
        let mut app = router(
            AdminState {
                ledger: ledger.clone(),
                registry: None,
                cfg: None,
                file_store: None,
                admin_password: String::new(),
            },
            Some(&dir),
        );

        for path in [
            "/api/gse/collect-items",
            "/api/gse/agent-configs",
            "/api/gse/nope",
        ] {
            let resp = app
                .clone()
                .oneshot(req("GET", path, None))
                .await
                .expect("oneshot");
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
            let ct = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            assert!(ct.starts_with("application/json"), "{path}: {ct}");
        }

        // SPA 客户端路由仍回退 index.html。
        let (status, body) = send(&mut app, req("GET", "/hosts", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("spa-root"), "{body}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn constant_time_eq_matches_only_identical() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
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
                file_store: None,
                admin_password: String::new(),
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
    async fn access_points_crud_and_spec_requires_registered_agent() {
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

        // 未登记的 Agent 不能存 spec。
        let (status, body) = send(&mut app, req("GET", "/api/gse/agents/ghost/spec", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let (status, body) = send(
            &mut app,
            req(
                "PUT",
                "/api/gse/agents/ghost/spec",
                Some(r#"{"params":{"heartbeat_interval_secs":30},"items":[]}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
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
                "PUT",
                "/api/gse/agents/a-1/spec",
                Some(r#"{"params":{"heartbeat_interval_secs":30},"items":[]}"#),
            ),
        )
        .await;
        assert!(ledger.get_agent_spec("a-1").await.expect("get").is_some());

        let (status, _) = send(&mut app, req("DELETE", "/api/gse/agents/a-1", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(ledger.get_agent("a-1").await.expect("get").is_none());
        assert!(
            ledger.get_agent_spec("a-1").await.expect("get").is_none(),
            "agent spec should be cascaded away"
        );
        // host 不随 agent 删除而消失。
        assert!(ledger.get_host("h-1").await.expect("get").is_some());
    }

    #[tokio::test]
    async fn dataplanes_crud_and_validation() {
        let (mut app, ledger) = app_ledger("dataplanes").await;

        // 缺 ingest_url -> 400
        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/dataplanes",
                Some(r#"{"service_id":"ds-1","query_url":"http://q:9090"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("ingest_url"), "{body}");

        // 登记 -> 201，状态归为 unknown
        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/dataplanes",
                Some(
                    r#"{"service_id":"ds-1","ingest_url":"http://10.0.0.5:8081","query_url":"http://10.0.0.5:9090"}"#,
                ),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(body.contains("ds-1"), "{body}");

        // 列表与单条（落库后状态归为 unknown）
        let (status, body) = send(&mut app, req("GET", "/api/gse/dataplanes", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("\"status\":\"unknown\""), "{body}");
        let (status, body) = send(&mut app, req("GET", "/api/gse/dataplanes/ds-1", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("10.0.0.5:8081"), "{body}");
        let (status, _) = send(&mut app, req("GET", "/api/gse/dataplanes/ghost", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // 探活回写后 pick 能命中。
        ledger
            .set_dataplane_status("ds-1", "online", Some("t"))
            .await
            .unwrap();
        assert_eq!(
            ledger.pick_ingest_url("agent-1").await.unwrap().as_deref(),
            Some("http://10.0.0.5:8081")
        );

        // 删除
        let (status, _) = send(&mut app, req("DELETE", "/api/gse/dataplanes/ds-1", None)).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = send(&mut app, req("GET", "/api/gse/dataplanes/ds-1", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn agent_spec_validation_masking_and_roundtrip() {
        let (mut app, ledger) = app_ledger("agent-spec").await;
        for id in ["a-1", "a-2"] {
            ledger
                .upsert_agent(&Agent {
                    agent_id: id.to_string(),
                    host_id: "h-1".to_string(),
                    access_point_id: None,
                    token: "tok".to_string(),
                    version: String::new(),
                    install_path: String::new(),
                    status: "unknown".to_string(),
                    last_heartbeat_at: None,
                    registered_at: ledger_stamp(),
                })
                .await
                .expect("seed agent");
        }
        let put = |body: &str| req("PUT", "/api/gse/agents/a-1/spec", Some(body));

        // 非法 kind。
        let (status, body) = send(
            &mut app,
            req(
                "PUT",
                "/api/gse/agents/a-1/spec",
                Some(r#"{"items":[{"name":"x","kind":"nope","collector":{}}]}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("kind"), "{body}");

        // 采集项缺 name。
        let (status, body) = send(
            &mut app,
            req(
                "PUT",
                "/api/gse/agents/a-1/spec",
                Some(r#"{"items":[{"kind":"metrics_host","collector":{}}]}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("name"), "{body}");

        // log_file 缺 path_patterns。
        let (status, body) = send(
            &mut app,
            req(
                "PUT",
                "/api/gse/agents/a-1/spec",
                Some(r#"{"items":[{"name":"x","kind":"log_file","collector":{}}]}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("path_patterns"), "{body}");

        // eBPF：类型白名单 + 明显非法字段在第一道闸就拒。
        let (status, body) = send(
            &mut app,
            put(r#"{"items":[{"name":"ebpf","kind":"ebpf_network","collector":{"port_include":"8080"}}]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("array of ports"), "{body}");
        let (status, body) = send(
            &mut app,
            put(r#"{"items":[{"name":"ebpf","kind":"ebpf_tcp","collector":{"bucket_secs":600}}]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("1..=60"), "{body}");
        let (status, body) = send(
            &mut app,
            put(r#"{"items":[{"name":"ebpf","kind":"ebpf_process","collector":{"raw_events_enabled":true,"raw_events_sample_ratio":1.5}}]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("raw_events_sample_ratio"), "{body}");

        // apm_otlp：名单与攒批上限。
        let (status, body) = send(
            &mut app,
            put(r#"{"items":[{"name":"apm","kind":"apm_otlp","collector":{"service_allowlist":"order-api"}}]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("must be an array"), "{body}");
        let (status, body) = send(
            &mut app,
            put(r#"{"items":[{"name":"apm","kind":"apm_otlp","collector":{"batch_max_records":9000}}]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("1..=5000"), "{body}");

        // 心跳周期：0 与超过判活窗口 1/3 都要在写库前拦住
        // （cfg 为 None → 兜底上限 30s，即默认判活窗口 90s 的三分之一）。
        let (status, body) = send(
            &mut app,
            put(r#"{"params":{"heartbeat_interval_secs":0},"items":[]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("heartbeat_interval_secs"), "{body}");
        let (status, body) = send(
            &mut app,
            put(r#"{"params":{"heartbeat_interval_secs":999},"items":[]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("heartbeat_interval_secs"), "{body}");

        // item_id 重复。
        let (status, body) = send(
            &mut app,
            put(r#"{"items":[{"item_id":"dup","name":"a","kind":"metrics_host"},{"item_id":"dup","name":"b","kind":"metrics_host"}]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("重复"), "{body}");

        // 保留字 agent_id。
        let (status, body) = send(
            &mut app,
            req(
                "PUT",
                "/api/gse/agents/apply/spec",
                Some(r#"{"params":{"heartbeat_interval_secs":30},"items":[]}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("保留字"), "{body}");

        // 合法：四种 eBPF 类型都要能建（此前白名单漏过 ebpf_syscall），
        // 加一个 apm_otlp 与 log_file，并验证 storage 归一。
        let items = [
            r#"{"name":"ebpf-net","kind":"ebpf_network","collector":{"bucket_secs":10,"flush_interval_secs":10,"port_include":[8080,8443],"include_loopback":false,"raw_events_enabled":false}}"#,
            r#"{"name":"ebpf-tcp","kind":"ebpf_tcp","collector":{"flush_interval_secs":10}}"#,
            r#"{"name":"ebpf-proc","kind":"ebpf_process","collector":{"flush_interval_secs":10}}"#,
            r#"{"name":"ebpf-sys","kind":"ebpf_syscall","collector":{"flush_interval_secs":10}}"#,
            r#"{"name":"apm","kind":"apm_otlp","collector":{"service_allowlist":["order-api"],"batch_max_records":50,"flush_interval_secs":10},"storage":{"retention_days":3}}"#,
            r#"{"name":"app log","kind":"log_file","collector":{"path_patterns":["/var/log/*.log"]}}"#,
        ];
        let body = format!(
            r#"{{"params":{{"heartbeat_interval_secs":15}},"items":[{}]}}"#,
            items.join(",")
        );
        let (status, resp) = send(&mut app, put(&body)).await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        assert!(resp.contains("\"retention_days\":3"), "{resp}");
        assert!(
            resp.contains("\"retention_days\":1"),
            "缺省应回落 1 天: {resp}"
        );
        assert!(resp.contains("ebpf_syscall"), "{resp}");

        // 生成的 item_id 稳定：读回来还是那一批。
        let (_, body) = send(&mut app, req("GET", "/api/gse/agents/a-1/spec", None)).await;
        let view: serde_json::Value = serde_json::from_str(&body).expect("json");
        let ids: Vec<String> = view["desired"]["spec"]["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|i| i["item_id"].as_str().expect("item_id").to_string())
            .collect();
        assert_eq!(ids.len(), 6);
        assert!(ids.iter().all(|id| !id.is_empty()));
        // 有期望但从未上报：按口径是 unknown（无生效快照）。
        // 页面靠 desired != null 区分「配了没下发」与「什么都没配」。
        assert_eq!(view["sync_status"], "unknown");
        assert_eq!(view["session_state"], "absent");

        // 再存一次同样的 items（带原 item_id）→ revision 不变（幂等）。
        let rev1 = view["desired"]["revision"]
            .as_str()
            .expect("rev")
            .to_string();
        let items_with_ids: Vec<String> = view["desired"]["spec"]["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|i| serde_json::to_string(i).expect("encode"))
            .collect();
        let body = format!(
            r#"{{"params":{{"heartbeat_interval_secs":15}},"items":[{}]}}"#,
            items_with_ids.join(",")
        );
        let (status, resp) = send(&mut app, put(&body)).await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        assert!(
            resp.contains(&rev1),
            "同内容重复保存 revision 必须不变: {resp}"
        );

        // 脱敏：写入的 token 不得出现在响应里，读出去只有占位值。
        let (status, resp) = send(
            &mut app,
            put(r#"{"params":{"heartbeat_interval_secs":15,"token":"s3cret"},"items":[]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        assert!(!resp.contains("s3cret"), "凭据不得出现在响应里: {resp}");
        assert!(resp.contains("***"), "{resp}");
        // 且必须真的落库（认证用的是 agents.token）。
        let (token, prev) = ledger
            .agent_tokens("a-1")
            .await
            .expect("tokens")
            .expect("exists");
        assert_eq!(token, "s3cret");
        assert_eq!(prev.as_deref(), Some("tok"), "旧 token 进宽限");

        // 用哨兵写回 = 保持原值。
        let (status, resp) = send(
            &mut app,
            put(r#"{"params":{"heartbeat_interval_secs":15,"token":"***"},"items":[]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        let desired = ledger
            .get_agent_spec("a-1")
            .await
            .expect("get")
            .expect("exists");
        assert_eq!(
            desired.spec.params.token.as_deref(),
            Some("s3cret"),
            "哨兵写回不得把凭据洗掉"
        );

        // 没有原值却传哨兵 → 400（前端不该自己造占位值）。
        let (status, resp) = send(
            &mut app,
            put(r#"{"params":{"heartbeat_interval_secs":15,"token":"***","otlp_token":"***"},"items":[]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");

        // 列表：两台 Agent 都在，且不带凭据。
        let (status, body) = send(&mut app, req("GET", "/api/gse/agent-specs", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("a-2"), "{body}");
        assert!(!body.contains("s3cret"), "{body}");

        // 管理端口独立部署（无注册表）时下发返回 503，而不是假装成功。
        let (status, body) = send(
            &mut app,
            req("POST", "/api/gse/agents/a-1/spec/apply", None),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");

        // 旧路由整组消失。
        for path in [
            "/api/gse/collect-items",
            "/api/gse/collect-items/i-1",
            "/api/gse/agent-configs",
            "/api/gse/agent-configs/a-1",
        ] {
            let (status, _) = send(&mut app, req("GET", path, None)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path} 应当已删除");
        }
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
                file_store: None,
                admin_password: String::new(),
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

    /// 3.3 回归（本次事故形态）：台账 `status = online`（心跳新鲜）但会话
    /// `absent`（连接已死）时，`GET /api/gse/agents` 必须能体现差异 ——
    /// `session_state = absent` 且 `job_channel_available = false`。
    #[tokio::test]
    async fn list_agents_exposes_session_state_distinct_from_heartbeat_status() {
        let db = test_db("agents-session-state");
        let ledger = Arc::new(Ledger::new(&db).expect("open"));
        ledger.init().await.expect("init");
        // 台账：心跳新鲜 → status = online
        ledger
            .upsert_agent(&Agent {
                agent_id: "a-1".to_string(),
                host_id: "h-1".to_string(),
                access_point_id: None,
                token: "tok".to_string(),
                version: "1".to_string(),
                install_path: String::new(),
                status: "online".to_string(),
                last_heartbeat_at: Some("1".to_string()),
                registered_at: String::new(),
            })
            .await
            .expect("agent");
        // 注册表里**没有**该 agent 的会话 → 连接已死
        let app = router(
            AdminState {
                ledger: ledger.clone(),
                registry: Some(Arc::new(SessionRegistry::new())),
                cfg: None,
                file_store: None,
                admin_password: String::new(),
            },
            None,
        );
        let mut app = app;
        let (status, body) = send(&mut app, req("GET", "/api/gse/agents", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).expect("json");
        let a = &v[0];
        assert_eq!(a["status"], "online", "心跳口径仍是 online：{body}");
        assert_eq!(
            a["session_state"], "absent",
            "会话口径应显示 absent（连接已死）：{body}"
        );
        assert_eq!(
            a["job_channel_available"], false,
            "作业通道不可用必须可查询：{body}"
        );
    }

    async fn app_with_jobs(name: &str) -> (Router, Arc<Ledger>) {
        let db = test_db(name);
        let ledger = Arc::new(Ledger::new(&db).expect("open"));
        ledger.init().await.expect("init");
        let store =
            Arc::new(JobFileStore::open(format!("{db}.job-files")).expect("job file store"));
        let app = router(
            AdminState {
                ledger: ledger.clone(),
                registry: Some(Arc::new(SessionRegistry::new())),
                cfg: Some(Arc::new(ServerConfig::default())),
                file_store: Some(store),
                admin_password: String::new(),
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
        let (status, body) = send(&mut app, req("POST", "/api/gse/jobs/job-src/rerun", None)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    }

    #[tokio::test]
    async fn job_rerun_merges_and_validates() {
        let (mut app, ledger) = app_with_jobs("rerun-merge").await;
        insert_job(&ledger, "job-src").await;

        // 空请求体：以来源作业参数提交，目标 Agent 离线 -> 409。
        let (status, body) = send(&mut app, req("POST", "/api/gse/jobs/job-src/rerun", None)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("unavailable"), "{body}");

        // 显式覆盖脚本且超出上限 -> 400。
        let long = "a".repeat(ServerConfig::default().job_max_script_bytes + 1);
        let payload = format!(r#"{{"script":"{long}"}}"#);
        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/jobs/job-src/rerun",
                Some(payload.as_str()),
            ),
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
                ..Default::default()
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
                ..Default::default()
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

        let (status, body) = send(
            &mut app,
            req("GET", "/api/gse/job-templates?name=coll", None),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("collect"), "{body}");
        let (status, body) = send(
            &mut app,
            req("GET", "/api/gse/job-templates?name=nomatch", None),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, "[]");

        let (status, body) = send(
            &mut app,
            req(
                "GET",
                &format!("/api/gse/job-templates/{template_id}"),
                None,
            ),
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
            req(
                "DELETE",
                &format!("/api/gse/job-templates/{template_id}"),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = send(
            &mut app,
            req(
                "GET",
                &format!("/api/gse/job-templates/{template_id}"),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = send(&mut app, req("GET", "/api/gse/job-templates/ghost", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn job_templates_validation_and_conflict() {
        let (mut app, _ledger) = app_ledger("tpl-validation").await;

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/job-templates",
                Some(r#"{"script":"echo hi"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("name"), "{body}");

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/job-templates",
                Some(r#"{"name":"x","script":""}"#),
            ),
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
        let (_, created_body) = send(
            &mut app,
            req("POST", "/api/gse/job-templates", Some(tpl_body)),
        )
        .await;
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
        let (_, created_body) = send(
            &mut app,
            req("POST", "/api/gse/job-templates", Some(tpl_body)),
        )
        .await;
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

    #[tokio::test]
    async fn metrics_exposes_sessions_and_component() {
        let metrics = SelfMetrics::new("gse-server", "127.0.0.1:7101").unwrap();
        let db = test_db("metrics");
        let ledger = Arc::new(Ledger::new(&db).expect("open"));
        ledger.init().await.expect("init");
        metrics.set_hook(Arc::new(GseScrapeHook {
            ledger,
            registry: Some(Arc::new(SessionRegistry::new())),
        }));
        let app = metrics.metrics_router();
        let (status, body) = send(&mut app.clone(), req("GET", "/metrics", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("vectorman_gse_sessions"), "{body}");
        assert!(body.contains("component=\"gse-server\""), "{body}");
    }

    #[tokio::test]
    async fn file_job_submit_validation() {
        let (mut app, _ledger) = app_with_jobs("file-val").await;

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/jobs",
                Some(r#"{"kind":"file_transfer","destination":{"type":"server_temp"}}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("source"), "{body}");

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/jobs",
                Some(
                    r#"{"kind":"file_transfer","source":{"type":"server_temp","file_id":"a"},"destination":{"type":"server_temp"}}"#,
                ),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/jobs",
                Some(
                    r#"{"kind":"file_transfer","source":{"type":"agent","agent_id":"a","path":"/tmp/x"},"destination":{"type":"agent","agent_id":"a","path":"/tmp/x"}}"#,
                ),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/jobs",
                Some(
                    r#"{"kind":"file_transfer","source":{"type":"agent","agent_id":"a1","path":"/tmp/x"},"destination":{"type":"agent","agent_id":"a2","path":"/tmp/y"}}"#,
                ),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("unavailable"), "{body}");
    }

    #[tokio::test]
    async fn job_files_upload_accepts_payload_above_axum_default_limit() {
        // 回归：axum 的 DefaultBodyLimit 默认 2MB，而 job_max_file_bytes 是 64MB。
        // 不覆盖时 2MB 以上的上传会被截断，且报错是「multipart 解析失败」，
        // 看不出是限额 —— 用 3MB 钉住这个行为。
        let (mut app, _ledger) = app_with_jobs("job-files-big").await;
        let boundary = "----gsebig";
        let payload = vec![b'x'; 3 * 1024 * 1024];
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(&payload);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let request = Request::builder()
            .method("POST")
            .uri("/api/gse/job-files")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .expect("multipart");
        let (status, body) = send(&mut app, request).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(body.contains("big.bin"), "{body}");
        assert!(body.contains(&payload.len().to_string()), "{body}");
    }

    #[tokio::test]
    async fn job_files_upload_list_download_delete() {
        let (mut app, _ledger) = app_with_jobs("job-files").await;
        let boundary = "----gseboundary";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\nContent-Type: application/octet-stream\r\n\r\nhello\r\n--{boundary}--\r\n"
        );
        let request = Request::builder()
            .method("POST")
            .uri("/api/gse/job-files")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .expect("multipart");
        let (status, body) = send(&mut app, request).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(body.contains("a.txt"), "{body}");
        let file_id = body
            .split("\"file_id\":\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .expect("file_id")
            .to_string();

        let (status, body) = send(&mut app, req("GET", "/api/gse/job-files", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains(&file_id), "{body}");

        let (status, body) = send(
            &mut app,
            req("GET", &format!("/api/gse/job-files/{file_id}"), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, "hello");

        let (status, _) = send(
            &mut app,
            req("DELETE", &format!("/api/gse/job-files/{file_id}"), None),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = send(
            &mut app,
            req("GET", &format!("/api/gse/job-files/{file_id}"), None),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = send(&mut app, req("DELETE", "/api/gse/job-files/nope", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn file_job_cannot_save_as_template() {
        let (mut app, ledger) = app_with_jobs("file-tpl").await;
        let mut job = crate::ledger::NewJob {
            job_id: "job-ft".to_string(),
            agent_id: "a-1".to_string(),
            timeout_secs: 30,
            created_at: ledger_stamp(),
            kind: "file_transfer".to_string(),
            ..Default::default()
        };
        job.source = Some(gse_proto::FileEndpoint::Agent {
            agent_id: "a-1".to_string(),
            path: "/tmp/a".to_string(),
        });
        job.destination = Some(gse_proto::FileEndpoint::ServerTemp { file_id: None });
        ledger.insert_job(&job).await.expect("insert");

        let (status, body) = send(
            &mut app,
            req(
                "POST",
                "/api/gse/jobs/job-ft/save-as-template",
                Some(r#"{"name":"nope"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    #[tokio::test]
    async fn job_files_upload_rejects_oversize() {
        let db = test_db("job-files-big");
        let ledger = Arc::new(Ledger::new(&db).expect("open"));
        ledger.init().await.expect("init");
        let store = Arc::new(JobFileStore::open(format!("{db}.job-files")).expect("store"));
        let cfg = ServerConfig {
            job_max_file_bytes: 4,
            ..ServerConfig::default()
        };
        let mut app = router(
            AdminState {
                ledger,
                registry: Some(Arc::new(SessionRegistry::new())),
                cfg: Some(Arc::new(cfg)),
                file_store: Some(store),
                admin_password: String::new(),
            },
            None,
        );
        let boundary = "----gseboundary";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.bin\"\r\nContent-Type: application/octet-stream\r\n\r\nhello world\r\n--{boundary}--\r\n"
        );
        let request = Request::builder()
            .method("POST")
            .uri("/api/gse/job-files")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .expect("multipart");
        let (status, body) = send(&mut app, request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("exceeds"), "{body}");
    }
}
