//! gse-agent 核心库：配置、外连、认证、心跳与指令执行。
//! bins/gse-agent 仅作为进程入口调用本库。

pub mod config;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use geminio::app::Error;
use geminio::{dial, Bytes, DialOptions, End};
use gse_proto::{AuthReply, AuthRequest, Command, Heartbeat, Receipt};

pub use config::{load_config, AgentConfig};

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
    authenticate(&end, &cfg.agent_id, &cfg.token).await?;
    heartbeat_loop(&end, &cfg.agent_id, cfg.heartbeat_interval_secs).await
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
