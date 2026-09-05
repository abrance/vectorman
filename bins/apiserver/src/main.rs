use std::process::ExitCode;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use dataplane_core::{
    load_config, resolve_data_paths, sql_value_to_json, json_params_to_sql_values, AuthN, DataplaneError,
    ErrorCode, NoopAuth, RequestMeta, SqlResult,
};
use dataplane_file::{DirFileStore, FileStore};
use dataplane_kv::{KvStore, RedbKvStore};
use dataplane_log::{LogStore, TantivyLogStore};
use dataplane_sql::{RelationalStore, SqliteRelationalStore};
use dataplane_ts::{PromResult, PromResultType, TimeSeriesStore, TsinkTimeSeriesStore};
use serde::Deserialize;
use serde_json::{json, Value};

const ENGINE_DIR_MODE_REQUIRED: &str = "apiserver requires a directory data_path (single-file mode only supports sqlite, and this server enables all engines)";

#[derive(Parser)]
struct Args {
    /// 配置文件路径；缺省时读取 ./config.toml，若不存在则使用内置默认值。
    #[arg(long)]
    config: Option<String>,
}

#[derive(Clone)]
struct AppState {
    file: Arc<dyn FileStore>,
    kv: Arc<dyn KvStore>,
    sql: Arc<dyn RelationalStore>,
    ts: Arc<dyn TimeSeriesStore>,
    log: Arc<dyn LogStore>,
    auth: Arc<dyn AuthN>,
}

fn exit_with(prefix: &str, e: DataplaneError) -> ExitCode {
    eprintln!("{prefix}: {}: {}", e.code.as_str(), e.message);
    ExitCode::FAILURE
}

fn json_err(status: StatusCode, e: DataplaneError) -> Response {
    (status, Json(json!({"error": e.message, "code": e.code.as_str()}))).into_response()
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
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or_default().to_string()))
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

async fn sql_exec(State(state): State<AppState>, body: Result<Json<SqlRequest>, axum::extract::rejection::JsonRejection>) -> Response {
    let body = match body {
        Ok(b) => b.0,
        Err(_) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                DataplaneError::new(ErrorCode::InvalidArgument, "invalid JSON body or missing 'sql' field"),
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
            let metric: Value = s.metric.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
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
    Json(json!({"status": "error", "errorType": e.code.as_str(), "error": e.message})).into_response()
}

fn parse_time_param(v: &str) -> Result<i64, DataplaneError> {
    v.parse::<f64>()
        .map(|sec| (sec * 1_000_000.0) as i64)
        .map_err(|_| DataplaneError::new(ErrorCode::InvalidArgument, format!("invalid time parameter: {v}")))
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

async fn prom_query(
    State(state): State<AppState>,
    Query(params): Query<Value>,
) -> Response {
    let expr = params.get("query").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    if expr.is_empty() {
        return prom_error_response(DataplaneError::new(ErrorCode::InvalidArgument, "missing 'query' parameter"));
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

async fn prom_query_range(
    State(state): State<AppState>,
    Query(params): Query<Value>,
) -> Response {
    let expr = params.get("query").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    if expr.is_empty() {
        return prom_error_response(DataplaneError::new(ErrorCode::InvalidArgument, "missing 'query' parameter"));
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
            Err(_) => return prom_error_response(DataplaneError::new(ErrorCode::InvalidArgument, "invalid 'step' parameter")),
        },
        Some(other) => return prom_error_response(DataplaneError::new(ErrorCode::InvalidArgument, format!("invalid 'step' parameter: {other}"))),
        None => return prom_error_response(DataplaneError::new(ErrorCode::InvalidArgument, "missing 'step' parameter")),
    };
    if step <= 0 {
        return prom_error_response(DataplaneError::new(ErrorCode::InvalidArgument, "'step' must be positive"));
    }
    match state.ts.query_range(&expr, start, end, step).await {
        Ok(r) => Json(json!({"status": "success", "data": prom_result_to_json(&r)})).into_response(),
        Err(e) => prom_error_response(e),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();

    let cfg = if let Some(path) = &args.config {
        load_config(Some(path))
    } else if std::path::Path::new("config.toml").exists() {
        load_config(Some("config.toml"))
    } else {
        load_config(None)
    };
    let cfg = match cfg {
        Ok(c) => c,
        Err(e) => return exit_with("config error", e),
    };

    let paths = match resolve_data_paths(&cfg.data_path) {
        Ok(p) => p,
        Err(e) => return exit_with("data_path error", e),
    };
    if !paths.is_directory {
        return exit_with("data_path error", DataplaneError::new(ErrorCode::ConfigInvalid, ENGINE_DIR_MODE_REQUIRED));
    }
    if let Err(e) = paths.ensure_dirs() {
        return exit_with("data_path error", e);
    }

    let file: Arc<dyn FileStore> = Arc::new(DirFileStore::new(paths.files.clone()));
    let kv: Arc<dyn KvStore> = match RedbKvStore::new(&paths.kv) {
        Ok(s) => Arc::new(s),
        Err(e) => return exit_with("engine kv init failed", e),
    };
    let sql: Arc<dyn RelationalStore> = match SqliteRelationalStore::new(&paths.sql) {
        Ok(s) => Arc::new(s),
        Err(e) => return exit_with("engine sql init failed", e),
    };
    let ts: Arc<dyn TimeSeriesStore> = match TsinkTimeSeriesStore::new(&paths.ts) {
        Ok(s) => Arc::new(s),
        Err(e) => return exit_with("engine ts init failed", e),
    };
    let log: Arc<dyn LogStore> = match TantivyLogStore::new(&paths.logs) {
        Ok(s) => Arc::new(s),
        Err(e) => return exit_with("engine log init failed", e),
    };

    let state = AppState {
        file,
        kv,
        sql,
        ts,
        log,
        auth: Arc::new(NoopAuth),
    };

    let sql_router = Router::new()
        .route("/health", get(health))
        .route("/v1/sql", post(sql_exec))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state.clone());

    let prom_router = Router::new()
        .route("/health", get(health))
        .route("/api/v1/query", get(prom_query))
        .route("/api/v1/query_range", get(prom_query_range))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state);

    let sql_listener = match tokio::net::TcpListener::bind(&cfg.sql_http.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bind sql_http {} failed: {e}", cfg.sql_http.listen);
            return ExitCode::FAILURE;
        }
    };
    let prom_listener = match tokio::net::TcpListener::bind(&cfg.prom_http.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bind prom_http {} failed: {e}", cfg.prom_http.listen);
            return ExitCode::FAILURE;
        }
    };

    println!("sql_http={} prom_http={}", cfg.sql_http.listen, cfg.prom_http.listen);

    let sql_fut = axum::serve(sql_listener, sql_router);
    let prom_fut = axum::serve(prom_listener, prom_router);
    let _ = tokio::try_join!(sql_fut, prom_fut);
    ExitCode::SUCCESS
}