use std::process::ExitCode;

use gse_server_core::{load_config, Server};

#[tokio::main]
async fn main() -> ExitCode {
    let cfg_path =
        std::env::var("GSE_SERVER_CONFIG").unwrap_or_else(|_| "gse-server.toml".to_string());
    let cfg = match load_config(&cfg_path) {
        Ok(c) => c,
        Err(reason) => {
            eprintln!("gse-server: config_invalid path={cfg_path} reason={reason}");
            return ExitCode::FAILURE;
        }
    };
    let (server, addr) = match Server::bind(cfg).await {
        Ok(x) => x,
        Err(e) => {
            eprintln!("gse-server: bind failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("gse-server: listening on {addr}, config enabled");
    match server.run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gse-server: runtime error: {e}");
            ExitCode::FAILURE
        }
    }
}
