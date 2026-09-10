//! 作业执行器：受理 Server 下发的 `job_exec`，在本地执行脚本并通过
//! `job_result` 回传完整结果。单个 Agent 的并发由信号量限制为 1（可配置）。

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use geminio::{Bytes, End};
use gse_proto::{JobAck, JobExec, JobResult, JobStatus};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::AgentConfig;

/// 执行器运行参数，来源于 AgentConfig 的作业相关字段。
#[derive(Debug, Clone)]
pub struct JobConfig {
    pub allowed_interpreters: Vec<String>,
    pub default_interpreter: String,
    pub max_concurrent_jobs: usize,
    pub work_dir: Option<String>,
}

impl JobConfig {
    pub fn from_agent(cfg: &AgentConfig) -> Self {
        Self {
            allowed_interpreters: cfg.allowed_interpreters.clone(),
            default_interpreter: cfg.job_default_interpreter.clone(),
            max_concurrent_jobs: cfg.max_concurrent_jobs,
            work_dir: cfg.job_work_dir.clone(),
        }
    }
}

/// 作业执行器。`handle_exec` 快速返回受理结果，实际执行在后台任务中完成。
#[derive(Clone)]
pub struct JobExecutor {
    cfg: Arc<JobConfig>,
    permits: Arc<Semaphore>,
}

impl JobExecutor {
    pub fn new(cfg: JobConfig) -> Self {
        let permits = Arc::new(Semaphore::new(cfg.max_concurrent_jobs.max(1)));
        Self {
            cfg: Arc::new(cfg),
            permits,
        }
    }

    fn interpreter_allowed(&self, interpreter: &str) -> bool {
        self.cfg
            .allowed_interpreters
            .iter()
            .any(|i| i == interpreter)
    }

    /// 校验并受理一个作业：解释器白名单、并发上限。受理成功后派生后台任务执行，
    /// 完成后调用 Server 的 `job_result`。
    pub async fn handle_exec(&self, req: &Bytes, end: End) -> JobAck {
        let mut exec: JobExec = match serde_json::from_slice(req) {
            Ok(v) => v,
            Err(e) => {
                return JobAck {
                    job_id: String::new(),
                    accepted: false,
                    reason: Some(format!("bad job payload: {e}")),
                }
            }
        };
        if exec.interpreter.is_empty() {
            exec.interpreter = self.cfg.default_interpreter.clone();
        }
        if !self.interpreter_allowed(&exec.interpreter) {
            return JobAck {
                job_id: exec.job_id,
                accepted: false,
                reason: Some("interpreter_not_allowed".to_string()),
            };
        }
        let permit = match self.permits.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                return JobAck {
                    job_id: exec.job_id,
                    accepted: false,
                    reason: Some("busy".to_string()),
                }
            }
        };
        let cfg = self.cfg.clone();
        let ack_job_id = exec.job_id.clone();
        tokio::spawn(async move {
            let result = run_job(&cfg, exec).await;
            send_result(&end, &result, &result.job_id).await;
            drop(permit);
        });
        JobAck {
            job_id: ack_job_id,
            accepted: true,
            reason: None,
        }
    }
}

async fn send_result(end: &End, result: &JobResult, job_id: &str) {
    match serde_json::to_vec(result) {
        Ok(body) => {
            if let Err(e) = end.call("job_result", Bytes::from(body)).await {
                eprintln!("gse-agent: job_result {job_id} failed: {e}");
            }
        }
        Err(e) => eprintln!("gse-agent: encode job_result {job_id} failed: {e}"),
    }
}

/// 执行脚本并采集结果。所有失败路径均返回终态 JobResult，不再向上传播错误。
async fn run_job(cfg: &JobConfig, exec: JobExec) -> JobResult {
    let started = now_micros();
    let script_path = match write_script(cfg, &exec) {
        Ok(p) => p,
        Err(e) => {
            return base_result(&exec, JobStatus::Failed, started)
                .error(format!("write script failed: {e}"))
        }
    };
    let mut command = Command::new(&exec.interpreter);
    command.arg(&script_path);
    command.args(&exec.args);
    if let Some(dir) = &exec.working_dir {
        command.current_dir(dir);
    }
    for (k, v) in &exec.env {
        command.env(k, v);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        // SAFETY: setsid 在子进程 fork 后、exec 前调用，只影响当前进程组。
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            cleanup(&script_path);
            return base_result(&exec, JobStatus::Failed, started)
                .error(format!("spawn failed: {e}"));
        }
    };

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let out_task = tokio::spawn(read_capped(stdout, exec.stdout_limit_bytes as usize));
    let err_task = tokio::spawn(read_capped(stderr, exec.stderr_limit_bytes as usize));

    let timeout = Duration::from_secs(exec.timeout_secs.max(1));
    let (status, exit_code, signal, mut error) = match tokio::time::timeout(timeout, child.wait()).await
    {
        Ok(Ok(es)) => finish_from_exit(&es),
        Ok(Err(e)) => (JobStatus::Failed, None, None, Some(format!("wait failed: {e}"))),
        Err(_) => terminate(&mut child).await,
    };

    let (stdout, stdout_truncated) = join_capped(out_task).await;
    let (stderr, stderr_truncated) = join_capped(err_task).await;
    cleanup(&script_path);

    let mut result = base_result(&exec, status, started);
    result.exit_code = exit_code;
    result.signal = signal;
    result.stdout = stdout;
    result.stdout_truncated = stdout_truncated;
    result.stderr = stderr;
    result.stderr_truncated = stderr_truncated;
    result.error = error.take();
    result
}

/// 超时处理：先向整个进程组发 SIGTERM，宽限 5s 后 SIGKILL。
/// 发送给进程组（负 pid）以覆盖脚本派生的子进程，避免其持有管道导致读取阻塞。
async fn terminate(child: &mut tokio::process::Child) -> (JobStatus, Option<i32>, Option<i32>, Option<String>) {
    if let Some(pid) = child.id() {
        kill_group(pid, libc::SIGTERM);
    }
    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(es)) => {
            let (_, code, sig, _) = finish_from_exit(&es);
            (JobStatus::Timeout, code, sig, Some("timeout".to_string()))
        }
        _ => {
            if let Some(pid) = child.id() {
                kill_group(pid, libc::SIGKILL);
            }
            let es = child.wait().await.ok();
            let code = es.as_ref().and_then(|e| e.code());
            let sig = es.as_ref().and_then(exit_signal);
            (JobStatus::Timeout, code, sig, Some("timeout".to_string()))
        }
    }
}

#[cfg(unix)]
fn kill_group(pid: u32, signal: i32) {
    // SAFETY: kill 仅发送信号，不访问进程内存；负 pid 表示进程组。
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: u32, _signal: i32) {}

/// 等待输出采集任务；正常情况下子进程结束后管道立即 EOF。
/// 若脚本派生的守护进程逃逸出进程组并持续持有管道，则限时放弃以避免挂起。
async fn join_capped(task: tokio::task::JoinHandle<(String, bool)>) -> (String, bool) {
    match tokio::time::timeout(Duration::from_secs(2), task).await {
        Ok(Ok(v)) => v,
        Ok(Err(_)) => (String::new(), false),
        Err(_) => (String::new(), true),
    }
}

fn finish_from_exit(es: &std::process::ExitStatus) -> (JobStatus, Option<i32>, Option<i32>, Option<String>) {
    if let Some(code) = es.code() {
        let status = if code == 0 {
            JobStatus::Succeeded
        } else {
            JobStatus::Failed
        };
        (status, Some(code), None, None)
    } else {
        (
            JobStatus::Failed,
            None,
            exit_signal(es),
            Some("terminated by signal".to_string()),
        )
    }
}

#[cfg(unix)]
fn exit_signal(es: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    es.signal()
}

#[cfg(not(unix))]
fn exit_signal(_es: &std::process::ExitStatus) -> Option<i32> {
    None
}

fn base_result(exec: &JobExec, status: JobStatus, started: i64) -> JobResult {
    JobResult {
        job_id: exec.job_id.clone(),
        status,
        exit_code: None,
        signal: None,
        stdout: String::new(),
        stdout_truncated: false,
        stderr: String::new(),
        stderr_truncated: false,
        started_at_micros: started,
        finished_at_micros: now_micros(),
        error: None,
    }
}

trait ErrorSetter {
    fn error(self, msg: String) -> Self;
}

impl ErrorSetter for JobResult {
    fn error(mut self, msg: String) -> Self {
        self.error = Some(msg);
        self.finished_at_micros = now_micros();
        self
    }
}

/// 读取输出，最多保留 `limit` 字节；超出后继续排空管道但不再累积。
async fn read_capped<R: AsyncRead + Unpin>(mut r: R, limit: usize) -> (String, bool) {
    let mut buf = [0u8; 8192];
    let mut out: Vec<u8> = Vec::new();
    let mut truncated = false;
    loop {
        match r.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                if !truncated {
                    let remaining = limit.saturating_sub(out.len());
                    let take = remaining.min(n);
                    out.extend_from_slice(&buf[..take]);
                    if take < n {
                        truncated = true;
                    }
                }
            }
            Err(_) => break,
        }
    }
    (String::from_utf8_lossy(&out).into_owned(), truncated)
}

fn write_script(cfg: &JobConfig, exec: &JobExec) -> std::io::Result<PathBuf> {
    let base = cfg
        .work_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("gse-jobs");
    std::fs::create_dir_all(&dir)?;
    let ext = match exec.interpreter.as_str() {
        "python3" | "python" => "py",
        _ => "sh",
    };
    let path = dir.join(format!("{}.{}", sanitize(&exec.job_id), ext));
    std::fs::write(&path, &exec.script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(path)
}

fn sanitize(job_id: &str) -> String {
    let cleaned: String = job_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if cleaned.is_empty() {
        "job".to_string()
    } else {
        cleaned
    }
}

fn cleanup(path: &PathBuf) {
    let _ = std::fs::remove_file(path);
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
    use gse_proto::JobStatus;
    use std::collections::BTreeMap;

    fn exec(script: &str, timeout_secs: u64) -> JobExec {
        JobExec {
            job_id: format!("job-test-{}", now_micros()),
            interpreter: "bash".to_string(),
            script: script.to_string(),
            args: vec![],
            env: BTreeMap::new(),
            working_dir: None,
            timeout_secs,
            stdout_limit_bytes: 1024 * 1024,
            stderr_limit_bytes: 1024 * 1024,
        }
    }

    fn cfg(dir: &std::path::Path) -> JobConfig {
        JobConfig {
            allowed_interpreters: vec!["bash".into(), "sh".into(), "python3".into()],
            default_interpreter: "bash".into(),
            max_concurrent_jobs: 1,
            work_dir: Some(dir.to_string_lossy().into_owned()),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gse-job-test-{tag}-{}", now_micros()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn successful_job_captures_stdout() {
        let dir = temp_dir("ok");
        let result = run_job(&cfg(&dir), exec("echo hello; echo oops >&2", 30)).await;
        assert_eq!(result.status, JobStatus::Succeeded);
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.stdout.trim(), "hello");
        assert_eq!(result.stderr.trim(), "oops");
        assert!(!result.stdout_truncated);
        assert!(result.finished_at_micros >= result.started_at_micros);
    }

    #[tokio::test]
    async fn nonzero_exit_is_failed() {
        let dir = temp_dir("fail");
        let result = run_job(&cfg(&dir), exec("exit 3", 30)).await;
        assert_eq!(result.status, JobStatus::Failed);
        assert_eq!(result.exit_code, Some(3));
    }

    #[tokio::test]
    async fn timeout_marks_timeout_and_kills() {
        let dir = temp_dir("timeout");
        let started = std::time::Instant::now();
        let result = run_job(&cfg(&dir), exec("sleep 30", 1)).await;
        assert_eq!(result.status, JobStatus::Timeout);
        assert!(started.elapsed() < Duration::from_secs(15));
    }

    #[tokio::test]
    async fn stdout_is_truncated_at_limit() {
        let dir = temp_dir("trunc");
        let mut e = exec("echo 1234567890", 30);
        e.stdout_limit_bytes = 4;
        let result = run_job(&cfg(&dir), e).await;
        assert_eq!(result.status, JobStatus::Succeeded);
        assert_eq!(result.stdout, "1234");
        assert!(result.stdout_truncated);
    }

    #[tokio::test]
    async fn script_file_is_removed_after_run() {
        let dir = temp_dir("cleanup");
        let e = exec("true", 30);
        let result = run_job(&cfg(&dir), e).await;
        assert_eq!(result.status, JobStatus::Succeeded);
        let jobs_dir = dir.join("gse-jobs");
        let remaining: Vec<_> = std::fs::read_dir(&jobs_dir)
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(remaining.is_empty(), "temp script should be cleaned up");
    }

    #[tokio::test]
    async fn reject_unknown_interpreter() {
        let executor = JobExecutor::new(cfg(&temp_dir("reject")));
        assert!(executor.interpreter_allowed("bash"));
        assert!(executor.interpreter_allowed("python3"));
        assert!(!executor.interpreter_allowed("perl"));
    }
}
