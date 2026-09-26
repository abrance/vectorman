//! gse-agent 核心库：配置、外连、认证、心跳与指令执行。
//! bins/gse-agent 仅作为进程入口调用本库。

pub mod collect;
pub mod config;
pub mod file_io;
pub mod job;
pub mod upgrade;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use geminio::app::Error;
use geminio::{dial, Bytes, DialOptions, End};
use gse_proto::{AuthReply, AuthRequest, CollectItemsReply, Command, Heartbeat, Receipt};

pub use config::{load_config, AgentConfig};
use job::{JobConfig, JobExecutor};

const BACKOFF_MAX_SECS: u64 = 60;

/// 重连退避的下一步。
///
/// 曾经连上过（`connected`）说明这是一次「连上又断」，退避必须重置为 1s；
/// 否则连续失败时指数增长、封顶 `BACKOFF_MAX_SECS`。
/// 抽成独立函数是为了可测：`run` 里的策略与测试验证的是同一份实现。
fn next_backoff(current: u64, connected: bool) -> u64 {
    if connected {
        return 1;
    }
    (current * 2).min(BACKOFF_MAX_SECS)
}

#[derive(Debug)]
pub enum AgentError {
    AuthFailed(String),
    ConnError(ConnError),
}

/// 连接 Server 并保持心跳，断线后指数退避重连。
/// 认证失败返回 Err，调用方应以非零退出码结束进程。
pub async fn run(cfg: AgentConfig) -> Result<(), String> {
    let mut backoff: u64 = 1;
    loop {
        match connect_once(&cfg).await {
            Ok(()) => return Ok(()),
            Err(AgentError::AuthFailed(reason)) => {
                return Err(format!("auth rejected: {reason}"));
            }
            Err(AgentError::ConnError(e)) => {
                // 曾经成功建立过连接（认证通过 + 心跳跑起来）后失败，说明这是一次
                // 「连上又断」而非「一直连不上」——退避必须重置，否则一次成功后
                // 再断要白等 60s（原来 backoff 跨 connect_once 单调增长）。
                eprintln!(
                    "gse-agent: connection error: {}, retry in {backoff}s",
                    e.message
                );
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = next_backoff(backoff, e.connected);
            }
        }
    }
}

/// 连接失败的原因，区分「从未连上」与「连上后断开」。
#[derive(Debug)]
pub struct ConnError {
    pub message: String,
    /// 本次失败前是否曾成功建立连接（认证通过）。
    pub connected: bool,
}

impl ConnError {
    /// 连接尚未建立就失败（dial / register / auth 阶段）。
    fn new(message: String) -> Self {
        Self {
            message,
            connected: false,
        }
    }

    /// 连接已建立（认证通过、心跳跑过）之后失败。
    fn after_connected(message: String) -> Self {
        Self {
            message,
            connected: true,
        }
    }
}

async fn connect_once(cfg: &AgentConfig) -> Result<(), AgentError> {
    let (end, _drivers) = dial(&cfg.server_addr, DialOptions::default())
        .await
        .map_err(|e| AgentError::ConnError(ConnError::new(format!("dial: {e}"))))?;
    if let Err(e) = end
        .register(
            "exec",
            move |req: Bytes| async move { handle_exec(&req).await },
        )
        .await
    {
        return Err(AgentError::ConnError(ConnError::new(format!(
            "register exec: {e}"
        ))));
    }
    let executor = JobExecutor::new(JobConfig::from_agent(cfg));
    let jobs_end = end.clone();
    if let Err(e) = end
        .register("job_exec", move |req: Bytes| {
            let executor = executor.clone();
            let end = jobs_end.clone();
            async move {
                let ack = executor.handle_exec(&req, end).await;
                let body = serde_json::to_vec(&ack).map_err(|e| Error::Remote(e.to_string()))?;
                Ok(Bytes::from(body))
            }
        })
        .await
    {
        return Err(AgentError::ConnError(ConnError::new(format!(
            "register job_exec: {e}"
        ))));
    }
    let file_io = file_io::FileIo::new();
    let file_read = file_io.clone();
    if let Err(e) = end
        .register("file_read", move |req: Bytes| {
            let file_io = file_read.clone();
            async move { file_io.handle_read(&req).await }
        })
        .await
    {
        return Err(AgentError::ConnError(ConnError::new(format!(
            "register file_read: {e}"
        ))));
    }
    let file_write = file_io.clone();
    if let Err(e) = end
        .register("file_write", move |req: Bytes| {
            let file_io = file_write.clone();
            async move { file_io.handle_write(&req).await }
        })
        .await
    {
        return Err(AgentError::ConnError(ConnError::new(format!(
            "register file_write: {e}"
        ))));
    }
    let collector = collect::CollectorHandle::new_with_otlp(
        cfg.agent_id.clone(),
        end.clone(),
        collect::OtlpOptions {
            enabled: cfg.otlp_enabled,
            listen: cfg.otlp_listen.clone(),
            max_body_bytes: cfg.otlp_max_body_bytes,
            token: cfg.otlp_token.clone(),
            allowed_cidrs: cfg.otlp_allowed_cidrs.clone(),
        },
    );
    let collect_handler = collector.clone();
    if let Err(e) = end
        .register("collect_items", move |req: Bytes| {
            let handler = collect_handler.clone();
            async move {
                match serde_json::from_slice::<CollectItemsReply>(&req) {
                    Ok(reply) => handler.apply(reply),
                    Err(e) => eprintln!("gse-agent: bad collect_items push: {e}"),
                }
                Ok(Bytes::from_static(b"{}"))
            }
        })
        .await
    {
        return Err(AgentError::ConnError(ConnError::new(format!(
            "register collect_items: {e}"
        ))));
    }
    authenticate(&end, &cfg.agent_id, &cfg.token).await?;
    pull_collect_items(&end, &collector).await;
    collector.pull_addr_now();
    heartbeat_loop(&end, &cfg.agent_id, cfg.heartbeat_interval_secs).await
}

/// 认证后立刻拉取本 Agent 的采集项整表。
async fn pull_collect_items(end: &End, collector: &collect::CollectorHandle) {
    match end.call("collect_items", Bytes::from_static(b"{}")).await {
        Ok(resp) => match serde_json::from_slice::<CollectItemsReply>(&resp) {
            Ok(reply) => collector.apply(reply),
            Err(e) => eprintln!("gse-agent: bad collect_items reply: {e}"),
        },
        Err(e) => eprintln!("gse-agent: collect_items rpc failed: {e}"),
    }
}

async fn authenticate(end: &End, agent_id: &str, token: &str) -> Result<(), AgentError> {
    let req = AuthRequest {
        agent_id: agent_id.to_string(),
        token: token.to_string(),
    };
    let body = Bytes::from(
        serde_json::to_vec(&req)
            .map_err(|e| AgentError::ConnError(ConnError::new(e.to_string())))?,
    );
    let resp = end
        .call("auth", body)
        .await
        .map_err(|e| AgentError::ConnError(ConnError::new(format!("auth rpc: {e}"))))?;
    let reply: AuthReply = serde_json::from_slice(&resp)
        .map_err(|e| AgentError::AuthFailed(format!("bad auth reply: {e}")))?;
    if !reply.ok {
        return Err(AgentError::AuthFailed(reply.reason.unwrap_or_default()));
    }
    Ok(())
}

async fn heartbeat_loop(end: &End, agent_id: &str, interval_secs: u64) -> Result<(), AgentError> {
    loop {
        // 升级结果补报：升级时作业通道已断，结果落本机文件，
        // 由重启后的 agent 在心跳里带出一次，随后标记已上报。
        let hb = Heartbeat {
            agent_id: agent_id.to_string(),
            ts_micros: now_micros(),
            upgrade_result: upgrade::unreported_result().map(to_report),
        };
        let body = Bytes::from(
            serde_json::to_vec(&hb)
                .map_err(|e| AgentError::ConnError(ConnError::new(e.to_string())))?,
        );
        let carried_result = hb.upgrade_result.is_some();
        end.call("heartbeat", body).await.map_err(|e| {
            AgentError::ConnError(ConnError::after_connected(format!("heartbeat rpc: {e}")))
        })?;
        // **心跳成功送达之后**才标记已上报：发送失败时下次心跳继续带，
        // 否则结果会丢（重启一次就没了）。
        if carried_result {
            if let Err(e) = upgrade::mark_reported() {
                eprintln!("gse-agent: mark upgrade result reported failed: {e}");
            }
        }
        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
    }
}

/// 本机升级结果 → 上行表示。
fn to_report(r: upgrade::UpgradeResult) -> gse_proto::UpgradeReport {
    let outcome = match r.outcome {
        upgrade::UpgradeOutcome::Succeeded => "succeeded",
        upgrade::UpgradeOutcome::RolledBack => "rolled_back",
        upgrade::UpgradeOutcome::Failed => "failed",
    };
    gse_proto::UpgradeReport {
        started_at: r.started_at,
        finished_at: r.finished_at,
        from_version: r.from_version,
        to_version: r.to_version,
        from_sha256: r.from_sha256,
        to_sha256: r.to_sha256,
        outcome: outcome.to_string(),
        detail: r.detail,
    }
}

async fn handle_exec(req: &Bytes) -> Result<Bytes, Error> {
    let cmd: Command = match serde_json::from_slice(req) {
        Ok(c) => c,
        Err(e) => {
            let receipt = Receipt {
                command_id: String::new(),
                ok: false,
                message: Some(format!("bad command payload: {e}")),
            };
            return Ok(Bytes::from(
                serde_json::to_vec(&receipt).map_err(|e| Error::Remote(e.to_string()))?,
            ));
        }
    };
    let receipt = if cmd.name == "ping" {
        Receipt {
            command_id: cmd.id,
            ok: true,
            message: Some("pong".to_string()),
        }
    } else {
        Receipt {
            command_id: cmd.id,
            ok: false,
            message: Some(format!("unknown command: {}", cmd.name)),
        }
    };
    Ok(Bytes::from(
        serde_json::to_vec(&receipt).map_err(|e| Error::Remote(e.to_string()))?,
    ))
}

fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_on_repeated_failures() {
        assert_eq!(next_backoff(1, false), 2);
        assert_eq!(next_backoff(2, false), 4);
        assert_eq!(next_backoff(32, false), 60, "封顶 60s");
        assert_eq!(next_backoff(60, false), 60);
    }

    #[test]
    fn backoff_resets_after_a_successful_connection() {
        // 「连上又断」：退避回到 1s，不必白等 60s。
        assert_eq!(next_backoff(60, true), 1);
        assert_eq!(next_backoff(1, true), 1);
    }

    #[test]
    fn conn_error_distinguishes_never_connected_from_disconnected() {
        assert!(!ConnError::new("dial: refused".to_string()).connected);
        assert!(ConnError::after_connected("heartbeat rpc".to_string()).connected);
    }
}
