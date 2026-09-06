use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use geminio::app::Error;
use geminio::{Bytes, End, EndListener, ListenOptions};
use gse_proto::{AuthReply, AuthRequest, Command, GseError, Heartbeat, Receipt};

use crate::config::ServerConfig;
use crate::http;
use crate::ledger::{ledger_stamp, AccessPoint, Ledger};
use crate::session::{now_micros, Session, SessionRegistry, SessionState};

const LIVENESS_SCAN_INTERVAL_SECS: u64 = 5;
const COMMAND_TIMEOUT_SECS: u64 = 60;

static CMD_SEQ: AtomicU64 = AtomicU64::new(0);

/// gse-server 实例：监听、会话注册表、台账与配置的封装。可同进程调用 send_command。
pub struct Server {
    pub registry: Arc<SessionRegistry>,
    pub ledger: Arc<Ledger>,
    pub cfg: Arc<ServerConfig>,
    listener: EndListener,
}

/// 将 `ip:port` 拆出地址段，供接入点登记。
fn addr_host(listen: &str) -> String {
    listen
        .rsplit_once(':')
        .map(|(h, _)| h.to_string())
        .unwrap_or_else(|| listen.to_string())
}

impl Server {
    /// 绑定监听地址，打开台账数据库并自登记接入点。
    pub async fn bind(cfg: ServerConfig) -> Result<(Arc<Server>, SocketAddr), Error> {
        let ledger =
            Arc::new(Ledger::new(&cfg.db).map_err(|e| Error::Remote(format!("open ledger: {e}")))?);
        ledger
            .init()
            .await
            .map_err(|e| Error::Remote(format!("init ledger: {e}")))?;

        let listener = EndListener::bind(&cfg.listen, ListenOptions::default()).await?;
        let addr = listener.local_addr()?;

        let ap = AccessPoint {
            id: format!("gse-server:{}", cfg.listen),
            name: "gse-server".to_string(),
            server_ip: addr_host(&cfg.listen),
            rpc_port: addr.port() as i32,
            file_port: None,
            data_port: None,
            created_at: ledger_stamp(),
        };
        ledger
            .upsert_access_point(&ap)
            .await
            .map_err(|e| Error::Remote(format!("register access point: {}", e.message)))?;

        let server = Arc::new(Server {
            registry: Arc::new(SessionRegistry::new()),
            ledger,
            cfg: Arc::new(cfg),
            listener,
        });
        Ok((server, addr))
    }

    /// 启动 accept 循环、存活检测与 HTTP 管理端口，随进程运行。
    pub async fn run(&self) -> Result<(), Error> {
        tokio::spawn(run_liveness(
            self.registry.clone(),
            self.ledger.clone(),
            Duration::from_secs(LIVENESS_SCAN_INTERVAL_SECS),
            Duration::from_secs(self.cfg.heartbeat_timeout_secs),
        ));
        if self.cfg.http_enabled {
            let admin = http::AdminState {
                ledger: self.ledger.clone(),
                registry: Some(self.registry.clone()),
            };
            let listen = self.cfg.http_listen.clone();
            tokio::spawn(async move {
                if let Err(e) = http::serve(admin, &listen).await {
                    eprintln!("gse-server: http management failed: {:?}", e);
                }
            });
        }
        loop {
            let (end, _drivers) = self.listener.accept().await?;
            let registry = self.registry.clone();
            let cfg = self.cfg.clone();
            let ledger = self.ledger.clone();
            tokio::spawn(handle_conn(end, registry, cfg, ledger));
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
    ledger: Arc<Ledger>,
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
            if session.state == SessionState::Offline {
                if let Err(e) = ledger.mark_offline(&session.agent_id).await {
                    eprintln!(
                        "gse-server: mark_offline {} failed: {}",
                        session.agent_id, e.message
                    );
                }
            }
        }
    }
}

async fn handle_conn(
    end: End,
    registry: Arc<SessionRegistry>,
    cfg: Arc<ServerConfig>,
    ledger: Arc<Ledger>,
) {
    let end_auth = end.clone();
    let registry_auth = registry.clone();
    let cfg_auth = cfg.clone();
    let ledger_auth = ledger.clone();
    if let Err(e) = end
        .register("auth", {
            move |req: Bytes| {
                let end = end_auth.clone();
                let registry = registry_auth.clone();
                let cfg = cfg_auth.clone();
                let ledger = ledger_auth.clone();
                async move {
                    let reply = handle_auth(&req, &end, &registry, &cfg, &ledger).await;
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
    let ledger_heartbeat = ledger;
    if let Err(e) = end
        .register("heartbeat", move |req: Bytes| {
            let registry = registry_heartbeat.clone();
            let ledger = ledger_heartbeat.clone();
            async move {
                handle_heartbeat(&req, &registry, &ledger).await;
                Ok(Bytes::new())
            }
        })
        .await
    {
        eprintln!("gse-server: register heartbeat failed: {e}");
    }
}

async fn handle_heartbeat(req: &Bytes, registry: &SessionRegistry, ledger: &Ledger) {
    if let Ok(hb) = serde_json::from_slice::<Heartbeat>(req) {
        registry.touch(&hb.agent_id, now_micros()).await;
        let now = now_micros().to_string();
        if let Err(e) = ledger.mark_heartbeat(&hb.agent_id, &now).await {
            eprintln!(
                "gse-server: mark_heartbeat {} failed: {}",
                hb.agent_id, e.message
            );
        }
    }
}

async fn handle_auth(
    req: &Bytes,
    end: &End,
    registry: &SessionRegistry,
    cfg: &ServerConfig,
    ledger: &Ledger,
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
    let authenticated = if !cfg.auth_enabled {
        true
    } else {
        match ledger.check_auth(&req.agent_id, &req.token).await {
            Ok(ok) => ok,
            Err(e) => {
                eprintln!("gse-server: auth lookup failed: {:?}", e);
                return AuthReply {
                    ok: false,
                    reason: Some("auth lookup failed".to_string()),
                };
            }
        }
    };
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
        .insert(Session::new(agent_id.clone(), end.clone(), now))
        .await;
    if let Err(e) = ledger.mark_online(&agent_id, &now.to_string()).await {
        eprintln!("gse-server: mark_online {agent_id} failed: {}", e.message);
    }
    AuthReply {
        ok: true,
        reason: None,
    }
}
