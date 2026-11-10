//! IT-CHAIN-01：gse-server → dataserver → gse-agent **三个真进程**的端到端链路。
//!
//! 断言顺序与真实链路一致：agent 认证后上线 → 解析并生效 GSE 下发的采集项 →
//! 按采集项把 metrics 上报到 dataserver `/v1/ingest` → Prom 即时查询回读带
//! `agent_id` / `item_id` 标签的点。
//!
//! 场景说明（前置数据、步骤、期望、清理）见 `test_plan/IT-CHAIN-01.md`。
//!
//! 设计约束：
//! - 不依赖固定端口（全部 `127.0.0.1:0` 现取现用）与外部中间件（redb + tsink 本地后端）。
//! - 不用 `#[ignore]`、不做「缺东西就 return」的静默跳过：缺二进制或启动失败直接 panic 并带上
//!   三个子进程的 stdout/stderr，避免「假绿」。
//! - 子进程由 [`Proc`] 的 `Drop` 回收（kill + wait）。

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const AGENT_ID: &str = "agent-it-chain";
const TOKEN: &str = "it-chain-token";
const ITEM_ID: &str = "item-it-chain";
const METRIC: &str = "cpu_usage";
/// 整条链路的等待上限：agent 每 1s 采集一次，`cpu_usage` 需要两轮快照（约 2s）。
const TIMEOUT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------- 子进程

/// 三个子进程的日志汇到一个缓冲区：任何断言失败都能整段打出来。
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<(String, String)>>>);

impl Logs {
    fn new() -> Self {
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
}

impl std::fmt::Display for Logs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let all = self.0.lock().expect("logs lock");
        for name in ["gse-server", "dataserver", "gse-agent"] {
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

// ---------------------------------------------------------------- 工具

struct Chain {
    logs: Logs,
    procs: Vec<Proc>,
}

impl Chain {
    fn new() -> Self {
        Self {
            logs: Logs::new(),
            procs: Vec::new(),
        }
    }

    fn start(
        &mut self,
        name: &'static str,
        bin: &Path,
        args: &[&str],
        envs: &[(&str, String)],
    ) -> usize {
        self.procs
            .push(Proc::spawn(name, bin, args, envs, &self.logs));
        self.procs.len() - 1
    }

    fn check_alive(&mut self) {
        for proc in self.procs.iter_mut() {
            proc.check_alive();
        }
    }
}

/// 现取一个回环空端口（跟着进程启动的毫秒级竞态不可避免，冲突时会以端口占用 panic）。
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 三个被测二进制。
///
/// `CARGO_BIN_EXE_<name>` 只覆盖**当前包**的 bin，所以 gse-server / gse-agent 先按
/// `cargo build` 产出，再取同目录下的可执行文件。缺任何一个是硬失败，不跳过。
fn chain_bins() -> (PathBuf, PathBuf, PathBuf) {
    let dataserver = PathBuf::from(env!("CARGO_BIN_EXE_dataserver"));
    let dir = dataserver
        .parent()
        .expect("dataserver bin dir")
        .to_path_buf();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // 顺带保证版本与源码一致：cargo test 本身不会构建这两个包。
    let status = Command::new(&cargo)
        .current_dir(&root)
        .args(["build", "-p", "gse-server", "-p", "gse-agent"])
        .status()
        .unwrap_or_else(|e| panic!("执行 `cargo build -p gse-server -p gse-agent` 失败：{e}"));
    assert!(
        status.success(),
        "`cargo build -p gse-server -p gse-agent` 失败，无法启动三真进程链路"
    );
    let bin = |name: &str| {
        let path = dir.join(name);
        assert!(
            path.is_file(),
            "缺少被测二进制 {name}（期望路径 {}），请先在仓库根执行 `cargo build -p gse-server -p gse-agent`",
            path.display()
        );
        path
    };
    (bin("gse-server"), bin("gse-agent"), dataserver)
}

fn http(method: &str, url: &str, body: Option<&str>) -> Result<(u16, String), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();
    let request = match method {
        "GET" => agent.get(url),
        "POST" => agent.post(url),
        "PUT" => agent.put(url),
        other => return Err(format!("不支持的 method {other}")),
    };
    let response = match body {
        Some(b) => request
            .set("Content-Type", "application/json")
            .send_string(b),
        None => request.call(),
    };
    match response {
        Ok(resp) => Ok((resp.status(), resp.into_string().unwrap_or_default())),
        Err(ureq::Error::Status(code, resp)) => Ok((code, resp.into_string().unwrap_or_default())),
        Err(e) => Err(format!("{method} {url}: {e}")),
    }
}

fn http_json(
    method: &str,
    url: &str,
    body: Option<&str>,
) -> Result<(u16, serde_json::Value), String> {
    let (status, text) = http(method, url, body)?;
    let value = serde_json::from_str(&text)
        .map_err(|e| format!("{method} {url} 返回非 JSON: {e}: {text}"))?;
    Ok((status, value))
}

/// 轮询直到 `probe` 返回 `Ok`；超时 panic 并带上最后一次观察值和子进程日志。
fn wait_for<T>(
    chain: &mut Chain,
    what: &str,
    mut probe: impl FnMut(&mut Chain) -> Result<T, String>,
) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match probe(chain) {
            Ok(value) => return value,
            Err(reason) => {
                chain.check_alive();
                if Instant::now() >= deadline {
                    panic!(
                        "等待「{what}」超时（{}s）\n最后一次观察: {reason}\n{}",
                        TIMEOUT.as_secs(),
                        chain.logs
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

// ---------------------------------------------------------------- 用例

#[test]
fn it_chain_agent_online_ingest_and_query_with_labels() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("data")).expect("create data dir");

    let (gse_server_bin, gse_agent_bin, dataserver_bin) = chain_bins();

    // 端口全部现取：gse-server 的 agent 口 / 管理口 / 自监控口，dataserver 的 SQL / Prom / 自监控口。
    let gse_agent_port = free_port();
    let gse_admin_port = free_port();
    let gse_metrics_port = free_port();
    let ds_sql_port = free_port();
    let ds_prom_port = free_port();
    let ds_metrics_port = free_port();
    let otlp_port = free_port();

    let gse_admin_url = format!("http://127.0.0.1:{gse_admin_port}");
    let ds_sql_url = format!("http://127.0.0.1:{ds_sql_port}");
    let ds_prom_url = format!("http://127.0.0.1:{ds_prom_port}");

    // ---- 1. gse-server 真进程（临时台账 sqlite）
    let gse_server_cfg = root.join("gse-server.toml");
    std::fs::write(
        &gse_server_cfg,
        format!(
            "listen = \"127.0.0.1:{gse_agent_port}\"\n\
             auth_enabled = true\n\
             db = \"{}\"\n\
             http_enabled = true\n\
             http_listen = \"127.0.0.1:{gse_admin_port}\"\n\
             metrics_listen = \"127.0.0.1:{gse_metrics_port}\"\n\
             dataplane_probe_interval_secs = 1\n\
             heartbeat_interval_secs = 1\n\
             heartbeat_timeout_secs = 30\n\
             session_probe_interval_secs = 2\n",
            root.join("gse-server.db").display()
        ),
    )
    .expect("write gse-server.toml");

    let mut chain = Chain::new();
    chain.start(
        "gse-server",
        &gse_server_bin,
        &[],
        &[("GSE_SERVER_CONFIG", gse_server_cfg.display().to_string())],
    );

    // ---- 2. dataserver 真进程（redb + tsink，本地目录；管理口反代到 gse-server）
    let ds_cfg = root.join("dataserver.toml");
    std::fs::write(
        &ds_cfg,
        format!(
            "data_path = \"{data}\"\n\
             gse_admin_url = \"{gse_admin_url}\"\n\
             kv_backend = \"redb\"\n\
             ts_backend = \"tsink\"\n\
             ts_retention_enforced = false\n\
             ts_clean_interval_secs = 3600\n\
             apm_enabled = false\n\
             self_metrics_interval_secs = 0\n\
             \n\
             [sql_http]\nlisten = \"127.0.0.1:{ds_sql_port}\"\n\
             \n[prom_http]\nlisten = \"127.0.0.1:{ds_prom_port}\"\n\
             \n[metrics_http]\nlisten = \"127.0.0.1:{ds_metrics_port}\"\n\
             \n[auth]\nenabled = false\n",
            data = root.join("data").display(),
        ),
    )
    .expect("write dataserver.toml");

    chain.start(
        "dataserver",
        &dataserver_bin,
        &["--config", ds_cfg.to_str().expect("utf8 path")],
        &[],
    );

    // ---- 3. 等两个 HTTP 面健康
    wait_for(&mut chain, "gse-server /health", |_| {
        let (status, _) = http("GET", &format!("{gse_admin_url}/health"), None)?;
        if status == 200 {
            Ok(())
        } else {
            Err(format!("gse-server /health 返回 {status}"))
        }
    });
    wait_for(&mut chain, "dataserver /health", |_| {
        let (status, body) = http("GET", &format!("{ds_sql_url}/health"), None)?;
        if status == 200 {
            Ok(())
        } else {
            Err(format!("dataserver /health 返回 {status}: {body}"))
        }
    });

    // ---- 4. 台账登记 agent（认证前必须存在：auth 只 UPDATE status）
    let (status, body) = http(
        "POST",
        &format!("{gse_admin_url}/api/gse/agents"),
        Some(
            &serde_json::json!({
                "agent_id": AGENT_ID,
                "host_id": "host-it-chain",
                "token": TOKEN,
            })
            .to_string(),
        ),
    )
    .expect("create agent");
    assert!(
        (200..300).contains(&status),
        "登记 agent 失败: {status} {body}\n{}",
        chain.logs
    );

    // ---- 5. 登记 dataserver 为数据面，等探活 online（agent 才会拿到 ingest_url）
    let (status, body) = http(
        "POST",
        &format!("{gse_admin_url}/api/gse/dataplanes"),
        Some(
            &serde_json::json!({
                "service_id": "ds-it-chain",
                "ingest_url": ds_sql_url,
                "query_url": ds_prom_url,
            })
            .to_string(),
        ),
    )
    .expect("create dataplane");
    assert!(
        (200..300).contains(&status),
        "登记 dataplane 失败: {status} {body}\n{}",
        chain.logs
    );
    wait_for(&mut chain, "gse-server 台账里 dataplane online", |_| {
        let (status, value) = http_json(
            "GET",
            &format!("{gse_admin_url}/api/gse/dataplanes/ds-it-chain"),
            None,
        )?;
        if status != 200 {
            return Err(format!("查 dataplane 返回 {status}: {value}"));
        }
        match value["status"].as_str() {
            Some("online") => Ok(()),
            other => Err(format!("status={other:?}")),
        }
    });

    // ---- 6. 写该 Agent 的期望 spec：1s 一轮的主机指标采集项
    let (status, body) = http(
        "PUT",
        &format!("{gse_admin_url}/api/gse/agents/{AGENT_ID}/spec"),
        Some(
            &serde_json::json!({
                "params": {"heartbeat_interval_secs": 1},
                "items": [{
                    "item_id": ITEM_ID,
                    "name": "it-chain host metrics",
                    "kind": "metrics_host",
                    "enabled": true,
                    "collector": {"interval_secs": 1},
                    "storage": {"retention_days": 1},
                }],
            })
            .to_string(),
        ),
    )
    .expect("put agent spec");
    assert!(
        (200..300).contains(&status),
        "写 spec 失败: {status} {body}\n{}",
        chain.logs
    );

    // ---- 7. gse-agent 真进程
    let agent_cfg = root.join("gse-agent.toml");
    std::fs::write(
        &agent_cfg,
        format!(
            "server_addr = \"127.0.0.1:{gse_agent_port}\"\n\
             agent_id = \"{AGENT_ID}\"\n\
             token = \"{TOKEN}\"\n\
             heartbeat_interval_secs = 1\n\
             otlp_enabled = false\n\
             otlp_listen = \"127.0.0.1:{otlp_port}\"\n\
             job_work_dir = \"{}\"\n",
            root.join("jobs").display()
        ),
    )
    .expect("write gse-agent.toml");
    chain.start(
        "gse-agent",
        &gse_agent_bin,
        &[],
        &[("GSE_AGENT_CONFIG", agent_cfg.display().to_string())],
    );

    // ---- 8. agent 上线（认证 + 心跳写回台账）
    wait_for(&mut chain, "台账里 agent online", |_| {
        let (status, value) = http_json(
            "GET",
            &format!("{gse_admin_url}/api/gse/agents/{AGENT_ID}"),
            None,
        )?;
        if status != 200 {
            return Err(format!("查 agent 返回 {status}: {value}"));
        }
        let agent_status = value["status"].as_str().unwrap_or("");
        let heartbeat = value["last_heartbeat_at"].as_str().unwrap_or("");
        if agent_status == "online" && !heartbeat.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "status={agent_status:?} last_heartbeat_at={heartbeat:?}"
            ))
        }
    });

    // ---- 9. agent 解析并生效下发 spec（回执经心跳落库 → synced）
    wait_for(
        &mut chain,
        "spec synced（agent 已生效下发采集项）",
        |_| {
            let (status, value) = http_json(
                "GET",
                &format!("{gse_admin_url}/api/gse/agents/{AGENT_ID}/spec"),
                None,
            )?;
            if status != 200 {
                return Err(format!("查 spec 返回 {status}: {value}"));
            }
            let sync = value["sync_status"].as_str().unwrap_or("");
            let applied_item = value["applied"]["spec"]["items"][0]["item_id"]
                .as_str()
                .unwrap_or("");
            if sync == "synced" && applied_item == ITEM_ID {
                Ok(())
            } else {
                Err(format!(
                    "sync_status={sync:?} applied_item={applied_item:?}"
                ))
            }
        },
    );

    // ---- 10. 数据落到时序库：Prom 即时查询能回读到带标签的点
    let (value, labels) = wait_for(&mut chain, "Prom 查到 cpu_usage 数据点", |_| {
        let url = format!(
            "{ds_prom_url}/api/v1/query?query={METRIC}&time={}",
            now_secs()
        );
        let (status, value) = http_json("GET", &url, None)?;
        if status != 200 {
            return Err(format!("Prom 查询返回 {status}: {value}"));
        }
        assert_eq!(value["status"], "success", "Prom 查询失败: {value}");
        let Some(result) = value["data"]["result"].as_array() else {
            return Err(format!("data.result 不是数组: {value}"));
        };
        let Some(first) = result.first() else {
            return Err("data.result 为空（尚未入库）".to_string());
        };
        let metric = first["metric"].clone();
        if metric["agent_id"] != AGENT_ID || metric["item_id"] != ITEM_ID {
            return Err(format!("标签不匹配: {metric}"));
        }
        let Some(sample) = first["value"].as_array().and_then(|v| v.get(1)) else {
            return Err(format!("value 结构异常: {first}"));
        };
        // Prom 的 `value[1]` 是 JSON 数字（字符串形式也一并接受，避免不同实现差异）。
        let parsed = sample
            .as_f64()
            .or_else(|| sample.as_str().and_then(|s| s.parse::<f64>().ok()));
        if parsed.is_none() {
            return Err(format!("value 不是数字: {sample}"));
        }
        Ok((parsed.expect("checked"), metric))
    });
    assert!(
        (0.0..=100.0).contains(&value),
        "cpu_usage 超出 0..=100: {value}"
    );
    assert_eq!(labels["agent_id"], AGENT_ID);
    assert_eq!(labels["item_id"], ITEM_ID);

    // ---- 11. 全程三个进程都没有异常退出
    chain.check_alive();
}
