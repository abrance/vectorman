use std::process::ExitCode;

use gse_agent_core::{load_config, run};

fn info_flag(args: impl IntoIterator<Item = impl AsRef<str>>) -> Option<&'static str> {
    let mut help = false;
    let mut version = false;
    for arg in args {
        match arg.as_ref() {
            "-h" | "--help" => help = true,
            "-V" | "--version" => version = true,
            _ => {}
        }
    }
    if help {
        Some("help")
    } else if version {
        Some("version")
    } else {
        None
    }
}

fn print_info(bin: &str, about: &str, flag: &str) {
    match flag {
        "help" => {
            println!("{bin} {}", vectorman_version::VERSION);
            println!("{about}");
            println!();
            println!("Usage: {bin} [OPTIONS]");
            println!();
            println!("Options:");
            println!("  -h, --help     Print help");
            println!("  -V, --version  Print version");
            println!();
            println!("Configuration:");
            println!("  GSE_AGENT_CONFIG   Path to gse-agent.toml (default: ./gse-agent.toml)");
        }
        _ => println!("{bin} {}", vectorman_version::VERSION),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    if let Some(flag) = info_flag(std::env::args().skip(1)) {
        print_info("gse-agent", "Vectorman GSE Agent", flag);
        return ExitCode::SUCCESS;
    }
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

#[cfg(test)]
mod tests {
    use super::info_flag;

    #[test]
    fn help_does_not_need_config() {
        assert_eq!(info_flag(["--help"]), Some("help"));
        assert_eq!(info_flag(["-h"]), Some("help"));
        assert_eq!(info_flag(["--help", "--version"]), Some("help"));
    }

    #[test]
    fn version_does_not_need_config() {
        assert_eq!(info_flag(["--version"]), Some("version"));
        assert_eq!(info_flag(["-V"]), Some("version"));
    }

    #[test]
    fn other_args_fall_through_to_config() {
        assert_eq!(info_flag(["--config", "x.toml"]), None);
        assert_eq!(info_flag(Vec::<&str>::new()), None);
    }
}
