use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use dataplane_core::{load_config, resolve_data_paths, DataplaneError, ErrorCode, NoopAuth};
use dataplane_file::{DirFileStore, FileStore};
use dataplane_kv::{KvStore, RedbKvStore};
use dataplane_log::{LogStore, TantivyLogStore};
use dataplane_sql::{RelationalStore, SqliteRelationalStore};
use dataplane_ts::{TimeSeriesStore, TsRetentionConfig, TsinkTimeSeriesStore};
use dataserver::cleanup::{now_micros, run_cleanup, TsCleanTracker};
use dataserver::http::{prom_router, sql_router, AppState, LocalTsSink};
use vectorman_metrics::SelfMetrics;

const ENGINE_DIR_MODE_REQUIRED: &str = "dataserver requires a directory data_path (single-file mode only supports sqlite, and this server enables all engines)";

#[derive(Parser)]
#[command(
    name = "dataserver",
    version = vectorman_version::VERSION,
    about = "Vectorman Dataserver (SQL/Prom HTTP)"
)]
struct Args {
    /// 配置文件路径；缺省时读取 ./config.toml，若不存在则使用内置默认值。
    #[arg(long)]
    config: Option<String>,
}

fn exit_with(prefix: &str, e: DataplaneError) -> ExitCode {
    eprintln!("{prefix}: {}: {}", e.code.as_str(), e.message);
    ExitCode::FAILURE
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
        return exit_with(
            "data_path error",
            DataplaneError::new(ErrorCode::ConfigInvalid, ENGINE_DIR_MODE_REQUIRED),
        );
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
    if let Err(e) = dataplane_apm::bootstrap(sql.as_ref()).await {
        return exit_with("apm schema init failed", e);
    }

    let ts_retention = TsRetentionConfig {
        retention_days: cfg.ts_retention_days,
        enforced: cfg.ts_retention_enforced,
        cardinality_limit: cfg.ts_cardinality_limit,
        memory_limit_bytes: cfg.ts_memory_limit_bytes,
        wal_size_limit_bytes: cfg.ts_wal_size_limit_bytes,
    };
    let ts: Arc<dyn TimeSeriesStore> = match TsinkTimeSeriesStore::new(&paths.ts, ts_retention) {
        Ok(s) => Arc::new(s),
        Err(e) => return exit_with("engine ts init failed", e),
    };
    let log: Arc<dyn LogStore> = match TantivyLogStore::new(&paths.logs) {
        Ok(s) => Arc::new(s),
        Err(e) => return exit_with("engine log init failed", e),
    };

    // APM 派生数据：摘要累加器 + 服务端点半。`apm_enabled=false` 时不创建，
    // 接入路径退回无钩子的 `apply`（既有行为）。
    let apm = if cfg.apm_enabled {
        let sink = Arc::new(dataplane_apm::ApmSink::new(
            sql.clone(),
            dataplane_apm::ApmSinkConfig {
                endpoint_retention_days: cfg.apm_endpoint_retention_days,
                ..dataplane_apm::ApmSinkConfig::default()
            },
        ));
        match sink.reload(now_micros()).await {
            Ok(n) if n > 0 => println!("apm: reloaded {n} live traces"),
            Ok(_) => {}
            Err(e) => eprintln!("apm: reload failed: {}: {}", e.code.as_str(), e.message),
        }
        Some(sink)
    } else {
        None
    };

    let metrics = match SelfMetrics::new("dataserver", &cfg.sql_http.listen) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("metrics init failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let state = AppState {
        file,
        kv,
        sql,
        ts,
        log,
        auth: Arc::new(NoopAuth),
        gse_admin_url: cfg.gse_admin_url.clone(),
        metrics: Some(metrics.clone()),
        apm: apm.clone(),
    };

    let web_dir = cfg.http_web_dir.as_deref().map(std::path::Path::new);
    if let Some(dir) = cfg.http_web_dir.as_deref() {
        if std::path::Path::new(dir).join("index.html").is_file() {
            println!("dataserver: serving web dist from {dir}");
        } else {
            eprintln!("dataserver: http_web_dir {dir} missing index.html; static UI disabled");
        }
    }

    let sql_app = sql_router(state.clone(), web_dir);
    let prom_app = prom_router(state.clone());
    let metrics_app = metrics.clone().metrics_router();

    let cleanup_state = state.clone();
    let cleanup_metrics = metrics.clone();
    let global_ts_days = cfg.ts_retention_days;
    let clean_interval = cfg.ts_clean_interval_secs.max(60);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(clean_interval));
        let mut ts_tracker = TsCleanTracker::default();
        loop {
            ticker.tick().await;
            match run_cleanup(
                cleanup_state.log.as_ref(),
                cleanup_state.ts.as_ref(),
                cleanup_state.kv.as_ref(),
                cleanup_state.gse_admin_url.as_deref(),
                global_ts_days,
                now_micros(),
                &mut ts_tracker,
            )
            .await
            {
                Ok(report) => {
                    cleanup_metrics.inc_counter("dataserver_ts_clean_runs_total", 1.0);
                    if report.ts.tombstones_applied > 0 {
                        cleanup_metrics.inc_counter(
                            "dataserver_ts_tombstones_applied_total",
                            report.ts.tombstones_applied as f64,
                        );
                    }
                    println!(
                        "retention cleanup: log_deleted={} ts_items={} ts_matched_series={} ts_tombstones={}",
                        report.log_deleted,
                        report.ts.items,
                        report.ts.matched_series,
                        report.ts.tombstones_applied
                    );
                }
                Err(e) => {
                    cleanup_metrics.inc_counter("dataserver_ts_clean_errors_total", 1.0);
                    eprintln!("retention cleanup: {}: {}", e.code.as_str(), e.message);
                }
            }
        }
    });

    if let Some(sink) = apm.clone() {
        let flush_metrics = metrics.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            loop {
                ticker.tick().await;
                match sink.flush_due(now_micros()).await {
                    Ok(report) if report.traces > 0 || report.endpoints > 0 || report.edges > 0 => {
                        flush_metrics.inc_counter(
                            "dataserver_apm_trace_summaries_flushed_total",
                            report.traces as f64,
                        );
                        flush_metrics.inc_counter(
                            "dataserver_apm_endpoints_flushed_total",
                            report.endpoints as f64,
                        );
                        flush_metrics
                            .inc_counter("dataserver_apm_edges_flushed_total", report.edges as f64);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        flush_metrics.inc_counter("dataserver_apm_flush_errors_total", 1.0);
                        eprintln!("apm: flush failed: {}: {}", e.code.as_str(), e.message);
                    }
                }
                flush_metrics.set_gauge("dataserver_apm_live_traces", sink.live_traces() as f64);
                flush_metrics.set_gauge("dataserver_apm_dirty_traces", sink.dirty_traces() as f64);
                flush_metrics.set_gauge("dataserver_apm_paired_edges", sink.paired_edges() as f64);
                flush_metrics
                    .set_gauge("dataserver_apm_pending_spans", sink.pending_spans() as f64);
            }
        });
    }

    if cfg.self_metrics_interval_secs > 0 {
        let sink: Arc<dyn vectorman_metrics::MetricsSink> = Arc::new(LocalTsSink {
            ts: state.ts.clone(),
        });
        let flush_metrics = metrics.clone();
        let interval = Duration::from_secs(cfg.self_metrics_interval_secs);
        tokio::spawn(async move {
            vectorman_metrics::flush_loop(flush_metrics, sink, interval).await;
        });

        // 时序存储状态采样：`list_metrics` 昂贵，因此跟着自监控周期跑，不进写入路径。
        let stats_state = state.clone();
        let stats_metrics = metrics.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                let Ok(stats) = stats_state.ts.storage_stats().await else {
                    continue;
                };
                stats_metrics.set_gauge("dataserver_ts_series_count", stats.series_count as f64);
                stats_metrics.set_gauge(
                    "dataserver_ts_memory_used_bytes",
                    stats.memory_used_bytes as f64,
                );
                stats_metrics.set_gauge(
                    "dataserver_ts_memory_budget_bytes",
                    stats.memory_budget_bytes as f64,
                );
                stats_metrics
                    .set_gauge("dataserver_ts_wal_size_bytes", stats.wal_size_bytes as f64);
                stats_metrics.set_gauge(
                    "dataserver_ts_degraded",
                    if stats.degraded { 1.0 } else { 0.0 },
                );
            }
        });
    }

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
    let metrics_listener = match tokio::net::TcpListener::bind(&cfg.metrics_http.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bind metrics_http {} failed: {e}", cfg.metrics_http.listen);
            return ExitCode::FAILURE;
        }
    };

    println!(
        "sql_http={} prom_http={} metrics_http={} ts_retention_days={} ts_retention_enforced={}",
        cfg.sql_http.listen,
        cfg.prom_http.listen,
        cfg.metrics_http.listen,
        cfg.ts_retention_days,
        cfg.ts_retention_enforced
    );

    let sql_fut = axum::serve(sql_listener, sql_app);
    let prom_fut = axum::serve(prom_listener, prom_app);
    let metrics_fut = axum::serve(metrics_listener, metrics_app);
    let _ = tokio::try_join!(sql_fut, prom_fut, metrics_fut);
    ExitCode::SUCCESS
}
