//! vmctl：通过 HTTP 访问 gse-server 的节点只读接口与作业接口。

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;

pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:7101";
pub const WAIT_INTERVAL: Duration = Duration::from_secs(1);
pub const WAIT_TIMEOUT: Duration = Duration::from_secs(300);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(300);

/// CLI 输出：2xx 正文进 stdout，错误进 stderr。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub code: u8,
}

impl Output {
    fn ok(body: String) -> Self {
        Self {
            stdout: body,
            stderr: String::new(),
            code: 0,
        }
    }

    fn err(code: u8, stderr: String) -> Self {
        Self {
            stdout: String::new(),
            stderr,
            code,
        }
    }

    fn wait_done(body: String, code: u8) -> Self {
        Self {
            stdout: body,
            stderr: String::new(),
            code,
        }
    }
}

/// Wait 轮询间隔与时限；生产默认 1s / 300s，测试可注入更短值。
#[derive(Debug, Clone, Copy)]
pub struct WaitPolicy {
    pub interval: Duration,
    pub timeout: Duration,
}

impl Default for WaitPolicy {
    fn default() -> Self {
        Self {
            interval: WAIT_INTERVAL,
            timeout: WAIT_TIMEOUT,
        }
    }
}

pub trait Transport {
    fn exchange(
        &self,
        method: &str,
        url: &str,
        body: RequestBody<'_>,
    ) -> Result<HttpResponse, String>;

    fn send(&self, method: &str, url: &str, body: Option<&str>) -> Result<(u16, String), String> {
        let req = match body {
            Some(payload) => RequestBody::Json(payload),
            None => RequestBody::Empty,
        };
        let resp = self.exchange(method, url, req)?;
        Ok((
            resp.status,
            String::from_utf8_lossy(&resp.body).into_owned(),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestBody<'a> {
    Empty,
    Json(&'a str),
    MultipartFile {
        field: &'a str,
        filename: &'a str,
        data: &'a [u8],
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new() -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout_connect(CONNECT_TIMEOUT)
                .timeout_read(READ_TIMEOUT)
                .build(),
        }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for UreqTransport {
    fn exchange(
        &self,
        method: &str,
        url: &str,
        body: RequestBody<'_>,
    ) -> Result<HttpResponse, String> {
        let req = self.agent.request(method, url);
        let result = match body {
            RequestBody::Empty => req.call(),
            RequestBody::Json(payload) => req
                .set("Content-Type", "application/json")
                .send_string(payload),
            RequestBody::MultipartFile {
                field,
                filename,
                data,
            } => {
                let (content_type, bytes) = encode_multipart(field, filename, data);
                req.set("Content-Type", &content_type).send_bytes(&bytes)
            }
        };
        match result {
            Ok(resp) => read_ureq_response(resp.status(), resp),
            Err(ureq::Error::Status(code, resp)) => read_ureq_response(code, resp),
            Err(e) => Err(e.to_string()),
        }
    }
}

pub struct Client<'a, T: Transport> {
    pub base_url: String,
    pub transport: &'a T,
    pub wait: WaitPolicy,
}

#[derive(Debug, Clone, Default)]
pub struct JobSubmitSpec {
    pub kind: String,
    pub agent_id: String,
    pub script_file: String,
    pub interpreter: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<String>,
    pub working_dir: Option<String>,
    pub timeout_secs: Option<u64>,
    pub wait: bool,
    pub from_agent: Option<String>,
    pub from_path: Option<String>,
    pub to_agent: Option<String>,
    pub to_path: Option<String>,
    pub upload: Option<String>,
    /// `--kind agent_upgrade`：目标机上已就位的二进制路径。
    pub binary_path: Option<String>,
    /// `--kind agent_upgrade`：期望的 sha256（64 位十六进制）。
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct JobRerunSpec {
    pub job_id: String,
    pub agent_id: Option<String>,
    pub script_file: Option<String>,
    pub interpreter: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<String>,
    pub working_dir: Option<String>,
    pub timeout_secs: Option<u64>,
    pub wait: bool,
    pub dest_path: Option<String>,
}

#[derive(Serialize)]
struct JobSubmitBody {
    agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    interpreter: Option<String>,
    script: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    working_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_secs: Option<u64>,
}

#[derive(Serialize)]
struct FileJobBody {
    kind: &'static str,
    source: serde_json::Value,
    destination: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_secs: Option<u64>,
}

pub fn join_url(base: &str, path: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if path.is_empty() {
        return base.to_string();
    }
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

pub fn read_script_file(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("read script file {}: {e}", path.display()))
}

pub fn parse_env(pairs: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut env = BTreeMap::new();
    for item in pairs {
        match item.split_once('=') {
            Some((key, value)) if !key.is_empty() => {
                env.insert(key.to_string(), value.to_string());
            }
            _ => return Err(format!("invalid --env {item}, want KEY=VAL")),
        }
    }
    Ok(env)
}

fn filled(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

fn nonempty(value: &str) -> bool {
    !value.trim().is_empty()
}

fn opt_filled(value: &Option<String>) -> Option<&str> {
    filled(value.as_deref())
}

fn normalize_kind(kind: &str) -> String {
    let trimmed = kind.trim();
    if trimmed.is_empty() {
        "script".to_string()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

fn basename_or_upload(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "upload.bin".to_string())
}

fn encode_multipart(field: &str, filename: &str, data: &[u8]) -> (String, Vec<u8>) {
    let safe_name = filename.replace(['\r', '\n', '"'], "_");
    let boundary = format!(
        "----VmctlFormBoundary{:x}{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let header = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{safe_name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    );
    let footer = format!("\r\n--{boundary}--\r\n");
    let mut body = Vec::with_capacity(header.len() + data.len() + footer.len());
    body.extend_from_slice(header.as_bytes());
    body.extend_from_slice(data);
    body.extend_from_slice(footer.as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

fn read_ureq_response(status: u16, resp: ureq::Response) -> Result<HttpResponse, String> {
    let mut body = Vec::new();
    resp.into_reader()
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    Ok(HttpResponse { status, body })
}

fn file_id_of(body: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("parse upload json: {e}"))?;
    value
        .get("file_id")
        .and_then(|v| v.as_str())
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| "missing file_id in response".to_string())
}

fn http_bytes_to_output(result: Result<HttpResponse, String>) -> Output {
    match result {
        Ok(resp) if (200..300).contains(&resp.status) => {
            Output::ok(String::from_utf8_lossy(&resp.body).into_owned())
        }
        Ok(resp) => Output::err(1, String::from_utf8_lossy(&resp.body).into_owned()),
        Err(e) => Output::err(1, e),
    }
}

fn enc(segment: &str) -> String {
    urlencoding::encode(segment).into_owned()
}

fn http_to_output(result: Result<(u16, String), String>) -> Output {
    match result {
        Ok((status, body)) if (200..300).contains(&status) => Output::ok(body),
        Ok((_, body)) => Output::err(1, body),
        Err(e) => Output::err(1, e),
    }
}

fn job_id_of(body: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("parse job json: {e}"))?;
    value
        .get("job_id")
        .and_then(|v| v.as_str())
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| "missing job_id in response".to_string())
}

fn job_status_of(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .get("status")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
}

fn wait_exit_code(status: &str) -> Option<u8> {
    match status {
        "succeeded" => Some(0),
        "failed" | "rejected" | "lost" => Some(1),
        "timeout" => Some(2),
        _ => None,
    }
}

impl<'a, T: Transport> Client<'a, T> {
    pub fn health(&self) -> Output {
        self.get("/health")
    }

    pub fn hosts_list(&self) -> Output {
        self.get("/api/gse/hosts")
    }

    pub fn hosts_get(&self, host_id: &str) -> Output {
        self.get(&format!("/api/gse/hosts/{}", enc(host_id)))
    }

    pub fn agents_list(&self) -> Output {
        self.get("/api/gse/agents")
    }

    pub fn agents_get(&self, agent_id: &str) -> Output {
        self.get(&format!("/api/gse/agents/{}", enc(agent_id)))
    }

    pub fn jobs_get(&self, job_id: &str) -> Output {
        self.get(&format!("/api/gse/jobs/{}", enc(job_id)))
    }

    pub fn jobs_list(
        &self,
        agent_id: Option<&str>,
        status: Option<&str>,
        limit: Option<i64>,
    ) -> Output {
        let mut url = join_url(&self.base_url, "/api/gse/jobs");
        let mut query = Vec::new();
        if let Some(id) = agent_id.filter(|s| !s.is_empty()) {
            query.push(format!("agent_id={}", enc(id)));
        }
        if let Some(st) = status.filter(|s| !s.is_empty()) {
            query.push(format!("status={}", enc(st)));
        }
        if let Some(n) = limit {
            query.push(format!("limit={n}"));
        }
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query.join("&"));
        }
        http_to_output(self.transport.send("GET", &url, None))
    }

    pub fn jobs_submit(&self, spec: &JobSubmitSpec) -> Output {
        match normalize_kind(&spec.kind).as_str() {
            "script" => self.jobs_submit_script(spec),
            "file_transfer" => self.jobs_submit_file(spec),
            "agent_upgrade" => self.jobs_submit_agent_upgrade(spec),
            other => Output::err(
                1,
                format!("unsupported --kind {other}, want script, file_transfer or agent_upgrade"),
            ),
        }
    }

    /// 提交 `agent_upgrade` 作业：`--binary-path`（目标机上已就位的二进制）
    /// 与 `--sha256`（期望校验值）组成载荷，放进 `script` 字段的 JSON 里。
    fn jobs_submit_agent_upgrade(&self, spec: &JobSubmitSpec) -> Output {
        let Some(binary_path) = opt_filled(&spec.binary_path) else {
            return Output::err(1, "--kind agent_upgrade 需要 --binary-path".to_string());
        };
        let Some(sha256) = opt_filled(&spec.sha256) else {
            return Output::err(1, "--kind agent_upgrade 需要 --sha256".to_string());
        };
        if spec.agent_id.trim().is_empty() {
            return Output::err(1, "--kind agent_upgrade 需要 --agent-id".to_string());
        }
        let agent_id = spec.agent_id.trim();
        if sha256.len() != 64 || !sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            return Output::err(1, "--sha256 必须是 64 位十六进制".to_string());
        }
        let payload = serde_json::json!({
            "binary_path": binary_path,
            "sha256": sha256,
        });
        let body = serde_json::json!({
            "agent_id": agent_id,
            "kind": "agent_upgrade",
            "script": payload.to_string(),
            "timeout_secs": spec.timeout_secs.unwrap_or(120),
        });
        let url = format!("{}/api/gse/jobs", self.base_url);
        let body = body.to_string();
        http_to_output(self.transport.send("POST", &url, Some(&body)))
    }

    fn jobs_submit_script(&self, spec: &JobSubmitSpec) -> Output {
        if opt_filled(&spec.from_agent).is_some()
            || opt_filled(&spec.from_path).is_some()
            || opt_filled(&spec.to_agent).is_some()
            || opt_filled(&spec.to_path).is_some()
            || opt_filled(&spec.upload).is_some()
        {
            return Output::err(
                1,
                "file transfer flags cannot be used with script jobs".to_string(),
            );
        }
        if !nonempty(&spec.agent_id) || !nonempty(&spec.script_file) {
            return Output::err(
                1,
                "script job requires --agent-id and --script-file".to_string(),
            );
        }
        let script = match read_script_file(Path::new(&spec.script_file)) {
            Ok(s) => s,
            Err(e) => return Output::err(1, e),
        };
        let env = match parse_env(&spec.env) {
            Ok(m) => m,
            Err(e) => return Output::err(1, e),
        };
        let body = JobSubmitBody {
            agent_id: spec.agent_id.clone(),
            interpreter: spec.interpreter.clone(),
            script,
            args: spec.args.clone(),
            env,
            working_dir: spec.working_dir.clone(),
            timeout_secs: spec.timeout_secs,
        };
        let payload = match serde_json::to_string(&body) {
            Ok(s) => s,
            Err(e) => return Output::err(1, format!("encode submit body: {e}")),
        };
        let created = http_to_output(self.transport.send(
            "POST",
            &join_url(&self.base_url, "/api/gse/jobs"),
            Some(&payload),
        ));
        self.after_job_write(created, spec.wait)
    }

    fn jobs_submit_file(&self, spec: &JobSubmitSpec) -> Output {
        if nonempty(&spec.script_file) {
            return Output::err(
                1,
                "--kind file_transfer cannot be used with --script-file".to_string(),
            );
        }
        let upload = opt_filled(&spec.upload);
        let from_agent = opt_filled(&spec.from_agent);
        let from_path = opt_filled(&spec.from_path);
        let to_agent = opt_filled(&spec.to_agent);
        let to_path = opt_filled(&spec.to_path);
        if upload.is_some() && (from_agent.is_some() || from_path.is_some()) {
            return Output::err(1, "--upload cannot be used with --from-agent".to_string());
        }
        let (Some(to_agent), Some(to_path)) = (to_agent, to_path) else {
            return Output::err(
                1,
                "file transfer requires --to-agent and --to-path".to_string(),
            );
        };
        let source = if let Some(local) = upload {
            match self.upload_local_file(local) {
                Ok(file_id) => serde_json::json!({"type": "server_temp", "file_id": file_id}),
                Err(out) => return out,
            }
        } else if let (Some(agent_id), Some(path)) = (from_agent, from_path) {
            serde_json::json!({"type": "agent", "agent_id": agent_id, "path": path})
        } else {
            return Output::err(
                1,
                "file transfer requires --from-agent --from-path --to-agent --to-path, or --upload --to-agent --to-path".to_string(),
            );
        };
        let body = FileJobBody {
            kind: "file_transfer",
            source,
            destination: serde_json::json!({
                "type": "agent",
                "agent_id": to_agent,
                "path": to_path
            }),
            timeout_secs: spec.timeout_secs,
        };
        let payload = match serde_json::to_string(&body) {
            Ok(s) => s,
            Err(e) => return Output::err(1, format!("encode submit body: {e}")),
        };
        let created = http_to_output(self.transport.send(
            "POST",
            &join_url(&self.base_url, "/api/gse/jobs"),
            Some(&payload),
        ));
        self.after_job_write(created, spec.wait)
    }

    fn upload_local_file(&self, path: &str) -> Result<String, Output> {
        let out = self.jobs_files_upload(path);
        if out.code != 0 {
            return Err(out);
        }
        file_id_of(&out.stdout).map_err(|e| Output::err(1, e))
    }

    pub fn jobs_files_list(&self) -> Output {
        self.get("/api/gse/job-files")
    }

    pub fn jobs_files_upload(&self, path: &str) -> Output {
        let data = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => return Output::err(1, format!("read {path}: {e}")),
        };
        let filename = basename_or_upload(path);
        http_bytes_to_output(self.transport.exchange(
            "POST",
            &join_url(&self.base_url, "/api/gse/job-files"),
            RequestBody::MultipartFile {
                field: "file",
                filename: &filename,
                data: &data,
            },
        ))
    }

    pub fn jobs_files_download(&self, file_id: &str, output: &str) -> Output {
        let url = join_url(
            &self.base_url,
            &format!("/api/gse/job-files/{}", enc(file_id)),
        );
        match self.transport.exchange("GET", &url, RequestBody::Empty) {
            Ok(resp) if (200..300).contains(&resp.status) => {
                match std::fs::write(output, &resp.body) {
                    Ok(()) => Output::ok(String::new()),
                    Err(e) => Output::err(1, format!("write {output}: {e}")),
                }
            }
            Ok(resp) => Output::err(1, String::from_utf8_lossy(&resp.body).into_owned()),
            Err(e) => Output::err(1, e),
        }
    }

    pub fn jobs_files_delete(&self, file_id: &str) -> Output {
        http_to_output(self.transport.send(
            "DELETE",
            &join_url(
                &self.base_url,
                &format!("/api/gse/job-files/{}", enc(file_id)),
            ),
            None,
        ))
    }

    pub fn jobs_rerun(&self, spec: &JobRerunSpec) -> Output {
        let env = match parse_env(&spec.env) {
            Ok(m) => m,
            Err(e) => return Output::err(1, e),
        };
        let script = match spec.script_file.as_deref() {
            Some(path) => match read_script_file(Path::new(path)) {
                Ok(s) => Some(s),
                Err(e) => return Output::err(1, e),
            },
            None => None,
        };
        let mut obj = serde_json::Map::new();
        if let Some(id) = spec.agent_id.as_deref().filter(|s| !s.is_empty()) {
            obj.insert("agent_id".into(), serde_json::Value::String(id.to_string()));
        }
        if let Some(interp) = spec.interpreter.as_deref().filter(|s| !s.is_empty()) {
            obj.insert(
                "interpreter".into(),
                serde_json::Value::String(interp.to_string()),
            );
        }
        if let Some(s) = script {
            obj.insert("script".into(), serde_json::Value::String(s));
        }
        if !spec.args.is_empty() {
            obj.insert("args".into(), serde_json::json!(spec.args));
        }
        if !env.is_empty() {
            obj.insert("env".into(), serde_json::json!(env));
        }
        if let Some(dir) = spec.working_dir.clone() {
            obj.insert("working_dir".into(), serde_json::Value::String(dir));
        }
        if let Some(secs) = spec.timeout_secs {
            obj.insert("timeout_secs".into(), serde_json::json!(secs));
        }
        if let Some(path) = opt_filled(&spec.dest_path) {
            obj.insert(
                "dest_path".into(),
                serde_json::Value::String(path.to_string()),
            );
        }
        let url = join_url(
            &self.base_url,
            &format!("/api/gse/jobs/{}/rerun", enc(&spec.job_id)),
        );
        let created = if obj.is_empty() {
            http_to_output(self.transport.send("POST", &url, None))
        } else {
            let payload = serde_json::Value::Object(obj).to_string();
            http_to_output(self.transport.send("POST", &url, Some(&payload)))
        };
        self.after_job_write(created, spec.wait)
    }

    fn get(&self, path: &str) -> Output {
        http_to_output(
            self.transport
                .send("GET", &join_url(&self.base_url, path), None),
        )
    }

    fn after_job_write(&self, created: Output, wait: bool) -> Output {
        if created.code != 0 || !wait {
            return created;
        }
        match job_id_of(&created.stdout) {
            Ok(job_id) => self.wait_for_job(&job_id),
            Err(e) => Output::err(1, e),
        }
    }

    fn wait_for_job(&self, job_id: &str) -> Output {
        let url = join_url(&self.base_url, &format!("/api/gse/jobs/{}", enc(job_id)));
        let started = Instant::now();
        loop {
            match self.transport.send("GET", &url, None) {
                Ok((status, body)) if (200..300).contains(&status) => {
                    if let Some(st) = job_status_of(&body) {
                        if let Some(code) = wait_exit_code(&st) {
                            return Output::wait_done(body, code);
                        }
                    }
                    if started.elapsed() >= self.wait.timeout {
                        return Output::wait_done(body, 2);
                    }
                }
                Ok((_, body)) => return Output::err(1, body),
                Err(e) => return Output::err(1, e),
            }
            if !self.wait.interval.is_zero() {
                std::thread::sleep(self.wait.interval);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct Mock {
        inner: Mutex<MockInner>,
    }

    struct MockInner {
        next: VecDeque<Result<(u16, String), String>>,
        calls: Vec<(String, String, Option<String>)>,
    }

    impl Mock {
        fn new(next: Vec<Result<(u16, String), String>>) -> Self {
            Self {
                inner: Mutex::new(MockInner {
                    next: next.into(),
                    calls: Vec::new(),
                }),
            }
        }

        fn calls(&self) -> Vec<(String, String, Option<String>)> {
            self.inner.lock().expect("lock").calls.clone()
        }
    }

    impl Transport for Mock {
        fn exchange(
            &self,
            method: &str,
            url: &str,
            body: RequestBody<'_>,
        ) -> Result<HttpResponse, String> {
            let body = match body {
                RequestBody::Empty => None,
                RequestBody::Json(payload) => Some(payload.to_string()),
                RequestBody::MultipartFile {
                    field,
                    filename,
                    data,
                } => Some(format!("multipart:{field}:{filename}:{} bytes", data.len())),
            };
            let mut inner = self.inner.lock().expect("lock");
            inner
                .calls
                .push((method.to_string(), url.to_string(), body));
            match inner
                .next
                .pop_front()
                .unwrap_or_else(|| Err("no more mock responses".into()))
            {
                Ok((status, body)) => Ok(HttpResponse {
                    status,
                    body: body.into_bytes(),
                }),
                Err(e) => Err(e),
            }
        }
    }

    fn client<'a>(mock: &'a Mock, wait: WaitPolicy) -> Client<'a, Mock> {
        Client {
            base_url: "http://127.0.0.1:7101/".into(),
            transport: mock,
            wait,
        }
    }

    fn instant_wait() -> WaitPolicy {
        WaitPolicy {
            interval: Duration::ZERO,
            timeout: Duration::from_millis(20),
        }
    }

    #[test]
    fn join_url_trims_trailing_slash() {
        assert_eq!(
            join_url("http://127.0.0.1:7101/", "/api/gse/agents"),
            "http://127.0.0.1:7101/api/gse/agents"
        );
        assert_eq!(
            join_url("http://127.0.0.1:7101", "/health"),
            "http://127.0.0.1:7101/health"
        );
    }

    #[test]
    fn read_script_file_ok_and_missing() {
        let dir = std::env::temp_dir().join(format!("vmctl-script-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("run.sh");
        std::fs::write(&path, "echo ok").expect("write");
        assert_eq!(read_script_file(&path).expect("read"), "echo ok");
        let missing = dir.join("nope.sh");
        assert!(read_script_file(&missing)
            .unwrap_err()
            .contains("read script file"));
    }

    #[test]
    fn parse_env_accepts_key_val() {
        let env = parse_env(&["LANG=C".into(), "A=b=c".into()]).expect("env");
        assert_eq!(env.get("LANG").map(String::as_str), Some("C"));
        assert_eq!(env.get("A").map(String::as_str), Some("b=c"));
        assert!(parse_env(&["NOVALUE".into()]).is_err());
    }

    #[test]
    fn health_and_not_found_map_exit_codes() {
        let mock = Mock::new(vec![
            Ok((200, "{\"ok\":true}".into())),
            Ok((404, "{\"error\":\"nope\"}".into())),
            Err("connection refused".into()),
        ]);
        let c = client(&mock, WaitPolicy::default());
        let ok = c.health();
        assert_eq!(ok, Output::ok("{\"ok\":true}".into()));
        let nf = c.agents_get("ghost");
        assert_eq!(nf.code, 1);
        assert_eq!(nf.stderr, "{\"error\":\"nope\"}");
        let down = c.hosts_list();
        assert_eq!(down.code, 1);
        assert!(down.stderr.contains("connection refused"));
        let calls = mock.calls();
        assert_eq!(calls[0].1, "http://127.0.0.1:7101/health");
        assert_eq!(calls[1].1, "http://127.0.0.1:7101/api/gse/agents/ghost");
    }

    #[test]
    fn jobs_list_appends_filters() {
        let mock = Mock::new(vec![Ok((200, "[]".into()))]);
        let c = client(&mock, WaitPolicy::default());
        let out = c.jobs_list(Some("a-1"), Some("pending"), Some(10));
        assert_eq!(out.code, 0);
        assert_eq!(
            mock.calls()[0].1,
            "http://127.0.0.1:7101/api/gse/jobs?agent_id=a-1&status=pending&limit=10"
        );
    }

    #[test]
    fn submit_without_wait_prints_create_json() {
        let dir = std::env::temp_dir().join(format!("vmctl-submit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("job.sh");
        std::fs::write(&path, "echo hi").expect("write");
        let mock = Mock::new(vec![Ok((
            201,
            "{\"job_id\":\"j-1\",\"status\":\"pending\"}".into(),
        ))]);
        let c = client(&mock, WaitPolicy::default());
        let out = c.jobs_submit(&JobSubmitSpec {
            agent_id: "agent-1".into(),
            script_file: path.to_string_lossy().into_owned(),
            wait: false,
            ..JobSubmitSpec::default()
        });
        assert_eq!(out.code, 0);
        assert!(out.stdout.contains("j-1"));
        let body = mock.calls()[0].2.clone().expect("body");
        assert!(body.contains("\"agent_id\":\"agent-1\""));
        assert!(body.contains("echo hi"));
        assert_eq!(mock.calls().len(), 1);
    }

    #[test]
    fn submit_upload_missing_file_id_skips_job() {
        let dir = tmp_dir("upload-noid");
        let path = dir.join("pkg.tar");
        std::fs::write(&path, b"x").expect("write");
        let mock = Mock::new(vec![Ok((201, "{\"file_name\":\"pkg.tar\"}".into()))]);
        let c = client(&mock, WaitPolicy::default());
        let out = c.jobs_submit(&JobSubmitSpec {
            kind: "file_transfer".into(),
            upload: Some(path.to_string_lossy().into_owned()),
            to_agent: Some("web-02".into()),
            to_path: Some("/opt/pkg.tar".into()),
            ..JobSubmitSpec::default()
        });
        assert_eq!(out.code, 1);
        assert!(out.stderr.contains("missing file_id"));
        assert_eq!(mock.calls().len(), 1);
    }

    #[test]
    fn submit_upload_unreadable_local_file() {
        let mock = Mock::new(vec![]);
        let c = client(&mock, WaitPolicy::default());
        let out = c.jobs_submit(&JobSubmitSpec {
            kind: "file_transfer".into(),
            upload: Some("/no/such/vmctl-upload.bin".into()),
            to_agent: Some("web-02".into()),
            to_path: Some("/opt/pkg.tar".into()),
            ..JobSubmitSpec::default()
        });
        assert_eq!(out.code, 1);
        assert!(out.stderr.contains("read /no/such/vmctl-upload.bin"));
        assert!(mock.calls().is_empty());
    }

    #[test]
    fn submit_missing_script_file_exits_1() {
        let mock = Mock::new(vec![]);
        let c = client(&mock, WaitPolicy::default());
        let out = c.jobs_submit(&JobSubmitSpec {
            agent_id: "agent-1".into(),
            script_file: "/no/such/vmctl-script.sh".into(),
            wait: false,
            ..JobSubmitSpec::default()
        });
        assert_eq!(out.code, 1);
        assert!(out.stderr.contains("read script file"));
        assert!(mock.calls().is_empty());
    }

    #[test]
    fn wait_maps_terminal_status_to_exit_codes() {
        let dir = std::env::temp_dir().join(format!("vmctl-wait-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("job.sh");
        std::fs::write(&path, "true").expect("write");

        let mock = Mock::new(vec![
            Ok((201, "{\"job_id\":\"j-ok\"}".into())),
            Ok((200, "{\"job_id\":\"j-ok\",\"status\":\"succeeded\"}".into())),
        ]);
        let c = client(&mock, instant_wait());
        let ok = c.jobs_submit(&JobSubmitSpec {
            agent_id: "a".into(),
            script_file: path.to_string_lossy().into_owned(),
            wait: true,
            ..JobSubmitSpec::default()
        });
        assert_eq!(ok.code, 0);
        assert!(ok.stdout.contains("succeeded"));

        let mock = Mock::new(vec![
            Ok((201, "{\"job_id\":\"j-fail\"}".into())),
            Ok((200, "{\"job_id\":\"j-fail\",\"status\":\"failed\"}".into())),
        ]);
        let c = client(&mock, instant_wait());
        let fail = c.jobs_submit(&JobSubmitSpec {
            agent_id: "a".into(),
            script_file: path.to_string_lossy().into_owned(),
            wait: true,
            ..JobSubmitSpec::default()
        });
        assert_eq!(fail.code, 1);

        let mock = Mock::new(vec![
            Ok((201, "{\"job_id\":\"j-to\"}".into())),
            Ok((200, "{\"job_id\":\"j-to\",\"status\":\"timeout\"}".into())),
        ]);
        let c = client(&mock, instant_wait());
        let to = c.jobs_submit(&JobSubmitSpec {
            agent_id: "a".into(),
            script_file: path.to_string_lossy().into_owned(),
            wait: true,
            ..JobSubmitSpec::default()
        });
        assert_eq!(to.code, 2);
    }

    #[test]
    fn wait_pending_until_deadline_exits_2() {
        let mock = Mock::new(vec![
            Ok((201, "{\"job_id\":\"j-slow\"}".into())),
            Ok((200, "{\"job_id\":\"j-slow\",\"status\":\"pending\"}".into())),
            Ok((200, "{\"job_id\":\"j-slow\",\"status\":\"running\"}".into())),
        ]);
        let c = client(
            &mock,
            WaitPolicy {
                interval: Duration::ZERO,
                timeout: Duration::ZERO,
            },
        );
        let dir = std::env::temp_dir().join(format!("vmctl-pending-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("job.sh");
        std::fs::write(&path, "sleep 1").expect("write");
        let out = c.jobs_submit(&JobSubmitSpec {
            agent_id: "a".into(),
            script_file: path.to_string_lossy().into_owned(),
            wait: true,
            ..JobSubmitSpec::default()
        });
        assert_eq!(out.code, 2);
        assert!(out.stdout.contains("j-slow"));
    }

    #[test]
    fn rerun_without_overrides_posts_empty_body() {
        let mock = Mock::new(vec![Ok((201, "{\"job_id\":\"j-2\"}".into()))]);
        let c = client(&mock, WaitPolicy::default());
        let out = c.jobs_rerun(&JobRerunSpec {
            job_id: "j-1".into(),
            wait: false,
            ..JobRerunSpec::default()
        });
        assert_eq!(out.code, 0);
        let call = &mock.calls()[0];
        assert_eq!(call.0, "POST");
        assert_eq!(call.1, "http://127.0.0.1:7101/api/gse/jobs/j-1/rerun");
        assert_eq!(call.2, None);
    }

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vmctl-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn submit_script_json_omits_kind() {
        let dir = tmp_dir("script-kind");
        let path = dir.join("job.sh");
        std::fs::write(&path, "echo hi").expect("write");
        let mock = Mock::new(vec![Ok((201, "{\"job_id\":\"j-1\"}".into()))]);
        let c = client(&mock, WaitPolicy::default());
        let out = c.jobs_submit(&JobSubmitSpec {
            kind: "script".into(),
            agent_id: "agent-1".into(),
            script_file: path.to_string_lossy().into_owned(),
            ..JobSubmitSpec::default()
        });
        assert_eq!(out.code, 0);
        let body = mock.calls()[0].2.clone().expect("body");
        assert!(!body.contains("\"kind\""));
        assert!(body.contains("\"agent_id\":\"agent-1\""));
    }

    #[test]
    fn submit_agent_upgrade_posts_structured_payload() {
        let mock = Mock::new(vec![Ok((201, "{\"job_id\":\"j-up\"}".into()))]);
        let c = client(&mock, WaitPolicy::default());
        let out = c.jobs_submit(&JobSubmitSpec {
            kind: "agent_upgrade".into(),
            agent_id: "web-01".into(),
            binary_path: Some("/tmp/gse-agent-new".into()),
            sha256: Some("a".repeat(64)),
            ..JobSubmitSpec::default()
        });
        assert_eq!(out.code, 0);
        let body: serde_json::Value =
            serde_json::from_str(mock.calls()[0].2.as_deref().expect("body")).expect("json");
        assert_eq!(body["kind"], "agent_upgrade");
        assert_eq!(body["agent_id"], "web-01");
        // 载荷放在 script 字段的 JSON 字符串里
        let spec: serde_json::Value =
            serde_json::from_str(body["script"].as_str().expect("script")).expect("spec json");
        assert_eq!(spec["binary_path"], "/tmp/gse-agent-new");
        assert_eq!(spec["sha256"].as_str().expect("sha").len(), 64);
    }

    #[test]
    fn submit_agent_upgrade_requires_binary_path_and_sha256() {
        let mock = Mock::new(vec![]);
        let c = client(&mock, WaitPolicy::default());
        let missing_bin = c.jobs_submit(&JobSubmitSpec {
            kind: "agent_upgrade".into(),
            agent_id: "a".into(),
            sha256: Some("a".repeat(64)),
            ..JobSubmitSpec::default()
        });
        assert_eq!(missing_bin.code, 1);
        assert!(missing_bin.stderr.contains("--binary-path"));

        let bad_sha = c.jobs_submit(&JobSubmitSpec {
            kind: "agent_upgrade".into(),
            agent_id: "a".into(),
            binary_path: Some("/tmp/x".into()),
            sha256: Some("zz".into()),
            ..JobSubmitSpec::default()
        });
        assert_eq!(bad_sha.code, 1);
        assert!(bad_sha.stderr.contains("64 位十六进制"));
        assert_eq!(mock.calls().len(), 0, "参数不合法时不得发请求");
    }

    #[test]
    fn submit_file_transfer_agent_to_agent() {
        let mock = Mock::new(vec![Ok((201, "{\"job_id\":\"j-ft\"}".into()))]);
        let c = client(&mock, WaitPolicy::default());
        let out = c.jobs_submit(&JobSubmitSpec {
            kind: "file_transfer".into(),
            from_agent: Some("web-01".into()),
            from_path: Some("/var/log/app.log".into()),
            to_agent: Some("web-02".into()),
            to_path: Some("/tmp/app.log".into()),
            timeout_secs: Some(300),
            ..JobSubmitSpec::default()
        });
        assert_eq!(out.code, 0);
        let body: serde_json::Value =
            serde_json::from_str(mock.calls()[0].2.as_deref().expect("body")).expect("json");
        assert_eq!(body["kind"], "file_transfer");
        assert_eq!(body["source"]["type"], "agent");
        assert_eq!(body["source"]["agent_id"], "web-01");
        assert_eq!(body["source"]["path"], "/var/log/app.log");
        assert_eq!(body["destination"]["type"], "agent");
        assert_eq!(body["destination"]["agent_id"], "web-02");
        assert_eq!(body["destination"]["path"], "/tmp/app.log");
        assert_eq!(body["timeout_secs"], 300);
        assert!(body.get("script").is_none());
    }

    #[test]
    fn submit_file_transfer_upload_then_push() {
        let dir = tmp_dir("upload");
        let path = dir.join("pkg.tar");
        std::fs::write(&path, b"tar-bytes").expect("write");
        let mock = Mock::new(vec![
            Ok((201, "{\"file_id\":\"file-1\"}".into())),
            Ok((201, "{\"job_id\":\"j-up\"}".into())),
        ]);
        let c = client(&mock, WaitPolicy::default());
        let out = c.jobs_submit(&JobSubmitSpec {
            kind: "file_transfer".into(),
            upload: Some(path.to_string_lossy().into_owned()),
            to_agent: Some("web-02".into()),
            to_path: Some("/opt/pkg.tar".into()),
            ..JobSubmitSpec::default()
        });
        assert_eq!(out.code, 0);
        let calls = mock.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "POST");
        assert_eq!(calls[0].1, "http://127.0.0.1:7101/api/gse/job-files");
        assert!(calls[0]
            .2
            .as_deref()
            .expect("multipart")
            .starts_with("multipart:file:pkg.tar:"));
        let body: serde_json::Value =
            serde_json::from_str(calls[1].2.as_deref().expect("job body")).expect("json");
        assert_eq!(body["source"]["type"], "server_temp");
        assert_eq!(body["source"]["file_id"], "file-1");
        assert_eq!(body["destination"]["agent_id"], "web-02");
        assert_eq!(body["destination"]["path"], "/opt/pkg.tar");
    }

    #[test]
    fn submit_upload_failure_skips_job() {
        let dir = tmp_dir("upload-fail");
        let path = dir.join("pkg.tar");
        std::fs::write(&path, b"x").expect("write");
        let mock = Mock::new(vec![Ok((400, "{\"error\":\"too big\"}".into()))]);
        let c = client(&mock, WaitPolicy::default());
        let out = c.jobs_submit(&JobSubmitSpec {
            kind: "file_transfer".into(),
            upload: Some(path.to_string_lossy().into_owned()),
            to_agent: Some("web-02".into()),
            to_path: Some("/opt/pkg.tar".into()),
            ..JobSubmitSpec::default()
        });
        assert_eq!(out.code, 1);
        assert_eq!(out.stderr, "{\"error\":\"too big\"}");
        assert_eq!(mock.calls().len(), 1);
    }

    #[test]
    fn submit_rejects_kind_and_flag_conflicts() {
        let mock = Mock::new(vec![]);
        let c = client(&mock, WaitPolicy::default());
        let unknown = c.jobs_submit(&JobSubmitSpec {
            kind: "copy".into(),
            ..JobSubmitSpec::default()
        });
        assert_eq!(unknown.code, 1);
        assert!(unknown.stderr.contains("unsupported --kind"));

        let mixed = c.jobs_submit(&JobSubmitSpec {
            kind: "file_transfer".into(),
            script_file: "run.sh".into(),
            from_agent: Some("a".into()),
            from_path: Some("/tmp/a".into()),
            to_agent: Some("b".into()),
            to_path: Some("/tmp/b".into()),
            ..JobSubmitSpec::default()
        });
        assert_eq!(mixed.code, 1);
        assert!(mixed.stderr.contains("--script-file"));

        let both_src = c.jobs_submit(&JobSubmitSpec {
            kind: "file_transfer".into(),
            from_agent: Some("a".into()),
            upload: Some("./pkg.tar".into()),
            to_agent: Some("b".into()),
            to_path: Some("/tmp/b".into()),
            ..JobSubmitSpec::default()
        });
        assert_eq!(both_src.code, 1);
        assert!(both_src.stderr.contains("--upload"));
        assert!(mock.calls().is_empty());
    }

    #[test]
    fn wait_file_job_maps_terminal_status() {
        let mock = Mock::new(vec![
            Ok((201, "{\"job_id\":\"j-ft\"}".into())),
            Ok((200, "{\"job_id\":\"j-ft\",\"status\":\"succeeded\"}".into())),
        ]);
        let c = client(&mock, instant_wait());
        let out = c.jobs_submit(&JobSubmitSpec {
            kind: "file_transfer".into(),
            from_agent: Some("a".into()),
            from_path: Some("/tmp/a".into()),
            to_agent: Some("b".into()),
            to_path: Some("/tmp/b".into()),
            wait: true,
            ..JobSubmitSpec::default()
        });
        assert_eq!(out.code, 0);
        assert!(out.stdout.contains("succeeded"));
    }

    #[test]
    fn rerun_includes_dest_path() {
        let mock = Mock::new(vec![Ok((201, "{\"job_id\":\"j-3\"}".into()))]);
        let c = client(&mock, WaitPolicy::default());
        let out = c.jobs_rerun(&JobRerunSpec {
            job_id: "j-1".into(),
            dest_path: Some("/tmp/app.log".into()),
            agent_id: Some("web-03".into()),
            ..JobRerunSpec::default()
        });
        assert_eq!(out.code, 0);
        let body: serde_json::Value =
            serde_json::from_str(mock.calls()[0].2.as_deref().expect("body")).expect("json");
        assert_eq!(body["dest_path"], "/tmp/app.log");
        assert_eq!(body["agent_id"], "web-03");
    }

    #[test]
    fn jobs_files_crud() {
        let dir = tmp_dir("files");
        let src = dir.join("pkg.bin");
        let dst = dir.join("out.bin");
        std::fs::write(&src, b"\0abc").expect("write");

        let mock = Mock::new(vec![
            Ok((200, "[{\"file_id\":\"file-1\"}]".into())),
            Ok((201, "{\"file_id\":\"file-1\"}".into())),
            Ok((200, String::from_utf8(b"\0abc".to_vec()).expect("latin1"))),
            Ok((204, String::new())),
        ]);
        let c = client(&mock, WaitPolicy::default());

        let list = c.jobs_files_list();
        assert_eq!(list.code, 0);
        assert_eq!(mock.calls()[0].0, "GET");
        assert_eq!(mock.calls()[0].1, "http://127.0.0.1:7101/api/gse/job-files");

        let upload = c.jobs_files_upload(&src.to_string_lossy());
        assert_eq!(upload.code, 0);
        assert!(mock.calls()[1]
            .2
            .as_deref()
            .expect("multipart")
            .contains("multipart:file:pkg.bin:"));

        let download = c.jobs_files_download("file-1", &dst.to_string_lossy());
        assert_eq!(download.code, 0);
        assert!(download.stdout.is_empty());
        assert_eq!(std::fs::read(&dst).expect("read"), b"\0abc");

        let delete = c.jobs_files_delete("file-1");
        assert_eq!(delete.code, 0);
        assert!(delete.stdout.is_empty());
        assert_eq!(mock.calls()[3].0, "DELETE");
        assert_eq!(
            mock.calls()[3].1,
            "http://127.0.0.1:7101/api/gse/job-files/file-1"
        );
    }
}
