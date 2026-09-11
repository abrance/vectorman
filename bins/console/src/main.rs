use std::process::ExitCode;

use console::{load_config, serve, Catalog};

#[tokio::main]
async fn main() -> ExitCode {
    let cfg_path = std::env::var("CONSOLE_CONFIG").unwrap_or_else(|_| "console.toml".to_string());
    let cfg = match load_config(&cfg_path) {
        Ok(c) => c,
        Err(reason) => {
            eprintln!("console: config_invalid path={cfg_path} reason={reason}");
            return ExitCode::FAILURE;
        }
    };
    let catalog = match Catalog::open(&cfg.data_file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("console: catalog_invalid path={} reason={e}", cfg.data_file);
            return ExitCode::FAILURE;
        }
    };
    match serve(cfg, catalog).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("console: runtime error: {e}");
            ExitCode::FAILURE
        }
    }
}
