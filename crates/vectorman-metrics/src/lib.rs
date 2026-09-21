//! Shared Prometheus self-metrics for vectorman server processes.

mod process;
mod text;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::extract::{MatchedPath, State};
use axum::http::{header, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use prometheus::{
    Counter, Encoder, Gauge, HistogramOpts, HistogramVec, IntCounterVec, Opts, Registry,
    TextEncoder,
};

use crate::process::ProcessSampler;

pub use text::MetricSample;

const HTTP_BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Called before encoding so components can refresh domain gauges.
#[async_trait]
pub trait ScrapeHook: Send + Sync {
    async fn on_scrape(&self, metrics: &SelfMetrics);
}

/// Destination for dataserver self-metric snapshots.
#[async_trait]
pub trait MetricsSink: Send + Sync {
    async fn write(&self, samples: &[MetricSample], timestamp_micros: i64) -> Result<(), String>;
}

/// Per-process Prometheus registry plus HTTP helpers.
pub struct SelfMetrics {
    registry: Registry,
    cpu: Counter,
    rss: Gauge,
    uptime: Gauge,
    http_requests: IntCounterVec,
    http_duration: HistogramVec,
    gauges: Mutex<HashMap<String, Gauge>>,
    counters: Mutex<HashMap<String, Counter>>,
    sampler: ProcessSampler,
    hook: RwLock<Option<Arc<dyn ScrapeHook>>>,
}

impl SelfMetrics {
    pub fn new(component: &str, instance: &str) -> Result<Arc<Self>, String> {
        let mut labels = HashMap::new();
        labels.insert("component".to_string(), component.to_string());
        labels.insert("instance".to_string(), instance.to_string());
        labels.insert("data_id".to_string(), "self".to_string());
        let registry = Registry::new_custom(None, Some(labels)).map_err(|e| e.to_string())?;

        let cpu = Counter::new(
            "vectorman_process_cpu_seconds_total",
            "Process CPU time in seconds",
        )
        .map_err(|e| e.to_string())?;
        let rss = Gauge::new(
            "vectorman_process_resident_memory_bytes",
            "Process resident memory in bytes",
        )
        .map_err(|e| e.to_string())?;
        let uptime = Gauge::new(
            "vectorman_process_uptime_seconds",
            "Process uptime in seconds",
        )
        .map_err(|e| e.to_string())?;
        let http_requests = IntCounterVec::new(
            Opts::new("vectorman_http_requests_total", "HTTP requests"),
            &["method", "path", "status"],
        )
        .map_err(|e| e.to_string())?;
        let http_duration = HistogramVec::new(
            HistogramOpts::new(
                "vectorman_http_request_duration_seconds",
                "HTTP request duration in seconds",
            )
            .buckets(HTTP_BUCKETS.to_vec()),
            &["method", "path"],
        )
        .map_err(|e| e.to_string())?;

        registry
            .register(Box::new(cpu.clone()))
            .map_err(|e| e.to_string())?;
        registry
            .register(Box::new(rss.clone()))
            .map_err(|e| e.to_string())?;
        registry
            .register(Box::new(uptime.clone()))
            .map_err(|e| e.to_string())?;
        registry
            .register(Box::new(http_requests.clone()))
            .map_err(|e| e.to_string())?;
        registry
            .register(Box::new(http_duration.clone()))
            .map_err(|e| e.to_string())?;

        Ok(Arc::new(Self {
            registry,
            cpu,
            rss,
            uptime,
            http_requests,
            http_duration,
            gauges: Mutex::new(HashMap::new()),
            counters: Mutex::new(HashMap::new()),
            sampler: ProcessSampler::new(),
            hook: RwLock::new(None),
        }))
    }

    pub fn set_hook(&self, hook: Arc<dyn ScrapeHook>) {
        *self.hook.write().unwrap() = Some(hook);
    }

    pub fn set_gauge(&self, name: &str, value: f64) {
        let mut map = self.gauges.lock().unwrap();
        if let Some(g) = map.get(name) {
            g.set(value);
            return;
        }
        let Ok(g) = Gauge::new(name, name) else {
            return;
        };
        if self.registry.register(Box::new(g.clone())).is_err() {
            return;
        }
        g.set(value);
        map.insert(name.to_string(), g);
    }

    pub fn inc_counter(&self, name: &str, by: f64) {
        if by <= 0.0 {
            return;
        }
        let mut map = self.counters.lock().unwrap();
        if let Some(c) = map.get(name) {
            c.inc_by(by);
            return;
        }
        let Ok(c) = Counter::new(name, name) else {
            return;
        };
        if self.registry.register(Box::new(c.clone())).is_err() {
            return;
        }
        c.inc_by(by);
        map.insert(name.to_string(), c);
    }

    pub fn observe_http(&self, method: &str, path: &str, status: &str, seconds: f64) {
        self.http_requests
            .with_label_values(&[method, path, status])
            .inc();
        self.http_duration
            .with_label_values(&[method, path])
            .observe(seconds);
    }

    fn refresh_process(&self) {
        let sample = self.sampler.sample();
        self.uptime.set(sample.uptime_secs);
        if let Some(rss) = sample.rss_bytes {
            self.rss.set(rss);
        }
        if let Some(cpu) = sample.cpu_secs {
            if cpu > 0.0 {
                self.cpu.inc_by(cpu);
            }
        }
    }

    fn encode_text(&self) -> Result<String, String> {
        let families = self.registry.gather();
        let encoder = TextEncoder::new();
        let mut buf = Vec::new();
        encoder
            .encode(&families, &mut buf)
            .map_err(|e| e.to_string())?;
        String::from_utf8(buf).map_err(|e| e.to_string())
    }

    pub async fn render(&self) -> Result<String, String> {
        self.refresh_process();
        let hook = self.hook.read().unwrap().clone();
        if let Some(h) = hook {
            h.on_scrape(self).await;
        }
        self.encode_text()
    }

    pub async fn snapshot(&self) -> Result<Vec<MetricSample>, String> {
        let text = self.render().await?;
        text::parse_prom_text(&text)
    }

    pub fn metrics_router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/metrics", get(handle_metrics))
            .with_state(self)
    }

    pub async fn serve(self: Arc<Self>, listen: &str) -> Result<(), String> {
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .map_err(|e| format!("bind {listen}: {e}"))?;
        axum::serve(listener, self.metrics_router())
            .await
            .map_err(|e| e.to_string())
    }
}

/// Wrap a business router with HTTP request metrics. Metrics port must not use this.
pub fn apply_http_metrics<S>(router: Router<S>, metrics: Option<Arc<SelfMetrics>>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    match metrics {
        Some(m) => router.layer(middleware::from_fn_with_state(m, track_http)),
        None => router,
    }
}

pub async fn track_http(
    State(metrics): State<Arc<SelfMetrics>>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let method = req.method().as_str().to_string();
    let path = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "unmatched".to_string());
    let start = std::time::Instant::now();
    let resp = next.run(req).await;
    let status = resp.status().as_u16().to_string();
    metrics.observe_http(&method, &path, &status, start.elapsed().as_secs_f64());
    resp
}

async fn handle_metrics(State(metrics): State<Arc<SelfMetrics>>) -> Response {
    match metrics.render().await {
        Ok(text) => (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            text,
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// Periodically snapshot and write. Logs write failures and keeps going.
pub async fn flush_loop(metrics: Arc<SelfMetrics>, sink: Arc<dyn MetricsSink>, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        match metrics.snapshot().await {
            Ok(samples) => {
                if let Err(e) = sink.write(&samples, now_micros()).await {
                    eprintln!("self-metrics flush: {e}");
                }
            }
            Err(e) => eprintln!("self-metrics snapshot: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    async fn send(app: &Router, req: Request<Body>) -> (StatusCode, String, Option<String>) {
        let resp = app.clone().oneshot(req).await.expect("oneshot");
        let status = resp.status();
        let ctype = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("collect")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned(), ctype)
    }

    #[tokio::test]
    async fn encode_contains_uptime_and_component() {
        let metrics = SelfMetrics::new("dataserver", "0.0.0.0:8081").unwrap();
        let text = metrics.render().await.unwrap();
        assert!(text.contains("vectorman_process_uptime_seconds"), "{text}");
        assert!(text.contains("component=\"dataserver\""), "{text}");
        assert!(text.contains("data_id=\"self\""), "{text}");
        assert!(text.contains("instance=\"0.0.0.0:8081\""), "{text}");
    }

    #[tokio::test]
    async fn http_middleware_increments_counter() {
        let metrics = SelfMetrics::new("dataserver", "127.0.0.1:8081").unwrap();
        let app = apply_http_metrics(
            Router::new().route("/health", get(|| async { "ok" })),
            Some(metrics.clone()),
        );
        let (st, body, _) = send(
            &app,
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");

        let text = metrics.render().await.unwrap();
        assert!(text.contains("vectorman_http_requests_total"), "{text}");
        assert!(text.contains("path=\"/health\""), "{text}");
        assert!(text.contains("method=\"GET\""), "{text}");
        assert!(text.contains("status=\"200\""), "{text}");
    }

    #[tokio::test]
    async fn snapshot_tags_match_text_and_histogram_has_bucket() {
        let metrics = SelfMetrics::new("gse-server", "127.0.0.1:7101").unwrap();
        let app = apply_http_metrics(
            Router::new().route("/health", get(|| async { "ok" })),
            Some(metrics.clone()),
        );
        let _ = send(
            &app,
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        let text = metrics.render().await.unwrap();
        let samples = metrics.snapshot().await.unwrap();
        let uptime = samples
            .iter()
            .find(|s| s.name == "vectorman_process_uptime_seconds")
            .expect("uptime sample");
        assert_eq!(uptime.tags.get("component").unwrap(), "gse-server");
        assert_eq!(uptime.tags.get("instance").unwrap(), "127.0.0.1:7101");
        assert_eq!(uptime.tags.get("data_id").unwrap(), "self");
        assert!(text.contains("vectorman_process_uptime_seconds"));
        assert!(text.contains("component=\"gse-server\""));

        assert!(samples.iter().any(|s| {
            s.name == "vectorman_http_request_duration_seconds_bucket" && s.tags.contains_key("le")
        }));
        assert!(text.contains("vectorman_http_request_duration_seconds_bucket"));
    }

    #[tokio::test]
    async fn metrics_route_skips_http_counter() {
        let metrics = SelfMetrics::new("console", "0.0.0.0:7200").unwrap();
        let app = metrics.clone().metrics_router();
        let (st, body, ctype) = send(
            &app,
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(
            ctype.as_deref(),
            Some("text/plain; version=0.0.4; charset=utf-8")
        );
        assert!(body.contains("vectorman_process_uptime_seconds"), "{body}");
        assert!(!body.contains("vectorman_http_requests_total{"), "{body}");
    }
}
