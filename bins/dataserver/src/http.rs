use std::path::Path;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path as PathParam, Query, State};
use axum::http::{Request, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use dataplane_core::{
    json_params_to_sql_values, sql_value_to_json, AuthN, DataplaneError, ErrorCode, RequestMeta,
    SqlResult,
};
use dataplane_file::FileStore;
use dataplane_ingest::{apply, search, DataEnvelope, LogSearchQuery, StreamIndex};
use dataplane_kv::KvStore;
use dataplane_log::LogStore;
use dataplane_sql::RelationalStore;
use dataplane_ts::{PromResult, PromResultType, TimeSeriesStore};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::services::{ServeDir, ServeFile};

use crate::cleanup::{
    clamp_retention_days, gse_call, join_gse_url, now_micros, retain_key, retention_days_from_item,
    MICROS_PER_DAY,
};

#[derive(Clone)]
pub struct AppState {
    #[allow(dead_code)]
    pub file: Arc<dyn FileStore>,
    pub kv: Arc<dyn KvStore>,
    pub sql: Arc<dyn RelationalStore>,
    pub ts: Arc<dyn TimeSeriesStore>,
    pub log: Arc<dyn LogStore>,
    pub auth: Arc<dyn AuthN>,
    pub gse_admin_url: Option<String>,
}

fn json_err(status: StatusCode, e: DataplaneError) -> Response {
    (
        status,
        Json(json!({"error": e.message, "code": e.code.as_str()})),
    )
        .into_response()
}

fn map_err(e: DataplaneError) -> Response {
    let status = match e.code {
        ErrorCode::InvalidArgument => StatusCode::BAD_REQUEST,
        ErrorCode::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    json_err(status, e)
}

fn unavailable(msg: impl Into<String>) -> DataplaneError {
    DataplaneError::new(ErrorCode::Unavailable, msg)
}

/// 鉴权中间件：在请求链最外层调用 `AuthN`。
async fn auth_middleware(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let meta = RequestMeta {
        method: req.method().to_string(),
        path: req.uri().path().to_string(),
        headers,
        peer_addr: None,
    };
    if let Err(e) = state.auth.check(&meta).await {
        return json_err(StatusCode::UNAUTHORIZED, e);
    }
    next.run(req).await
}

async fn health() -> Response {
    Json(json!({"status": "ok"})).into_response()
}

#[derive(Deserialize)]
struct SqlRequest {
    sql: String,
    #[serde(default)]
    params: Vec<Value>,
}

fn sql_rows_to_json(res: &SqlResult) -> Value {
    let rows: Vec<Vec<Value>> = res
        .rows
        .iter()
        .map(|row| row.iter().map(sql_value_to_json).collect())
        .collect();
    json!({"columns": res.columns, "rows": rows})
}

async fn sql_exec(
    State(state): State<AppState>,
    body: Result<Json<SqlRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let body = match body {
        Ok(b) => b.0,
        Err(_) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                DataplaneError::new(
                    ErrorCode::InvalidArgument,
                    "invalid JSON body or missing 'sql' field",
                ),
            );
        }
    };
    let params = match json_params_to_sql_values(&body.params) {
        Ok(p) => p,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e),
    };
    match state.sql.execute(&body.sql, &params).await {
        Ok(res) => Json(sql_rows_to_json(&res)).into_response(),
        Err(e) => json_err(StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

fn prom_result_to_json(r: &PromResult) -> Value {
    let result_type = match r.result_type {
        PromResultType::Vector => "vector",
        PromResultType::Matrix => "matrix",
    };
    let result: Vec<Value> = r
        .result
        .iter()
        .map(|s| {
            let metric: Value = s
                .metric
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect();
            match r.result_type {
                PromResultType::Vector => {
                    let (ts_us, v) = s.value.unwrap_or((0, 0.0));
                    json!({"metric": metric, "value": [ts_us as f64 / 1_000_000.0, v]})
                }
                PromResultType::Matrix => {
                    let values: Vec<Value> = s
                        .values
                        .clone()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(t, v)| json!([t as f64 / 1_000_000.0, v]))
                        .collect();
                    json!({"metric": metric, "values": values})
                }
            }
        })
        .collect();
    json!({"resultType": result_type, "result": result})
}

fn prom_error_response(e: DataplaneError) -> Response {
    Json(json!({"status": "error", "errorType": e.code.as_str(), "error": e.message}))
        .into_response()
}

fn parse_time_param(v: &str) -> Result<i64, DataplaneError> {
    v.parse::<f64>()
        .map(|sec| (sec * 1_000_000.0) as i64)
        .map_err(|_| {
            DataplaneError::new(
                ErrorCode::InvalidArgument,
                format!("invalid time parameter: {v}"),
            )
        })
}

/// 从查询参数读取必需的时间参数（秒，转微秒）。
fn required_time_us(params: &Value, name: &str) -> Result<i64, DataplaneError> {
    match params.get(name) {
        Some(Value::String(s)) => parse_time_param(s),
        Some(other) => Err(DataplaneError::new(
            ErrorCode::InvalidArgument,
            format!("invalid '{name}' parameter: {other}"),
        )),
        None => Err(DataplaneError::new(
            ErrorCode::InvalidArgument,
            format!("missing '{name}' parameter"),
        )),
    }
}

/// 从查询参数读取可选时间参数（秒，转微秒）。
fn optional_time_us(params: &Value, name: &str) -> Result<Option<i64>, DataplaneError> {
    match params.get(name) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(parse_time_param(s)?)),
        Some(other) => Err(DataplaneError::new(
            ErrorCode::InvalidArgument,
            format!("invalid '{name}' parameter: {other}"),
        )),
    }
}

async fn prom_query(State(state): State<AppState>, Query(params): Query<Value>) -> Response {
    let expr = params
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if expr.is_empty() {
        return prom_error_response(DataplaneError::new(
            ErrorCode::InvalidArgument,
            "missing 'query' parameter",
        ));
    }
    let eval_time = match optional_time_us(&params, "time") {
        Ok(v) => v,
        Err(e) => return prom_error_response(e),
    };
    match state.ts.query_instant(&expr, eval_time).await {
        Ok(r) => Json(json!({"status": "success", "data": prom_result_to_json(&r)})).into_response(),
        Err(e) => prom_error_response(e),
    }
}

async fn prom_query_range(State(state): State<AppState>, Query(params): Query<Value>) -> Response {
    let expr = params
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if expr.is_empty() {
        return prom_error_response(DataplaneError::new(
            ErrorCode::InvalidArgument,
            "missing 'query' parameter",
        ));
    }
    let start = match required_time_us(&params, "start") {
        Ok(v) => v,
        Err(e) => return prom_error_response(e),
    };
    let end = match required_time_us(&params, "end") {
        Ok(v) => v,
        Err(e) => return prom_error_response(e),
    };
    let step = match params.get("step") {
        Some(Value::String(s)) => match s.parse::<i64>() {
            Ok(v) => v,
            Err(_) => {
                return prom_error_response(DataplaneError::new(
                    ErrorCode::InvalidArgument,
                    "invalid 'step' parameter",
                ))
            }
        },
        Some(other) => {
            return prom_error_response(DataplaneError::new(
                ErrorCode::InvalidArgument,
                format!("invalid 'step' parameter: {other}"),
            ))
        }
        None => {
            return prom_error_response(DataplaneError::new(
                ErrorCode::InvalidArgument,
                "missing 'step' parameter",
            ))
        }
    };
    if step <= 0 {
        return prom_error_response(DataplaneError::new(
            ErrorCode::InvalidArgument,
            "'step' must be positive",
        ));
    }
    match state.ts.query_range(&expr, start, end, step).await {
        Ok(r) => Json(json!({"status": "success", "data": prom_result_to_json(&r)})).into_response(),
        Err(e) => prom_error_response(e),
    }
}

async fn ingest(
    State(state): State<AppState>,
    body: Result<Json<DataEnvelope>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let envelope = match body {
        Ok(b) => b.0,
        Err(_) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                DataplaneError::invalid_argument("invalid JSON body"),
            );
        }
    };
    match apply(envelope, state.ts.as_ref(), state.log.as_ref(), state.kv.as_ref()).await {
        Ok(reply) => Json(reply).into_response(),
        Err(e) => map_err(e),
    }
}

async fn logs_search(
    State(state): State<AppState>,
    body: Result<Json<LogSearchQuery>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let query = match body {
        Ok(b) => b.0,
        Err(_) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                DataplaneError::invalid_argument("invalid JSON body"),
            );
        }
    };
    match search(state.log.as_ref(), query).await {
        Ok(records) => {
            let records: Vec<Value> = records
                .into_iter()
                .map(|r| {
                    json!({
                        "id": r.id,
                        "timestamp": r.timestamp,
                        "level": r.level,
                        "message": r.message,
                        "labels": r.labels,
                    })
                })
                .collect();
            Json(json!({"records": records})).into_response()
        }
        Err(e) => map_err(e),
    }
}

fn parse_stream_key(key: &str) -> Option<(String, String, String)> {
    let rest = key.strip_prefix("stream/")?;
    let mut parts = rest.splitn(3, '/');
    let agent_id = parts.next()?.to_string();
    let data_type = parts.next()?.to_string();
    let data_id = parts.next()?.to_string();
    if agent_id.is_empty() || data_type.is_empty() || data_id.is_empty() {
        return None;
    }
    Some((agent_id, data_type, data_id))
}

async fn streams(State(state): State<AppState>) -> Response {
    match state.kv.scan_prefix(b"stream/").await {
        Ok(rows) => {
            let mut streams = Vec::new();
            for (key, value) in rows {
                let key_s = String::from_utf8_lossy(&key);
                let Some((agent_id, data_type, data_id)) = parse_stream_key(&key_s) else {
                    continue;
                };
                let Ok(meta) = serde_json::from_slice::<StreamIndex>(&value) else {
                    continue;
                };
                streams.push(json!({
                    "agent_id": agent_id,
                    "data_type": data_type,
                    "data_id": data_id,
                    "last_seen_micros": meta.last_seen_micros,
                    "accepted": meta.accepted,
                }));
            }
            Json(json!({"streams": streams})).into_response()
        }
        Err(e) => map_err(e),
    }
}

fn require_gse(state: &AppState) -> Result<&str, DataplaneError> {
    state
        .gse_admin_url
        .as_deref()
        .ok_or_else(|| unavailable("gse_admin_url is not configured"))
}

async fn forward_gse(state: &AppState, method: &str, gse_path: &str, query: Option<&str>, body: &[u8]) -> Response {
    let base = match require_gse(state) {
        Ok(u) => u,
        Err(e) => return map_err(e),
    };
    let url = join_gse_url(base, gse_path, query);
    match gse_call(method, &url, body).await {
        Ok((status, text)) => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            (
                code,
                [("content-type", "application/json")],
                text,
            )
                .into_response()
        }
        Err(e) => map_err(e),
    }
}

async fn collect_list(State(state): State<AppState>, uri: Uri) -> Response {
    forward_gse(&state, "GET", "/api/gse/collect-items", uri.query(), b"").await
}

async fn collect_create(State(state): State<AppState>, uri: Uri, body: Bytes) -> Response {
    forward_gse(
        &state,
        "POST",
        "/api/gse/collect-items",
        uri.query(),
        &body,
    )
    .await
}

async fn collect_get(
    State(state): State<AppState>,
    PathParam(item_id): PathParam<String>,
    uri: Uri,
) -> Response {
    let path = format!("/api/gse/collect-items/{item_id}");
    forward_gse(&state, "GET", &path, uri.query(), b"").await
}

async fn collect_put(
    State(state): State<AppState>,
    PathParam(item_id): PathParam<String>,
    uri: Uri,
    body: Bytes,
) -> Response {
    let path = format!("/api/gse/collect-items/{item_id}");
    forward_gse(&state, "PUT", &path, uri.query(), &body).await
}

async fn collect_delete(
    State(state): State<AppState>,
    PathParam(item_id): PathParam<String>,
) -> Response {
    let base = match require_gse(&state) {
        Ok(u) => u.to_string(),
        Err(e) => return map_err(e),
    };
    let get_url = join_gse_url(&base, &format!("/api/gse/collect-items/{item_id}"), None);
    let days = match gse_call("GET", &get_url, b"").await {
        Ok((200, body)) => serde_json::from_str::<Value>(&body)
            .map(|v| retention_days_from_item(&v))
            .unwrap_or(1),
        _ => 1,
    };
    let days = clamp_retention_days(days);
    let until = now_micros() + days as i64 * MICROS_PER_DAY;
    let payload = json!({"until_micros": until});
    if let Err(e) = state
        .kv
        .set(retain_key(&item_id).as_bytes(), payload.to_string().as_bytes())
        .await
    {
        return map_err(e);
    }
    forward_gse(
        &state,
        "DELETE",
        &format!("/api/gse/collect-items/{item_id}"),
        None,
        b"",
    )
    .await
}

async fn agents_list(State(state): State<AppState>, uri: Uri) -> Response {
    forward_gse(&state, "GET", "/api/gse/agents", uri.query(), b"").await
}

fn api_routes(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/sql", post(sql_exec))
        .route("/v1/ingest", post(ingest))
        .route("/v1/logs/search", post(logs_search))
        .route("/v1/streams", get(streams))
        .route("/api/v1/query", get(prom_query))
        .route("/api/v1/query_range", get(prom_query_range))
        .route("/v1/collect-items", get(collect_list).post(collect_create))
        .route(
            "/v1/collect-items/{item_id}",
            get(collect_get).put(collect_put).delete(collect_delete),
        )
        .route("/v1/agents", get(agents_list))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state)
}

/// SQL 口路由：接入、查询、流、采集项反代、可选 SPA。
pub fn sql_router(state: AppState, web_dir: Option<&Path>) -> Router {
    let api = api_routes(state);
    match web_dir {
        Some(dir) => {
            let index = dir.join("index.html");
            api.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)))
        }
        None => api,
    }
}

/// Prom 口路由，处理器与 SQL 口 `/api/v1/query*` 相同。
pub fn prom_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/query", get(prom_query))
        .route("/api/v1/query_range", get(prom_query_range))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use dataplane_core::{resolve_data_paths, NoopAuth};
    use dataplane_file::DirFileStore;
    use dataplane_kv::RedbKvStore;
    use dataplane_log::TantivyLogStore;
    use dataplane_sql::SqliteRelationalStore;
    use dataplane_ts::TsinkTimeSeriesStore;
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;
    use crate::cleanup::{apply_retention, LiveItem};

    struct TestEnv {
        state: AppState,
        _dir: tempfile::TempDir,
    }

    fn test_env(gse_admin_url: Option<String>) -> TestEnv {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        std::fs::create_dir_all(&root).unwrap();
        let paths = resolve_data_paths(root.to_str().unwrap()).unwrap();
        paths.ensure_dirs().unwrap();
        let file: Arc<dyn FileStore> = Arc::new(DirFileStore::new(paths.files.clone()));
        let kv: Arc<dyn KvStore> = Arc::new(RedbKvStore::new(&paths.kv).unwrap());
        let sql: Arc<dyn RelationalStore> =
            Arc::new(SqliteRelationalStore::new(&paths.sql).unwrap());
        let ts: Arc<dyn TimeSeriesStore> =
            Arc::new(TsinkTimeSeriesStore::new(&paths.ts).unwrap());
        let log: Arc<dyn LogStore> = Arc::new(TantivyLogStore::new(&paths.logs).unwrap());
        TestEnv {
            state: AppState {
                file,
                kv,
                sql,
                ts,
                log,
                auth: Arc::new(NoopAuth),
                gse_admin_url,
            },
            _dir: dir,
        }
    }

    async fn send(app: &Router, req: Request<Body>) -> (StatusCode, String) {
        let resp = app.clone().oneshot(req).await.expect("oneshot");
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

    const TS: i64 = 1_710_000_000_000_000;

    fn metrics_envelope() -> String {
        json!({
            "batch_id": "b-m",
            "data_type": "metrics",
            "data_id": "item-1",
            "agent_id": "agent-1",
            "host_id": "host-1",
            "sent_at_micros": TS,
            "records": [{
                "record_id": "m1",
                "timestamp": TS,
                "measurement": "cpu_usage",
                "tags": {},
                "field_name": "value",
                "field_value": 12.5
            }]
        })
        .to_string()
    }

    fn logs_envelope(record_id: &str, data_id: &str, message: &str) -> String {
        json!({
            "batch_id": "b-l",
            "data_type": "logs",
            "data_id": data_id,
            "agent_id": "agent-1",
            "host_id": "host-1",
            "sent_at_micros": TS,
            "records": [{
                "record_id": record_id,
                "timestamp": TS,
                "level": "error",
                "message": message,
                "source": "/var/log/app.log",
                "labels": {}
            }]
        })
        .to_string()
    }

    #[tokio::test]
    async fn ingest_prom_logs_streams_and_health() {
        let env = test_env(None);
        let app = sql_router(env.state.clone(), None);

        let (st, body) = send(&app, req("GET", "/health", None)).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(body.contains("\"status\":\"ok\""));

        let (st, body) = send(&app, req("POST", "/v1/ingest", Some(&metrics_envelope()))).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(body.contains("\"status\":\"ok\""), "{body}");

        let (st, body) = send(
            &app,
            req("GET", "/api/v1/query?query=cpu_usage&time=1710000000", None),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(body.contains("cpu_usage") || body.contains("12.5"), "{body}");

        let (st, body) = send(
            &app,
            req("POST", "/v1/ingest", Some(&logs_envelope("l1", "item-1", "listen failed"))),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        let (st, body) = send(
            &app,
            req(
                "POST",
                "/v1/logs/search",
                Some(r#"{"data_type":"logs","message_query":"listen"}"#),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(body.contains("listen failed"), "{body}");

        let (st, body) = send(
            &app,
            req(
                "POST",
                "/v1/logs/search",
                Some(r#"{"data_type":"apm"}"#),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(!body.contains("listen failed"), "{body}");

        let (st, body) = send(
            &app,
            req(
                "POST",
                "/v1/logs/search",
                Some(r#"{"from_ts":2,"to_ts":1}"#),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("invalid_argument"), "{body}");

        let (st, body) = send(&app, req("GET", "/v1/streams", None)).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(body.contains("agent-1"), "{body}");
        assert!(body.contains("item-1"), "{body}");
    }

    #[tokio::test]
    async fn collect_items_unavailable_without_gse_url() {
        let env = test_env(None);
        let app = sql_router(env.state.clone(), None);
        let (st, body) = send(&app, req("GET", "/v1/collect-items", None)).await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert!(body.contains("unavailable"), "{body}");
    }

    #[tokio::test]
    async fn proxy_collect_items_and_agents() {
        let (gse_url, handle) = spawn_mock_gse().await;
        let env = test_env(Some(gse_url));
        let app = sql_router(env.state.clone(), None);

        let (st, body) = send(
            &app,
            req("POST", "/v1/collect-items", Some(r#"{"name":"cpu"}"#)),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(body.contains("/api/gse/collect-items"), "{body}");
        assert!(body.contains("POST"), "{body}");

        let (st, body) = send(&app, req("GET", "/v1/agents?online=1", None)).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(body.contains("/api/gse/agents"), "{body}");
        assert!(body.contains("online=1"), "{body}");

        handle.abort();
    }

    #[tokio::test]
    async fn delete_writes_retain_and_cleanup_drops_logs() {
        let (gse_url, handle) = spawn_mock_gse().await;
        let env = test_env(Some(gse_url));
        let app = sql_router(env.state.clone(), None);

        let (st, body) = send(
            &app,
            req(
                "POST",
                "/v1/ingest",
                Some(&logs_envelope("old-1", "item-del", "stale line")),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        let (st, body) = send(&app, req("DELETE", "/v1/collect-items/item-del", None)).await;
        assert_eq!(st, StatusCode::OK, "{body}");

        let retain = env
            .state
            .kv
            .get(b"retain/item-del")
            .await
            .unwrap();
        let meta: Value = serde_json::from_slice(&retain).unwrap();
        assert!(meta["until_micros"].as_i64().unwrap() > now_micros());

        env.state
            .kv
            .set(
                b"retain/item-del",
                br#"{"until_micros":1}"#,
            )
            .await
            .unwrap();
        apply_retention(
            env.state.log.as_ref(),
            env.state.kv.as_ref(),
            &[],
            now_micros(),
        )
        .await
        .unwrap();

        let (st, body) = send(
            &app,
            req(
                "POST",
                "/v1/logs/search",
                Some(r#"{"data_id":"item-del","message_query":"stale"}"#),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(!body.contains("stale line"), "{body}");

        handle.abort();
    }

    #[tokio::test]
    async fn live_retention_deletes_old_logs() {
        let env = test_env(None);
        let app = sql_router(env.state.clone(), None);
        let (st, body) = send(
            &app,
            req(
                "POST",
                "/v1/ingest",
                Some(&logs_envelope("old-live", "item-live", "ancient")),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        apply_retention(
            env.state.log.as_ref(),
            env.state.kv.as_ref(),
            &[LiveItem {
                item_id: "item-live".into(),
                retention_days: 1,
            }],
            TS + 2 * MICROS_PER_DAY,
        )
        .await
        .unwrap();

        let (st, body) = send(
            &app,
            req(
                "POST",
                "/v1/logs/search",
                Some(r#"{"data_id":"item-live"}"#),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(!body.contains("ancient"), "{body}");
    }

    #[tokio::test]
    async fn web_dir_serves_static_and_spa_fallback() {
        let dir = std::env::temp_dir().join(format!(
            "dataserver-web-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("index.html"), "<html>spa-root</html>").unwrap();
        std::fs::write(dir.join("assets/app.js"), "console.log(1)").unwrap();

        let env = test_env(None);
        let app = sql_router(env.state.clone(), Some(&dir));

        let (st, body) = send(&app, req("GET", "/assets/app.js", None)).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body, "console.log(1)");

        let (st, body) = send(&app, req("GET", "/metrics", None)).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(body.contains("spa-root"), "{body}");

        let (st, body) = send(&app, req("GET", "/health", None)).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(body.contains("ok"), "{body}");
    }

    async fn spawn_mock_gse() -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new().fallback(mock_gse_echo);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock gse");
        let addr = listener.local_addr().expect("addr");
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), handle)
    }

    async fn mock_gse_echo(method: Method, uri: Uri, body: Bytes) -> Response {
        Json(json!({
            "ok": true,
            "method": method.as_str(),
            "path": uri.path(),
            "query": uri.query(),
            "body": String::from_utf8_lossy(&body),
            "item_id": "item-del",
            "storage": {"retention_days": 2}
        }))
        .into_response()
    }
}
