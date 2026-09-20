//! 文件作业编排：校验提交、后台分块中转。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use geminio::Bytes;
use gse_proto::{
    FileEndpoint, FileReadReply, FileReadReq, FileWriteReply, FileWriteReq, GseError, JobResult,
    JobStatus,
};
use serde::Deserialize;

use crate::config::ServerConfig;
use crate::hashutil::{b64_encode, sha256_hex};
use crate::job_file_store::JobFileStore;
use crate::ledger::{JobRecord, Ledger, NewJob};
use crate::server::new_job_id;
use crate::session::{now_micros, Session, SessionRegistry, SessionState};

#[derive(Debug, Clone, Deserialize)]
pub struct FileJobSubmit {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub source: Option<FileEndpoint>,
    #[serde(default)]
    pub destination: Option<FileEndpoint>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

struct TransferOutcome {
    file_name: String,
    file_bytes: i64,
    file_sha256: String,
    file_id: Option<String>,
}

fn missing(field: &str) -> GseError {
    GseError::new(
        "invalid_argument",
        format!("missing required field: {field}"),
    )
}

fn validate_agent_path(path: &str) -> Result<(), GseError> {
    if path.trim().is_empty() {
        return Err(missing("path"));
    }
    if !Path::new(path).is_absolute() {
        return Err(GseError::new("invalid_argument", "path must be absolute"));
    }
    Ok(())
}

fn list_primary(source: &FileEndpoint, dest: &FileEndpoint) -> String {
    dest.agent_id()
        .or_else(|| source.agent_id())
        .unwrap_or("server")
        .to_string()
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "file".to_string())
}

async fn require_online(registry: &SessionRegistry, agent_id: &str) -> Result<(), GseError> {
    match registry.get(agent_id).await {
        Some(s) if s.state == SessionState::Online => Ok(()),
        _ => Err(GseError::new(
            "unavailable",
            format!("agent {agent_id} not online"),
        )),
    }
}

fn reply_err(err: String) -> GseError {
    let code = err.split(':').next().unwrap_or("").trim().to_string();
    let code = if code.is_empty() {
        "rpc_error".to_string()
    } else {
        code
    };
    GseError::new(code, err)
}

/// 提交文件作业：落库 pending 后后台传输，立即返回当前记录。
pub async fn submit_file_job(
    ledger: Arc<Ledger>,
    registry: Arc<SessionRegistry>,
    cfg: Arc<ServerConfig>,
    store: Arc<JobFileStore>,
    req: FileJobSubmit,
    rerun_of: Option<String>,
) -> Result<JobRecord, GseError> {
    if !cfg.jobs_enabled {
        return Err(GseError::new("unavailable", "jobs disabled"));
    }
    let source = req.source.clone().ok_or_else(|| missing("source"))?;
    let destination = req
        .destination
        .clone()
        .ok_or_else(|| missing("destination"))?;
    validate_endpoint("source", &source, true)?;
    validate_endpoint("destination", &destination, false)?;

    match (&source, &destination) {
        (FileEndpoint::ServerTemp { .. }, FileEndpoint::ServerTemp { .. }) => {
            return Err(GseError::new(
                "invalid_argument",
                "copying between server temp files is not supported",
            ));
        }
        (
            FileEndpoint::Agent {
                agent_id: a,
                path: p,
            },
            FileEndpoint::Agent {
                agent_id: b,
                path: q,
            },
        ) if a == b && p == q => {
            return Err(GseError::new(
                "invalid_argument",
                "source and destination are the same agent path",
            ));
        }
        _ => {}
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

    for id in [source.agent_id(), destination.agent_id()]
        .into_iter()
        .flatten()
    {
        require_online(&registry, id).await?;
    }

    if let FileEndpoint::ServerTemp { file_id: Some(id) } = &source {
        store.head(id)?;
    }

    let job_id = new_job_id();
    let created_at = now_micros().to_string();
    let agent_id = list_primary(&source, &destination);
    let source_agent_id = source.agent_id().map(str::to_string);
    let dest_agent_id = destination.agent_id().map(str::to_string);
    ledger
        .insert_job(&NewJob {
            job_id: job_id.clone(),
            agent_id,
            interpreter: String::new(),
            script: String::new(),
            timeout_secs: timeout,
            created_at,
            kind: "file_transfer".to_string(),
            source: Some(source),
            destination: Some(destination),
            source_agent_id,
            dest_agent_id,
            rerun_of,
            ..Default::default()
        })
        .await?;

    let bg_ledger = ledger.clone();
    let bg_registry = registry.clone();
    let bg_cfg = cfg.clone();
    let bg_store = store.clone();
    let bg_id = job_id.clone();
    tokio::spawn(async move {
        run_file_transfer(bg_ledger, bg_registry, bg_cfg, bg_store, bg_id).await;
    });

    ledger
        .get_job(&job_id)
        .await?
        .ok_or_else(|| GseError::new("not_found", format!("job {job_id} vanished")))
}

fn validate_endpoint(role: &str, ep: &FileEndpoint, source: bool) -> Result<(), GseError> {
    match ep {
        FileEndpoint::Agent { agent_id, path } => {
            if agent_id.trim().is_empty() {
                return Err(missing(&format!("{role}.agent_id")));
            }
            validate_agent_path(path)?;
            Ok(())
        }
        FileEndpoint::ServerTemp { file_id } => {
            if source {
                match file_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    Some(_) => Ok(()),
                    None => Err(missing("source.file_id")),
                }
            } else {
                Ok(())
            }
        }
    }
}

pub async fn run_file_transfer(
    ledger: Arc<Ledger>,
    registry: Arc<SessionRegistry>,
    cfg: Arc<ServerConfig>,
    store: Arc<JobFileStore>,
    job_id: String,
) {
    let now = now_micros().to_string();
    if let Err(e) = ledger.mark_dispatched(&job_id, &now).await {
        eprintln!("gse-server: mark_dispatched {job_id} failed: {}", e.message);
        return;
    }
    let job = match ledger.get_job(&job_id).await {
        Ok(Some(j)) => j,
        Ok(None) => return,
        Err(e) => {
            eprintln!("gse-server: get job {job_id} failed: {}", e.message);
            return;
        }
    };
    let timeout = Duration::from_secs(job.timeout_secs.max(1));
    let result = tokio::time::timeout(
        timeout,
        transfer_job(&ledger, &registry, &cfg, &store, &job),
    )
    .await;
    match result {
        Ok(Ok(out)) => {
            let _ = ledger
                .set_job_file_meta(
                    &job_id,
                    Some(&out.file_name),
                    Some(out.file_bytes),
                    Some(&out.file_sha256),
                    out.file_id.as_deref(),
                )
                .await;
            let _ = finish_job_status(&ledger, &job_id, JobStatus::Succeeded, None).await;
        }
        Ok(Err(e)) => {
            let status = match e.code.as_str() {
                "lost" => JobStatus::Lost,
                "timeout" => JobStatus::Timeout,
                _ => JobStatus::Failed,
            };
            let _ = finish_job_status(&ledger, &job_id, status, Some(e.message)).await;
        }
        Err(_) => {
            let _ = finish_job_status(
                &ledger,
                &job_id,
                JobStatus::Timeout,
                Some("transfer timed out".to_string()),
            )
            .await;
        }
    }
}

async fn finish_job_status(
    ledger: &Ledger,
    job_id: &str,
    status: JobStatus,
    error: Option<String>,
) -> Result<(), GseError> {
    ledger
        .finish_job(
            &JobResult {
                job_id: job_id.to_string(),
                status,
                exit_code: None,
                signal: None,
                stdout: String::new(),
                stdout_truncated: false,
                stderr: String::new(),
                stderr_truncated: false,
                started_at_micros: 0,
                finished_at_micros: now_micros(),
                error,
            },
            &now_micros().to_string(),
        )
        .await
}

async fn transfer_job(
    ledger: &Ledger,
    registry: &SessionRegistry,
    cfg: &ServerConfig,
    store: &JobFileStore,
    job: &JobRecord,
) -> Result<TransferOutcome, GseError> {
    let source = job.source.clone().ok_or_else(|| missing("source"))?;
    let dest = job
        .destination
        .clone()
        .ok_or_else(|| missing("destination"))?;
    let deadline = Instant::now() + Duration::from_secs(job.timeout_secs.max(1));
    let chunk = cfg.job_file_chunk_bytes.max(1);
    let max_bytes = cfg.job_max_file_bytes;

    match (&source, &dest) {
        (
            FileEndpoint::Agent { agent_id, path },
            FileEndpoint::Agent {
                agent_id: da,
                path: dp,
            },
        ) => {
            pump_agent_to_agent(
                ledger, registry, job, agent_id, path, da, dp, chunk, max_bytes, deadline,
            )
            .await
        }
        (FileEndpoint::Agent { agent_id, path }, FileEndpoint::ServerTemp { .. }) => {
            pump_agent_to_temp(
                ledger, registry, store, job, agent_id, path, chunk, max_bytes, deadline,
            )
            .await
        }
        (FileEndpoint::ServerTemp { file_id }, FileEndpoint::Agent { agent_id, path }) => {
            let id = file_id
                .as_deref()
                .ok_or_else(|| missing("source.file_id"))?;
            pump_temp_to_agent(
                ledger, registry, store, job, id, agent_id, path, chunk, max_bytes, deadline,
            )
            .await
        }
        (FileEndpoint::ServerTemp { .. }, FileEndpoint::ServerTemp { .. }) => Err(GseError::new(
            "invalid_argument",
            "copying between server temp files is not supported",
        )),
    }
}

#[allow(clippy::too_many_arguments)]
async fn pump_agent_to_agent(
    ledger: &Ledger,
    registry: &SessionRegistry,
    job: &JobRecord,
    src_agent: &str,
    src_path: &str,
    dst_agent: &str,
    dst_path: &str,
    chunk: u64,
    max_bytes: u64,
    deadline: Instant,
) -> Result<TransferOutcome, GseError> {
    let mut offset = 0u64;
    let mut size = 0u64;
    let mut running = false;
    let file_sha256;
    loop {
        let read = file_read(
            registry,
            src_agent,
            &job.job_id,
            src_path,
            offset,
            chunk,
            max_bytes,
            deadline,
        )
        .await?;
        if !running {
            ledger
                .mark_running(&job.job_id, &now_micros().to_string())
                .await?;
            running = true;
            size = read.size;
            if size > max_bytes {
                return Err(GseError::new(
                    "file_too_large",
                    format!("file exceeds {max_bytes} bytes"),
                ));
            }
        }
        let eof = read.eof;
        let sha = read.file_sha256.clone();
        file_write(
            registry,
            dst_agent,
            &job.job_id,
            dst_path,
            offset,
            eof,
            &read.data_b64,
            &read.chunk_sha256,
            if eof { sha.clone() } else { None },
            deadline,
        )
        .await?;
        if eof {
            file_sha256 = sha.unwrap_or_default();
            break;
        }
        let n = decoded_len(&read.data_b64)?;
        if n == 0 {
            return Err(GseError::new("rpc_error", "empty chunk before eof"));
        }
        offset += n;
    }
    Ok(TransferOutcome {
        file_name: basename(src_path),
        file_bytes: size as i64,
        file_sha256,
        file_id: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn pump_agent_to_temp(
    ledger: &Ledger,
    registry: &SessionRegistry,
    store: &JobFileStore,
    job: &JobRecord,
    src_agent: &str,
    src_path: &str,
    chunk: u64,
    max_bytes: u64,
    deadline: Instant,
) -> Result<TransferOutcome, GseError> {
    let dest_id = format!("file-{}", job.job_id);
    let file_name = basename(src_path);
    let mut writer = store.begin_write(&dest_id, &file_name)?;
    let mut offset = 0u64;
    let mut running = false;
    let file_sha256;
    loop {
        let read = match file_read(
            registry,
            src_agent,
            &job.job_id,
            src_path,
            offset,
            chunk,
            max_bytes,
            deadline,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                writer.abort();
                return Err(e);
            }
        };
        if !running {
            if let Err(e) = ledger
                .mark_running(&job.job_id, &now_micros().to_string())
                .await
            {
                writer.abort();
                return Err(e);
            }
            running = true;
            if read.size > max_bytes {
                writer.abort();
                return Err(GseError::new(
                    "file_too_large",
                    format!("file exceeds {max_bytes} bytes"),
                ));
            }
        }
        let data = match crate::hashutil::b64_decode(&read.data_b64) {
            Ok(d) => d,
            Err(e) => {
                writer.abort();
                return Err(GseError::new("invalid_argument", e));
            }
        };
        if let Err(e) = writer.write_all(&data) {
            writer.abort();
            return Err(e);
        }
        if read.eof {
            file_sha256 = read.file_sha256.unwrap_or_default();
            break;
        }
        if data.is_empty() {
            writer.abort();
            return Err(GseError::new("rpc_error", "empty chunk before eof"));
        }
        offset += data.len() as u64;
    }
    let meta = writer.finish()?;
    if !file_sha256.is_empty() && meta.sha256 != file_sha256 {
        let _ = store.delete(&dest_id);
        return Err(GseError::new("checksum_mismatch", "checksum mismatch"));
    }
    Ok(TransferOutcome {
        file_name: meta.file_name,
        file_bytes: meta.size_bytes as i64,
        file_sha256: meta.sha256,
        file_id: Some(meta.file_id),
    })
}

#[allow(clippy::too_many_arguments)]
async fn pump_temp_to_agent(
    ledger: &Ledger,
    registry: &SessionRegistry,
    store: &JobFileStore,
    job: &JobRecord,
    file_id: &str,
    dst_agent: &str,
    dst_path: &str,
    chunk: u64,
    max_bytes: u64,
    deadline: Instant,
) -> Result<TransferOutcome, GseError> {
    let meta = store.head(file_id)?;
    if meta.size_bytes > max_bytes {
        return Err(GseError::new(
            "file_too_large",
            format!("file exceeds {max_bytes} bytes"),
        ));
    }
    let path = store.content_path(file_id)?;
    let mut file = File::open(&path).map_err(|e| GseError::new("rpc_error", e.to_string()))?;
    ledger
        .mark_running(&job.job_id, &now_micros().to_string())
        .await?;
    let mut offset = 0u64;
    let mut buf = vec![0u8; chunk as usize];
    loop {
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| GseError::new("rpc_error", e.to_string()))?;
        let n = file
            .read(&mut buf)
            .map_err(|e| GseError::new("rpc_error", e.to_string()))?;
        let eof = offset + n as u64 >= meta.size_bytes;
        let data = &buf[..n];
        file_write(
            registry,
            dst_agent,
            &job.job_id,
            dst_path,
            offset,
            eof,
            &b64_encode(data),
            &sha256_hex(data),
            if eof { Some(meta.sha256.clone()) } else { None },
            deadline,
        )
        .await?;
        if eof {
            break;
        }
        if n == 0 {
            return Err(GseError::new("rpc_error", "empty chunk before eof"));
        }
        offset += n as u64;
    }
    Ok(TransferOutcome {
        file_name: meta.file_name,
        file_bytes: meta.size_bytes as i64,
        file_sha256: meta.sha256,
        file_id: Some(file_id.to_string()),
    })
}

fn decoded_len(b64: &str) -> Result<u64, GseError> {
    crate::hashutil::b64_decode(b64)
        .map(|d| d.len() as u64)
        .map_err(|e| GseError::new("invalid_argument", e))
}

fn chunk_timeout(deadline: Instant) -> Result<Duration, GseError> {
    let rem = deadline.saturating_duration_since(Instant::now());
    if rem.is_zero() {
        return Err(GseError::new("timeout", "transfer timed out"));
    }
    Ok(rem.min(Duration::from_secs(30)))
}

async fn online_session(registry: &SessionRegistry, agent_id: &str) -> Result<Session, GseError> {
    match registry.get(agent_id).await {
        Some(s) if s.state == SessionState::Online => Ok(s),
        _ => Err(GseError::new("lost", format!("agent {agent_id} offline"))),
    }
}

#[allow(clippy::too_many_arguments)]
async fn file_read(
    registry: &SessionRegistry,
    agent_id: &str,
    job_id: &str,
    path: &str,
    offset: u64,
    length: u64,
    max_bytes: u64,
    deadline: Instant,
) -> Result<FileReadReply, GseError> {
    let session = online_session(registry, agent_id).await?;
    let req = FileReadReq {
        job_id: job_id.to_string(),
        path: path.to_string(),
        offset,
        length,
        max_bytes,
    };
    let body =
        serde_json::to_vec(&req).map_err(|e| GseError::new("invalid_argument", e.to_string()))?;
    let reply: FileReadReply = rpc(&session, "file_read", &body, chunk_timeout(deadline)?).await?;
    if let Some(err) = reply.error {
        return Err(reply_err(err));
    }
    Ok(reply)
}

#[allow(clippy::too_many_arguments)]
async fn file_write(
    registry: &SessionRegistry,
    agent_id: &str,
    job_id: &str,
    path: &str,
    offset: u64,
    eof: bool,
    data_b64: &str,
    chunk_sha256: &str,
    file_sha256: Option<String>,
    deadline: Instant,
) -> Result<FileWriteReply, GseError> {
    let session = online_session(registry, agent_id).await?;
    let req = FileWriteReq {
        job_id: job_id.to_string(),
        path: path.to_string(),
        offset,
        eof,
        data_b64: data_b64.to_string(),
        chunk_sha256: chunk_sha256.to_string(),
        file_sha256,
    };
    let body =
        serde_json::to_vec(&req).map_err(|e| GseError::new("invalid_argument", e.to_string()))?;
    let reply: FileWriteReply =
        rpc(&session, "file_write", &body, chunk_timeout(deadline)?).await?;
    if let Some(err) = reply.error {
        return Err(reply_err(err));
    }
    Ok(reply)
}

async fn rpc<T: serde::de::DeserializeOwned>(
    session: &Session,
    method: &str,
    body: &[u8],
    timeout: Duration,
) -> Result<T, GseError> {
    match tokio::time::timeout(
        timeout,
        session.end.call(method, Bytes::copy_from_slice(body)),
    )
    .await
    {
        Ok(Ok(resp)) => {
            serde_json::from_slice(&resp).map_err(|e| GseError::new("rpc_error", e.to_string()))
        }
        Ok(Err(e)) => Err(GseError::new("lost", e.to_string())),
        Err(_) => Err(GseError::new("rpc_error", format!("{method} timed out"))),
    }
}

/// 以来源文件作业为默认值合并覆盖后再次提交。
#[allow(clippy::too_many_arguments)]
pub async fn submit_file_rerun(
    ledger: Arc<Ledger>,
    registry: Arc<SessionRegistry>,
    cfg: Arc<ServerConfig>,
    store: Arc<JobFileStore>,
    source: &JobRecord,
    timeout_secs: Option<u64>,
    dest_agent_id: Option<String>,
    dest_path: Option<String>,
) -> Result<JobRecord, GseError> {
    let src = source.source.clone().ok_or_else(|| missing("source"))?;
    let mut dest = source
        .destination
        .clone()
        .ok_or_else(|| missing("destination"))?;
    if let FileEndpoint::ServerTemp { file_id } = &src {
        let id = file_id
            .as_deref()
            .ok_or_else(|| missing("source.file_id"))?;
        store.head(id)?;
    }
    if let FileEndpoint::Agent { agent_id, path } = &mut dest {
        if let Some(id) = dest_agent_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            *agent_id = id.to_string();
        }
        if let Some(p) = dest_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            *path = p.to_string();
        }
    }
    if let FileEndpoint::ServerTemp { file_id } = &mut dest {
        *file_id = None;
    }
    submit_file_job(
        ledger,
        registry,
        cfg,
        store,
        FileJobSubmit {
            kind: "file_transfer".to_string(),
            source: Some(src),
            destination: Some(dest),
            timeout_secs: Some(timeout_secs.unwrap_or(source.timeout_secs)),
        },
        Some(source.job_id.clone()),
    )
    .await
}

pub async fn run_job_file_cleanup(store: Arc<JobFileStore>, interval: Duration, retain_secs: u64) {
    loop {
        tokio::time::sleep(interval).await;
        if let Err(e) = store.delete_expired(now_micros(), retain_secs) {
            eprintln!("gse-server: job file cleanup failed: {}", e.message);
        }
    }
}
