//! `IT-DEDUP-01`：接入去重（`ingest/{record_id}`）在**真 dataserver 进程**上的有效性、
//! 键数上界与清理路径。
//!
//! 动机：`ingest/` 去重键只写不删会单调增长。v1.3.15 线上 11 分钟里键数从 199 涨到 3942
//! （清理阶段被 `?` 短路整轮空转），趋近 Redis `maxmemory 2gb` 后就轮到接入 5xx。
//! 这件事此前只有「监控上看键数」这种事后手段，没有任何可判定的回归断言。
//!
//! 断言口径分三层，缺一层就退化成「HTTP 返回 200 就算过」：
//! 1. **HTTP 侧**：同 `record_id` 多轮投递后，时序点集不增长（点数 == 唯一 record 数）。
//! 2. **存储侧**：进程优雅停机后直接打开同一个 `kv.redb`，`scan_prefix("ingest/")` 的键数
//!    恰好等于唯一 record 数，且 `record_id` 逐个对得上、值是 8 字节小端微秒。
//! 3. **清理侧**：`prune_prefix_before("ingest/", 未来时间)` 删掉的键数等于上一步键数，
//!    删完再数为 0，且不越界删掉 `stream/` 前缀的键。
//!
//! 注意 `IngestReply.accepted` 对重复记录**仍然 +1**（`apply_one` 里重复走 `Ok(())`），
//! 所以 `accepted` 不能当去重的证据——本测试只把它当「请求被受理」的旁证。
//!
//! 场景定义（唯一事实来源）：`test_plan/IT-DEDUP-01.md`。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dataplane_kv::{KvStore, RedbKvStore};
use serde_json::{json, Value};
use vectorman_test_support::util::free_port;

const BIN: &str = env!("CARGO_BIN_EXE_dataserver");

/// 同一批 record 重复投递的轮数。≥5 才够说明「不是第一次的残留」。
const ROUNDS: u32 = 5;
/// 唯一 record 数（每条一个点），也是去重后 `ingest/` 键数的期望值。
const RECORDS: usize = 3;
/// 测量名与点值：值域刻意区分，方便断言「回读的就是这批点」。
const MEASUREMENT: &str = "it_dedup_gauge";
const ITEM_ID: &str = "dedup-item";
const AGENT_ID: &str = "dedup-agent";

/// `apply_env()` 会读的全部变量。测试必须把它们清空，否则开发机上残留的
/// `DATASERVER_*` / `DP_*` 会把配置文件里的值悄悄改掉（尤其 `dp_data_path` 与 KV 后端）。
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
    /// 用 SIGTERM 而不是 `Child::kill()`（SIGKILL）：下一步要在停机后用 `redb` 打开
    /// 同一个 `kv.redb`，SIGKILL 抢不到文件锁，测的就变成「文件锁报错」而不是去重语义。
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
                        "[{}] 收到 SIGTERM 后 {deadline:?} 内没有退出：停机会挂住，kv.redb 的文件锁也就放不掉。\n{logs}",
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

/// 写一份配置：本地 redb + tsink，清理周期拉到 1 小时（本测试只跑几秒，避免后台清理
/// 把 `ingest/` 键提前删掉、让「键数 == 唯一 record 数」变成时序竞态）。
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

fn record_ids() -> Vec<String> {
    (0..RECORDS).map(|i| format!("dedup-r{i}")).collect()
}

/// 一批 **完全相同** 的信封：每轮的 `record_id` / 时间戳 / 测量 / 值都一样。
/// 时间戳取「当下整秒往前退 RECORDS 秒」，全部落在保留窗口内、且是整秒（`step=1` 踩得到）。
fn payload(base_micros: i64, round: u32) -> String {
    let mut records = Vec::new();
    for (i, id) in record_ids().into_iter().enumerate() {
        records.push(json!({
            "record_id": id,
            "timestamp": base_micros + i as i64 * 1_000_000,
            "measurement": MEASUREMENT,
            "tags": {"src": "it_dedup", "host": "dedup-host"},
            "field_name": "value",
            "field_value": 0.5 + i as f64
        }));
    }
    json!({
        // batch_id 每轮不同：模拟「Agent 重发同一个批次但其实换个批次号」，
        // 去重只能靠 record_id，不能被 batch_id 蒙过去。
        "batch_id": format!("dedup-batch-{round}"),
        "data_type": "metrics",
        "data_id": ITEM_ID,
        "agent_id": AGENT_ID,
        "host_id": "dedup-host",
        "sent_at_micros": base_micros,
        "records": records
    })
    .to_string()
}

fn sample_value(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or_else(|| panic!("样本值既不是数字也不是可解析字符串: {v}"))
}

/// 一段窗口内的全部样本，`[(ts, value), ...]` 按时间升序。
fn range_samples(
    server: &Server,
    measurement: &str,
    start_us: i64,
    end_us: i64,
) -> Vec<(i64, f64)> {
    let (from, to) = (start_us as f64 / 1e6, end_us as f64 / 1e6);
    let url = format!("/api/v1/query_range?query={measurement}&start={from}&end={to}&step=1");
    let (st, body) = server.get(&url);
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
            metric["src"], "it_dedup",
            "[{}] 标签 src 必须原样回读: {body}",
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

fn series_count(server: &Server) -> u64 {
    let (st, body) = server.get("/v1/ts/stats");
    assert_eq!(st, 200, "[{}] /v1/ts/stats 失败: {body}", server.label);
    let stats: Value = serde_json::from_str(&body).expect("stats json");
    stats["series_count"]
        .as_u64()
        .unwrap_or_else(|| panic!("[{}] series_count 缺失: {body}", server.label))
}

/// 期望点集：唯一 record 各一个点，值是 0.5 / 1.5 / 2.5。
fn expected_samples(base_micros: i64) -> Vec<(i64, f64)> {
    (0..RECORDS)
        .map(|i| (base_micros + i as i64 * 1_000_000, 0.5 + i as f64))
        .collect()
}

/// `scan_prefix("ingest/")` 的键（去掉前缀的 `record_id` → 原始值字节），排序后返回。
fn ingest_keys(kv: &RedbKvStore) -> Vec<(String, Vec<u8>)> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let kvs = rt
        .block_on(kv.scan_prefix(dataplane_ingest::INGEST_PREFIX))
        .unwrap_or_else(|e| panic!("scan_prefix(ingest/) 失败: {}", e.message));
    let mut out: Vec<(String, Vec<u8>)> = kvs
        .into_iter()
        .map(|(k, v)| {
            let key = String::from_utf8(k).expect("键必须是 UTF-8");
            let id = key
                .strip_prefix("ingest/")
                .unwrap_or_else(|| panic!("scan_prefix(ingest/) 返回了不属于该前缀的键: {key}"))
                .to_string();
            (id, v)
        })
        .collect();
    out.sort();
    out
}

/// 场景主体：5 轮同批投递 → SIGTERM 停机 → 直读 kv.redb 数键 → 剪枝清理。
#[test]
fn dedup_keys_bounded_and_prunable() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let data_path = dir.path().join("data");
    let base = (now_micros() / 1_000_000) * 1_000_000 - RECORDS as i64 * 1_000_000;
    let window_end = base + (RECORDS as i64 - 1) * 1_000_000;
    let expected = expected_samples(base);

    let (config, port) = write_config(dir.path(), &data_path);
    let mut server = spawn_server(&config, port, "dedup#1");
    wait_healthy(&mut server).unwrap_or_else(|e| panic!("{e}"));

    // ---- 第一轮：写入并确认点数到位（tsink 本地写入是即时的，但仍给一个短轮询窗口）----
    let first = payload(base, 1);
    let (st, body) = server.post("/v1/ingest", &first);
    assert_eq!(st, 200, "首轮 ingest 失败: {body}");
    let reply: Value = serde_json::from_str(&body).expect("ingest json");
    assert_eq!(
        reply["accepted"].as_u64(),
        Some(RECORDS as u64),
        "首轮应受理 {RECORDS} 条，实际应答: {reply}"
    );

    let deadline = Instant::now() + Duration::from_secs(30);
    let first_samples = loop {
        let s = range_samples(&server, MEASUREMENT, base, window_end);
        if s.len() == RECORDS || Instant::now() >= deadline {
            break s;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        first_samples, expected,
        "首轮写入后应回读到唯一 record 对应的 {RECORDS} 个点"
    );
    let count_after_first = series_count(&server);

    // ---- 第 2..N 轮：同批重发，点数与序列数必须**一动不动** ----
    for round in 2..=ROUNDS {
        let body = payload(base, round);
        let (st, resp) = server.post("/v1/ingest", &body);
        assert_eq!(st, 200, "第 {round} 轮 ingest 失败: {resp}");
        let reply: Value = serde_json::from_str(&resp).expect("ingest json");
        // 事实核对：重复记录仍计入 accepted（`apply_one` 重复走 `Ok(())`）。
        // 所以这一条**不是**去重的证据，只是「请求被受理」的旁证。
        assert_eq!(
            reply["accepted"].as_u64(),
            Some(RECORDS as u64),
            "第 {round} 轮重复记录仍会计入 accepted（当前实现如此），实际应答: {reply}"
        );
        assert_eq!(
            reply["status"], "ok",
            "第 {round} 轮不应产生失败记录: {reply}"
        );

        // 给异步写入一点时间再判定；判定标准是**严格相等**，不是「不炸就算过」。
        std::thread::sleep(Duration::from_millis(150));
        let samples = range_samples(&server, MEASUREMENT, base, window_end);
        assert_eq!(
            samples.len(),
            RECORDS,
            "第 {round} 轮重复投递后点数必须仍是 {RECORDS}（去重生效），实际 {}: {samples:?}",
            samples.len()
        );
        assert_eq!(
            samples, expected,
            "第 {round} 轮重复投递不能改动已有样本值: {samples:?}"
        );
        assert_eq!(
            series_count(&server),
            count_after_first,
            "第 {round} 轮重复投递后 series_count 必须与首轮相同（{count_after_first}）"
        );
    }

    // ---- 优雅停机：SIGTERM 后自己退，退出码 0（redb 文件锁随之释放）----
    let status = server.terminate(Duration::from_secs(30));
    assert!(
        status.success(),
        "[dedup#1] 收到 SIGTERM 后退出码应为 0，实际 {status:?}\n{}",
        server.drain_logs()
    );
    drop(server);

    // ---- 存储侧：同一个 data_path 的 kv.redb 里，ingest/ 键必须恰好等于唯一 record 数 ----
    let kv_path = data_path.join("kv.redb");
    assert!(
        kv_path.is_file(),
        "停机后应存在 KV 数据库 {}（data_path 目录模式下 kv = <data_path>/kv.redb）",
        kv_path.display()
    );
    let kv = RedbKvStore::new(&kv_path)
        .unwrap_or_else(|e| panic!("打开 {} 失败: {}", kv_path.display(), e.message));

    let keys = ingest_keys(&kv);
    let ids: Vec<&str> = keys.iter().map(|(id, _)| id.as_str()).collect();
    let want: Vec<String> = record_ids();
    assert_eq!(
        ids,
        want.iter().map(String::as_str).collect::<Vec<_>>(),
        "{ROUNDS} 轮 × {RECORDS} 条重复投递后，ingest/ 键数必须等于唯一 record 数（{RECORDS}），\
         实际 {} 个: {ids:?}",
        keys.len()
    );

    // 值必须是 8 字节小端微秒接入时间：清理端 `prune_prefix_before` 靠它判过期，
    // 写成空值就会被当成「旧版本空值」直接删掉（等价于去重键永不生效）。
    let floor = base - 3600 * 1_000_000;
    let ceiling = now_micros() + 60 * 1_000_000;
    for (id, value) in &keys {
        assert_eq!(
            value.len(),
            8,
            "ingest/{id} 的值必须是 8 字节小端微秒（旧版本空值会被清理端直接删）: {value:?}"
        );
        let micros = i64::from_le_bytes(value.as_slice().try_into().expect("8 字节"));
        assert!(
            micros > floor && micros <= ceiling,
            "ingest/{id} 的时间戳应在接入窗口内（{floor} < t ≤ {ceiling}），实际 {micros}"
        );
    }

    // 停机后去重键仍在 → 重启后同一条记录重发也不会重复入库（跨进程去重的前提）。
    // 这里直接验键存在，重启路径本身已由 IT-TS-RESTART-01 覆盖。

    // ---- 清理侧：prune_prefix_before 删掉的正好是这批键，且不越界 ----
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let stream_before = rt
        .block_on(kv.scan_prefix(b"stream/"))
        .expect("scan_prefix(stream/)")
        .len();
    assert!(
        stream_before >= 1,
        "接入成功后应有 stream/ 流索引键（否则说明落库链路没走通），实际 {stream_before}"
    );

    // 未来时间作为 cutoff：等价于「保留期已过」，真实清理端就是这么调用的。
    let cutoff = now_micros() + 1_000_000;
    let removed = rt
        .block_on(kv.prune_prefix_before(dataplane_ingest::INGEST_PREFIX, cutoff))
        .unwrap_or_else(|e| panic!("prune_prefix_before(ingest/) 失败: {}", e.message));
    assert_eq!(
        removed, RECORDS as u64,
        "剪枝删掉的键数必须等于 ingest/ 下的键数（{RECORDS}），实际 {removed}"
    );
    assert!(
        ingest_keys(&kv).is_empty(),
        "剪枝后再数 ingest/ 必须为 0，实际: {:?}",
        ingest_keys(&kv)
    );
    assert_eq!(
        rt.block_on(kv.scan_prefix(dataplane_ingest::INGEST_PREFIX))
            .expect("rescan ingest/")
            .len(),
        0,
        "剪枝后 ingest/ 前缀必须为空"
    );

    // 前缀边界：剪 `ingest/` 不能顺手删掉 `stream/`（前缀处理写错时这里是第一个炸点）。
    let stream_after = rt
        .block_on(kv.scan_prefix(b"stream/"))
        .expect("scan_prefix(stream/)")
        .len();
    assert_eq!(
        stream_after, stream_before,
        "剪枝 ingest/ 不能影响 stream/ 前缀的键数（{stream_before} → {stream_after}）"
    );
}
