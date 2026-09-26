use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use geminio::app::Error;
use geminio::{Bytes, End, EndDrivers, EndListener, ListenOptions};
use gse_proto::{
    AuthReply, AuthRequest, CollectItemsReply, Command, DataplaneAddrReply, DataplaneAddrRequest,
    GseError, Heartbeat, JobAck, JobExec, JobResult, Receipt,
};

use crate::config::ServerConfig;
use crate::dataplane::probe_dataplanes;
use crate::http;
use crate::job_file_store::JobFileStore;
use crate::ledger::{ledger_stamp, AccessPoint, JobRecord, Ledger, NewJob};
use crate::rerun::{build_rerun_submit, RerunRequest};
use crate::session::{now_micros, Session, SessionRegistry, SessionState};

const LIVENESS_SCAN_INTERVAL_SECS: u64 = 5;

/// 心跳超时窗口（微秒），与 `ServerConfig::heartbeat_timeout_secs` 默认值一致，
/// 用于判定「心跳是否新鲜」。取 90s（配置默认值）。
fn heartbeat_window_micros() -> i64 {
    90 * 1_000_000
}

/// 单次活性探测的超时（秒）。
const SESSION_PROBE_TIMEOUT_SECS: u64 = 5;
const COMMAND_TIMEOUT_SECS: u64 = 60;

static CMD_SEQ: AtomicU64 = AtomicU64::new(0);
static JOB_SEQ: AtomicU64 = AtomicU64::new(0);

/// HTTP 提交作业的请求体；省略的解释器由 server 默认填充。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct JobSubmit {
    pub agent_id: String,
    /// 作业类型：`script`（默认）或 `agent_upgrade`。
    #[serde(default = "gse_proto::default_job_kind")]
    pub kind: String,
    #[serde(default)]
    pub interpreter: Option<String>,
    pub script: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

pub(crate) fn new_job_id() -> String {
    let seq = JOB_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("job-{}-{}", now_micros(), seq)
}

/// gse-server 实例：监听、会话注册表、台账与配置的封装。可同进程调用 send_command。
pub struct Server {
    pub registry: Arc<SessionRegistry>,
    pub ledger: Arc<Ledger>,
    pub cfg: Arc<ServerConfig>,
    pub file_store: Arc<JobFileStore>,
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
        // 上次运行遗留的在途作业无法恢复执行，统一置 lost。
        ledger
            .mark_lost_inflight_on_startup()
            .await
            .map_err(|e| Error::Remote(format!("recover jobs: {}", e.message)))?;

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

        let file_store = Arc::new(
            JobFileStore::open(cfg.resolved_job_file_dir())
                .map_err(|e| Error::Remote(format!("open job file store: {}", e.message)))?,
        );
        let server = Arc::new(Server {
            registry: Arc::new(SessionRegistry::new()),
            ledger,
            cfg: Arc::new(cfg),
            file_store,
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
        tokio::spawn(probe_dataplanes(
            self.ledger.clone(),
            Duration::from_secs(self.cfg.dataplane_probe_interval_secs),
        ));
        let metrics = vectorman_metrics::SelfMetrics::new("gse-server", &self.cfg.http_listen)
            .map_err(|e| Error::Remote(format!("metrics init: {e}")))?;
        metrics.set_hook(Arc::new(http::GseScrapeHook {
            ledger: self.ledger.clone(),
            registry: Some(self.registry.clone()),
        }));
        let metrics_listener = tokio::net::TcpListener::bind(&self.cfg.metrics_listen)
            .await
            .map_err(|e| Error::Remote(format!("bind metrics {}: {e}", self.cfg.metrics_listen)))?;
        println!(
            "gse-server: metrics listening on {}",
            self.cfg.metrics_listen
        );
        let metrics_app = metrics.clone().metrics_router();
        tokio::spawn(async move {
            if let Err(e) = axum::serve(metrics_listener, metrics_app).await {
                eprintln!("gse-server: metrics serve failed: {e}");
            }
        });
        tokio::spawn(crate::file_transfer::run_job_file_cleanup(
            self.file_store.clone(),
            Duration::from_secs(self.cfg.job_file_cleanup_interval_secs.max(1)),
            self.cfg.job_file_retain_secs,
        ));
        if self.cfg.http_enabled {
            let admin = http::AdminState {
                ledger: self.ledger.clone(),
                registry: Some(self.registry.clone()),
                cfg: Some(self.cfg.clone()),
                file_store: Some(self.file_store.clone()),
            };
            let listen = self.cfg.http_listen.clone();
            let web_dir = self.cfg.http_web_dir.clone();
            tokio::spawn(async move {
                if let Err(e) = http::serve(admin, &listen, web_dir, Some(metrics)).await {
                    eprintln!("gse-server: http management failed: {:?}", e);
                }
            });
        }
        loop {
            match self.listener.accept().await {
                Ok((end, drivers)) => {
                    let registry = self.registry.clone();
                    let cfg = self.cfg.clone();
                    let ledger = self.ledger.clone();
                    // `drivers` 必须持有（见 handle_conn 末尾注释：它不能用 await
                    // 探测连接结束，但持有它不影响探测路径）。
                    tokio::spawn(handle_conn(end, drivers, registry, cfg, ledger));
                }
                Err(e) => {
                    // 单个连接的握手/解码失败（如非法 wire-format）只影响该连接，
                    // 不能终止整个服务；记录后短暂退避再继续 accept。
                    eprintln!("gse-server: accept error: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
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
                format!("agent {agent_id} not online (session unavailable)"),
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

    /// 提交作业：校验后在 ledger 落 `pending` 并立即派发，返回最新记录。
    pub async fn submit_job(&self, req: JobSubmit) -> Result<JobRecord, GseError> {
        submit_job(&self.ledger, &self.registry, &self.cfg, req).await
    }
}

/// 提交作业：校验后在 ledger 落 `pending` 并立即派发，返回最新记录。
/// 供 `Server::submit_job` 与 HTTP handler 共用。
pub async fn submit_job(
    ledger: &Ledger,
    registry: &SessionRegistry,
    cfg: &ServerConfig,
    req: JobSubmit,
) -> Result<JobRecord, GseError> {
    submit_job_with_source(ledger, registry, cfg, req, None, None).await
}

/// 同 `submit_job`，但记录作业来源模板（由模板提交时传入）。
pub async fn submit_job_with_template(
    ledger: &Ledger,
    registry: &SessionRegistry,
    cfg: &ServerConfig,
    req: JobSubmit,
    template_id: Option<String>,
) -> Result<JobRecord, GseError> {
    submit_job_with_source(ledger, registry, cfg, req, template_id, None).await
}

/// 同 `submit_job`，但记录作业来源模板与来源作业（模板提交与重做共用）。
pub async fn submit_job_with_source(
    ledger: &Ledger,
    registry: &SessionRegistry,
    cfg: &ServerConfig,
    req: JobSubmit,
    template_id: Option<String>,
    rerun_of: Option<String>,
) -> Result<JobRecord, GseError> {
    if !cfg.jobs_enabled {
        return Err(GseError::new("unavailable", "jobs disabled"));
    }
    if req.agent_id.trim().is_empty() {
        return Err(GseError::new("invalid_argument", "agent_id required"));
    }
    if req.script.trim().is_empty() {
        return Err(GseError::new("invalid_argument", "script required"));
    }
    if req.script.len() > cfg.job_max_script_bytes {
        return Err(GseError::new(
            "invalid_argument",
            format!("script exceeds {} bytes", cfg.job_max_script_bytes),
        ));
    }
    let timeout = req.timeout_secs.unwrap_or(cfg.job_default_timeout_secs);
    if timeout == 0 || timeout > cfg.job_max_timeout_secs {
        return Err(GseError::new(
            "invalid_argument",
            format!(
                "timeout_secs must be within 1..={}",
                cfg.job_max_timeout_secs
            ),
        ));
    }
    match registry.get(&req.agent_id).await {
        Some(s) if s.state == SessionState::Online => {}
        _ => {
            // 措辞区分：Agent 确实离线 vs 心跳在线但会话不可用（连接已死）。
            // 这一处是用户最先看到的错误，尤其不能把连接问题说成 Agent 离线。
            let window = heartbeat_window_micros();
            let reason = ledger
                .lost_reason_for(&req.agent_id, now_micros(), window)
                .await;
            return Err(GseError::new(
                "unavailable",
                format!("agent {} not online ({reason})", req.agent_id),
            ));
        }
    }
    let kind = if req.kind.trim().is_empty() {
        gse_proto::default_job_kind()
    } else {
        req.kind.clone()
    };
    if kind == "agent_upgrade" {
        // 升级载荷必须是合法的 AgentUpgradeSpec —— 在受理阶段就挡住坏输入。
        let spec: gse_proto::AgentUpgradeSpec = serde_json::from_str(&req.script)
            .map_err(|e| GseError::new("invalid_argument", format!("bad upgrade spec: {e}")))?;
        if spec.binary_path.trim().is_empty() {
            return Err(GseError::new("invalid_argument", "binary_path required"));
        }
        if spec.sha256.trim().len() != 64 {
            return Err(GseError::new(
                "invalid_argument",
                "sha256 must be 64 hex chars",
            ));
        }
    }
    let interpreter = req
        .interpreter
        .filter(|i| !i.trim().is_empty())
        .unwrap_or_else(|| "bash".to_string());
    let job_id = new_job_id();
    let created_at = now_micros().to_string();
    ledger
        .insert_job(&NewJob {
            job_id: job_id.clone(),
            agent_id: req.agent_id.clone(),
            interpreter: interpreter.clone(),
            script: req.script.clone(),
            args: req.args.clone(),
            env: req.env.clone(),
            working_dir: req.working_dir.clone(),
            template_id,
            rerun_of,
            timeout_secs: timeout,
            created_at,
            ..Default::default()
        })
        .await?;

    let exec = JobExec {
        job_id: job_id.clone(),
        kind,
        interpreter,
        script: req.script,
        args: req.args,
        env: req.env,
        working_dir: req.working_dir,
        timeout_secs: timeout,
        stdout_limit_bytes: cfg.job_stdout_limit_bytes,
        stderr_limit_bytes: cfg.job_stderr_limit_bytes,
    };
    dispatch_job(ledger, registry, &req.agent_id, exec).await?;

    ledger
        .get_job(&job_id)
        .await?
        .ok_or_else(|| GseError::new("not_found", format!("job {job_id} vanished")))
}

/// 以来源作业为默认值合并重做覆盖后提交；记录来源作业，不修改来源记录。
pub async fn submit_rerun(
    ledger: &Ledger,
    registry: &SessionRegistry,
    cfg: &ServerConfig,
    source: &JobRecord,
    req: RerunRequest,
) -> Result<JobRecord, GseError> {
    let submit = build_rerun_submit(source, req);
    submit_job_with_source(
        ledger,
        registry,
        cfg,
        submit,
        None,
        Some(source.job_id.clone()),
    )
    .await
}

/// 调用 Agent 的 `job_exec` 并按受理结果流转状态：
/// 受理成功置 `running`，被拒绝置 `rejected`，RPC 失败/超时置 `lost`。
async fn dispatch_job(
    ledger: &Ledger,
    registry: &SessionRegistry,
    agent_id: &str,
    exec: JobExec,
) -> Result<(), GseError> {
    let session = match registry.get(agent_id).await {
        Some(s) if s.state == SessionState::Online => s,
        _ => {
            // 措辞区分：心跳窗口内却没有可用会话 ⇒ 连接已死（session unavailable），
            // 而不是「Agent 离线」—— 此前一律写 agent offline，误导排查。
            let window = heartbeat_window_micros();
            let reason = ledger.lost_reason_for(agent_id, now_micros(), window).await;
            ledger.mark_lost_by_agent(agent_id, &reason).await?;
            return Ok(());
        }
    };
    let job_id = exec.job_id.clone();
    let timeout_secs = exec.timeout_secs;
    let body =
        serde_json::to_vec(&exec).map_err(|e| GseError::new("invalid_argument", e.to_string()))?;
    let rpc_timeout = Duration::from_secs(timeout_secs.saturating_add(10));
    match tokio::time::timeout(rpc_timeout, session.end.call("job_exec", Bytes::from(body))).await {
        Ok(Ok(resp)) => match serde_json::from_slice::<JobAck>(&resp) {
            Ok(ack) if ack.accepted => {
                ledger
                    .mark_running(&job_id, &now_micros().to_string())
                    .await?;
            }
            Ok(ack) => {
                let reason = ack.reason.unwrap_or_else(|| "rejected".to_string());
                ledger.mark_rejected(&job_id, &reason).await?;
            }
            Err(_) => {
                ledger.mark_rejected(&job_id, "bad job ack").await?;
            }
        },
        Ok(Err(e)) => {
            eprintln!("gse-server: job_exec {job_id} rpc failed: {e}");
            // 会话对象在、但调用失败 ⇒ 应用层已确认连接不可用，措辞用 session unavailable。
            ledger
                .mark_lost_by_agent(agent_id, "session unavailable")
                .await?;
        }
        Err(_) => {
            eprintln!("gse-server: job_exec {job_id} timed out");
            // 超时是「下发后无回应」，不代表 Agent 离线；用独立措辞。
            ledger
                .mark_lost_by_agent(agent_id, "job_exec timeout")
                .await?;
        }
    }
    Ok(())
}

/// 作业结果落库：校验归属后写终态，未知或归属不符的结果丢弃。
pub async fn handle_job_result(ledger: &Ledger, conn_agent_id: &str, result: JobResult) {
    match ledger.get_job(&result.job_id).await {
        Ok(Some(job)) => {
            if job.agent_id != conn_agent_id {
                eprintln!(
                    "gse-server: drop job_result {} from {conn_agent_id} (owner {})",
                    result.job_id, job.agent_id
                );
                return;
            }
            if let Err(e) = ledger.finish_job(&result, &now_micros().to_string()).await {
                eprintln!(
                    "gse-server: finish_job {} failed: {}",
                    result.job_id, e.message
                );
            }
        }
        Ok(None) => {
            eprintln!("gse-server: job_result for unknown job {}", result.job_id);
        }
        Err(e) => {
            eprintln!(
                "gse-server: get_job {} failed: {}",
                result.job_id, e.message
            );
        }
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
            if matches!(session.state, SessionState::Offline | SessionState::Closed) {
                if let Err(e) = ledger
                    .mark_lost_by_agent(&session.agent_id, "agent offline")
                    .await
                {
                    eprintln!(
                        "gse-server: mark_lost_by_agent {} failed: {}",
                        session.agent_id, e.message
                    );
                }
            }
        }
    }
}

async fn handle_conn(
    end: End,
    _drivers: EndDrivers,
    registry: Arc<SessionRegistry>,
    cfg: Arc<ServerConfig>,
    ledger: Arc<Ledger>,
) {
    let authed: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let end_auth = end.clone();
    let registry_auth = registry.clone();
    let cfg_auth = cfg.clone();
    let ledger_auth = ledger.clone();
    let authed_auth = authed.clone();
    if let Err(e) = end
        .register("auth", {
            move |req: Bytes| {
                let end = end_auth.clone();
                let registry = registry_auth.clone();
                let cfg = cfg_auth.clone();
                let ledger = ledger_auth.clone();
                let authed = authed_auth.clone();
                async move {
                    let reply = handle_auth(&req, &end, &registry, &cfg, &ledger, &authed).await;
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

    let ledger_jobs = ledger.clone();
    let authed_jobs = authed.clone();
    if let Err(e) = end
        .register("job_result", move |req: Bytes| {
            let ledger = ledger_jobs.clone();
            let authed = authed_jobs.clone();
            async move {
                let conn_agent_id = authed.lock().ok().and_then(|g| g.clone());
                if let Some(agent_id) = conn_agent_id {
                    if let Ok(result) = serde_json::from_slice::<JobResult>(&req) {
                        handle_job_result(&ledger, &agent_id, result).await;
                    }
                }
                Ok(Bytes::new())
            }
        })
        .await
    {
        eprintln!("gse-server: register job_result failed: {e}");
    }

    let registry_heartbeat = registry.clone();
    let ledger_heartbeat = ledger.clone();
    let end_heartbeat = end.clone();
    let authed_heartbeat = authed.clone();
    let conn_client_id = end.client_id();
    if let Err(e) = end
        .register("heartbeat", move |req: Bytes| {
            let registry = registry_heartbeat.clone();
            let ledger = ledger_heartbeat.clone();
            let end = end_heartbeat.clone();
            let authed = authed_heartbeat.clone();
            async move {
                handle_heartbeat(&req, &end, conn_client_id, &authed, &registry, &ledger).await;
                Ok(Bytes::new())
            }
        })
        .await
    {
        eprintln!("gse-server: register heartbeat failed: {e}");
    }

    let ledger_addr = ledger.clone();
    let authed_addr = authed.clone();
    if let Err(e) = end
        .register("dataplane_addr", move |req: Bytes| {
            let ledger = ledger_addr.clone();
            let authed = authed_addr.clone();
            async move {
                let conn_agent_id = authed.lock().ok().and_then(|g| g.clone());
                let reply = handle_dataplane_addr(&req, conn_agent_id.as_deref(), &ledger).await;
                serde_json::to_vec(&reply)
                    .map(Bytes::from)
                    .map_err(|e| Error::Remote(e.to_string()))
            }
        })
        .await
    {
        eprintln!("gse-server: register dataplane_addr failed: {e}");
    }

    let ledger_items = ledger.clone();
    let authed_items = authed.clone();
    if let Err(e) = end
        .register("collect_items", move |_req: Bytes| {
            let ledger = ledger_items.clone();
            let authed = authed_items.clone();
            async move {
                let conn_agent_id = authed.lock().ok().and_then(|g| g.clone());
                let reply = handle_collect_items(conn_agent_id.as_deref(), &ledger).await;
                serde_json::to_vec(&reply)
                    .map(Bytes::from)
                    .map_err(|e| Error::Remote(e.to_string()))
            }
        })
        .await
    {
        eprintln!("gse-server: register collect_items failed: {e}");
    }

    // ——— 连接生命周期：主动探测连接活性，失活即清理本连接建立的会话 ———
    //
    // 为什么不用 `drivers.hub_driver`：**实测它不会在对端断开后 resolve**
    // （用真 agent 验证：60 秒仍未 resolve）。原因是 `DialogueHub::Router::run`
    // 的退出条件是 `inbound_rx` / `cmd_rx` 双双关闭，而 `cmd_tx` 由 `End` 持有 ——
    // 服务端自己还握着 `End`，`cmd_rx` 就不为 None。即「等 hub_driver 要先 drop End，
    // 而 drop End 前又要 await hub_driver」，构成死锁。
    //
    // 可用的活性信号是 `End::call`：对端断开后它在亚秒级返回传输层错误。
    // 这里周期性 probe：`Ok(_)`（含对端回的「未知方法」）说明连接活着；
    // 超时或传输层错误（multiplexer/end closed）说明连接已结束。
    //
    // 不这样做就会出现僵尸会话：连接早已断开，而心跳通道独立于连接存活、
    // 持续刷新 last_seen，使会话永远停在 Online；作业下发拿到它必然失败。
    let client_id = end.client_id();
    let mut consecutive_failures: u32 = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(cfg.session_probe_interval_secs.max(1))).await;
        let probe = tokio::time::timeout(
            Duration::from_secs(SESSION_PROBE_TIMEOUT_SECS),
            end.call(SESSION_PROBE_METHOD, Bytes::new()),
        )
        .await;
        let alive = match probe {
            // 正常回应（对端注册了同名方法）
            Ok(Ok(_)) => true,
            // 对端回了业务错误（未注册该方法）→ 连接是活的
            Ok(Err(e)) => {
                let msg = e.to_string();
                !(msg.contains("multiplexer closed") || msg.contains("end closed"))
            }
            // 超时：**不能直接判死** —— 连接可能只是忙。
            // 实测踩到：向 agent 传 10MB 时探测 5 秒超时，把**活着的会话**摘掉了，
            // 于是心跳继续到达（台账 online）而作业通道永久不可用。
            Err(_) => false,
        };
        if alive {
            consecutive_failures = 0;
            continue;
        }
        consecutive_failures += 1;
        // 连续 N 次失败才认定连接结束：单次超时/抖动不足以拆会话。
        if probe_should_close(
            consecutive_failures,
            cfg.session_probe_failures_before_close,
        ) {
            break;
        }
    }

    let conn_agent_id = authed.lock().ok().and_then(|g| g.clone());
    cleanup_connection(&registry, &ledger, conn_agent_id, client_id).await;
}

/// 活性探测的累计判定：**连续**失败达到阈值才认定连接结束。
///
/// 单次失败（尤其是超时）不足以拆会话 —— 连接可能只是忙于大文件传输。
fn probe_should_close(consecutive_failures: u32, threshold: u32) -> bool {
    consecutive_failures >= threshold.max(1)
}

/// 连接结束时的清理：摘掉本连接建立的会话，并（仅在真的摘掉时）把台账置离线。
///
/// 抽成独立函数是为了可测：`removed == false` 的分支是实测踩到的 bug ——
/// agent 重连后旧连接的清理把在线的 agent 误标成 offline。
async fn cleanup_connection(
    registry: &SessionRegistry,
    ledger: &Ledger,
    conn_agent_id: Option<String>,
    client_id: u64,
) {
    let Some(agent_id) = conn_agent_id else {
        println!("gse-server: unauthenticated connection ended (client_id={client_id})");
        return;
    };
    let removed = registry.remove_if_client(&agent_id, client_id).await;
    // **只有真的摘掉了会话才置 offline**：`removed == false` 说明期间 agent
    // 已重连、注册表里是新会话（别的 client_id）—— 那种情况下把台账置 offline
    // 会把在线的 agent 误标为离线。
    if removed {
        if let Err(e) = ledger.mark_offline(&agent_id).await {
            eprintln!(
                "gse-server: mark_offline {agent_id} on disconnect failed: {}",
                e.message
            );
        }
    }
    println!(
        "gse-server: agent {agent_id} connection ended (client_id={client_id}, session_removed={removed})"
    );
}

/// 连接活性探测使用的方法名。agent 不会注册它，对端会回「未知方法」——
/// 拿到任何回应都说明连接活着；超时或传输层错误才判定失活。
const SESSION_PROBE_METHOD: &str = "__vectorman_probe__";

/// Agent → Server 拉取本 Agent 应执行的采集项；未认证返回空表。
async fn handle_collect_items(conn_agent_id: Option<&str>, ledger: &Ledger) -> CollectItemsReply {
    let Some(agent_id) = conn_agent_id else {
        return CollectItemsReply::default();
    };
    match ledger.list_collect_items(Some(agent_id)).await {
        Ok(items) => CollectItemsReply {
            items: items.iter().map(|i| i.to_proto()).collect(),
        },
        Err(e) => {
            eprintln!(
                "gse-server: list collect_items for {agent_id} failed: {}",
                e.message
            );
            CollectItemsReply::default()
        }
    }
}

/// Server → Agent 推送该 Agent 过滤后的采集项整表；离线会话跳过。
pub async fn push_collect_items(ledger: &Ledger, registry: &SessionRegistry, agent_id: &str) {
    let Some(session) = registry.get(agent_id).await else {
        return;
    };
    if session.state != SessionState::Online {
        return;
    }
    let reply = handle_collect_items(Some(agent_id), ledger).await;
    let body = match serde_json::to_vec(&reply) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("gse-server: encode collect_items for {agent_id} failed: {e}");
            return;
        }
    };
    if let Err(e) = session.end.call("collect_items", Bytes::from(body)).await {
        eprintln!("gse-server: push collect_items to {agent_id} failed: {e}");
    }
}

/// 向多个 Agent 各推一份过滤后的整表，重复目标只推一次。
pub async fn push_collect_items_to(
    ledger: &Ledger,
    registry: &SessionRegistry,
    agent_ids: &[String],
) {
    let mut seen: Vec<&str> = Vec::new();
    for id in agent_ids {
        if seen.contains(&id.as_str()) {
            continue;
        }
        seen.push(id.as_str());
        push_collect_items(ledger, registry, id).await;
    }
}

/// 已认证连接拉取数据面地址；未认证或 agent_id 不符返回 `ok=false`。
async fn handle_dataplane_addr(
    req: &Bytes,
    conn_agent_id: Option<&str>,
    ledger: &Ledger,
) -> DataplaneAddrReply {
    let req: DataplaneAddrRequest = match serde_json::from_slice(req) {
        Ok(r) => r,
        Err(_) => {
            return DataplaneAddrReply {
                ok: false,
                ingest_url: None,
                host_id: None,
                reason: Some("bad dataplane_addr payload".to_string()),
            }
        }
    };
    let Some(conn_agent_id) = conn_agent_id else {
        return DataplaneAddrReply {
            ok: false,
            ingest_url: None,
            host_id: None,
            reason: Some("not authenticated".to_string()),
        };
    };
    if conn_agent_id != req.agent_id {
        return DataplaneAddrReply {
            ok: false,
            ingest_url: None,
            host_id: None,
            reason: Some("agent_id mismatch".to_string()),
        };
    }
    let host_id = match ledger.get_agent(&req.agent_id).await {
        Ok(Some(agent)) => Some(agent.host_id),
        _ => None,
    };
    match ledger.pick_ingest_url(&req.agent_id).await {
        Ok(Some(ingest_url)) => DataplaneAddrReply {
            ok: true,
            ingest_url: Some(ingest_url),
            host_id,
            reason: None,
        },
        Ok(None) => DataplaneAddrReply {
            ok: false,
            ingest_url: None,
            host_id,
            reason: Some("no online dataplane".to_string()),
        },
        Err(e) => DataplaneAddrReply {
            ok: false,
            ingest_url: None,
            host_id,
            reason: Some(e.message),
        },
    }
}

async fn handle_heartbeat(
    req: &Bytes,
    end: &End,
    client_id: u64,
    authed: &Arc<Mutex<Option<String>>>,
    registry: &SessionRegistry,
    ledger: &Ledger,
) {
    if let Ok(hb) = serde_json::from_slice::<Heartbeat>(req) {
        // 心跳到达即证明**这条连接是活的** —— 若注册表里没有该会话
        // （例如上一次活性探测误判、会话已被清理），据此重建，
        // 使作业通道自动恢复而不必等 agent 重连。
        //
        // 只对**已认证**连接重建；`Closed` 是终止态，不复活。
        let conn_agent_id = authed.lock().ok().and_then(|g| g.clone());
        if let Some(agent_id) = conn_agent_id {
            if agent_id == hb.agent_id {
                match registry.get(&agent_id).await {
                    None => {
                        let now = now_micros();
                        registry
                            .insert(Session::new(agent_id.clone(), end.clone(), now))
                            .await;
                        println!(
                            "gse-server: agent {agent_id} session rebuilt from heartbeat (client_id={client_id})"
                        );
                        if let Err(e) = ledger.mark_online(&agent_id, &now.to_string()).await {
                            eprintln!(
                                "gse-server: mark_online {agent_id} on heartbeat rebuild failed: {}",
                                e.message
                            );
                        }
                    }
                    Some(s) if s.state == SessionState::Closed => {
                        // 终止态不复活，也不 touch
                    }
                    Some(_) => {
                        registry.touch(&hb.agent_id, now_micros()).await;
                    }
                }
            } else {
                registry.touch(&hb.agent_id, now_micros()).await;
            }
        }
        let now = now_micros().to_string();
        if let Err(e) = ledger.mark_heartbeat(&hb.agent_id, &now).await {
            eprintln!(
                "gse-server: mark_heartbeat {} failed: {}",
                hb.agent_id, e.message
            );
        }
        // agent 自更新结果补报：落库并打日志（升级时作业通道已断，
        // 这是唯一的回报路径）。
        if let Some(r) = hb.upgrade_result.as_ref() {
            println!(
                "gse-server: agent {} upgrade {} ({} -> {}, detail={})",
                hb.agent_id, r.outcome, r.from_version, r.to_version, r.detail
            );
            match serde_json::to_string(r) {
                Ok(json) => {
                    if let Err(e) = ledger.mark_upgrade_result(&hb.agent_id, &json).await {
                        eprintln!(
                            "gse-server: mark_upgrade_result {} failed: {}",
                            hb.agent_id, e.message
                        );
                    }
                }
                Err(e) => eprintln!("gse-server: encode upgrade result failed: {e}"),
            }
        }
    }
}

async fn handle_auth(
    req: &Bytes,
    end: &End,
    registry: &SessionRegistry,
    cfg: &ServerConfig,
    ledger: &Ledger,
    authed: &Arc<Mutex<Option<String>>>,
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
    if let Ok(mut guard) = authed.lock() {
        *guard = Some(agent_id.clone());
    }
    if let Err(e) = ledger.mark_online(&agent_id, &now.to_string()).await {
        eprintln!("gse-server: mark_online {agent_id} failed: {}", e.message);
    }
    AuthReply {
        ok: true,
        reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gse_proto::JobStatus;

    fn tmp_db(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("gse-jobsched-{}-{tag}.db", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    async fn ledger(tag: &str) -> Arc<Ledger> {
        let ledger = Arc::new(Ledger::new(&tmp_db(tag)).expect("open ledger"));
        ledger.init().await.expect("init");
        ledger
    }

    async fn insert_pending(ledger: &Ledger, job_id: &str, agent_id: &str) {
        ledger
            .insert_job(&NewJob {
                job_id: job_id.to_string(),
                agent_id: agent_id.to_string(),
                interpreter: "bash".to_string(),
                script: "echo hi".to_string(),
                args: vec![],
                env: BTreeMap::new(),
                working_dir: None,
                template_id: None,
                rerun_of: None,
                timeout_secs: 30,
                created_at: ledger_stamp(),
                ..Default::default()
            })
            .await
            .expect("insert job");
    }

    fn succeeded(job_id: &str) -> JobResult {
        JobResult {
            job_id: job_id.to_string(),
            status: JobStatus::Succeeded,
            exit_code: Some(0),
            signal: None,
            stdout: "hi".to_string(),
            stdout_truncated: false,
            stderr: String::new(),
            stderr_truncated: false,
            started_at_micros: now_micros(),
            finished_at_micros: now_micros(),
            error: None,
        }
    }

    #[tokio::test]
    async fn job_result_from_wrong_agent_is_dropped() {
        let ledger = ledger("owner").await;
        insert_pending(&ledger, "job-1", "agent-a").await;

        handle_job_result(&ledger, "agent-b", succeeded("job-1")).await;
        assert_eq!(
            ledger.get_job("job-1").await.unwrap().unwrap().status,
            JobStatus::Pending
        );

        handle_job_result(&ledger, "agent-a", succeeded("job-1")).await;
        let job = ledger.get_job("job-1").await.unwrap().unwrap();
        assert_eq!(job.status, JobStatus::Succeeded);
        assert_eq!(job.exit_code, Some(0));
        assert_eq!(job.stdout.as_deref(), Some("hi"));
    }

    #[tokio::test]
    async fn result_writes_are_idempotent_in_terminal_state() {
        let ledger = ledger("idempotent").await;
        insert_pending(&ledger, "job-2", "agent-a").await;
        handle_job_result(&ledger, "agent-a", succeeded("job-2")).await;

        let mut second = succeeded("job-2");
        second.status = JobStatus::Failed;
        second.exit_code = Some(9);
        handle_job_result(&ledger, "agent-a", second).await;

        let job = ledger.get_job("job-2").await.unwrap().unwrap();
        assert_eq!(job.status, JobStatus::Succeeded);
        assert_eq!(job.exit_code, Some(0));
    }

    #[tokio::test]
    async fn job_result_for_unknown_job_is_noop() {
        let ledger = ledger("unknown").await;
        handle_job_result(&ledger, "agent-a", succeeded("nope")).await;
        assert!(ledger.get_job("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn dataplane_addr_requires_auth_and_online_service() {
        use crate::ledger::{Agent, DataplaneService};

        let ledger = ledger("dp-addr").await;
        ledger
            .upsert_agent(&Agent {
                agent_id: "a-1".to_string(),
                host_id: "h-1".to_string(),
                access_point_id: None,
                token: "tok".to_string(),
                version: "0.1.0".to_string(),
                install_path: "/opt/gse".to_string(),
                status: "online".to_string(),
                last_heartbeat_at: None,
                registered_at: ledger_stamp(),
            })
            .await
            .unwrap();

        let body = Bytes::from(
            serde_json::to_vec(&DataplaneAddrRequest {
                agent_id: "a-1".to_string(),
            })
            .unwrap(),
        );

        // 未认证
        let reply = handle_dataplane_addr(&body, None, &ledger).await;
        assert!(!reply.ok);
        assert_eq!(reply.reason.as_deref(), Some("not authenticated"));

        // agent_id 与会话不符
        let mismatch = Bytes::from(
            serde_json::to_vec(&DataplaneAddrRequest {
                agent_id: "a-2".to_string(),
            })
            .unwrap(),
        );
        let reply = handle_dataplane_addr(&mismatch, Some("a-1"), &ledger).await;
        assert!(!reply.ok);
        assert_eq!(reply.reason.as_deref(), Some("agent_id mismatch"));

        // 已认证但无 online 数据面
        let reply = handle_dataplane_addr(&body, Some("a-1"), &ledger).await;
        assert!(!reply.ok);
        assert_eq!(reply.reason.as_deref(), Some("no online dataplane"));
        assert_eq!(reply.host_id.as_deref(), Some("h-1"));

        // online 后返回 ingest_url 与 host_id
        ledger
            .upsert_dataplane(&DataplaneService {
                service_id: "ds-1".to_string(),
                ingest_url: "http://10.0.0.5:8081".to_string(),
                query_url: "http://10.0.0.5:9090".to_string(),
                status: "unknown".to_string(),
                last_seen_at: None,
                registered_at: ledger_stamp(),
            })
            .await
            .unwrap();
        ledger
            .set_dataplane_status("ds-1", "online", Some("t"))
            .await
            .unwrap();
        let reply = handle_dataplane_addr(&body, Some("a-1"), &ledger).await;
        assert!(reply.ok);
        assert_eq!(reply.ingest_url.as_deref(), Some("http://10.0.0.5:8081"));
        assert_eq!(reply.host_id.as_deref(), Some("h-1"));
    }

    async fn item(ledger: &Ledger, id: &str, agents: &[&str]) {
        ledger
            .upsert_collect_item(&crate::ledger::CollectItem {
                item_id: id.to_string(),
                agent_ids: agents.iter().map(|a| a.to_string()).collect(),
                name: format!("item-{id}"),
                kind: "metrics_host".to_string(),
                enabled: true,
                collector: serde_json::json!({"interval_secs": 15}),
                storage: serde_json::json!({"retention_days": 1}),
                updated_at: ledger_stamp(),
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn collect_items_pull_filters_by_agent_and_reflects_delete() {
        let ledger = ledger("pull-items").await;
        item(&ledger, "i-1", &["a-1", "a-2"]).await;
        item(&ledger, "i-2", &["a-2"]).await;

        // 未认证返回空表。
        assert!(handle_collect_items(None, &ledger).await.items.is_empty());

        // 只有目标列表包含自己的项。
        let a1 = handle_collect_items(Some("a-1"), &ledger).await;
        assert_eq!(a1.items.len(), 1);
        assert_eq!(a1.items[0].item_id, "i-1");
        let a2 = handle_collect_items(Some("a-2"), &ledger).await;
        assert_eq!(a2.items.len(), 2);
        let a3 = handle_collect_items(Some("a-3"), &ledger).await;
        assert!(a3.items.is_empty());

        // 删除 i-1 后 a-1 的剩余列表为空，a-2 只剩 i-2。
        assert!(ledger.delete_collect_item("i-1").await.unwrap());
        assert!(handle_collect_items(Some("a-1"), &ledger)
            .await
            .items
            .is_empty());
        let a2 = handle_collect_items(Some("a-2"), &ledger).await;
        assert_eq!(a2.items.len(), 1);
        assert_eq!(a2.items[0].item_id, "i-2");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn push_collect_items_reaches_online_agent() {
        use geminio::{dial, DialOptions};

        let ledger = ledger("push-items").await;
        item(&ledger, "i-1", &["a-1"]).await;
        item(&ledger, "i-2", &["a-2"]).await;

        let listener = EndListener::bind("127.0.0.1:0", ListenOptions::default())
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let client_task = tokio::spawn(async move {
            let (client, _drivers) = dial(addr, DialOptions::default()).await.expect("dial");
            client
                .register("collect_items", move |req: Bytes| {
                    let reply: CollectItemsReply =
                        serde_json::from_slice(&req).expect("decode push");
                    let _ = tx.send(reply);
                    async move { Ok(Bytes::new()) }
                })
                .await
                .expect("register");
            client
        });
        let (server_end, _drivers) = listener.accept().await.expect("accept");
        let client_end = client_task.await.expect("client");

        let registry = SessionRegistry::new();
        registry
            .insert(Session::new("a-1", server_end, now_micros()))
            .await;

        push_collect_items(&ledger, &registry, "a-1").await;
        let pushed = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("push timed out")
            .expect("push received");
        assert_eq!(pushed.items.len(), 1);
        assert_eq!(pushed.items[0].item_id, "i-1");
        assert_eq!(pushed.items[0].agent_ids, vec!["a-1"]);

        // 未知/离线会话静默跳过。
        push_collect_items(&ledger, &registry, "a-3").await;
        let _ = client_end;
    }

    /// 测试用：登记一个 agent 并置为 online。
    async fn online_agent(ledger: &Ledger, agent_id: &str) {
        ledger
            .upsert_agent(&crate::ledger::Agent {
                agent_id: agent_id.to_string(),
                host_id: "h-1".to_string(),
                access_point_id: None,
                token: "tok".to_string(),
                version: "1".to_string(),
                install_path: String::new(),
                status: "unknown".to_string(),
                last_heartbeat_at: None,
                registered_at: ledger_stamp(),
            })
            .await
            .expect("agent");
        ledger.mark_online(agent_id, "1").await.expect("online");
    }

    /// **重连接管回归**（实测踩到的 bug）：旧连接结束时，若注册表里已是
    /// 新连接建立的会话（不同 client_id），**不得**把台账置 offline ——
    /// 否则在线的 agent 会被误标离线（现象：status=offline 而 session_state=online）。
    /// **单次探测失败不得拆会话**（实测踩到：传 10MB 时探测超时，
    /// 把活着的会话摘掉了，于是心跳还在到达但作业通道永久不可用）。
    #[test]
    fn probe_requires_consecutive_failures_before_closing() {
        assert!(!probe_should_close(1, 3), "单次失败不足以拆会话");
        assert!(!probe_should_close(2, 3));
        assert!(probe_should_close(3, 3), "连续 3 次才认定连接结束");
        assert!(probe_should_close(4, 3));
        // 阈值下限为 1，配置成 0 也不至于「零次失败就拆」
        assert!(probe_should_close(1, 0));
        assert!(!probe_should_close(0, 0));
    }

    /// 心跳可**重建**会话（规格 Requirement 2）：心跳到达即证明连接活着，
    /// 据此恢复作业通道，不必等 agent 重连。
    #[tokio::test]
    async fn heartbeat_rebuilds_missing_session() {
        use geminio::{dial, DialOptions};

        let ledger = ledger("hb-rebuild").await;
        online_agent(&ledger, "a-1").await;
        let listener = EndListener::bind("127.0.0.1:0", ListenOptions::default())
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let accept_task = tokio::spawn(async move {
            let (e, _d) = listener.accept().await.expect("accept");
            e
        });
        let (_c, _cd) = dial(addr, DialOptions::default()).await.expect("dial");
        let end = accept_task.await.expect("accept task");
        let cid = end.client_id();

        let registry = SessionRegistry::new();
        let authed: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(Some("a-1".to_string())));
        // 注册表里没有会话（模拟被误清理）
        assert!(registry.get("a-1").await.is_none());

        let hb = Heartbeat {
            agent_id: "a-1".to_string(),
            ts_micros: now_micros(),
            upgrade_result: None,
        };
        let body = Bytes::from(serde_json::to_vec(&hb).expect("encode"));
        handle_heartbeat(&body, &end, cid, &authed, &registry, &ledger).await;

        let s = registry.get("a-1").await.expect("心跳应重建会话");
        assert_eq!(s.client_id, cid);
        assert_eq!(s.state, SessionState::Online);
    }

    /// 未认证连接的心跳**不得**建会话。
    #[tokio::test]
    async fn heartbeat_does_not_build_session_for_unauthenticated_connection() {
        use geminio::{dial, DialOptions};

        let ledger = ledger("hb-unauth").await;
        let listener = EndListener::bind("127.0.0.1:0", ListenOptions::default())
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let accept_task = tokio::spawn(async move {
            let (e, _d) = listener.accept().await.expect("accept");
            e
        });
        let (_c, _cd) = dial(addr, DialOptions::default()).await.expect("dial");
        let end = accept_task.await.expect("accept task");
        let cid = end.client_id();

        let registry = SessionRegistry::new();
        let authed: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let hb = Heartbeat {
            agent_id: "a-1".to_string(),
            ts_micros: now_micros(),
            upgrade_result: None,
        };
        let body = Bytes::from(serde_json::to_vec(&hb).expect("encode"));
        handle_heartbeat(&body, &end, cid, &authed, &registry, &ledger).await;
        assert!(
            registry.get("a-1").await.is_none(),
            "未认证连接的心跳不得建会话"
        );
    }

    /// `Closed` 是终止态：心跳不得复活它。
    #[tokio::test]
    async fn heartbeat_does_not_revive_closed_session() {
        use geminio::{dial, DialOptions};

        let ledger = ledger("hb-closed").await;
        online_agent(&ledger, "a-1").await;
        let listener = EndListener::bind("127.0.0.1:0", ListenOptions::default())
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let accept_task = tokio::spawn(async move {
            let (e, _d) = listener.accept().await.expect("accept");
            e
        });
        let (_c, _cd) = dial(addr, DialOptions::default()).await.expect("dial");
        let end = accept_task.await.expect("accept task");
        let cid = end.client_id();

        let registry = SessionRegistry::new();
        let mut closed = Session::new("a-1", end.clone(), now_micros());
        closed.close();
        registry.insert(closed).await;

        let authed: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(Some("a-1".to_string())));
        let hb = Heartbeat {
            agent_id: "a-1".to_string(),
            ts_micros: now_micros(),
            upgrade_result: None,
        };
        let body = Bytes::from(serde_json::to_vec(&hb).expect("encode"));
        handle_heartbeat(&body, &end, cid, &authed, &registry, &ledger).await;

        let s = registry.get("a-1").await.expect("会话仍在");
        assert_eq!(s.state, SessionState::Closed, "Closed 不得被心跳复活");
    }

    #[tokio::test]
    async fn cleanup_keeps_ledger_online_when_newer_session_took_over() {
        use geminio::{dial, DialOptions};

        let ledger = ledger("cleanup-takeover").await;
        online_agent(&ledger, "a-1").await;

        // 同一条 listener 上两条连接 → 两个不同的 client_id
        let listener = EndListener::bind("127.0.0.1:0", ListenOptions::default())
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let accept_task = tokio::spawn(async move {
            let (e1, _d1) = listener.accept().await.expect("accept 1");
            let (e2, _d2) = listener.accept().await.expect("accept 2");
            (e1, e2)
        });
        let (_c1, _cd1) = dial(addr, DialOptions::default()).await.expect("dial 1");
        let (_c2, _cd2) = dial(addr, DialOptions::default()).await.expect("dial 2");
        let (old_end, new_end) = accept_task.await.expect("accept task");
        let old_cid = old_end.client_id();
        let new_cid = new_end.client_id();
        assert_ne!(old_cid, new_cid);

        let registry = SessionRegistry::new();
        registry
            .insert(Session::new("a-1", old_end, now_micros()))
            .await;
        registry
            .insert(Session::new("a-1", new_end, now_micros()))
            .await;

        // 旧连接现在才结束 —— 会话不得被摘、台账不得被置离线
        cleanup_connection(&registry, &ledger, Some("a-1".to_string()), old_cid).await;

        let cur = registry.get("a-1").await.expect("新会话必须保留");
        assert_eq!(cur.client_id, new_cid);
        let a = ledger.get_agent("a-1").await.expect("get").expect("exists");
        assert_eq!(a.status, "online", "重连接管时不得把在线的 agent 误标离线");
    }

    /// 对照：会话确实是自己的（无接管）→ 摘掉并置 offline。
    #[tokio::test]
    async fn cleanup_removes_session_and_marks_offline_without_takeover() {
        use geminio::{dial, DialOptions};

        let ledger = ledger("cleanup-plain").await;
        online_agent(&ledger, "a-1").await;

        let listener = EndListener::bind("127.0.0.1:0", ListenOptions::default())
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let accept_task = tokio::spawn(async move {
            let (e, _d) = listener.accept().await.expect("accept");
            e
        });
        let (_c, _cd) = dial(addr, DialOptions::default()).await.expect("dial");
        let end = accept_task.await.expect("accept task");
        let cid = end.client_id();

        let registry = SessionRegistry::new();
        registry
            .insert(Session::new("a-1", end, now_micros()))
            .await;

        cleanup_connection(&registry, &ledger, Some("a-1".to_string()), cid).await;

        assert!(registry.get("a-1").await.is_none(), "会话应被摘掉");
        let a = ledger.get_agent("a-1").await.expect("get").expect("exists");
        assert_eq!(a.status, "offline");
    }

    /// 未认证连接结束时只打日志，不动任何会话。
    #[tokio::test]
    async fn cleanup_ignores_unauthenticated_connection() {
        let ledger = ledger("cleanup-unauth").await;
        online_agent(&ledger, "a-1").await;

        let registry = SessionRegistry::new();
        cleanup_connection(&registry, &ledger, None, 1).await;

        let a = ledger.get_agent("a-1").await.expect("get").expect("exists");
        assert_eq!(a.status, "online", "未认证连接的结束不得影响台账");
    }

    #[tokio::test]
    async fn submit_job_rejects_offline_agent() {
        let cfg = ServerConfig {
            listen: "127.0.0.1:0".to_string(),
            db: tmp_db("offline"),
            auth_enabled: false,
            http_enabled: false,
            ..Default::default()
        };
        let (server, _addr) = Server::bind(cfg).await.expect("bind");
        let err = server
            .submit_job(JobSubmit {
                kind: gse_proto::default_job_kind(),
                agent_id: "ghost".to_string(),
                interpreter: None,
                script: "echo hi".to_string(),
                args: vec![],
                env: BTreeMap::new(),
                working_dir: None,
                timeout_secs: None,
            })
            .await
            .expect_err("offline agent must be rejected");
        assert_eq!(err.code, "unavailable");
        // 从未登记过该 agent ⇒ 措辞是 agent offline（不是 session unavailable）。
        assert!(
            err.message.contains("agent offline"),
            "未登记过的 agent 应报 agent offline，实际: {}",
            err.message
        );
    }

    /// 措辞区分：心跳在窗口内但会话不存在 ⇒ `session unavailable`。
    ///
    /// 这正是本次事故的形态 —— 此前一律报 `agent offline`，
    /// 把「连接已死」误报成「Agent 离线」，误导排查。
    #[tokio::test]
    async fn submit_job_reports_session_unavailable_when_heartbeat_is_fresh() {
        let cfg = ServerConfig {
            listen: "127.0.0.1:0".to_string(),
            db: tmp_db("session-unavail"),
            auth_enabled: false,
            http_enabled: false,
            ..Default::default()
        };
        let (server, _addr) = Server::bind(cfg).await.expect("bind");
        // 心跳新鲜（刚刚上报）但注册表里没有会话 → 连接已死。
        let now = now_micros().to_string();
        server
            .ledger
            .upsert_agent(&crate::ledger::Agent {
                agent_id: "zombie".to_string(),
                host_id: "h-1".to_string(),
                access_point_id: None,
                token: "tok".to_string(),
                version: "1".to_string(),
                install_path: String::new(),
                status: "online".to_string(),
                last_heartbeat_at: Some(now.clone()),
                registered_at: now,
            })
            .await
            .expect("agent");

        let err = server
            .submit_job(JobSubmit {
                kind: gse_proto::default_job_kind(),
                agent_id: "zombie".to_string(),
                interpreter: None,
                script: "echo hi".to_string(),
                args: vec![],
                env: BTreeMap::new(),
                working_dir: None,
                timeout_secs: None,
            })
            .await
            .expect_err("no session must be rejected");
        assert_eq!(err.code, "unavailable");
        assert!(
            err.message.contains("session unavailable"),
            "心跳新鲜但无会话应报 session unavailable（僵尸会话形态），实际: {}",
            err.message
        );
        assert!(
            !err.message.contains("agent offline"),
            "不得把连接已死误报为 agent offline，实际: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn submit_job_validates_script_and_timeout() {
        let cfg = ServerConfig {
            listen: "127.0.0.1:0".to_string(),
            db: tmp_db("validate"),
            auth_enabled: false,
            http_enabled: false,
            job_max_script_bytes: 8,
            ..Default::default()
        };
        let (server, _addr) = Server::bind(cfg).await.expect("bind");

        let empty = server
            .submit_job(JobSubmit {
                kind: gse_proto::default_job_kind(),
                agent_id: "ghost".to_string(),
                interpreter: None,
                script: "  ".to_string(),
                args: vec![],
                env: BTreeMap::new(),
                working_dir: None,
                timeout_secs: None,
            })
            .await
            .expect_err("empty script");
        assert_eq!(empty.code, "invalid_argument");

        let too_long = server
            .submit_job(JobSubmit {
                kind: gse_proto::default_job_kind(),
                agent_id: "ghost".to_string(),
                interpreter: None,
                script: "123456789".to_string(),
                args: vec![],
                env: BTreeMap::new(),
                working_dir: None,
                timeout_secs: None,
            })
            .await
            .expect_err("script too long");
        assert_eq!(too_long.code, "invalid_argument");

        let bad_timeout = server
            .submit_job(JobSubmit {
                kind: gse_proto::default_job_kind(),
                agent_id: "ghost".to_string(),
                interpreter: None,
                script: "hi".to_string(),
                args: vec![],
                env: BTreeMap::new(),
                working_dir: None,
                timeout_secs: Some(0),
            })
            .await
            .expect_err("timeout zero");
        assert_eq!(bad_timeout.code, "invalid_argument");
    }

    #[tokio::test]
    async fn submit_job_disabled_returns_unavailable() {
        let cfg = ServerConfig {
            listen: "127.0.0.1:0".to_string(),
            db: tmp_db("disabled"),
            auth_enabled: false,
            http_enabled: false,
            jobs_enabled: false,
            ..Default::default()
        };
        let (server, _addr) = Server::bind(cfg).await.expect("bind");
        let err = server
            .submit_job(JobSubmit {
                kind: gse_proto::default_job_kind(),
                agent_id: "ghost".to_string(),
                interpreter: None,
                script: "echo hi".to_string(),
                args: vec![],
                env: BTreeMap::new(),
                working_dir: None,
                timeout_secs: None,
            })
            .await
            .expect_err("jobs disabled");
        assert_eq!(err.code, "unavailable");
    }
}
