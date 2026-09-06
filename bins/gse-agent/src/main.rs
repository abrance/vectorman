use std::process::ExitCode;

use gse_agent_core::{load_config, run};

#[tokio::main]
async fn main() -> ExitCode {
    let cfg_path =
        std::env::var("GSE_AGENT_CONFIG").unwrap_or_else(|_| "gse-agent.toml".to_string());
    let cfg = match load_config(&cfg_path) {
        Ok(c) => c,
        Err(reason) => {
            eprintln!("gse-agent: config_invalid path={cfg_path} reason={reason}");
            return ExitCode::FAILURE;
        }
    };
    match run(cfg).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gse-agent: {e}");
            ExitCode::FAILURE
        }
    }
}
