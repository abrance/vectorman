use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use dataplane_core::{load_config, resolve_data_paths, DataplaneError, ErrorCode, NoopAuth};
use dataplane_file::{DirFileStore, FileStore};
use dataplane_kv::{KvStore, RedbKvStore};
use dataplane_log::{LogStore, TantivyLogStore};
use dataplane_sql::{RelationalStore, SqliteRelationalStore};
use dataplane_ts::{TimeSeriesStore, TsinkTimeSeriesStore};
use dataserver::cleanup::{now_micros, run_cleanup};
use dataserver::http::{prom_router, sql_router, AppState};

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
        gse_admin_url: cfg.gse_admin_url.clone(),
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

    let cleanup_state = state.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(3600));
        loop {
            ticker.tick().await;
            if let Err(e) = run_cleanup(
                cleanup_state.log.as_ref(),
                cleanup_state.kv.as_ref(),
                cleanup_state.gse_admin_url.as_deref(),
                now_micros(),
            )
            .await
            {
                eprintln!("retention cleanup: {}: {}", e.code.as_str(), e.message);
            }
        }
    });

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

    println!(
        "sql_http={} prom_http={}",
        cfg.sql_http.listen, cfg.prom_http.listen
    );

    let sql_fut = axum::serve(sql_listener, sql_app);
    let prom_fut = axum::serve(prom_listener, prom_app);
    let _ = tokio::try_join!(sql_fut, prom_fut);
    ExitCode::SUCCESS
}
