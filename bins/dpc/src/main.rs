use std::process::ExitCode;

use clap::{Parser, Subcommand};
use dataplane_core::ErrorCode;
use serde_json::json;

#[derive(Parser)]
#[command(
    name = "dpc",
    version = vectorman_version::VERSION,
    about = "dataplane 运维命令行：通过 HTTP 访问 dataserver"
)]
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
    /// 向 dataserver 检索日志
    Logs {
        /// 数据类型：logs / apm / ebpf
        #[arg(long)]
        data_type: Option<String>,
        /// 按 Agent 过滤
        #[arg(long)]
        agent_id: Option<String>,
        /// 按采集项（data_id）过滤
        #[arg(long)]
        data_id: Option<String>,
        /// 按日志级别过滤
        #[arg(long)]
        level: Option<String>,
        /// 正文关键词
        #[arg(long)]
        query: Option<String>,
        /// 起始时间（Unix 微秒）
        #[arg(long)]
        from_ts: Option<i64>,
        /// 结束时间（Unix 微秒）
        #[arg(long)]
        to_ts: Option<i64>,
        /// 最大返回条数
        #[arg(long)]
        limit: Option<usize>,
    },
    /// 时序存储运行状态（保留窗口、基数、内存、WAL）
    Ts {
        #[command(subcommand)]
        cmd: TsCommand,
    },
    /// 检索 trace 列表，`POST /v1/traces/search`
    Traces {
        /// 根服务名
        #[arg(long)]
        service: Option<String>,
        /// 根操作名
        #[arg(long)]
        operation: Option<String>,
        /// 状态：ok / error
        #[arg(long)]
        status: Option<String>,
        /// 最小耗时（毫秒）
        #[arg(long)]
        min_duration_ms: Option<i64>,
        /// 按 Agent 过滤
        #[arg(long)]
        agent_id: Option<String>,
        /// 按采集项（data_id）过滤
        #[arg(long)]
        data_id: Option<String>,
        /// 排序字段：start_ts / duration_micros
        #[arg(long)]
        sort: Option<String>,
        /// 排序方向：asc / desc
        #[arg(long)]
        order: Option<String>,
        /// 起始时间（Unix 微秒）
        #[arg(long)]
        from_ts: Option<i64>,
        /// 结束时间（Unix 微秒）
        #[arg(long)]
        to_ts: Option<i64>,
        /// 最大返回条数
        #[arg(long)]
        limit: Option<usize>,
    },
    /// 查看单个 trace 的摘要与 span，`GET /v1/traces/{trace_id}`
    Trace {
        /// 32 位 hex 的 trace_id
        trace_id: String,
    },
    /// 检索服务拓扑边，`POST /v1/edges/search`
    Edges {
        /// 源服务
        #[arg(long)]
        src: Option<String>,
        /// 目标服务
        #[arg(long)]
        dst: Option<String>,
        /// 数据来源：otlp / ebpf
        #[arg(long)]
        source: Option<String>,
        /// 最小调用次数
        #[arg(long)]
        min_requests: Option<i64>,
        /// 起始时间（Unix 微秒）
        #[arg(long)]
        from_ts: Option<i64>,
        /// 结束时间（Unix 微秒）
        #[arg(long)]
        to_ts: Option<i64>,
        /// 最大返回条数
        #[arg(long)]
        limit: Option<usize>,
    },
}

#[derive(Subcommand)]
enum TsCommand {
    /// 查询 `GET /v1/ts/stats`
    Stats,
    /// 按序列选择删除历史点，`POST /v1/ts/delete`
    Delete {
        /// 指标名（measurement）；省略表示不限
        #[arg(long)]
        metric: Option<String>,
        /// label matcher，形如 `k=v`，可重复；`!=` 表示不等
        #[arg(long = "matcher", value_name = "K=V")]
        matchers: Vec<String>,
        /// 时间范围起点（Unix 微秒，含）
        #[arg(long)]
        from_ts: i64,
        /// 时间范围终点（Unix 微秒，不含）
        #[arg(long)]
        to_ts: i64,
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
        Command::Logs {
            data_type,
            agent_id,
            data_id,
            level,
            query,
            from_ts,
            to_ts,
            limit,
        } => cmd_logs(
            &cli.sql_url,
            logs_body(
                data_type.as_deref(),
                agent_id.as_deref(),
                data_id.as_deref(),
                level.as_deref(),
                query.as_deref(),
                *from_ts,
                *to_ts,
                *limit,
            ),
        ),
        Command::Traces {
            service,
            operation,
            status,
            min_duration_ms,
            agent_id,
            data_id,
            sort,
            order,
            from_ts,
            to_ts,
            limit,
        } => cmd_post(
            &cli.sql_url,
            "/v1/traces/search",
            traces_body(
                service.as_deref(),
                operation.as_deref(),
                status.as_deref(),
                *min_duration_ms,
                agent_id.as_deref(),
                data_id.as_deref(),
                sort.as_deref(),
                order.as_deref(),
                *from_ts,
                *to_ts,
                *limit,
            ),
        ),
        Command::Trace { trace_id } => cmd_get(&cli.sql_url, &format!("/v1/traces/{trace_id}")),
        Command::Edges {
            src,
            dst,
            source,
            min_requests,
            from_ts,
            to_ts,
            limit,
        } => cmd_post(
            &cli.sql_url,
            "/v1/edges/search",
            edges_body(
                src.as_deref(),
                dst.as_deref(),
                source.as_deref(),
                *min_requests,
                *from_ts,
                *to_ts,
                *limit,
            ),
        ),
        Command::Ts { cmd } => match cmd {
            TsCommand::Stats => cmd_ts_stats(&cli.sql_url),
            TsCommand::Delete {
                metric,
                matchers,
                from_ts,
                to_ts,
            } => cmd_ts_delete(
                &cli.sql_url,
                ts_delete_body(metric.as_deref(), matchers, *from_ts, *to_ts),
            ),
        },
    }
}

/// 拼 `/v1/ts/delete` 请求体。matcher 支持 `k=v` 与 `k!=v`。
fn ts_delete_body(
    metric: Option<&str>,
    matchers: &[String],
    from_ts: i64,
    to_ts: i64,
) -> Result<serde_json::Value, DpcError> {
    let mut list = Vec::new();
    for raw in matchers {
        let (name, op, value) = if let Some((k, v)) = raw.split_once("!=") {
            (k, "not_equal", v)
        } else if let Some((k, v)) = raw.split_once('=') {
            (k, "equal", v)
        } else {
            return Err(DpcError {
                url: String::new(),
                reason: format!("matcher must be K=V or K!=V: {raw}"),
            });
        };
        if name.is_empty() || value.is_empty() {
            return Err(DpcError {
                url: String::new(),
                reason: format!("matcher name/value must not be empty: {raw}"),
            });
        }
        list.push(json!({ "name": name, "op": op, "value": value }));
    }
    let mut body = serde_json::Map::new();
    if let Some(m) = metric {
        body.insert("measurement".into(), m.into());
    }
    body.insert("matchers".into(), serde_json::Value::Array(list));
    body.insert("from_ts".into(), from_ts.into());
    body.insert("to_ts".into(), to_ts.into());
    Ok(serde_json::Value::Object(body))
}

/// trace 列表过滤条件拼成 `/v1/traces/search` 请求体；`--min-duration-ms` 转微秒。
#[allow(clippy::too_many_arguments)]
fn traces_body(
    service: Option<&str>,
    operation: Option<&str>,
    status: Option<&str>,
    min_duration_ms: Option<i64>,
    agent_id: Option<&str>,
    data_id: Option<&str>,
    sort: Option<&str>,
    order: Option<&str>,
    from_ts: Option<i64>,
    to_ts: Option<i64>,
    limit: Option<usize>,
) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    for (key, value) in [
        ("service", service),
        ("operation", operation),
        ("status", status),
        ("agent_id", agent_id),
        ("data_id", data_id),
        ("sort", sort),
        ("order", order),
    ] {
        if let Some(value) = value {
            body.insert(key.into(), value.into());
        }
    }
    if let Some(ms) = min_duration_ms {
        body.insert(
            "min_duration_micros".into(),
            (ms.saturating_mul(1_000)).into(),
        );
    }
    if let Some(v) = from_ts {
        body.insert("from_ts".into(), v.into());
    }
    if let Some(v) = to_ts {
        body.insert("to_ts".into(), v.into());
    }
    if let Some(v) = limit {
        body.insert("limit".into(), v.into());
    }
    serde_json::Value::Object(body)
}

/// 边列表过滤条件拼成 `/v1/edges/search` 请求体。
#[allow(clippy::too_many_arguments)]
fn edges_body(
    src: Option<&str>,
    dst: Option<&str>,
    source: Option<&str>,
    min_requests: Option<i64>,
    from_ts: Option<i64>,
    to_ts: Option<i64>,
    limit: Option<usize>,
) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    for (key, value) in [
        ("src_service", src),
        ("dst_service", dst),
        ("source", source),
    ] {
        if let Some(value) = value {
            body.insert(key.into(), value.into());
        }
    }
    if let Some(v) = min_requests {
        body.insert("min_requests".into(), v.into());
    }
    if let Some(v) = from_ts {
        body.insert("from_ts".into(), v.into());
    }
    if let Some(v) = to_ts {
        body.insert("to_ts".into(), v.into());
    }
    if let Some(v) = limit {
        body.insert("limit".into(), v.into());
    }
    serde_json::Value::Object(body)
}

/// 向 SQL 口 POST JSON 并打印响应。
fn cmd_post(base: &str, path: &str, body: serde_json::Value) -> Result<(), DpcError> {
    let url = format!("{base}{path}");
    let resp = ureq::post(&url)
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
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

/// 向 SQL 口 GET 并打印响应。
fn cmd_get(base: &str, path: &str) -> Result<(), DpcError> {
    let url = format!("{base}{path}");
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

/// 过滤条件拼成 `/v1/logs/search` 请求体；未提供的字段不下发。
#[allow(clippy::too_many_arguments)]
fn logs_body(
    data_type: Option<&str>,
    agent_id: Option<&str>,
    data_id: Option<&str>,
    level: Option<&str>,
    query: Option<&str>,
    from_ts: Option<i64>,
    to_ts: Option<i64>,
    limit: Option<usize>,
) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    if let Some(v) = data_type {
        body.insert("data_type".into(), v.into());
    }
    if let Some(v) = agent_id {
        body.insert("agent_id".into(), v.into());
    }
    if let Some(v) = data_id {
        body.insert("data_id".into(), v.into());
    }
    if let Some(v) = level {
        body.insert("level".into(), v.into());
    }
    if let Some(v) = query {
        body.insert("message_query".into(), v.into());
    }
    if let Some(v) = from_ts {
        body.insert("from_ts".into(), v.into());
    }
    if let Some(v) = to_ts {
        body.insert("to_ts".into(), v.into());
    }
    if let Some(v) = limit {
        body.insert("limit".into(), v.into());
    }
    serde_json::Value::Object(body)
}

fn cmd_logs(base: &str, body: serde_json::Value) -> Result<(), DpcError> {
    let url = format!("{base}/v1/logs/search");
    let resp = ureq::post(&url)
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
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

fn cmd_ts_stats(base: &str) -> Result<(), DpcError> {
    let url = format!("{base}/v1/ts/stats");
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

fn cmd_ts_delete(base: &str, body: Result<serde_json::Value, DpcError>) -> Result<(), DpcError> {
    let body = body?;
    let url = format!("{base}/v1/ts/delete");
    let resp = ureq::post(&url)
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
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
            reason: format!("dataserver unhealthy: {text}"),
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

#[cfg(test)]
mod tests {
    use super::{edges_body, logs_body, traces_body, ts_delete_body};
    use serde_json::json;

    #[test]
    fn logs_body_keeps_only_provided_filters() {
        assert_eq!(
            logs_body(None, None, None, None, None, None, None, None),
            json!({})
        );
        assert_eq!(
            logs_body(
                Some("logs"),
                Some("agent-1"),
                None,
                None,
                Some("error"),
                None,
                None,
                Some(50)
            ),
            json!({"data_type": "logs", "agent_id": "agent-1", "message_query": "error", "limit": 50})
        );
    }

    #[test]
    fn traces_body_converts_min_duration_ms_and_keeps_only_provided_filters() {
        assert_eq!(
            traces_body(None, None, None, None, None, None, None, None, None, None, None),
            json!({})
        );
        assert_eq!(
            traces_body(
                Some("order-api"),
                Some("GET /orders"),
                Some("error"),
                Some(500),
                Some("agent-1"),
                Some("item-1"),
                Some("duration_micros"),
                Some("desc"),
                Some(10),
                Some(20),
                Some(5),
            ),
            json!({
                "service": "order-api",
                "operation": "GET /orders",
                "status": "error",
                "agent_id": "agent-1",
                "data_id": "item-1",
                "sort": "duration_micros",
                "order": "desc",
                "min_duration_micros": 500_000,
                "from_ts": 10,
                "to_ts": 20,
                "limit": 5
            })
        );
    }

    #[test]
    fn edges_body_keeps_only_provided_filters() {
        assert_eq!(
            edges_body(None, None, None, None, None, None, None),
            json!({})
        );
        assert_eq!(
            edges_body(
                Some("gateway"),
                Some("order-api"),
                Some("otlp"),
                Some(10),
                None,
                None,
                Some(50),
            ),
            json!({
                "src_service": "gateway",
                "dst_service": "order-api",
                "source": "otlp",
                "min_requests": 10,
                "limit": 50
            })
        );
    }

    #[test]
    fn ts_delete_body_maps_matchers() {
        assert_eq!(
            ts_delete_body(
                Some("apm_service_requests_total"),
                &["service=order-api".to_string(), "agent_id!=a1".to_string()],
                0,
                100,
            )
            .unwrap(),
            json!({
                "measurement": "apm_service_requests_total",
                "matchers": [
                    {"name": "service", "op": "equal", "value": "order-api"},
                    {"name": "agent_id", "op": "not_equal", "value": "a1"}
                ],
                "from_ts": 0,
                "to_ts": 100
            })
        );
        assert!(ts_delete_body(None, &["oops".to_string()], 0, 1).is_err());
        assert!(ts_delete_body(None, &["=v".to_string()], 0, 1).is_err());
    }
}
