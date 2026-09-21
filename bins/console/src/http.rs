use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};

use crate::catalog::{Catalog, CatalogError};
use crate::config::ConsoleConfig;
use vectorman_metrics::SelfMetrics;

#[derive(Clone)]
struct AppState {
    catalog: Arc<Catalog>,
}

#[derive(Debug, Deserialize)]
struct AppInput {
    name: String,
    url: String,
    tags: Option<Vec<String>>,
}

fn err_json(status: StatusCode, e: &CatalogError) -> Response {
    (
        status,
        Json(json!({"error": e.code(), "message": e.to_string()})),
    )
        .into_response()
}

fn map_err(e: CatalogError) -> Response {
    let status = match e {
        CatalogError::InvalidName(_)
        | CatalogError::InvalidUrl(_)
        | CatalogError::LimitExceeded
        | CatalogError::InvalidTag(_)
        | CatalogError::TagLimitExceeded => StatusCode::BAD_REQUEST,
        CatalogError::NameConflict => StatusCode::CONFLICT,
        CatalogError::NotFound => StatusCode::NOT_FOUND,
        CatalogError::Persist(_) | CatalogError::Corrupt(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    err_json(status, &e)
}

struct ConsoleScrapeHook {
    catalog: Arc<Catalog>,
}

#[async_trait::async_trait]
impl vectorman_metrics::ScrapeHook for ConsoleScrapeHook {
    async fn on_scrape(&self, metrics: &SelfMetrics) {
        let n = self.catalog.list().await.len();
        metrics.set_gauge("vectorman_console_apps", n as f64);
    }
}

pub fn router(catalog: Arc<Catalog>, web_dir: Option<&std::path::Path>) -> Router {
    build_router(catalog, web_dir, None)
}

fn build_router(
    catalog: Arc<Catalog>,
    web_dir: Option<&std::path::Path>,
    metrics: Option<Arc<SelfMetrics>>,
) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/api/console/apps", get(list_apps).post(create_app))
        .route(
            "/api/console/apps/{app_id}",
            axum::routing::put(update_app).delete(delete_app),
        )
        .with_state(AppState { catalog });
    let app = match web_dir {
        Some(dir) => {
            let index = dir.join("index.html");
            api.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)))
        }
        None => api,
    };
    vectorman_metrics::apply_http_metrics(app, metrics)
}

pub async fn serve(cfg: ConsoleConfig, catalog: Catalog) -> Result<(), String> {
    let web_path = std::path::Path::new(&cfg.web_dir);
    let web_dir = if web_path.join("index.html").is_file() {
        println!(
            "console: serving web dist from {} on {}",
            cfg.web_dir, cfg.listen
        );
        Some(cfg.web_dir.clone())
    } else {
        eprintln!(
            "console: web_dir {} missing index.html; static UI disabled",
            cfg.web_dir
        );
        None
    };
    let catalog = Arc::new(catalog);
    let metrics = SelfMetrics::new("console", &cfg.listen)?;
    metrics.set_hook(Arc::new(ConsoleScrapeHook {
        catalog: catalog.clone(),
    }));
    let app = build_router(
        catalog,
        web_dir.as_deref().map(std::path::Path::new),
        Some(metrics.clone()),
    );
    let listener = tokio::net::TcpListener::bind(&cfg.listen)
        .await
        .map_err(|e| format!("bind {}: {e}", cfg.listen))?;
    let metrics_listener = tokio::net::TcpListener::bind(&cfg.metrics_listen)
        .await
        .map_err(|e| format!("bind {}: {e}", cfg.metrics_listen))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("local_addr: {e}"))?;
    println!("console: listening on {addr}");
    println!("console: metrics listening on {}", cfg.metrics_listen);
    let metrics_app = metrics.metrics_router();
    tokio::spawn(async move {
        if let Err(e) = axum::serve(metrics_listener, metrics_app).await {
            eprintln!("console: metrics serve failed: {e}");
        }
    });
    axum::serve(listener, app)
        .await
        .map_err(|e| format!("serve: {e}"))
}

async fn health() -> Response {
    Json(json!({"status": "ok"})).into_response()
}

async fn list_apps(State(state): State<AppState>) -> Response {
    Json(state.catalog.list().await).into_response()
}

async fn create_app(
    State(state): State<AppState>,
    body: Result<Json<AppInput>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(input) = match body {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid_name","message": format!("invalid JSON body: {e}")})),
            )
                .into_response();
        }
    };
    match state
        .catalog
        .create(input.name, input.url, input.tags.unwrap_or_default())
        .await
    {
        Ok(app) => (StatusCode::CREATED, Json(app)).into_response(),
        Err(e) => map_err(e),
    }
}

async fn update_app(
    State(state): State<AppState>,
    Path(app_id): Path<String>,
    body: Result<Json<AppInput>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(input) = match body {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid_name","message": format!("invalid JSON body: {e}")})),
            )
                .into_response();
        }
    };
    match state
        .catalog
        .update(
            &app_id,
            input.name,
            input.url,
            input.tags.unwrap_or_default(),
        )
        .await
    {
        Ok(app) => Json(app).into_response(),
        Err(e) => map_err(e),
    }
}

async fn delete_app(State(state): State<AppState>, Path(app_id): Path<String>) -> Response {
    match state.catalog.delete(&app_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => map_err(e),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    fn tmp_file(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("console-http-{}-{name}.json", std::process::id()))
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

    #[tokio::test]
    async fn crud_and_health() {
        let path = tmp_file("crud");
        let _ = std::fs::remove_file(&path);
        let app = router(Arc::new(Catalog::open(&path).unwrap()), None);

        let (st, body) = send(&app, req("GET", "/health", None)).await;
        assert_eq!(st, StatusCode::OK);
        assert!(body.contains("ok"));

        let (st, body) = send(
            &app,
            req(
                "POST",
                "/api/console/apps",
                Some(r#"{"name":"GSE","url":"http://127.0.0.1:7101"}"#),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{body}");
        let created: serde_json::Value = serde_json::from_str(&body).unwrap();
        let id = created["app_id"].as_str().unwrap().to_string();
        assert!(created["tags"].as_array().unwrap().is_empty());

        let (st, body) = send(&app, req("GET", "/api/console/apps", None)).await;
        assert_eq!(st, StatusCode::OK);
        let listed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
        assert_eq!(listed.len(), 1);

        let (st, _) = send(
            &app,
            req(
                "PUT",
                &format!("/api/console/apps/{id}"),
                Some(r#"{"name":"GSE2","url":"https://example.com"}"#),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        let (st, body) = send(
            &app,
            req("DELETE", &format!("/api/console/apps/{id}"), None),
        )
        .await;
        assert_eq!(st, StatusCode::NO_CONTENT, "{body}");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn conflict_and_not_found_status() {
        let path = tmp_file("status");
        let _ = std::fs::remove_file(&path);
        let app = router(Arc::new(Catalog::open(&path).unwrap()), None);
        let _ = send(
            &app,
            req(
                "POST",
                "/api/console/apps",
                Some(r#"{"name":"A","url":"http://127.0.0.1"}"#),
            ),
        )
        .await;
        let (st, body) = send(
            &app,
            req(
                "POST",
                "/api/console/apps",
                Some(r#"{"name":"A","url":"http://127.0.0.1/x"}"#),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::CONFLICT);
        assert!(body.contains("name_conflict"));

        let (st, body) = send(&app, req("DELETE", "/api/console/apps/missing", None)).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert!(body.contains("not_found"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn tags_create_update_and_validation() {
        let path = tmp_file("tags");
        let _ = std::fs::remove_file(&path);
        let app = router(Arc::new(Catalog::open(&path).unwrap()), None);

        let (st, body) = send(
            &app,
            req(
                "POST",
                "/api/console/apps",
                Some(r#"{"name":"GSE","url":"http://127.0.0.1:7101","tags":["prod","gse"]}"#),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{body}");
        let created: serde_json::Value = serde_json::from_str(&body).unwrap();
        let id = created["app_id"].as_str().unwrap().to_string();
        assert_eq!(created["tags"], serde_json::json!(["prod", "gse"]));

        let (st, body) = send(
            &app,
            req(
                "PUT",
                &format!("/api/console/apps/{id}"),
                Some(r#"{"name":"GSE","url":"http://127.0.0.1:7101","tags":[" prod ","prod"]}"#),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        let updated: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(updated["tags"], serde_json::json!(["prod"]));

        let (st, body) = send(
            &app,
            req(
                "POST",
                "/api/console/apps",
                Some(r#"{"name":"B","url":"http://127.0.0.1","tags":["  "]}"#),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(body.contains("invalid_tag"), "{body}");

        let too_many: Vec<String> = (0..11).map(|i| format!("t{i}")).collect();
        let payload = serde_json::json!({
            "name": "C",
            "url": "http://127.0.0.1",
            "tags": too_many
        })
        .to_string();
        let (st, body) = send(&app, req("POST", "/api/console/apps", Some(&payload))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(body.contains("tag_limit_exceeded"), "{body}");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn metrics_exposes_apps_and_component() {
        let path = tmp_file("metrics");
        let _ = std::fs::remove_file(&path);
        let catalog = Arc::new(Catalog::open(&path).unwrap());
        let metrics = SelfMetrics::new("console", "0.0.0.0:7200").unwrap();
        metrics.set_hook(Arc::new(ConsoleScrapeHook {
            catalog: catalog.clone(),
        }));
        let app = metrics.metrics_router();
        let (st, body) = send(&app, req("GET", "/metrics", None)).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(body.contains("vectorman_console_apps"), "{body}");
        assert!(body.contains("component=\"console\""), "{body}");
        let _ = std::fs::remove_file(&path);
    }
}
