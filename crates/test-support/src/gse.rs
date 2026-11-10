//! `gse-server` / `gse-agent` 真进程编排：写配置 → 起进程 → 等健康 → 台账登记 → spec 下发 → 等到生效。
//!
//! 为什么必须真进程：agent 的「拉 spec → 解析 → 起采集器 → 上报」全部在 `gse-agent-core` 的
//! 运行时里，进程内造一个 fake server 会把这条链路绕过去。`it_chain` 原来是唯一的使用者，
//! 代码在测试文件里；`IT-VM-AGENT-01` 需要同一套编排，所以搬到这里共用。
//!
//! 约定与 `proc::Dataserver` 一致：缺二进制 / 启动失败 / 等待超时都**直接 panic** 并带上
//! 三个子进程的日志尾部，绝不静默跳过。

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::http;
use crate::util::free_port;

/// 台账 / 心跳 / spec 生效的默认等待上限（agent 心跳 1s，两轮快照约 2s）。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// 子进程日志汇到一个缓冲区：任何断言失败都能整段打出来。
#[derive(Clone, Default)]
pub struct Logs(Arc<Mutex<Vec<(String, String)>>>);

impl Logs {
    pub fn new() -> Self {
        Self::default()
    }

    fn append(&self, name: &str, line: String) {
        self.0
            .lock()
            .expect("logs lock")
            .push((name.to_string(), line));
    }

    fn tail_lines<R: Read + Send + 'static>(&self, name: &str, stream: R) {
        let logs = self.clone();
        let name = name.to_string();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                match line {
                    Ok(l) => logs.append(&name, l),
                    Err(_) => break,
                }
            }
        });
    }

    /// 某个进程的全部输出（按写入顺序）。
    pub fn lines(&self, name: &str) -> Vec<String> {
        self.0
            .lock()
            .expect("logs lock")
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, l)| l.clone())
            .collect()
    }
}

impl std::fmt::Display for Logs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let all = self.0.lock().expect("logs lock");
        for name in ["gse-server", "gse-agent"] {
            writeln!(f, "--- {name} stdout/stderr ---")?;
            let lines: Vec<&str> = all
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, l)| l.as_str())
                .collect();
            if lines.is_empty() {
                writeln!(f, "(无输出)")?;
            } else {
                // 只保留尾部，失败诊断够用且不会把整段输出刷进终端。
                for line in lines.iter().skip(lines.len().saturating_sub(40)) {
                    writeln!(f, "{line}")?;
                }
            }
        }
        Ok(())
    }
}

/// 受管子进程：`Drop` 里 kill + wait，保证测试失败也不留孤儿进程。
struct Proc {
    name: &'static str,
    child: Child,
    logs: Logs,
}

impl Proc {
    fn spawn(
        name: &'static str,
        bin: &Path,
        args: &[&str],
        envs: &[(&str, String)],
        logs: &Logs,
    ) -> Self {
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // 先清掉继承来的配置环境变量，再注入本用例的值：外部 shell 里的
        // DATASERVER_* / DP_* / GSE_* 会把临时配置覆盖掉，症状是「进程起来了但连错地方」。
        for (key, _) in std::env::vars() {
            if key.starts_with("DATASERVER_") || key.starts_with("DP_") || key.starts_with("GSE_") {
                cmd.env_remove(&key);
            }
        }
        for (key, value) in envs {
            cmd.env(key, value);
        }
        let mut child = cmd
            .spawn()
            .unwrap_or_else(|e| panic!("启动 {name} 失败（{}）：{e}", bin.display()));
        logs.tail_lines(name, child.stdout.take().expect("stdout"));
        logs.tail_lines(name, child.stderr.take().expect("stderr"));
        Self {
            name,
            child,
            logs: logs.clone(),
        }
    }

    fn check_alive(&mut self) {
        if let Some(status) = self.child.try_wait().expect("try_wait") {
            panic!("{} 提前退出（{status}）\n{}", self.name, self.logs);
        }
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 一个跑着的 `gse-server`，配套能起 `gse-agent` 与写台账的方法。
pub struct Gse {
    root: PathBuf,
    logs: Logs,
    procs: Vec<Proc>,
    agent_port: u16,
    admin_url: String,
    timeout: Duration,
}

impl Gse {
    /// 在 `root` 下写配置、起 `gse-server` 真进程并等到 `/health`。
    ///
    /// 目录里还会放 `gse-server.db`（临时台账 sqlite）与 `gse-agent.toml`。
    pub fn start(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root).expect("create gse dir");
        let (gse_server_bin, _agent_bin) = build_bins();

        let agent_port = free_port();
        let admin_port = free_port();
        let metrics_port = free_port();
        let admin_url = format!("http://127.0.0.1:{admin_port}");

        let cfg = root.join("gse-server.toml");
        std::fs::write(
            &cfg,
            format!(
                "listen = \"127.0.0.1:{agent_port}\"\n\
                 auth_enabled = true\n\
                 db = \"{}\"\n\
                 http_enabled = true\n\
                 http_listen = \"127.0.0.1:{admin_port}\"\n\
                 metrics_listen = \"127.0.0.1:{metrics_port}\"\n\
                 dataplane_probe_interval_secs = 1\n\
                 heartbeat_interval_secs = 1\n\
                 heartbeat_timeout_secs = 30\n\
                 session_probe_interval_secs = 2\n",
                root.join("gse-server.db").display()
            ),
        )
        .expect("write gse-server.toml");

        let logs = Logs::new();
        let server = Proc::spawn(
            "gse-server",
            &gse_server_bin,
            &[],
            &[("GSE_SERVER_CONFIG", cfg.display().to_string())],
            &logs,
        );
        let mut gse = Self {
            root,
            logs,
            procs: vec![server],
            agent_port,
            admin_url,
            timeout: DEFAULT_TIMEOUT,
        };
        gse.wait_for("gse-server /health 就绪", |gse| {
            match http::get(&format!("{}/health", gse.admin_url)) {
                Ok((200, _)) => Ok(()),
                Ok((status, _)) => Err(format!("gse-server /health 返回 {status}")),
                Err(e) => Err(e),
            }
        });
        gse
    }

    pub fn admin_url(&self) -> &str {
        &self.admin_url
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn logs(&self) -> &Logs {
        &self.logs
    }

    /// 覆盖等待上限（默认 [`DEFAULT_TIMEOUT`]）。
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 台账登记 agent（**认证前必须存在**：auth 只 `UPDATE status`）。
    pub fn register_agent(&self, agent_id: &str, host_id: &str, token: &str) {
        let (status, body) = self.post_json(
            "/api/gse/agents",
            &json!({"agent_id": agent_id, "host_id": host_id, "token": token}).to_string(),
        );
        assert!(
            (200..300).contains(&status),
            "登记 agent 失败: {status} {body}\n{}",
            self.logs
        );
    }

    /// 登记数据面并等到台账里 `status = online`（agent 才会拿到 ingest_url）。
    pub fn register_dataplane(&mut self, service_id: &str, ingest_url: &str, query_url: &str) {
        let (status, body) = self.post_json(
            "/api/gse/dataplanes",
            &json!({
                "service_id": service_id,
                "ingest_url": ingest_url,
                "query_url": query_url,
            })
            .to_string(),
        );
        assert!(
            (200..300).contains(&status),
            "登记 dataplane 失败: {status} {body}\n{}",
            self.logs
        );
        self.wait_for(format!("台账里 dataplane {service_id} online"), |gse| {
            let (status, value) =
                gse.get_json(&format!("/api/gse/dataplanes/{service_id}"))?;
            if status != 200 {
                return Err(format!("查 dataplane 返回 {status}: {value}"));
            }
            match value["status"].as_str() {
                Some("online") => Ok(()),
                other => Err(format!("status={other:?}")),
            }
        });
    }

    /// 写该 Agent 的期望 spec（整体覆盖，`spec` 就是 `PUT .../spec` 的 body）。
    pub fn put_spec(&self, agent_id: &str, spec: &Value) {
        let (status, body) = self.put_json(&format!("/api/gse/agents/{agent_id}/spec"), &spec.to_string());
        assert!(
            (200..300).contains(&status),
            "写 spec 失败: {status} {body}\n{}",
            self.logs
        );
    }

    /// 一条 `metrics_host` 采集项的 spec（主机指标：`cpu_usage` / `mem_usage`）。
    pub fn put_host_metrics_spec(
        &self,
        agent_id: &str,
        item_id: &str,
        name: &str,
        interval_secs: u64,
    ) {
        self.put_spec(
            agent_id,
            &json!({
                "params": {"heartbeat_interval_secs": 1},
                "items": [{
                    "item_id": item_id,
                    "name": name,
                    "kind": "metrics_host",
                    "enabled": true,
                    "collector": {"interval_secs": interval_secs},
                    "storage": {"retention_days": 1},
                }],
            }),
        );
    }

    /// 起 `gse-agent` 真进程（配置写 `<root>/gse-agent.toml`）。
    pub fn start_agent(&mut self, agent_id: &str, token: &str) {
        let (_, agent_bin) = build_bins();
        let otlp_port = free_port();
        let cfg = self.root.join("gse-agent.toml");
        std::fs::write(
            &cfg,
            format!(
                "server_addr = \"127.0.0.1:{}\"\n\
                 agent_id = \"{agent_id}\"\n\
                 token = \"{token}\"\n\
                 heartbeat_interval_secs = 1\n\
                 otlp_enabled = false\n\
                 otlp_listen = \"127.0.0.1:{otlp_port}\"\n\
                 job_work_dir = \"{}\"\n",
                self.agent_port,
                self.root.join("jobs").display()
            ),
        )
        .expect("write gse-agent.toml");
        let proc = Proc::spawn(
            "gse-agent",
            &agent_bin,
            &[],
            &[("GSE_AGENT_CONFIG", cfg.display().to_string())],
            &self.logs,
        );
        self.procs.push(proc);
    }

    /// 等台账里 agent `online` 且有心跳（认证 + 心跳写回）。
    pub fn wait_agent_online(&mut self, agent_id: &str) {
        self.wait_for(format!("台账里 agent {agent_id} online"), |gse| {
            let (status, value) = gse.get_json(&format!("/api/gse/agents/{agent_id}"))?;
            if status != 200 {
                return Err(format!("查 agent 返回 {status}: {value}"));
            }
            let agent_status = value["status"].as_str().unwrap_or("");
            let heartbeat = value["last_heartbeat_at"].as_str().unwrap_or("");
            if agent_status == "online" && !heartbeat.is_empty() {
                Ok(())
            } else {
                Err(format!("status={agent_status:?} last_heartbeat_at={heartbeat:?}"))
            }
        });
    }

    /// 等 agent 解析并生效下发 spec（回执经心跳落库 → `synced`）。
    pub fn wait_spec_synced(&mut self, agent_id: &str, item_id: &str) {
        self.wait_for(format!("spec {item_id} synced"), |gse| {
            let (status, value) = gse.get_json(&format!("/api/gse/agents/{agent_id}/spec"))?;
            if status != 200 {
                return Err(format!("查 spec 返回 {status}: {value}"));
            }
            let sync = value["sync_status"].as_str().unwrap_or("");
            let applied = value["applied"]["spec"]["items"][0]["item_id"]
                .as_str()
                .unwrap_or("");
            if sync == "synced" && applied == item_id {
                Ok(())
            } else {
                Err(format!("sync_status={sync:?} applied_item={applied:?}"))
            }
        });
    }

    /// 轮询直到 `probe` 返回 `Ok`；超时 panic 并带上最后一次观察值和子进程日志。
    pub fn wait_for<T>(
        &mut self,
        what: impl Into<String>,
        mut probe: impl FnMut(&mut Self) -> Result<T, String>,
    ) -> T {
        let what = what.into();
        let started = Instant::now();
        loop {
            match probe(self) {
                Ok(value) => return value,
                Err(reason) => {
                    self.check_alive();
                    if started.elapsed() >= self.timeout {
                        panic!(
                            "等待「{what}」超时（{:?}）\n最后一次观察: {reason}\n{}",
                            self.timeout, self.logs
                        );
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// 所有子进程都还活着（用例结尾调用：全程无异常退出）。
    pub fn check_alive(&mut self) {
        for proc in self.procs.iter_mut() {
            proc.check_alive();
        }
    }

    fn get_json(&self, path: &str) -> Result<(u16, Value), String> {
        let (status, text) = http::get(&format!("{}{path}", self.admin_url))?;
        let value = serde_json::from_str(&text)
            .map_err(|e| format!("GET {path} 返回非 JSON: {e}: {}", http::tail(&text)))?;
        Ok((status, value))
    }

    fn post_json(&self, path: &str, body: &str) -> (u16, String) {
        http::post_json(&format!("{}{path}", self.admin_url), body)
            .unwrap_or_else(|e| panic!("POST {path}: {e}\n{}", self.logs))
    }

    fn put_json(&self, path: &str, body: &str) -> (u16, String) {
        http::put_json(&format!("{}{path}", self.admin_url), body)
            .unwrap_or_else(|e| panic!("PUT {path}: {e}\n{}", self.logs))
    }
}

/// 两个 gse 二进制（`gse-server`、`gse-agent`）。
///
/// `CARGO_BIN_EXE_<name>` 只覆盖**当前包**的 bin，所以先按 `cargo build` 产出，再取同
/// profile 目录下的可执行文件。缺任何一个是硬失败，不跳过。
pub fn build_bins() -> (PathBuf, PathBuf) {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let status = Command::new(&cargo)
        .current_dir(&root)
        .args(["build", "-p", "gse-server", "-p", "gse-agent"])
        .status()
        .unwrap_or_else(|e| panic!("执行 `cargo build -p gse-server -p gse-agent` 失败：{e}"));
    assert!(
        status.success(),
        "`cargo build -p gse-server -p gse-agent` 失败，无法启动真进程链路"
    );
    let dir = profile_dir();
    let bin = |name: &str| {
        let path = dir.join(name);
        assert!(
            path.is_file(),
            "缺少被测二进制 {name}（期望路径 {}），请先在仓库根执行 `cargo build -p gse-server -p gse-agent`",
            path.display()
        );
        path
    };
    (bin("gse-server"), bin("gse-agent"))
}

/// 可执行文件目录：测试二进制在 `<target>/<profile>/deps/` 下，往上一层就是 `<target>/<profile>`。
/// 这样不必假设 `CARGO_TARGET_DIR` 或 profile，和 `CARGO_BIN_EXE_*` 落在同一处。
fn profile_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    exe.parent()
        .and_then(Path::parent)
        .expect("测试二进制应在 <target>/<profile>/deps/ 下")
        .to_path_buf()
}
