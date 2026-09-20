//! gse-agent 核心库：配置、外连、认证、心跳与指令执行。
//! bins/gse-agent 仅作为进程入口调用本库。

pub mod collect;
pub mod config;
pub mod file_io;
pub mod job;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use geminio::app::Error;
use geminio::{dial, Bytes, DialOptions, End};
use gse_proto::{AuthReply, AuthRequest, CollectItemsReply, Command, Heartbeat, Receipt};

pub use config::{load_config, AgentConfig};
use job::{JobConfig, JobExecutor};

const BACKOFF_MAX_SECS: u64 = 60;

#[derive(Debug)]
pub enum AgentError {
    AuthFailed(String),
    ConnError(String),
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
                eprintln!("gse-agent: connection error: {e}, retry in {backoff}s");
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(BACKOFF_MAX_SECS);
            }
        }
    }
}

async fn connect_once(cfg: &AgentConfig) -> Result<(), AgentError> {
    let (end, _drivers) = dial(&cfg.server_addr, DialOptions::default())
        .await
        .map_err(|e| AgentError::ConnError(format!("dial: {e}")))?;
    if let Err(e) = end
        .register(
            "exec",
            move |req: Bytes| async move { handle_exec(&req).await },
        )
        .await
    {
        return Err(AgentError::ConnError(format!("register exec: {e}")));
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
        return Err(AgentError::ConnError(format!("register job_exec: {e}")));
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
        return Err(AgentError::ConnError(format!("register file_read: {e}")));
    }
    let file_write = file_io.clone();
    if let Err(e) = end
        .register("file_write", move |req: Bytes| {
            let file_io = file_write.clone();
            async move { file_io.handle_write(&req).await }
        })
        .await
    {
        return Err(AgentError::ConnError(format!("register file_write: {e}")));
    }
    let collector = collect::CollectorHandle::new(cfg.agent_id.clone(), end.clone());
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
        return Err(AgentError::ConnError(format!(
            "register collect_items: {e}"
        )));
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
    let body =
        Bytes::from(serde_json::to_vec(&req).map_err(|e| AgentError::ConnError(e.to_string()))?);
    let resp = end
        .call("auth", body)
        .await
        .map_err(|e| AgentError::ConnError(format!("auth rpc: {e}")))?;
    let reply: AuthReply = serde_json::from_slice(&resp)
        .map_err(|e| AgentError::AuthFailed(format!("bad auth reply: {e}")))?;
    if !reply.ok {
        return Err(AgentError::AuthFailed(reply.reason.unwrap_or_default()));
    }
    Ok(())
}

async fn heartbeat_loop(end: &End, agent_id: &str, interval_secs: u64) -> Result<(), AgentError> {
    loop {
        let hb = Heartbeat {
            agent_id: agent_id.to_string(),
            ts_micros: now_micros(),
        };
        let body =
            Bytes::from(serde_json::to_vec(&hb).map_err(|e| AgentError::ConnError(e.to_string()))?);
        end.call("heartbeat", body)
            .await
            .map_err(|e| AgentError::ConnError(format!("heartbeat rpc: {e}")))?;
        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
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
