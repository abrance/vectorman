//! IT-VMCTL-01：真 `vmctl` 二进制 → 真 `gse-server` 台账 → 真 `dataserver` 流索引。
//!
//! 覆盖的问题：`agents status` / `agents doctor` 对 gse-server 返回字段的解析此前**只有手写 JSON
//! 夹具**，夹具里的字段名是我们自己写的，所以真实字段漂移测不出来 —— `4a91826` 就是这么漏的：
//! `ebpf_tcp` 被按「eBPF 家族都出边记录」的猜测映射到 `ebpf_edges`，而它实际只在 `metrics` 上报，
//! 于是一个健康的采集项被报成 `not_reporting`。
//!
//! 本用例的输入全部来自真进程：
//! - spec / 会话 / 心跳来自真 `gse-server` 台账（HTTP 管理口写入 + 真 `gse-agent` 认证心跳）；
//! - 流索引来自真 `dataserver`（真 `gse-agent` 上报的 metrics + 经 `/v1/ingest` 真实接入路径写入的
//!   `ebpf_tcp`/`ebpf_network` 两条流）；断言直接读 `/v1/streams` 台账，不与夹具比对。
//!
//! 场景说明（前置数据、步骤、期望、清理）见 `test_plan/IT-VMCTL-01.md`。
//!
//! 设计约束：
//! - 端口与目录全部现取（`127.0.0.1:0` + 临时目录），不依赖外部中间件与本机既有部署。
//! - 无 `#[ignore]`、无 feature gate、无「缺东西就 return」：缺二进制或启动失败直接 panic 并带上
//!   子进程 stdout/stderr，避免「假绿」。
//! - 子进程由 [`Proc`] 的 `Drop` 回收（kill + wait）。

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use vectorman_test_support::util::free_port;

const AGENT_ID: &str = "agent-it-vmctl";
const HOST_ID: &str = "host-it-vmctl";
const TOKEN: &str = "it-vmctl-token";
const ITEM_METRICS: &str = "item-it-vmctl-metrics";
const ITEM_TCP: &str = "item-it-vmctl-tcp";
const ITEM_NET: &str = "item-it-vmctl-net";
const ITEM_LOG: &str = "item-it-vmctl-log";
/// 起停 + 上报的等待上限（含测试内的 `cargo build`）。
const TIMEOUT: Duration = Duration::from_secs(180);

// ---------------------------------------------------------------- 临时目录

/// 临时根目录：`Drop` 里递归删除（台账 sqlite、redb、tsink、日志）。
struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("it-vmctl-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create temp root");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------- 子进程

/// 子进程日志汇到一个缓冲区：任何断言失败都能整段打出来。
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
                for line in lines.iter().skip(lines.len().saturating_sub(40)) {
                    writeln!(f, "{line}")?;
                }
            }
        }
        Ok(())
    }
}

/// 受管子进程：`Drop` 里 kill + wait，测试失败也不留孤儿进程。
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
        // 清掉继承来的配置环境变量：外部 shell 里的 DATASERVER_* / DP_* / GSE_* 会把
        // 临时配置覆盖掉，症状是「进程起来了但连错地方」。
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

    /// 停掉一个子进程（`drop` 即 kill + wait）。用于冻结流索引计数后再逐项比对。
    fn stop(&mut self, idx: usize) {
        drop(self.procs.remove(idx));
    }
}

// ---------------------------------------------------------------- 工具

fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// 被测二进制：`vmctl` 由 `CARGO_BIN_EXE_vmctl` 提供，另外两个真进程先 `cargo build` 再取同目录可执行文件。
/// 缺任何一个都是硬失败 —— 不跳过。
fn it_bins() -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let vmctl = PathBuf::from(env!("CARGO_BIN_EXE_vmctl"));
    let dir = vmctl.parent().expect("vmctl bin dir").to_path_buf();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // cargo test 只构建 vmctl 自己；gse-server / gse-agent / dataserver 要显式构建。
    let status = Command::new(&cargo)
        .current_dir(&root)
        .args([
            "build",
            "-p",
            "gse-server",
            "-p",
            "gse-agent",
            "-p",
            "dataserver",
        ])
        .status()
        .unwrap_or_else(|e| {
            panic!("执行 `cargo build -p gse-server -p gse-agent -p dataserver` 失败：{e}")
        });
    assert!(
        status.success(),
        "`cargo build -p gse-server -p gse-agent -p dataserver` 失败，无法启动真进程链路"
    );
    let bin = |name: &str| {
        let path = dir.join(name);
        assert!(
            path.is_file(),
            "缺少被测二进制 {name}（期望路径 {}），请先在仓库根执行 \
             `cargo build -p gse-server -p gse-agent -p dataserver`",
            path.display()
        );
        path
    };
    (
        vmctl,
        bin("gse-server"),
        bin("gse-agent"),
        bin("dataserver"),
    )
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

/// 轮询直到 `probe` 返回 `Ok`；超时 panic 并带上最后一次观察值与子进程日志。
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

/// 跑真 `vmctl` 二进制，返回 `(退出码, stdout, stderr)`。
fn run_vmctl(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_vmctl"))
        .args(args)
        .output()
        .expect("执行 vmctl 二进制失败");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `dataserver /v1/streams` 的一行（真实流索引台账）。
#[derive(Debug, Clone)]
struct Stream {
    data_type: String,
    data_id: String,
    last_seen_micros: i64,
    accepted: i64,
}

fn ledger_streams(ds_url: &str) -> Result<Vec<Stream>, String> {
    let (status, value) = http_json("GET", &format!("{ds_url}/v1/streams"), None)?;
    if status != 200 {
        return Err(format!("/v1/streams 返回 {status}: {value}"));
    }
    let arr = value["streams"]
        .as_array()
        .ok_or_else(|| format!("streams 不是数组: {value}"))?;
    Ok(arr
        .iter()
        .filter_map(|s| {
            Some(Stream {
                data_type: s.get("data_type")?.as_str()?.to_string(),
                data_id: s.get("data_id")?.as_str()?.to_string(),
                last_seen_micros: s.get("last_seen_micros")?.as_i64()?,
                accepted: s.get("accepted")?.as_i64()?,
            })
        })
        .collect())
}

/// `vmctl` 表格输出的一行。`last_seen` 为相对年龄，无 stream 时该列为空（整行少一列）。
#[derive(Debug, Clone)]
struct Row {
    item_id: String,
    data_type: String,
    last_seen: String,
    accepted: i64,
    verdict: String,
}

/// 解析 `agents status` / `agents doctor` 的采集项表格。
///
/// 两处表头列数不同（6 列 vs 5 列），所以按「最后两列是 accepted / verdict」取值，
/// 有 6 列时 `cells[3]` 才是 last_seen —— 这样也不会因为空单元格（少一列）漏行。
fn parse_rows(stdout: &str, known_items: &[String]) -> Vec<Row> {
    let mut rows = Vec::new();
    for line in stdout.lines() {
        let cells: Vec<&str> = line.split_whitespace().collect();
        if cells.len() < 4 || !known_items.iter().any(|i| i == cells[0]) {
            continue;
        }
        let verdict = cells[cells.len() - 1].to_string();
        let accepted = cells[cells.len() - 2]
            .parse::<i64>()
            .unwrap_or_else(|_| panic!("accepted 列不是整数: {line}"));
        let last_seen = if cells.len() == 6 {
            cells[3].to_string()
        } else {
            String::new()
        };
        rows.push(Row {
            item_id: cells[0].to_string(),
            data_type: cells[1].to_string(),
            last_seen,
            accepted,
            verdict,
        });
    }
    rows
}

// ---------------------------------------------------------------- 用例

#[test]
fn it_vmctl_status_and_doctor_agree_with_gse_ledger() {
    let root = TempRoot::new();
    std::fs::create_dir_all(root.path().join("data")).expect("create data dir");
    std::fs::create_dir_all(root.path().join("jobs")).expect("create jobs dir");

    let (_, gse_server_bin, gse_agent_bin, dataserver_bin) = it_bins();

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

    let mut chain = Chain::new();

    // ---- 1. gse-server 真进程（临时台账 sqlite）
    let gse_cfg = root.path().join("gse-server.toml");
    std::fs::write(
        &gse_cfg,
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
            root.path().join("gse-server.db").display()
        ),
    )
    .expect("write gse-server.toml");
    chain.start(
        "gse-server",
        &gse_server_bin,
        &[],
        &[("GSE_SERVER_CONFIG", gse_cfg.display().to_string())],
    );

    // ---- 2. dataserver 真进程（redb + tsink，本地目录；管理口反代到 gse-server）
    let ds_cfg = root.path().join("dataserver.toml");
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
            data = root.path().join("data").display(),
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

    // ---- 4. 台账登记 agent（auth 只 UPDATE status，登记必须先于认证）
    let (status, body) = http(
        "POST",
        &format!("{gse_admin_url}/api/gse/agents"),
        Some(
            &serde_json::json!({
                "agent_id": AGENT_ID,
                "host_id": HOST_ID,
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
                "service_id": "ds-it-vmctl",
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
    wait_for(&mut chain, "台账里 dataplane online", |_| {
        let (status, value) = http_json(
            "GET",
            &format!("{gse_admin_url}/api/gse/dataplanes/ds-it-vmctl"),
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

    // ---- 6. 期望 spec：一个真采的 metrics_host + 两个 eBPF 项（其中 ebpf_tcp 是 4a91826 的现场）
    //      + 一个停用的 log_file（停用项不该参与判定）
    let (status, body) = http(
        "PUT",
        &format!("{gse_admin_url}/api/gse/agents/{AGENT_ID}/spec"),
        Some(
            &serde_json::json!({
                "params": {"heartbeat_interval_secs": 1},
                "items": [
                    {
                        "item_id": ITEM_METRICS,
                        "name": "it-vmctl host metrics",
                        "kind": "metrics_host",
                        "enabled": true,
                        "collector": {"interval_secs": 1},
                        "storage": {"retention_days": 1},
                    },
                    {
                        "item_id": ITEM_TCP,
                        "name": "it-vmctl ebpf tcp",
                        "kind": "ebpf_tcp",
                        "enabled": true,
                        "collector": {"interval_secs": 60},
                        "storage": {"retention_days": 1},
                    },
                    {
                        "item_id": ITEM_NET,
                        "name": "it-vmctl ebpf network",
                        "kind": "ebpf_network",
                        "enabled": true,
                        "collector": {"interval_secs": 60},
                        "storage": {"retention_days": 1},
                    },
                    {
                        "item_id": ITEM_LOG,
                        "name": "it-vmctl app log (disabled)",
                        "kind": "log_file",
                        "enabled": false,
                        "collector": {"interval_secs": 15, "path_patterns": ["/tmp/it-vmctl/*.log"]},
                        "storage": {"retention_days": 1},
                    },
                ],
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

    // ---- 7. gse-agent 真进程：认证 + 心跳 + 生效 spec + 上报 metrics_host
    let agent_cfg = root.path().join("gse-agent.toml");
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
            root.path().join("jobs").display()
        ),
    )
    .expect("write gse-agent.toml");
    let agent_idx = chain.start(
        "gse-agent",
        &gse_agent_bin,
        &[],
        &[("GSE_AGENT_CONFIG", agent_cfg.display().to_string())],
    );

    // ---- 8. agent 上线（认证 + 心跳落库）
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

    // ---- 9. agent 解析并生效下发 spec（回执经心跳落库 → synced）。
    //         注意：eBPF 项在本机可能前置校验失败（非 root / 无 BTF），agent 会**只降级该项**，
    //         不影响 spec 生效，也不影响其它采集项 —— 这正是要覆盖的现场条件。
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
            let applied: Vec<&str> = value["applied"]["spec"]["items"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|i| i.get("item_id").and_then(|x| x.as_str()))
                        .collect()
                })
                .unwrap_or_default();
            if sync == "synced" && applied.len() == 4 {
                Ok(())
            } else {
                Err(format!("sync_status={sync:?} applied_items={applied:?}"))
            }
        },
    );

    // ---- 10. 流索引：真 agent 上报的 metrics_host 必须落到 dataserver
    wait_for(
        &mut chain,
        "dataserver 里出现 metrics_host 的流",
        |_| {
            let streams = ledger_streams(&ds_sql_url)?;
            if streams
                .iter()
                .any(|s| s.data_type == "metrics" && s.data_id == ITEM_METRICS)
            {
                Ok(())
            } else {
                Err(format!("streams={streams:?}"))
            }
        },
    );

    // ---- 11. 走 dataserver 真实接入路径补两条 eBPF 流：ebpf_tcp 只发 metrics（与真实现一致），
    //         ebpf_network 发 ebpf_edges。**故意不给 ebpf_tcp 发 ebpf_edges** —— 这就是
    //         4a91826 的判定现场：按 `emits_edges` 猜映射的实现会在这里判 not_reporting。
    let ts = now_micros();
    let (status, body) = http(
        "POST",
        &format!("{ds_sql_url}/v1/ingest"),
        Some(
            &serde_json::json!({
                "batch_id": "b-it-vmctl-tcp",
                "data_type": "metrics",
                "data_id": ITEM_TCP,
                "agent_id": AGENT_ID,
                "host_id": HOST_ID,
                "sent_at_micros": ts,
                "records": [{
                    "record_id": "r-it-vmctl-tcp-1",
                    "timestamp": ts,
                    "measurement": "tcp_connect_latency_micros",
                    "tags": {"proto": "tcp"},
                    "field_name": "value",
                    "field_value": 1234.0
                }]
            })
            .to_string(),
        ),
    )
    .expect("ingest tcp metrics");
    assert!(
        (200..300).contains(&status),
        "ebpf_tcp 的 metrics 接入失败: {status} {body}\n{}",
        chain.logs
    );

    let (status, body) = http(
        "POST",
        &format!("{ds_sql_url}/v1/ingest"),
        Some(
            &serde_json::json!({
                "batch_id": "b-it-vmctl-net",
                "data_type": "ebpf_edges",
                "data_id": ITEM_NET,
                "agent_id": AGENT_ID,
                "host_id": HOST_ID,
                "sent_at_micros": ts,
                "records": [{
                    "record_id": "e-it-vmctl-net-1",
                    "timestamp": ts,
                    "bucket_micros": 10_000_000,
                    "protocol": "tcp",
                    "src_ip": "10.10.0.1",
                    "src_port": 41000,
                    "dst_ip": "10.10.0.2",
                    "dst_port": 80,
                    "connections": 5,
                    "bytes_sent": 1024,
                    "bytes_recv": 2048,
                    "duration_micros_sum": 5000,
                    "duration_micros_max": 1200,
                    "tcp_retrans": 0,
                    "tcp_resets": 0,
                    "failures": 0
                }]
            })
            .to_string(),
        ),
    )
    .expect("ingest net edges");
    assert!(
        (200..300).contains(&status),
        "ebpf_network 的 ebpf_edges 接入失败: {status} {body}\n{}",
        chain.logs
    );

    wait_for(&mut chain, "dataserver 里出现两条 eBPF 流", |_| {
        let streams = ledger_streams(&ds_sql_url)?;
        let has = |dt: &str, id: &str| streams.iter().any(|s| s.data_type == dt && s.data_id == id);
        if has("metrics", ITEM_TCP) && has("ebpf_edges", ITEM_NET) {
            Ok(())
        } else {
            Err(format!("streams={streams:?}"))
        }
    });

    // ---- 12. 台账侧的「期望值」全部从真进程读，不写死：
    //         采集项集合来自 gse-server 的 desired spec，流来自 dataserver 的 /v1/streams。
    let (status, spec_view) = http_json(
        "GET",
        &format!("{gse_admin_url}/api/gse/agents/{AGENT_ID}/spec"),
        None,
    )
    .expect("read spec");
    assert_eq!(status, 200, "读 spec 失败: {spec_view}");
    let items = spec_view["desired"]["spec"]["items"]
        .as_array()
        .expect("desired.spec.items 必须是数组");
    let mut enabled: BTreeMap<String, bool> = BTreeMap::new();
    for item in items {
        enabled.insert(
            item["item_id"].as_str().expect("item_id").to_string(),
            item["enabled"].as_bool().unwrap_or(false),
        );
    }
    assert_eq!(
        enabled.len(),
        4,
        "前置条件：台账里必须是 4 个采集项，实际 {enabled:?}"
    );
    let known_items: Vec<String> = enabled.keys().cloned().collect();

    // ---- 13. vmctl doctor（agent 仍在线）：会话口径必须来自台账，且所有启用项 reporting
    let (code, out, err) = run_vmctl(&[
        "--url",
        &gse_admin_url,
        "--data-url",
        &ds_sql_url,
        "agents",
        "doctor",
        AGENT_ID,
    ]);
    assert_eq!(
        code, 0,
        "vmctl agents doctor 退出码应为 0\n--- stdout ---\n{out}\n--- stderr ---\n{err}\n{}",
        chain.logs
    );
    assert!(
        out.contains("session_state: online"),
        "doctor 没把台账里的会话状态解析出来\n{out}"
    );
    assert!(out.contains("summary: ok"), "doctor 汇总不是 ok\n{out}");
    // 会话口径与台账一致（不是「默认 online」）
    let (status, agents) =
        http_json("GET", &format!("{gse_admin_url}/api/gse/agents"), None).expect("read agents");
    assert_eq!(status, 200, "{agents}");
    let ledger_session = agents
        .as_array()
        .expect("agents 必须是数组")
        .iter()
        .find(|a| a["agent_id"].as_str() == Some(AGENT_ID))
        .expect("台账里应有该 agent")["session_state"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert_eq!(
        ledger_session, "online",
        "前置条件：台账 session_state 必须 online，实际 {ledger_session:?}"
    );
    let doctor_rows = parse_rows(&out, &known_items);
    assert_eq!(doctor_rows.len(), 4, "doctor 采集项表格行数不对\n{out}");
    let doctor_tcp = doctor_rows
        .iter()
        .find(|r| r.item_id == ITEM_TCP)
        .expect("doctor 缺少 ebpf_tcp 行");
    assert_eq!(
        doctor_tcp.data_type, "metrics",
        "doctor 把 ebpf_tcp 的 data_type 解析成了 {}（4a91826：它只在 metrics 上报）\n{out}",
        doctor_tcp.data_type
    );
    assert_eq!(
        doctor_tcp.verdict, "reporting",
        "doctor 把健康的 ebpf_tcp 判成 {}\n{out}",
        doctor_tcp.verdict
    );

    // ---- 14. 停掉 agent 冻结 stream 计数，再把 vmctl 输出与 /v1/streams 台账逐项比对
    chain.stop(agent_idx);
    let mut previous = ledger_streams(&ds_sql_url).expect("streams");
    let settle_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let now = ledger_streams(&ds_sql_url).expect("streams");
        let same = now.len() == previous.len()
            && now.iter().zip(previous.iter()).all(|(a, b)| {
                a.data_id == b.data_id
                    && a.data_type == b.data_type
                    && a.accepted == b.accepted
                    && a.last_seen_micros == b.last_seen_micros
            });
        if same {
            break;
        }
        assert!(
            Instant::now() < settle_deadline,
            "停了 agent 之后流索引仍在变，无法逐项比对\n{now:?}"
        );
        previous = now;
    }

    let ledger = ledger_streams(&ds_sql_url).expect("streams");
    let find_stream = |data_type: &str, data_id: &str| {
        ledger
            .iter()
            .find(|s| s.data_type == data_type && s.data_id == data_id)
    };
    // 前置条件自检：traps 成立才有意义
    assert!(
        find_stream("ebpf_edges", ITEM_TCP).is_none(),
        "前置条件：台账里不该有 ebpf_tcp 的 ebpf_edges 流，否则测不出映射错误\n{ledger:?}"
    );
    assert!(
        find_stream("metrics", ITEM_TCP).is_some(),
        "前置条件：台账里应有 ebpf_tcp 的 metrics 流\n{ledger:?}"
    );
    assert!(
        find_stream("ebpf_edges", ITEM_NET).is_some(),
        "前置条件：台账里应有 ebpf_network 的 ebpf_edges 流\n{ledger:?}"
    );

    let (code, out, err) = run_vmctl(&[
        "--url",
        &gse_admin_url,
        "--data-url",
        &ds_sql_url,
        "agents",
        "status",
        AGENT_ID,
    ]);
    assert_eq!(
        code, 0,
        "vmctl agents status 退出码应为 0\n--- stdout ---\n{out}\n--- stderr ---\n{err}"
    );
    assert!(out.contains("spec: synced"), "spec 未同步\n{out}");
    assert!(out.contains("summary: ok"), "{out}");

    let rows = parse_rows(&out, &known_items);
    assert_eq!(
        rows.len(),
        enabled.len(),
        "表格行数应等于台账采集项数\n{out}"
    );
    let mut seen: Vec<&str> = rows.iter().map(|r| r.item_id.as_str()).collect();
    seen.sort_unstable();
    let mut want: Vec<&str> = known_items.iter().map(String::as_str).collect();
    want.sort_unstable();
    assert_eq!(seen, want, "vmctl 报的 item_id 与台账不一致\n{out}");

    for row in &rows {
        let expect_enabled = enabled[&row.item_id];
        if !expect_enabled {
            assert_eq!(
                row.verdict, "disabled",
                "停用采集项应判 disabled，实际 {}（{}）",
                row.verdict, row.item_id
            );
            assert_eq!(row.accepted, 0, "停用采集项不应有 accepted 计数");
            assert_eq!(row.data_type, "logs", "log_file 的 data_type 应为 logs");
            continue;
        }
        // 每个启用项：vmctl 报的 (data_type, item_id) 必须在台账流索引里真实存在，
        // 且计数完全一致 —— 这就是「解析结果 = gse-server/dataserver 台账」的逐项断言。
        let stream = find_stream(&row.data_type, &row.item_id).unwrap_or_else(|| {
            panic!(
                "vmctl 报了台账里不存在的流: {} / {}\n{out}",
                row.item_id, row.data_type
            )
        });
        assert_eq!(
            row.accepted, stream.accepted,
            "accepted 与台账不一致: {} / {}",
            row.item_id, row.data_type
        );
        assert_eq!(
            row.verdict, "reporting",
            "采集项 {} 在台账有 {} 流且新鲜，应判 reporting，实际 {}\n{out}",
            row.item_id, row.data_type, row.verdict
        );
        assert!(
            !row.last_seen.is_empty(),
            "reporting 行缺少 last_seen: {} / {}",
            row.item_id,
            row.data_type
        );
        let age = now_micros() - stream.last_seen_micros;
        assert!(
            age <= 60_000_000,
            "台账里的 last_seen 不新鲜（{age}µs），判定前提不成立"
        );
    }

    // ---- 15. 4a91826 的定点断言：ebpf_tcp 的状态来自 metrics，不是 ebpf_edges
    let tcp = rows.iter().find(|r| r.item_id == ITEM_TCP).expect("tcp 行");
    assert_eq!(
        tcp.data_type, "metrics",
        "ebpf_tcp 必须按 metrics 核验（4a91826：报 ebpf_edges 就是把 emits_edges 当成了产出类型）\n{out}"
    );
    assert_eq!(
        tcp.verdict, "reporting",
        "ebpf_tcp 在台账里有新鲜 metrics 流，判成 {} 说明映射漂移回归了\n{out}",
        tcp.verdict
    );
    let net = rows.iter().find(|r| r.item_id == ITEM_NET).expect("net 行");
    assert_eq!(
        net.data_type, "ebpf_edges",
        "ebpf_network 是家族里唯一出边记录的，应按 ebpf_edges 核验\n{out}"
    );
    let metrics = rows
        .iter()
        .find(|r| r.item_id == ITEM_METRICS)
        .expect("metrics 行");
    assert_eq!(metrics.data_type, "metrics");

    chain.check_alive();
}
