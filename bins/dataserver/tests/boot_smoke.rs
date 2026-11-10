//! `IT-DS-BOOT-01`：dataserver 真进程的启动链路冒烟。
//!
//! 与 `e2e.rs`（进程内直接托管 axum router）的区别：这里跑的是**编出来的二进制**，
//! 所以 `--config` 解析、env 覆盖、后端装配、端口监听、优雅退出全在测试范围内；
//! 用 redb + tsink 本地后端，不依赖任何外部中间件，可以进 PR 门禁。
//!
//! 场景定义（唯一事实来源）：`test_plan/IT-DS-BOOT-01.md`。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");

/// `apply_env()` 会读的全部变量。测试必须把它们清空，否则开发机上残留的
/// `DATASERVER_*` 会把配置文件里的值悄悄改掉，用例就不测它自己声称测的东西了。
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

/// 拿一个当前空闲的端口。绑定后立刻释放，存在极小的被抢占窗口；
/// 冒烟测试用它足够，失败信息会明确说是 bind 失败。
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

/// 起一个 `dataserver --config <path>` 子进程；drop 时杀掉它。
struct Server {
    child: Child,
    base: String,
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
            .unwrap_or_else(|e| panic!("GET {path}: {e}"))
    }

    fn post(&self, path: &str, body: &str) -> (u16, String) {
        http("POST", &format!("{}{}", self.base, path), Some(body))
            .unwrap_or_else(|e| panic!("POST {path}: {e}"))
    }
}

/// 传输层错误（连不上/超时）返回 `Err` 而不是 panic：轮询 `/health` 时
/// 子进程还没 bind 是**正常**的中途状态。
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

/// 写出配置文件，返回 `(路径, sql_http 端口)`。`extra` 追加/覆盖后端相关键，
/// 用来构造非法配置。
///
/// `extra` 必须写在**第一个表头之前**：写在 `[metrics_http]` 后面会被 TOML 归到那个表里，
/// 而 `HttpListenConfig` 又会静默忽略未知字段，结果“非法配置”变成了合法的、真能起来。
fn write_config(dir: &Path, extra: &[(&str, &str)]) -> (PathBuf, u16) {
    let data = dir.join("data");
    let sql_port = free_port();
    let prom_port = free_port();
    let metrics_port = free_port();
    let mut toml = format!(
        r#"data_path = "{}"
self_metrics_interval_secs = 0
ts_clean_interval_secs = 3600
apm_enabled = false
apm_clean_interval_secs = 0
apm_agg_interval_secs = 0
"#,
        data.to_string_lossy()
    );
    // 缺省后端；`extra` 里给了同名键就不再写第二遍（TOML 重复键直接解析失败）。
    for (k, v) in [("kv_backend", "redb"), ("ts_backend", "tsink")] {
        if !extra.iter().any(|(ek, _)| *ek == k) {
            toml.push_str(&format!("{k} = \"{v}\"\n"));
        }
    }
    for (k, v) in extra {
        toml.push_str(&format!("{k} = \"{v}\"\n"));
    }
    toml.push_str(&format!(
        r#"
[sql_http]
listen = "127.0.0.1:{sql_port}"

[prom_http]
listen = "127.0.0.1:{prom_port}"

[metrics_http]
listen = "127.0.0.1:{metrics_port}"
"#
    ));
    let path = dir.join("config.toml");
    std::fs::write(&path, toml).expect("write config");
    (path, sql_port)
}

fn spawn_server(config: &Path, sql_port: u16) -> Server {
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
    }
}

fn wait_healthy(server: &mut Server) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        match http("GET", &format!("{}/health", server.base), None) {
            Ok((200, body)) => {
                if body.contains("\"status\":\"ok\"") {
                    return Ok(());
                }
                return Err(format!("/health 返回 200 但 body 不含 status=ok: {body}"));
            }
            _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = server.child.kill();
                let mut out = String::new();
                let mut err = String::new();
                if let Some(mut s) = server.child.stdout.take() {
                    let _ = s.read_to_string(&mut out);
                }
                if let Some(mut s) = server.child.stderr.take() {
                    let _ = s.read_to_string(&mut err);
                }
                return Err(format!(
                    "dataserver 40s 内 /health 没就绪\n--- stdout ---\n{out}\n--- stderr ---\n{err}"
                ));
            }
        }
    }
}

fn metrics_envelope(record_id: &str, ts: i64) -> String {
    json!({
        "batch_id": format!("boot-{record_id}"),
        "data_type": "metrics",
        "data_id": "boot-item",
        "agent_id": "boot-agent",
        "host_id": "boot-host",
        "sent_at_micros": ts,
        "records": [{
            "record_id": record_id,
            "timestamp": ts,
            "measurement": "boot_cpu_usage",
            "tags": {"src": "boot_smoke"},
            "field_name": "value",
            "field_value": 42.5
        }]
    })
    .to_string()
}

/// 跑一个**期望立刻退出**的进程，带看门狗。
///
/// 不用 `Command::output()`：它没有超时，万一「非法配置」其实合法、进程正常起来并
/// 一直服务，测试会永久挂住（实测踩过一次，CI 上就是几小时的假死）。
fn run_expecting_exit(
    mut cmd: Command,
    name: &str,
    deadline: Duration,
) -> (std::process::ExitStatus, String, String) {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn dataserver");
    let started = Instant::now();
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                let mut out = String::new();
                let mut err = String::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_string(&mut out);
                }
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err);
                }
                return (status, out, err);
            }
            None if started.elapsed() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "[{name}] dataserver 在 {deadline:?} 内没有退出：这份“非法配置”很可能其实是合法的，\
                     进程正常起来并开始服务了。先确认这些键写在配置文件第一个表头之前。"
                );
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// 场景主体：配置 → 装配 → `/health` → `/v1/ingest` → `/v1/ts/stats` + `/api/v1/query` 回读。
#[test]
fn boot_with_local_backends_serves_health_and_roundtrips_a_point() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (config, sql_port) = write_config(dir.path(), &[]);
    let mut server = spawn_server(&config, sql_port);
    wait_healthy(&mut server).unwrap_or_else(|e| panic!("{e}"));

    // 1. /v1/ts/stats 在空库上也要能读（后端装配成功的证据之一）。
    let (st, body) = server.get("/v1/ts/stats");
    assert_eq!(st, 200, "{body}");
    let stats: Value = serde_json::from_str(&body).expect("stats json");
    assert_eq!(
        stats["series_count"].as_u64(),
        Some(0),
        "空库 series_count 应为 0: {body}"
    );

    // 2. 写入一个点。时间戳取「当下整秒」：整秒是为了让下面 `time=` 参数（秒粒度）
    //    不会落在点之前；取当下是为了不被 ts_retention 窗口过滤掉。
    let ts = (now_micros() / 1_000_000) * 1_000_000;
    let (st, body) = server.post("/v1/ingest", &metrics_envelope("boot-1", ts));
    assert_eq!(st, 200, "ingest 失败: {body}");
    assert!(
        body.contains("\"accepted\":1") || body.contains("\"accepted\": 1"),
        "ingest 未确认接收: {body}"
    );

    // 3. 存下来了。
    let (st, body) = server.get("/v1/ts/stats");
    assert_eq!(st, 200, "{body}");
    let stats: Value = serde_json::from_str(&body).expect("stats json");
    assert!(
        stats["series_count"].as_u64().unwrap_or(0) >= 1,
        "写入后 series_count 应 ≥1: {body}"
    );

    // 4. 通过 Prom 兼容查询口回读同一个点，值和标签都要对。
    let eval = ts / 1_000_000;
    let (st, body) = server.get(&format!("/api/v1/query?query=boot_cpu_usage&time={eval}"));
    assert_eq!(st, 200, "query 失败: {body}");
    let resp: Value = serde_json::from_str(&body).expect("query json");
    assert_eq!(resp["status"], "success", "{body}");
    let series = resp["data"]["result"]
        .as_array()
        .unwrap_or_else(|| panic!("result 不是数组: {body}"));
    assert_eq!(series.len(), 1, "应命中 1 条序列: {body}");
    let value = series[0]["value"][1].as_f64().expect("样本值");
    assert!(
        (value - 42.5).abs() < 1e-9,
        "回读值应为 42.5，实际 {value}: {body}"
    );
    assert_eq!(
        series[0]["metric"]["src"], "boot_smoke",
        "标签必须在写入→存储→查询全链路保留: {body}"
    );
}

/// 非法配置必须**进程级**失败：退出码非 0，stderr 点名缺哪个变量/哪个键。
#[test]
fn invalid_backend_config_exits_nonzero_and_names_the_variable() {
    // (场景, 配置追加项, stderr 必须包含的片段)
    let cases: Vec<(&str, Vec<(&str, &str)>, Vec<&str>)> = vec![
        (
            "kv_backend 非法",
            vec![("kv_backend", "mysql")],
            vec!["unsupported kv_backend", "kv_backend"],
        ),
        (
            "kv_backend=redis 缺 redis_url",
            vec![("kv_backend", "redis")],
            vec![
                "kv_backend=redis requires redis_url",
                "DATASERVER_REDIS_URL",
            ],
        ),
        (
            "ts_backend=victoriametrics 缺 vm url",
            vec![("ts_backend", "victoriametrics")],
            vec![
                "ts_backend=victoriametrics",
                "vm_insert_url",
                "vm_select_url",
                "DATASERVER_VM_INSERT_URL",
                "DATASERVER_VM_SELECT_URL",
            ],
        ),
    ];

    for (name, extra, expect) in cases {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let (config, _) = write_config(dir.path(), &extra);
        let mut cmd = Command::new(BIN);
        cmd.arg("--config").arg(&config);
        for var in CONFIG_ENV_VARS {
            cmd.env_remove(var);
        }
        let (status, stdout, stderr) = run_expecting_exit(cmd, name, Duration::from_secs(30));
        assert!(
            !status.success(),
            "[{name}] 非法配置必须以非 0 退出，实际 status={status:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        for needle in expect {
            assert!(
                stderr.contains(needle),
                "[{name}] stderr 必须包含 {needle:?}\nstderr:\n{stderr}"
            );
        }
    }
}
