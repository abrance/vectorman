//! `dpc` 命令行入口：实现全在 `lib.rs`（与 `vmctl data` 共用）。

use clap::Parser;
use dpc::{Cli, Endpoints};
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let endpoints = Endpoints::new(cli.sql_url.clone(), cli.prom_url.clone());
    dpc::dispatch(&endpoints, &cli.command)
}
