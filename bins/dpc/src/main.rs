use std::process::ExitCode;

use clap::{Parser, Subcommand};
use dataplane_core::ErrorCode;

#[derive(Parser)]
#[command(name = "dpc", about = "dataplane 运维命令行：通过 HTTP 访问 apiserver")]
struct Cli {
    /// SQL HTTP 端口基址
    #[arg(long, default_value = "http://127.0.0.1:8081")]
    sql_url: String,

    /// Prometheus 查询端口基址
    #[arg(long, default_value = "http://127.0.0.1:9090")]
    prom_url: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 分别探测 SQL 与 Prom 两个端口的 /health
    Health,
    /// 向 SQL HTTP 发送一条语句
    Sql {
        #[arg(long)]
        stmt: String,
    },
    /// 向 Prometheus 查询 HTTP 发送即时查询
    Query {
        #[arg(long)]
        expr: String,
        /// 查询时刻（Unix 秒，可选）
        #[arg(long)]
        time: Option<String>,
    },
}

#[derive(Debug)]
struct DpcError {
    url: String,
    reason: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        eprintln!(
            "url={} reason={} code={}",
            e.url,
            e.reason,
            ErrorCode::Unavailable.as_str()
        );
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn run(cli: &Cli) -> Result<(), DpcError> {
    match &cli.command {
        Command::Health => cmd_health(cli),
        Command::Sql { stmt } => cmd_sql(&cli.sql_url, stmt),
        Command::Query { expr, time } => cmd_query(&cli.prom_url, expr, time.as_deref()),
    }
}

fn fetch_health(base: &str) -> Result<String, DpcError> {
    let url = format!("{base}/health");
    let resp = ureq::get(&url).call().map_err(|e| DpcError {
        url: url.clone(),
        reason: ureq_err_str(e),
    })?;
    let text = resp.into_string().map_err(|e| DpcError {
        url: url.clone(),
        reason: format!("read body: {e}"),
    })?;
    let status = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(str::to_string))
        .unwrap_or_default();
    if status != "ok" {
        return Err(DpcError {
            url,
            reason: format!("apiserver unhealthy: {text}"),
        });
    }
    Ok(text)
}

fn cmd_health(cli: &Cli) -> Result<(), DpcError> {
    let sql_url = cli.sql_url.clone();
    let prom_url = cli.prom_url.clone();
    let t1 = std::thread::spawn(move || fetch_health(&sql_url));
    let t2 = std::thread::spawn(move || fetch_health(&prom_url));
    let r1 = t1.join().unwrap_or_else(|_| {
        Err(DpcError {
            url: cli.sql_url.clone(),
            reason: "health thread panicked".to_string(),
        })
    });
    let r2 = t2.join().unwrap_or_else(|_| {
        Err(DpcError {
            url: cli.prom_url.clone(),
            reason: "health thread panicked".to_string(),
        })
    });
    match (r1, r2) {
        (Ok(a), Ok(b)) => {
            println!("sql_http: {a}");
            println!("prom_http: {b}");
            Ok(())
        }
        (Err(e), _) => Err(e),
        (_, Err(e)) => Err(e),
    }
}

fn cmd_sql(base: &str, stmt: &str) -> Result<(), DpcError> {
    let url = format!("{base}/v1/sql");
    let body = serde_json::json!({"sql": stmt, "params": []}).to_string();
    let resp = ureq::post(&url)
        .set("Content-Type", "application/json")
        .send_string(&body)
        .map_err(|e| DpcError {
            url: url.clone(),
            reason: ureq_err_str(e),
        })?;
    let text = resp.into_string().map_err(|e| DpcError {
        url,
        reason: format!("read body: {e}"),
    })?;
    println!("{text}");
    Ok(())
}

fn cmd_query(base: &str, expr: &str, time: Option<&str>) -> Result<(), DpcError> {
    let mut url = format!("{base}/api/v1/query?query={}", urlencoding::encode(expr));
    if let Some(t) = time {
        url.push_str(&format!("&time={}", urlencoding::encode(t)));
    }
    let resp = ureq::get(&url).call().map_err(|e| DpcError {
        url: url.clone(),
        reason: ureq_err_str(e),
    })?;
    let text = resp.into_string().map_err(|e| DpcError {
        url,
        reason: format!("read body: {e}"),
    })?;
    println!("{text}");
    Ok(())
}

fn ureq_err_str(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            format!("http status {code}: {body}")
        }
        ureq::Error::Transport(t) => t.to_string(),
    }
}
