//! `IT-TS-RESTART-01`：tsink 本地后端在**真进程 SIGTERM → 同 `data_path` 重启**后的数据一致性。
//!
//! 动机：tsink 的三次竞态修复之后一直没有 reopen 回归测试；运维口径里 WAL 回放要 4–6 分钟
//! （cloud3 实测），意味着「重启后数据还在」这件事只能靠测试保证，不能靠人肉观察。
//!
//! 断言口径是**等价**，不是「进程起来了就算过」：
//! 重启后读到的序列数、每条序列的样本时间戳与值，必须与重启前逐项相等。
//!
//! 场景定义（唯一事实来源）：`test_plan/IT-TS-RESTART-01.md`。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");

/// `apply_env()` 会读的全部变量。测试必须把它们清空，否则开发机上残留的
/// `DATASERVER_*` 会把配置文件里的值悄悄改掉（尤其是 `dp_data_path`，会让两次重启指向不同目录）。
const CONFIG_ENV_VARS: &[&str] = &[
    "DP_DATA_PATH",
    "DP_SQL_HTTP_LISTEN",
    "DP_PROM_HTTP_LISTEN",
    "DP_METRICS_HTTP_LISTEN",
    "DP_AUTH_ENABLED",
    "DP_SELF_METRICS_INTERVAL",
    "DATASERVER_HTTP_WEB_DIR",
    "DATASERVER_GSE_ADMIN_URL",
    "DATASERVER_GSE_ADMIN_PASSWORD",
    "DATASERVER_KV_CACHE_BYTES",
    "DATASERVER_INGEST_DEDUP_RETENTION_DAYS",
    "DATASERVER_TS_RETENTION_DAYS",
    "DATASERVER_TS_RETENTION_ENFORCED",
    "DATASERVER_TS_CARDINALITY_LIMIT",
    "DATASERVER_TS_MEMORY_LIMIT_BYTES",
    "DATASERVER_TS_WAL_SIZE_LIMIT_BYTES",
    "DATASERVER_TS_CLEAN_INTERVAL",
    "DATASERVER_APM_ENABLED",
    "DATASERVER_APM_RETENTION_DAYS",
    "DATASERVER_APM_ENDPOINT_RETENTION_DAYS",
    "DATASERVER_APM_MAX_BYTES",
    "DATASERVER_APM_CLEAN_INTERVAL",
    "DATASERVER_APM_AGG_INTERVAL",
    "DATASERVER_APM_INGEST_MAX_BATCHES_PER_SEC",
    "DATASERVER_APM_MIN_DURATION_MICROS_FOR_DETAIL",
    "DATASERVER_KV_BACKEND",
    "DATASERVER_REDIS_URL",
    "DATASERVER_REDIS_PASSWORD",
    "DATASERVER_REDIS_PREFIX",
    "DATASERVER_TS_BACKEND",
    "DATASERVER_VM_INSERT_URL",
    "DATASERVER_VM_SELECT_URL",
    "DATASERVER_VM_TENANT",
    "DATASERVER_VM_SINGLE_NODE",
    "DATASERVER_VM_BATCH_SIZE",
    "DATASERVER_VM_FLUSH_INTERVAL_MS",
    "DATASERVER_VM_QUEUE_CAPACITY",
    "DATASERVER_VM_DELETE_AUTH_KEY",
    "DATASERVER_VM_WHOLE_SERIES_DELETE",
];

/// 写入的样本数（每条测量）。时间戳是「当下整秒往前退 SAMPLE_COUNT 秒」，
/// 整秒是为了让 `query_range` 的秒粒度 step 刚好踩中每个点。
const SAMPLE_COUNT: i64 = 5;

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().expect("local_addr").port()
}

fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_micros() as i64
}

/// 起一个 `dataserver --config <path>` 子进程；drop 时杀掉它（已正常退出的进程 kill 无副作用）。
struct Server {
    child: Child,
    base: String,
    label: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    fn get(&self, path: &str) -> (u16, String) {
        http("GET", &format!("{}{}", self.base, path), None)
            .unwrap_or_else(|e| panic!("[{}] GET {path}: {e}", self.label))
    }

    fn post(&self, path: &str, body: &str) -> (u16, String) {
        http("POST", &format!("{}{}", self.base, path), Some(body))
            .unwrap_or_else(|e| panic!("[{}] POST {path}: {e}", self.label))
    }

    /// 优雅停止：发 SIGTERM，等进程自己退出，返回退出状态。
    ///
    /// 用 SIGTERM 而不是 `Child::kill()`（SIGKILL）：这条场景要验的正是**正常停机路径**，
    /// SIGKILL 会把「停机逻辑有没有把状态写坏」这件事绕过去。
    fn terminate(&mut self, deadline: Duration) -> std::process::ExitStatus {
        let pid = self.child.id() as libc::pid_t;
        // SAFETY: pid 来自我们自己 spawn 的子进程；进程已退出时 kill 只返回 ESRCH，无副作用。
        let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
        assert_eq!(
            rc,
            0,
            "[{}] 发 SIGTERM 失败（pid={pid}），errno={}",
            self.label,
            std::io::Error::last_os_error()
        );
        let started = Instant::now();
        loop {
            match self.child.try_wait().expect("try_wait") {
                Some(status) => return status,
                None if started.elapsed() > deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    let logs = self.drain_logs();
                    panic!(
                        "[{}] 收到 SIGTERM 后 {deadline:?} 内没有退出：停机会挂住，容器里就是优雅终止超时。\n{logs}",
                        self.label
                    );
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    fn drain_logs(&mut self) -> String {
        let mut out = String::new();
        let mut err = String::new();
        if let Some(mut s) = self.child.stdout.take() {
            let _ = s.read_to_string(&mut out);
        }
        if let Some(mut s) = self.child.stderr.take() {
            let _ = s.read_to_string(&mut err);
        }
        let tail = |s: &str| {
            let lines: Vec<&str> = s.lines().collect();
            lines[lines.len().saturating_sub(40)..].join("\n")
        };
        format!(
            "--- stdout ---\n{}\n--- stderr ---\n{}",
            tail(&out),
            tail(&err)
        )
    }
}

fn http(method: &str, url: &str, body: Option<&str>) -> Result<(u16, String), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();
    let req = match method {
        "GET" => agent.get(url),
        "POST" => agent.post(url),
        other => panic!("unsupported method {other}"),
    };
    let result = match body {
        Some(b) => req.set("Content-Type", "application/json").send_string(b),
        None => req.call(),
    };
    match result {
        Ok(resp) => Ok((resp.status(), resp.into_string().unwrap_or_default())),
        Err(ureq::Error::Status(code, resp)) => Ok((code, resp.into_string().unwrap_or_default())),
        Err(e) => Err(format!("transport error on {method} {url}: {e}")),
    }
}

/// 写一份配置：`data_path` 由调用方指定（重启前后必须是**同一个**），端口每次都新取，
/// 这样第二次启动不用等上一次释放的端口。
fn write_config(dir: &Path, data_path: &Path) -> (PathBuf, u16) {
    let sql_port = free_port();
    let prom_port = free_port();
    let metrics_port = free_port();
    let toml = format!(
        r#"data_path = "{}"
kv_backend = "redb"
ts_backend = "tsink"
self_metrics_interval_secs = 0
ts_clean_interval_secs = 3600
apm_enabled = false
apm_clean_interval_secs = 0
apm_agg_interval_secs = 0

[sql_http]
listen = "127.0.0.1:{sql_port}"

[prom_http]
listen = "127.0.0.1:{prom_port}"

[metrics_http]
listen = "127.0.0.1:{metrics_port}"
"#,
        data_path.to_string_lossy()
    );
    let path = dir.join(format!("config-{sql_port}.toml"));
    std::fs::write(&path, toml).expect("write config");
    (path, sql_port)
}

fn spawn_server(config: &Path, sql_port: u16, label: &str) -> Server {
    let mut cmd = Command::new(BIN);
    cmd.arg("--config")
        .arg(config)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for var in CONFIG_ENV_VARS {
        cmd.env_remove(var);
    }
    let child = cmd.spawn().expect("spawn dataserver");
    Server {
        child,
        base: format!("http://127.0.0.1:{sql_port}"),
        label: label.to_string(),
    }
}

fn wait_healthy(server: &mut Server) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match http("GET", &format!("{}/health", server.base), None) {
            Ok((200, body)) => {
                if body.contains("\"status\":\"ok\"") {
                    return Ok(());
                }
                return Err(format!(
                    "[{}] /health 返回 200 但 body 不含 status=ok: {body}",
                    server.label
                ));
            }
            _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let logs = server.drain_logs();
                let _ = server.child.kill();
                return Err(format!("[{}] 60s 内 /health 没就绪\n{logs}", server.label));
            }
        }
    }
}

/// 两条序列 × `SAMPLE_COUNT` 个点，各自有区分度很高的值，方便逐点比对。
fn ingest_points(server: &Server, base_micros: i64, round: &str, offset: f64) -> Value {
    let mut records = Vec::new();
    for (measurement, delta) in [("ts_restart_cpu", 0.0_f64), ("ts_restart_mem", 100.0_f64)] {
        for i in 0..SAMPLE_COUNT {
            let ts = base_micros + i * 1_000_000;
            records.push(json!({
                "record_id": format!("{round}-{measurement}-{i}"),
                "timestamp": ts,
                "measurement": measurement,
                "tags": {"src": "ts_restart", "host": "restart-host"},
                "field_name": "value",
                "field_value": offset + delta + i as f64 + 0.5
            }));
        }
    }
    let envelope = json!({
        "batch_id": format!("restart-{round}"),
        "data_type": "metrics",
        "data_id": "restart-item",
        "agent_id": "restart-agent",
        "host_id": "restart-host",
        "sent_at_micros": base_micros,
        "records": records
    })
    .to_string();
    let (st, body) = server.post("/v1/ingest", &envelope);
    assert_eq!(st, 200, "[{}] ingest 失败: {body}", server.label);
    let reply: Value = serde_json::from_str(&body).expect("ingest json");
    reply
}

/// Prom 兼容口把样本值写成 JSON 数字（`vector` 与 `matrix` 都是），这里同时容忍字符串形式，
/// 免得将来 Prom 编码变化把等价性断言变成解析失败。
fn sample_value(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or_else(|| panic!("样本值既不是数字也不是可解析字符串: {v}"))
}

/// 一条序列在一段时间窗内的全部样本，`[[ts, value], ...]`，按时间升序。
fn range_samples(
    server: &Server,
    measurement: &str,
    start_us: i64,
    end_us: i64,
) -> Vec<(i64, f64)> {
    let (from, to) = (start_us as f64 / 1e6, end_us as f64 / 1e6);
    let url = format!(
        "{}/api/v1/query_range?query={measurement}&start={from}&end={to}&step=1",
        server.base
    );
    let (st, body) = server.get(url.trim_start_matches(&server.base));
    assert_eq!(
        st, 200,
        "[{}] query_range {measurement} 失败: {body}",
        server.label
    );
    let resp: Value = serde_json::from_str(&body).expect("query_range json");
    assert_eq!(resp["status"], "success", "[{}] {body}", server.label);
    let result = resp["data"]["result"]
        .as_array()
        .unwrap_or_else(|| panic!("[{}] result 不是数组: {body}", server.label));
    let mut samples = Vec::new();
    for series in result {
        let metric = &series["metric"];
        assert_eq!(
            metric["src"], "ts_restart",
            "[{}] 标签 src 必须原样回读: {body}",
            server.label
        );
        assert_eq!(
            metric["host"], "restart-host",
            "[{}] 标签 host 必须原样回读: {body}",
            server.label
        );
        for pair in series["values"].as_array().expect("values 数组") {
            let ts = (pair[0].as_f64().expect("ts") * 1e6).round() as i64;
            samples.push((ts, sample_value(&pair[1])));
        }
    }
    samples.sort_by_key(|(ts, _)| *ts);
    samples
}

/// `(series_count, 两条测量的样本向量)` —— 重启前后的等价性就比对这一个快照。
fn snapshot(
    server: &Server,
    start_us: i64,
    end_us: i64,
) -> (u64, Vec<(i64, f64)>, Vec<(i64, f64)>) {
    let (st, body) = server.get("/v1/ts/stats");
    assert_eq!(st, 200, "[{}] /v1/ts/stats 失败: {body}", server.label);
    let stats: Value = serde_json::from_str(&body).expect("stats json");
    let series_count = stats["series_count"]
        .as_u64()
        .unwrap_or_else(|| panic!("[{}] series_count 缺失: {body}", server.label));
    (
        series_count,
        range_samples(server, "ts_restart_cpu", start_us, end_us),
        range_samples(server, "ts_restart_mem", start_us, end_us),
    )
}

/// 场景主体：写点 → SIGTERM 优雅停机 → 同一 `data_path` 重启 → 逐点比对等价。
#[test]
fn sigterm_and_restart_preserves_all_points() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    // 关键：`data_path` 在整个测试里只有这一份，两次启动都指向它。
    let data_path = dir.path().join("data");

    // 时间戳：当下整秒往前退 SAMPLE_COUNT 秒，全部落在 retained 窗口内。
    let base = (now_micros() / 1_000_000) * 1_000_000 - SAMPLE_COUNT * 1_000_000;

    // ---- 第一段进程：写入并留快照 ----
    let (config, port) = write_config(dir.path(), &data_path);
    let mut server = spawn_server(&config, port, "boot#1");
    wait_healthy(&mut server).unwrap_or_else(|e| panic!("{e}"));

    let reply = ingest_points(&server, base, "before", 0.0);
    assert_eq!(
        reply["accepted"].as_u64(),
        Some((SAMPLE_COUNT * 2) as u64),
        "应写入 {} 条，实际应答: {reply}",
        SAMPLE_COUNT * 2
    );

    let window_end = base + (SAMPLE_COUNT - 1) * 1_000_000;
    let (count_before, cpu_before, mem_before) = snapshot(&server, base, window_end);
    assert_eq!(
        cpu_before.len(),
        SAMPLE_COUNT as usize,
        "重启前 ts_restart_cpu 应有 {SAMPLE_COUNT} 个点: {cpu_before:?}"
    );
    assert_eq!(
        mem_before.len(),
        SAMPLE_COUNT as usize,
        "重启前 ts_restart_mem 应有 {SAMPLE_COUNT} 个点: {mem_before:?}"
    );
    assert!(
        count_before >= 2,
        "重启前 series_count 应 ≥2（两条序列），实际 {count_before}"
    );

    // ---- SIGTERM：必须自己退，且退出码 0 ----
    let status = server.terminate(Duration::from_secs(30));
    assert!(
        status.success(),
        "[boot#1] 收到 SIGTERM 后退出码应为 0，实际 {status:?}\n{}",
        server.drain_logs()
    );
    drop(server); // 显式收掉第一个进程，确保 60s 内端口/文件句柄都释放

    // ---- 第二段进程：同一个 data_path 重新打开 ----
    let (config2, port2) = write_config(dir.path(), &data_path);
    let mut server2 = spawn_server(&config2, port2, "boot#2");
    wait_healthy(&mut server2).unwrap_or_else(|e| panic!("{e}"));

    // WAL 回放可能不是瞬时的（运维口径 4–6 分钟是最大量级），所以给窗口轮询；
    // 但**判定是严格相等**，不是「有数据就算过」——少一个点就是丢了。
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut attempts = 0;
    let (count_after, cpu_after, mem_after) = loop {
        attempts += 1;
        let snap = snapshot(&server2, base, window_end);
        let complete = snap.0 >= 2
            && snap.1.len() == SAMPLE_COUNT as usize
            && snap.2.len() == SAMPLE_COUNT as usize;
        if complete || Instant::now() >= deadline {
            break snap;
        }
        std::thread::sleep(Duration::from_millis(200));
    };

    assert_eq!(
        count_after, count_before,
        "重启后 series_count 必须与重启前相同（{count_before}），实际 {count_after}，轮询 {attempts} 次"
    );
    assert_eq!(
        cpu_after, cpu_before,
        "重启后 ts_restart_cpu 的点集必须与重启前**逐点等价**\n重启前: {cpu_before:?}\n重启后: {cpu_after:?}\n{}",
        server2.drain_logs()
    );
    assert_eq!(
        mem_after, mem_before,
        "重启后 ts_restart_mem 的点集必须与重启前**逐点等价**\n重启前: {mem_before:?}\n重启后: {mem_after:?}\n{}",
        server2.drain_logs()
    );

    // 即时查询也要能看到重启前的最后一个点（不是只有 range 能读）。
    let last_ts = base + (SAMPLE_COUNT - 1) * 1_000_000;
    let (st, body) = server2.get(&format!(
        "/api/v1/query?query=ts_restart_cpu&time={}",
        last_ts / 1_000_000
    ));
    assert_eq!(st, 200, "[boot#2] instant query 失败: {body}");
    let resp: Value = serde_json::from_str(&body).expect("query json");
    let series = resp["data"]["result"]
        .as_array()
        .unwrap_or_else(|| panic!("[boot#2] result 不是数组: {body}"));
    assert_eq!(series.len(), 1, "[boot#2] 应命中 1 条序列: {body}");
    let value = sample_value(&series[0]["value"][1]);
    let expected_last = cpu_before.last().expect("重启前最后一个点").1;
    assert!(
        (value - expected_last).abs() < 1e-9,
        "重启后即时查询最后一个点的值应为 {expected_last}，实际 {value}: {body}"
    );

    // 重启后的 store 必须仍然可写（不是「只读地回放完就降级」）。
    let base2 = base + (SAMPLE_COUNT + 1) * 1_000_000;
    let reply = ingest_points(&server2, base2, "after", 200.0);
    assert_eq!(
        reply["accepted"].as_u64(),
        Some((SAMPLE_COUNT * 2) as u64),
        "重启后写入应全部被接收: {reply}"
    );
    // 新写入用 200.x 值域，和重启前的 0.x 完全分开：混在一起就说明覆盖了。
    let cpu_new = range_samples(
        &server2,
        "ts_restart_cpu",
        base2,
        base2 + (SAMPLE_COUNT - 1) * 1_000_000,
    );
    let expected_new: Vec<(i64, f64)> = (0..SAMPLE_COUNT)
        .map(|i| (base2 + i * 1_000_000, 200.0 + i as f64 + 0.5))
        .collect();
    assert_eq!(
        cpu_new, expected_new,
        "重启后新写入的点必须按原值读回（值域 200.x）: {cpu_new:?}"
    );
    // 老窗口必须仍是重启前那份，没有被新写入改动。
    assert_eq!(
        range_samples(&server2, "ts_restart_cpu", base, window_end),
        cpu_before,
        "新写入不能改动已有历史点"
    );
}
