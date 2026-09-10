use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use geminio::app::Error;
use geminio::{Bytes, End, EndListener, ListenOptions};
use gse_proto::{
    AuthReply, AuthRequest, Command, GseError, Heartbeat, JobAck, JobExec, JobResult, Receipt,
};

use crate::config::ServerConfig;
use crate::http;
use crate::ledger::{ledger_stamp, AccessPoint, JobRecord, Ledger, NewJob};
use crate::rerun::{build_rerun_submit, RerunRequest};
use crate::session::{now_micros, Session, SessionRegistry, SessionState};

const LIVENESS_SCAN_INTERVAL_SECS: u64 = 5;
const COMMAND_TIMEOUT_SECS: u64 = 60;

static CMD_SEQ: AtomicU64 = AtomicU64::new(0);
static JOB_SEQ: AtomicU64 = AtomicU64::new(0);

/// HTTP 提交作业的请求体；省略的解释器由 server 默认填充。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct JobSubmit {
    pub agent_id: String,
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

fn new_job_id() -> String {
    let seq = JOB_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("job-{}-{}", now_micros(), seq)
}

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
                cfg: Some(self.cfg.clone()),
            };
            let listen = self.cfg.http_listen.clone();
            let web_dir = self.cfg.http_web_dir.clone();
            tokio::spawn(async move {
                if let Err(e) = http::serve(admin, &listen, web_dir).await {
                    eprintln!("gse-server: http management failed: {:?}", e);
                }
            });
        }
        loop {
            match self.listener.accept().await {
                Ok((end, _drivers)) => {
                    let registry = self.registry.clone();
                    let cfg = self.cfg.clone();
                    let ledger = self.ledger.clone();
                    tokio::spawn(handle_conn(end, registry, cfg, ledger));
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
            return Err(GseError::new(
                "unavailable",
                format!("agent {} not online", req.agent_id),
            ))
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
        })
        .await?;

    let exec = JobExec {
        job_id: job_id.clone(),
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
            ledger.mark_lost_by_agent(agent_id).await?;
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
            ledger.mark_lost_by_agent(agent_id).await?;
        }
        Err(_) => {
            eprintln!("gse-server: job_exec {job_id} timed out");
            ledger.mark_lost_by_agent(agent_id).await?;
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
                if let Err(e) = ledger.mark_lost_by_agent(&session.agent_id).await {
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
