//! `dataserver` 真进程编排：写 toml → 起二进制 → 等 `/health` → SIGTERM 优雅停机。
//!
//! 为什么必须真进程：`--config` 解析、env 覆盖、后端装配（redb / VM / APM 任务）都在
//! `main.rs` 里，进程内托管 router 会把这一层全绕过去。既有 `boot_smoke` / `ts_restart` /
//! `it_dedup` 各自复制了一份同样的代码，新场景统一用这里。

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::http;
use crate::util::{free_port, wait_until};
use crate::vm::Vm;

/// `apply_env()` 会读的全部变量。不清掉的话，开发机上残留的 `DATASERVER_*` / `DP_*`
/// 会把配置文件里的值悄悄改掉，用例就不再测它自己声称测的东西。
pub const CONFIG_ENV_VARS: &[&str] = &[
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

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `redb`（默认）或 `redis`（配合 `extra` 里的 `redis_url`）。
    pub kv_backend: String,
    /// `tsink`（默认）或 `victoriametrics`。
    pub ts_backend: String,
    pub vm_insert_url: String,
    pub vm_select_url: String,
    pub vm_single_node: bool,
    pub vm_flush_interval_ms: u64,
    pub vm_batch_size: u64,
    pub apm_enabled: bool,
    /// 0 = 关闭聚合任务（测试断言 RED/边指标时必须 >0）。
    pub apm_agg_interval_secs: u64,
    /// 自监控快照写入时序库的周期；0（默认）= 关闭，与 `self_metrics_interval_secs = 0` 一致。
    pub self_metrics_interval_secs: u64,
    /// trace 接入的每秒批次上限；0（默认）= 不限流（与配置缺省一致）。
    pub apm_ingest_max_batches_per_sec: u64,
    /// APM 保留期任务的周期；0（默认）= 不挂该任务（与改动前的硬编码 `apm_clean_interval_secs = 0` 一致）。
    pub apm_clean_interval_secs: u64,
    /// 全局清理（ts/日志/边保留 + 去重键收敛）的周期；0 按改动前的硬编码 3600s 跑。
    /// 产品侧有 `max(60)` 下限，所以小于 60 的值实际按 60s 跑。
    pub ts_clean_interval_secs: u64,
    /// 配了就经 dataserver 反代 gse 台账（IT-CHAIN 类场景需要）。
    pub gse_admin_url: Option<String>,
    /// 追加/覆盖顶层键；值按**原始 TOML 片段**写入（字符串要自己带引号）。
    pub extra: Vec<(String, String)>,
}

impl Options {
    /// 本地 redb + tsink，APM 关闭：与既有 PR 门禁用例的缺省一致。
    pub fn local() -> Self {
        Self {
            kv_backend: "redb".into(),
            ts_backend: "tsink".into(),
            ..Self::default()
        }
    }

    /// 时序走真实 VictoriaMetrics。
    pub fn with_vm(mut self, vm: &Vm) -> Self {
        self.ts_backend = "victoriametrics".into();
        self.vm_insert_url = vm.insert_url.clone();
        self.vm_select_url = vm.select_url.clone();
        self.vm_single_node = vm.single_node;
        // 测试里等一个点进 VM 不该等太久；批量小一点也让单条也能立刻发出。
        self.vm_flush_interval_ms = self.vm_flush_interval_ms.max(50);
        if self.vm_batch_size == 0 {
            self.vm_batch_size = 100;
        }
        self
    }

    /// 打开 APM（trace → RED 指标、eBPF 边 → 边指标都由这个开关拉起）。
    pub fn with_apm(mut self, agg_interval_secs: u64) -> Self {
        self.apm_enabled = true;
        self.apm_agg_interval_secs = agg_interval_secs.max(1);
        self
    }

    /// 台账地址：dataserver 的 `/v1/gse/*` 反代到 gse-server 管理口（IT-CHAIN / IT-VM-AGENT 需要）。
    pub fn with_gse_admin(mut self, url: &str) -> Self {
        self.gse_admin_url = Some(url.to_string());
        self
    }

    /// 打开自监控快照：每 `secs` 秒把 `/metrics` 的全量样本写进时序后端（IT-VM-SELF 需要）。
    pub fn with_self_metrics(mut self, secs: u64) -> Self {
        self.self_metrics_interval_secs = secs;
        self
    }

    /// trace 接入限流：每秒最多 `max_per_sec` 批，超出回 429（IT-VM-SELF 限流自监控需要）。
    pub fn with_apm_ingest_limit(mut self, max_per_sec: u64) -> Self {
        self.apm_ingest_max_batches_per_sec = max_per_sec;
        self
    }

    /// APM 保留期任务周期（产品侧有 `max(60)` 下限，`secs < 60` 实际按 60s 跑）。
    pub fn with_apm_clean(mut self, secs: u64) -> Self {
        self.apm_clean_interval_secs = secs;
        self
    }

    /// 全局清理周期（产品侧有 `max(60)` 下限，`secs < 60` 实际按 60s 跑）。
    pub fn with_ts_clean(mut self, secs: u64) -> Self {
        self.ts_clean_interval_secs = secs;
        self
    }

    pub fn with_kv(mut self, backend: &str, url: &str) -> Self {
        self.kv_backend = backend.into();
        self.extra.push(("redis_url".into(), format!("\"{url}\"")));
        self
    }

    pub fn with_extra(mut self, key: &str, raw_toml: &str) -> Self {
        self.extra.push((key.into(), raw_toml.into()));
        self
    }
}

/// 一个跑着的 `dataserver` 真进程。
pub struct Dataserver {
    dir: PathBuf,
    config: PathBuf,
    opts: Options,
    bin: PathBuf,
    child: Option<Child>,
    base: String,
    prom_base: String,
    metrics_base: String,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    drainers: Vec<JoinHandle<()>>,
}

impl Dataserver {
    /// 在 `dir` 下写配置并启动；`dir` 由调用方的 `TempDir` 持有，restart 时复用同一份数据。
    ///
    /// `bin` 传调用方测试里的 `env!("CARGO_BIN_EXE_dataserver")`：这个变量只在
    /// dataserver 包的集成测试里存在，支撑库自己取不到。
    pub fn start(dir: impl AsRef<Path>, opts: Options, bin: impl Into<PathBuf>) -> Self {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(dir.join("data")).expect("create data dir");
        let mut ds = Self {
            config: dir.join("config.toml"),
            dir,
            opts,
            bin: bin.into(),
            child: None,
            base: String::new(),
            prom_base: String::new(),
            metrics_base: String::new(),
            stdout: Arc::new(Mutex::new(String::new())),
            stderr: Arc::new(Mutex::new(String::new())),
            drainers: Vec::new(),
        };
        ds.spawn();
        ds
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// Prom 查询口（`/api/v1/query`）；未配置时与 SQL 口同端口（`prom_http` 总是单独监听）。
    pub fn prom(&self) -> &str {
        &self.prom_base
    }

    pub fn metrics(&self) -> &str {
        &self.metrics_base
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn config_path(&self) -> &Path {
        &self.config
    }

    pub fn opts(&self) -> &Options {
        &self.opts
    }

    pub fn get(&self, path: &str) -> (u16, String) {
        http::get(&format!("{}{path}", self.base))
            .unwrap_or_else(|e| panic!("[dataserver] GET {path}: {e}"))
    }

    pub fn post(&self, path: &str, body: &str) -> (u16, String) {
        http::post_json(&format!("{}{path}", self.base), body)
            .unwrap_or_else(|e| panic!("[dataserver] POST {path}: {e}"))
    }

    /// `POST /v1/ingest`，断言 2xx 并返回应答 JSON（`accepted` / `failures` 由调用方断言）。
    pub fn ingest(&self, envelope: &str) -> Value {
        let (status, body) = self.post("/v1/ingest", envelope);
        assert!(
            (200..300).contains(&status),
            "ingest 失败（{status}）: {}",
            http::tail(&body)
        );
        serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("ingest 应答非 JSON（{e}）: {}", http::tail(&body)))
    }

    /// 断言这批记录全部被受理（`failures` 为空）。
    pub fn ingest_ok(&self, envelope: &str) {
        let reply = self.ingest(envelope);
        let failures = reply["failures"].as_array().cloned().unwrap_or_default();
        assert!(
            failures.is_empty(),
            "ingest 有记录被拒: {}",
            serde_json::to_string(&failures).unwrap_or_default()
        );
    }

    /// `POST /v1/sql`：直查/直改 dataserver 的 SQL 台账（只给测试夹具用）。
    pub fn sql(&self, sql: &str, params: &[Value]) -> Value {
        let body = serde_json::json!({ "sql": sql, "params": params });
        let (status, resp) = self.post("/v1/sql", &body.to_string());
        assert!(
            (200..300).contains(&status),
            "SQL 执行失败（{status}）: {}（{sql}）",
            http::tail(&resp)
        );
        serde_json::from_str(&resp)
            .unwrap_or_else(|e| panic!("SQL 应答非 JSON（{e}）: {}", http::tail(&resp)))
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.as_mut().map(|c| c.try_wait()), Some(Ok(None)))
    }

    /// 发 SIGTERM 并等进程自己退出（返回退出状态；调用方断言 code==0）。
    pub fn stop_gracefully(&mut self, timeout: Duration) -> ExitStatus {
        let Some(mut child) = self.child.take() else {
            panic!("dataserver 已经停止，不能再次 SIGTERM");
        };
        let started = Instant::now();
        let pid = child.id() as i32;
        let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
        assert_eq!(rc, 0, "SIGTERM 发送失败（pid={pid}）");
        loop {
            match child.try_wait().expect("try_wait") {
                Some(status) => {
                    self.join_drainers();
                    return status;
                }
                None if started.elapsed() > timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    self.join_drainers();
                    panic!(
                        "dataserver 收到 SIGTERM 后 {timeout:?} 没有退出\n{}",
                        self.logs_block()
                    );
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    /// 用同一个数据目录重启：新端口、同一 `data_path`，验证「数据是持久化在后端里」而不是进程残余。
    pub fn restart(&mut self) {
        if self.is_running() {
            let status = self.stop_gracefully(Duration::from_secs(20));
            assert!(
                status.success(),
                "restart 前的优雅停机退出码非 0: {status:?}\n{}",
                self.logs_block()
            );
        }
        self.spawn();
    }

    /// 当前抓到的 stdout / stderr（进程未退出时可能只到缓冲区边界）。
    pub fn logs(&self) -> (String, String) {
        (
            self.stdout.lock().map(|s| s.clone()).unwrap_or_default(),
            self.stderr.lock().map(|s| s.clone()).unwrap_or_default(),
        )
    }

    pub fn logs_block(&self) -> String {
        let (out, err) = self.logs();
        format!(
            "--- stdout ---\n{}\n--- stderr ---\n{}",
            tail_text(&out),
            tail_text(&err)
        )
    }

    fn spawn(&mut self) {
        let ports = self.write_config();
        let mut cmd = Command::new(&self.bin);
        cmd.arg("--config")
            .arg(&self.config)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for var in CONFIG_ENV_VARS {
            cmd.env_remove(var);
        }
        let mut child = cmd
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {} 失败: {e}", self.bin.display()));
        self.stdout = Arc::new(Mutex::new(String::new()));
        self.stderr = Arc::new(Mutex::new(String::new()));
        self.drainers = vec![
            drain(child.stdout.take(), Arc::clone(&self.stdout)),
            drain(child.stderr.take(), Arc::clone(&self.stderr)),
        ];
        self.child = Some(child);
        self.base = format!("http://127.0.0.1:{}", ports.0);
        self.prom_base = format!("http://127.0.0.1:{}", ports.1);
        self.metrics_base = format!("http://127.0.0.1:{}", ports.2);
        self.wait_healthy();
    }

    /// 写 `<dir>/config.toml`，返回 `(sql, prom, metrics)` 端口。
    fn write_config(&self) -> (u16, u16, u16) {
        let data = self.dir.join("data");
        let sql = free_port();
        let prom = free_port();
        let metrics = free_port();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut lines: Vec<String> = Vec::new();
        {
            let mut push = |key: &str, rendered: String| {
                if seen.insert(key.to_string()) {
                    lines.push(format!("{key} = {rendered}"));
                }
            };
            push(
                "data_path",
                toml_string(&data.to_string_lossy()).to_string(),
            );
            push(
                "self_metrics_interval_secs",
                self.opts.self_metrics_interval_secs.to_string(),
            );
            // 默认值保持改动前的硬编码：ts 清理 3600s、APM 保留任务不挂（0）。
            push(
                "ts_clean_interval_secs",
                if self.opts.ts_clean_interval_secs == 0 {
                    "3600".to_string()
                } else {
                    self.opts.ts_clean_interval_secs.to_string()
                },
            );
            push("apm_enabled", self.opts.apm_enabled.to_string());
            push(
                "apm_agg_interval_secs",
                self.opts.apm_agg_interval_secs.to_string(),
            );
            push(
                "apm_clean_interval_secs",
                self.opts.apm_clean_interval_secs.to_string(),
            );
            push(
                "apm_ingest_max_batches_per_sec",
                self.opts.apm_ingest_max_batches_per_sec.to_string(),
            );
            push("apm_min_duration_micros_for_detail", "0".into());
            push("kv_backend", toml_string(&self.opts.kv_backend).to_string());
            push("ts_backend", toml_string(&self.opts.ts_backend).to_string());
            if !self.opts.vm_insert_url.is_empty() {
                push(
                    "vm_insert_url",
                    toml_string(&self.opts.vm_insert_url).to_string(),
                );
            }
            if !self.opts.vm_select_url.is_empty() {
                push(
                    "vm_select_url",
                    toml_string(&self.opts.vm_select_url).to_string(),
                );
            }
            push("vm_single_node", self.opts.vm_single_node.to_string());
            if self.opts.vm_flush_interval_ms > 0 {
                push(
                    "vm_flush_interval_ms",
                    self.opts.vm_flush_interval_ms.to_string(),
                );
            }
            if self.opts.vm_batch_size > 0 {
                push("vm_batch_size", self.opts.vm_batch_size.to_string());
            }
            if let Some(url) = &self.opts.gse_admin_url {
                push("gse_admin_url", toml_string(url).to_string());
            }
            for (key, raw) in &self.opts.extra {
                push(key, raw.clone());
            }
        }
        let mut toml = lines.join("\n");
        toml.push_str(&format!(
            "\n\n[sql_http]\nlisten = \"127.0.0.1:{sql}\"\n\n[prom_http]\nlisten = \"127.0.0.1:{prom}\"\n\n[metrics_http]\nlisten = \"127.0.0.1:{metrics}\"\n"
        ));
        std::fs::write(&self.config, toml).expect("write dataserver config");
        (sql, prom, metrics)
    }

    fn wait_healthy(&mut self) {
        let base = self.base.clone();
        wait_until(
            "dataserver /health 就绪",
            Duration::from_secs(40),
            || match http::get(&format!("{base}/health")) {
                Ok((200, body)) if body.contains("\"status\":\"ok\"") => Some(()),
                Ok((200, body)) => panic!(
                    "/health 返回 200 但 body 不含 status=ok: {}",
                    http::tail(&body)
                ),
                _ => None,
            },
        );
    }

    fn join_drainers(&mut self) {
        for handle in self.drainers.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Drop for Dataserver {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.join_drainers();
    }
}

fn drain<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
    sink: Arc<Mutex<String>>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let Some(pipe) = pipe else { return };
        for line in BufReader::new(pipe).lines() {
            let Ok(line) = line else { break };
            if let Ok(mut buf) = sink.lock() {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
    })
}

/// 在同一进程里托管一个 `axum::Router`，返回 `http://host:port`。
///
/// 端口由内核分配（先 `bind:0` 再 `local_addr`），不存在「先取端口再绑」的抢占窗口，
/// 也不需要一个会泄漏的后台清理机制：任务随 runtime 结束。
///
/// 适用「只想要一个能打的 HTTP 口」的用例（如 `e2e` / `external_storage_e2e`）；
/// 要连带验证 `--config` 解析与后端装配的，用 [`Dataserver`] 起真进程。
pub async fn serve_router(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1:0");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn tail_text(text: &str) -> String {
    let limit = 4000;
    if text.len() <= limit {
        return text.to_string();
    }
    let mut start = text.len() - limit;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…(前文省略)\n{}", &text[start..])
}
