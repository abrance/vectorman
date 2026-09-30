//! gse-agent 核心库：配置、外连、认证、心跳与指令执行。
//! bins/gse-agent 仅作为进程入口调用本库。

pub mod collect;
pub mod config;
pub mod file_io;
pub mod job;
pub mod spec_apply;
pub mod upgrade;

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use geminio::app::Error;
use geminio::{dial, Bytes, DialOptions, End};
use gse_proto::{
    spec_outcome, AgentSpecAck, AgentSpecPush, AuthReply, AuthRequest, Command, Heartbeat,
    HeartbeatReply, Receipt,
};

pub use config::{load_config, AgentConfig};
use spec_apply::RuntimeConfig;

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
///
/// `cfg_path` 用于 `SIGHUP` 重读（本地手改配置后 `systemctl reload` / `kill -HUP` 生效）。
pub async fn run(cfg: AgentConfig, cfg_path: String) -> Result<(), String> {
    // 共享状态先建好：`RuntimeConfig` 与采集句柄必须持有同一个
    // `Arc<CollectShared>`，否则 OTLP 参数改了采集侧看不到。
    let mut shared = collect::CollectShared::new(cfg.agent_id.clone());
    shared.set_otlp_options(
        cfg.otlp_enabled,
        cfg.otlp_listen.clone(),
        cfg.otlp_max_body_bytes,
        cfg.otlp_token.clone(),
        cfg.otlp_allowed_cidrs.clone(),
    );
    let rt = RuntimeConfig::new(&cfg, Arc::new(shared));
    spawn_sighup_listener(rt.clone(), cfg_path);

    let mut backoff: u64 = 1;
    loop {
        match connect_once(&rt).await {
            // 会话正常结束（token 变更触发的主动重连）：立刻重连换新凭据。
            Ok(()) => {
                eprintln!("gse-agent: session ended, reconnecting");
                backoff = 1;
            }
            Err(AgentError::AuthFailed(reason)) => {
                return Err(format!("auth rejected: {reason}"));
            }
            Err(AgentError::ConnError(e)) => {
                // 曾经成功建立过连接（认证通过 + 心跳跑起来）后失败，说明这是一次
                // 「连上又断」而非「一直连不上」——退避必须重置，否则一次成功后
                // 再断要白等 60s。
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

async fn connect_once(rt: &Arc<RuntimeConfig>) -> Result<(), AgentError> {
    let (end, _drivers) = dial(rt.server_addr(), DialOptions::default())
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
    // 作业执行器每次现取：spec 变更会整体换实例，handler 不能抓一份旧的。
    let jobs_end = end.clone();
    let jobs_rt = rt.clone();
    if let Err(e) = end
        .register("job_exec", move |req: Bytes| {
            let rt = jobs_rt.clone();
            let end = jobs_end.clone();
            async move {
                let ack = rt.job_executor().await.handle_exec(&req, end).await;
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
    // 只负责起 supervisor；后续控制都走 `rt.collector()`（共享状态）。
    let _supervisor = collect::CollectorHandle::new_with_shared(rt.collector(), end.clone());

    // 入站：服务端推送的期望 spec。**不可变字段在这里先挡一道** ——
    // 改 `server_addr` / `agent_id` 只能改本地文件。
    let spec_rt = rt.clone();
    if let Err(e) = end
        .register("agent_spec", move |req: Bytes| {
            let rt = spec_rt.clone();
            async move {
                if let Some(reason) = spec_apply::rejects_immutable_fields(&req) {
                    eprintln!("gse-agent: spec rejected: {reason}");
                    let ack = AgentSpecAck {
                        revision: rt.applied_revision().await,
                        outcome: spec_outcome::REJECTED.to_string(),
                        applied: rt.applied_snapshot().await,
                        not_enforced: Vec::new(),
                        detail: reason,
                    };
                    let body =
                        serde_json::to_vec(&ack).map_err(|e| Error::Remote(e.to_string()))?;
                    return Ok(Bytes::from(body));
                }
                let push: AgentSpecPush = serde_json::from_slice(&req)
                    .map_err(|e| Error::Remote(format!("bad spec push: {e}")))?;
                let ack = spec_apply::apply_push(&rt, push).await;
                let body = serde_json::to_vec(&ack).map_err(|e| Error::Remote(e.to_string()))?;
                Ok(Bytes::from(body))
            }
        })
        .await
    {
        return Err(AgentError::ConnError(ConnError::new(format!(
            "register agent_spec: {e}"
        ))));
    }

    authenticate(&end, rt.agent_id(), &rt.token().await).await?;
    // 认证后自动拉取期望 spec：手动下发只是「不必等重连立即生效」，
    // 重连本身就是一次收敛（否则 Agent 会长期跑旧配置）。
    pull_spec(&end, rt).await;
    rt.collector().pull_addr_now().await;
    heartbeat_loop(&end, rt).await
}

/// 认证后立刻拉取本 Agent 的期望 spec 并应用。
async fn pull_spec(end: &End, rt: &Arc<RuntimeConfig>) {
    match end.call("agent_spec", Bytes::from_static(b"{}")).await {
        Ok(resp) => match serde_json::from_slice::<AgentSpecPush>(&resp) {
            Ok(push) => {
                let ack = spec_apply::apply_push(rt, push).await;
                println!(
                    "gse-agent: spec pulled, revision={} outcome={}",
                    ack.revision, ack.outcome
                );
            }
            Err(e) => eprintln!("gse-agent: bad agent_spec reply: {e}"),
        },
        Err(e) => eprintln!("gse-agent: agent_spec rpc failed: {e}"),
    }
}

async fn authenticate(end: &End, agent_id: &str, token: &str) -> Result<(), AgentError> {
    let req = AuthRequest {
        agent_id: agent_id.to_string(),
        token: token.to_string(),
        // 自报版本，服务端据此刷新台账 `agents.version`（否则控制台永远显示登记时的版本）。
        version: vectorman_version::VERSION.to_string(),
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

async fn heartbeat_loop(end: &End, rt: &Arc<RuntimeConfig>) -> Result<(), AgentError> {
    loop {
        // 生效快照一次性补报：下发回执只在「下发那一刻」到达，服务端落库前掉线、
        // 或心跳本身丢失，都会让它永久停在旧 revision。
        let spec = rt.pending_ack().await;
        let carried_spec = spec.is_some();
        // 升级结果补报：升级时作业通道已断，结果落本机文件，
        // 由重启后的 agent 在心跳里带出一次，随后标记已上报。
        let hb = Heartbeat {
            agent_id: rt.agent_id().to_string(),
            ts_micros: now_micros(),
            upgrade_result: upgrade::unreported_result().map(to_report),
            spec,
        };
        let body = Bytes::from(
            serde_json::to_vec(&hb)
                .map_err(|e| AgentError::ConnError(ConnError::new(e.to_string())))?,
        );
        let carried_result = hb.upgrade_result.is_some();
        let resp = end.call("heartbeat", body).await.map_err(|e| {
            AgentError::ConnError(ConnError::after_connected(format!("heartbeat rpc: {e}")))
        })?;
        // 服务端确认落库才停止补报；否则消耗一次机会继续带（上限见 spec_apply）。
        if carried_spec {
            match serde_json::from_slice::<HeartbeatReply>(&resp) {
                Ok(r) if r.spec_synced => rt.confirm_report(),
                _ => rt.decay_report(),
            }
        }
        // **心跳成功送达之后**才标记已上报：发送失败时下次心跳继续带，
        // 否则结果会丢（重启一次就没了）。
        if carried_result {
            if let Err(e) = upgrade::mark_reported() {
                eprintln!("gse-agent: mark upgrade result reported failed: {e}");
            }
        }
        // 心跳周期每轮现读：热改后下一个周期生效，不必重连。
        let interval = rt.heartbeat_interval_secs().max(1);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(interval)) => {}
            // token 变更要求重新认证：结束本会话，由 run() 立刻重连。
            _ = rt.wait_reauth() => {
                println!("gse-agent: token changed, reconnecting to re-authenticate");
                return Ok(());
            }
        }
    }
}

/// 监听 `SIGHUP`：本地手改配置后 `systemctl reload` / `kill -HUP <pid>` 即热应用。
#[cfg(unix)]
fn spawn_sighup_listener(rt: Arc<RuntimeConfig>, cfg_path: String) {
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut hup = match signal(SignalKind::hangup()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("gse-agent: cannot listen SIGHUP: {e}");
                return;
            }
        };
        while hup.recv().await.is_some() {
            reload_from_file(&rt, &cfg_path).await;
        }
    });
}

#[cfg(not(unix))]
fn spawn_sighup_listener(_rt: Arc<RuntimeConfig>, _cfg_path: String) {
    eprintln!("gse-agent: SIGHUP reload 仅支持 unix");
}

/// 重读本地配置文件并热应用。
///
/// **先解析成功再替换**：反过来的话，一次「文件写到一半」就会把生效配置清空。
async fn reload_from_file(rt: &Arc<RuntimeConfig>, cfg_path: &str) {
    match load_config(cfg_path) {
        Ok(cfg) => {
            // 身份与地址不可热改（它们决定连到哪、以谁的身份认证）：
            // 改了就让运维重启进程，避免出现「配置说 A、连接在 B」。
            if cfg.agent_id != rt.agent_id() || cfg.server_addr != rt.server_addr() {
                eprintln!(
                    "gse-agent: spec_reload_failed reason=identity_or_server_addr_changed（需改回并重启进程）"
                );
                return;
            }
            let params = spec_apply::params_from_config(&cfg);
            let ack = spec_apply::apply_local_file(rt, params).await;
            println!(
                "gse-agent: spec_reloaded source=file outcome={} not_enforced={:?}",
                ack.outcome, ack.not_enforced
            );
        }
        Err(reason) => eprintln!("gse-agent: spec_reload_failed reason={reason}"),
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
