//! vmctl：通过 HTTP 访问 gse-server 的节点只读接口与作业接口。

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;

pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:7101";
pub const WAIT_INTERVAL: Duration = Duration::from_secs(1);
pub const WAIT_TIMEOUT: Duration = Duration::from_secs(300);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(30);

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
    fn send(&self, method: &str, url: &str, body: Option<&str>) -> Result<(u16, String), String>;
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
    fn send(&self, method: &str, url: &str, body: Option<&str>) -> Result<(u16, String), String> {
        let req = match method {
            "GET" => self.agent.get(url),
            "POST" => self.agent.post(url),
            other => return Err(format!("unsupported method: {other}")),
        };
        let result = match body {
            Some(payload) => req
                .set("Content-Type", "application/json")
                .send_string(payload),
            None => req.call(),
        };
        match result {
            Ok(resp) => {
                let status = resp.status();
                let text = resp.into_string().unwrap_or_default();
                Ok((status, text))
            }
            Err(ureq::Error::Status(code, resp)) => {
                let text = resp.into_string().unwrap_or_default();
                Ok((code, text))
            }
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
    pub agent_id: String,
    pub script_file: String,
    pub interpreter: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<String>,
    pub working_dir: Option<String>,
    pub timeout_secs: Option<u64>,
    pub wait: bool,
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
        fn send(
            &self,
            method: &str,
            url: &str,
            body: Option<&str>,
        ) -> Result<(u16, String), String> {
            let mut inner = self.inner.lock().expect("lock");
            inner.calls.push((
                method.to_string(),
                url.to_string(),
                body.map(ToOwned::to_owned),
            ));
            inner
                .next
                .pop_front()
                .unwrap_or_else(|| Err("no more mock responses".into()))
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
}
