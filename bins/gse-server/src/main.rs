use std::process::ExitCode;

use gse_server_core::{load_config, Server};

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
            println!("  GSE_SERVER_CONFIG   Path to gse-server.toml (default: ./gse-server.toml)");
        }
        _ => println!("{bin} {}", vectorman_version::VERSION),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    if let Some(flag) = info_flag(std::env::args().skip(1)) {
        print_info("gse-server", "Vectorman GSE Server", flag);
        return ExitCode::SUCCESS;
    }
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
