use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use geminio::app::Error;
use geminio::{Bytes, End, EndListener, ListenOptions};
use gse_proto::{AuthReply, AuthRequest, Command, GseError, Heartbeat, Receipt};

use crate::config::ServerConfig;
use crate::session::{now_micros, Session, SessionRegistry, SessionState};

const LIVENESS_SCAN_INTERVAL_SECS: u64 = 5;
const COMMAND_TIMEOUT_SECS: u64 = 60;

static CMD_SEQ: AtomicU64 = AtomicU64::new(0);

/// gse-server 实例：监听、会话注册表与配置的封装。可同进程调用 send_command。
pub struct Server {
    pub registry: Arc<SessionRegistry>,
    pub cfg: Arc<ServerConfig>,
    listener: EndListener,
}

impl Server {
    /// 绑定监听地址并初始化会话注册表。
    pub async fn bind(cfg: ServerConfig) -> Result<(Arc<Server>, SocketAddr), Error> {
        let listener = EndListener::bind(&cfg.listen, ListenOptions::default()).await?;
        let addr = listener.local_addr()?;
        let server = Arc::new(Server {
            registry: Arc::new(SessionRegistry::new()),
            cfg: Arc::new(cfg),
            listener,
        });
        Ok((server, addr))
    }

    /// 启动 accept 循环与存活检测，随进程运行。
    pub async fn run(&self) -> Result<(), Error> {
        tokio::spawn(run_liveness(
            self.registry.clone(),
            Duration::from_secs(LIVENESS_SCAN_INTERVAL_SECS),
            Duration::from_secs(self.cfg.heartbeat_timeout_secs),
        ));
        loop {
            let (end, _drivers) = self.listener.accept().await?;
            let registry = self.registry.clone();
            let cfg = self.cfg.clone();
            tokio::spawn(handle_conn(end, registry, cfg));
        }
    }

    /// 当前会话快照。
    pub async fn sessions(&self) -> Vec<Session> {
        self.registry.list().await
    }

    /// 向指定 agent 下发指令，RPC 返回即回执。目标离线或无会话时返回 unavailable。
    pub async fn send_command(
        &self,
        agent_id: &str,
        name: &str,
        payload: Bytes,
    ) -> Result<Receipt, GseError> {
        let Some(session) = self.registry.get(agent_id).await else {
            return Err(GseError::new(
                "unavailable",
                format!("no session for agent {agent_id}"),
            ));
        };
        if session.state != SessionState::Online {
            return Err(GseError::new(
                "unavailable",
                format!("agent {agent_id} not online"),
            ));
        }
        let seq = CMD_SEQ.fetch_add(1, Ordering::Relaxed);
        let cmd = Command {
            id: seq.to_string(),
            name: name.to_string(),
            payload,
        };
        let bytes = Bytes::from(
            serde_json::to_vec(&cmd).map_err(|e| GseError::new("rpc_error", e.to_string()))?,
        );
        let resp = tokio::time::timeout(
            Duration::from_secs(COMMAND_TIMEOUT_SECS),
            session.end.call("exec", bytes),
        )
        .await
        .map_err(|_| {
            GseError::new(
                "unavailable",
                format!("command {name} to agent {agent_id} timed out"),
            )
        })?
        .map_err(|e| GseError::new("rpc_error", e.to_string()))?;
        serde_json::from_slice(&resp).map_err(|e| GseError::new("rpc_error", e.to_string()))
    }
}

async fn run_liveness(
    registry: Arc<SessionRegistry>,
    scan_interval: Duration,
    timeout_window: Duration,
) {
    let window = timeout_window.as_micros() as i64;
    loop {
        tokio::time::sleep(scan_interval).await;
        registry.advance_all(now_micros(), window).await;
        for session in registry.list().await {
            if session.state != SessionState::Online {
                println!(
                    "gse-server: session agent={} state={:?}",
                    session.agent_id, session.state
                );
            }
        }
    }
}

async fn handle_conn(end: End, registry: Arc<SessionRegistry>, cfg: Arc<ServerConfig>) {
    let end_auth = end.clone();
    let registry_auth = registry.clone();
    let cfg_auth = cfg.clone();
    if let Err(e) = end
        .register("auth", {
            move |req: Bytes| {
                let end = end_auth.clone();
                let registry = registry_auth.clone();
                let cfg = cfg_auth.clone();
                async move {
                    let reply = handle_auth(&req, &end, &registry, &cfg).await;
                    // 认证失败不回 close：让 reply（拒绝原因）送达 agent，由 agent 侧决定停止重连。
                    serde_json::to_vec(&reply)
                        .map(Bytes::from)
                        .map_err(|e| Error::Remote(e.to_string()))
                }
            }
        })
        .await
    {
        eprintln!("gse-server: register auth failed: {e}");
    }

    let registry_heartbeat = registry;
    if let Err(e) = end
        .register("heartbeat", move |req: Bytes| {
            let registry = registry_heartbeat.clone();
            async move {
                handle_heartbeat(&req, &registry).await;
                Ok(Bytes::new())
            }
        })
        .await
    {
        eprintln!("gse-server: register heartbeat failed: {e}");
    }
}

async fn handle_heartbeat(req: &Bytes, registry: &SessionRegistry) {
    if let Ok(hb) = serde_json::from_slice::<Heartbeat>(req) {
        registry.touch(&hb.agent_id, now_micros()).await;
    }
}

async fn handle_auth(
    req: &Bytes,
    end: &End,
    registry: &SessionRegistry,
    cfg: &ServerConfig,
) -> AuthReply {
    let req: AuthRequest = match serde_json::from_slice(req) {
        Ok(r) => r,
        Err(_) => {
            return AuthReply {
                ok: false,
                reason: Some("bad auth payload".to_string()),
            }
        }
    };
    let authenticated = !cfg.auth_enabled
        || cfg
            .agents
            .get(&req.agent_id)
            .is_some_and(|t| t == &req.token);
    if !authenticated {
        return AuthReply {
            ok: false,
            reason: Some("invalid agent_id or token".to_string()),
        };
    }
    let now = now_micros();
    let agent_id = req.agent_id;
    println!("gse-server: agent {agent_id} authenticated");
    registry
        .insert(Session::new(agent_id, end.clone(), now))
        .await;
    AuthReply {
        ok: true,
        reason: None,
    }
}
