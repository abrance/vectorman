use serde::{Deserialize, Serialize};

/// 默认配置文件内容，供 `config.toml.example` 使用。
pub const DEFAULT_CONFIG_TOML: &str = r#"# vectorman dataserver 配置示例（下面每一项的缺省值就是注释里写的值）
#
# 用法：复制为 config.toml（安装包布局为 <组件目录>/config.toml）。
# 环境变量覆盖（优先级高于文件）：
#   数据与监听：DP_DATA_PATH / DP_SQL_HTTP_LISTEN / DP_PROM_HTTP_LISTEN / DP_METRICS_HTTP_LISTEN
#   鉴权与自监控：DP_AUTH_ENABLED / DP_SELF_METRICS_INTERVAL
#   KV：DATASERVER_KV_BACKEND / DATASERVER_KV_CACHE_BYTES / DATASERVER_REDIS_URL /
#       DATASERVER_REDIS_PASSWORD / DATASERVER_REDIS_PREFIX
#   接入去重：DATASERVER_INGEST_DEDUP_RETENTION_DAYS
#   时序：DATASERVER_TS_BACKEND / DATASERVER_TS_RETENTION_DAYS /
#         DATASERVER_TS_RETENTION_ENFORCED / DATASERVER_TS_CARDINALITY_LIMIT /
#         DATASERVER_TS_MEMORY_LIMIT_BYTES / DATASERVER_TS_WAL_SIZE_LIMIT_BYTES /
#         DATASERVER_TS_CLEAN_INTERVAL
#   VM：DATASERVER_VM_INSERT_URL / DATASERVER_VM_SELECT_URL / DATASERVER_VM_TENANT /
#       DATASERVER_VM_SINGLE_NODE / DATASERVER_VM_BATCH_SIZE /
#       DATASERVER_VM_FLUSH_INTERVAL_MS / DATASERVER_VM_QUEUE_CAPACITY /
#       DATASERVER_VM_DELETE_AUTH_KEY / DATASERVER_VM_WHOLE_SERIES_DELETE
#   APM：DATASERVER_APM_ENABLED / DATASERVER_APM_RETENTION_DAYS /
#        DATASERVER_APM_ENDPOINT_RETENTION_DAYS / DATASERVER_APM_MAX_BYTES /
#        DATASERVER_APM_CLEAN_INTERVAL / DATASERVER_APM_AGG_INTERVAL /
#        DATASERVER_APM_INGEST_MAX_BATCHES_PER_SEC /
#        DATASERVER_APM_MIN_DURATION_MICROS_FOR_DETAIL
#   前端与 GSE：DATASERVER_HTTP_WEB_DIR / DATASERVER_GSE_ADMIN_URL /
#                DATASERVER_GSE_ADMIN_PASSWORD

# 数据目录：必须是目录（单文件仅支持 sqlite）。跨进程并发写同一目录不做文件锁。
data_path = "./data"
# 自监控指标写回时序库的周期（秒）；0 关闭。
self_metrics_interval_secs = 60

# ---- 本地 KV（redb）：接入去重键与页缓存 ----
# redb 页缓存上限（字节）；0 = 用内置缺省 64MiB。redb 自己的缺省是 1GiB，
# 容器内存上限 2GiB 时光页缓存就占掉一半，数据一涨就 OOMKilled。
kv_cache_bytes = 67108864
# `ingest/{record_id}` 去重键保留期（天）；0 = 不清理。
# 去重只防接入重试造成的重复，留几天足够；这些键只写不删会单调增长。
ingest_dedup_retention_days = 3

# ---- 时序聚合指标（ts_*）：网络/进程/syscall 指标与通用 metrics 都落在这里 ----
# 全局保留窗口（天）；每个采集项更短的保留期由下面的定时删除任务补齐。
ts_retention_days = 30
ts_retention_enforced = true
# 基数上限与内存上限（0 = 不限上限，按引擎默认）。
ts_cardinality_limit = 2000000
# ts_memory_limit_bytes = 0
# ts_wal_size_limit_bytes = 0
# 定时删除周期（秒）。注意：实现里有下限 60 秒，配更小的值不会更快。
ts_clean_interval_secs = 3600

# ---- APM（trace 明细、span 摘要、服务 RED、拓扑边）----
# 关闭后 /v1/traces/*、/v1/edges/*、/v1/apm/* 仍可查询，但没有新数据。
apm_enabled = true
# 端点表（服务名反查用）保留期（天）。
apm_endpoint_retention_days = 30
# APM 聚合节拍（秒）：span 摘要落库、边派生都按这个周期推进。
apm_agg_interval_secs = 60
# 采集项未显式给保留期时的缺省值（天）。
apm_retention_days_default = 3
# APM 数据全局容量上限（字节）；0 = 不限。超限时按最久远优先淘汰。
apm_max_bytes = 0
# APM 清理周期（秒），同样受下限 60 秒约束。
apm_clean_interval_secs = 3600
# trace 接入限流（每秒批次数）；0 = 不限。超限返回 429，Agent 按既有退避重试。
apm_ingest_max_batches_per_sec = 0
# 只写明细的耗时下限（微秒）；0 = 全部写明细。高于该阈值的 span 才落 LogStore 明细。
apm_min_duration_micros_for_detail = 0

# ---- 外置后端（默认全本地，外置只在需要横向扩存储时开）----
# KV 后端：redb（本地默认）| redis。切 redis 必须同时给 redis_url，否则启动即报错退出。
kv_backend = "redb"
# 时序后端：tsink（本地默认）| victoriametrics。切 VM 必须同时给 vm_insert_url 与
# vm_select_url，否则启动即报错退出（单机版两个地址可以相同）。
ts_backend = "tsink"
# redis_url = "redis://127.0.0.1:6379/0"
# redis_password = ""
# redis_prefix = "dp:"
# vm_insert_url = "http://127.0.0.1:8480"
# vm_select_url = "http://127.0.0.1:8481"
# vm_tenant = "0"
# vm_single_node = true
# vm_batch_size = 5000
# vm_flush_interval_ms = 200
# vm_queue_capacity = 200000
# vm_delete_auth_key = ""
# vm_whole_series_delete = false

[sql_http]
listen = "0.0.0.0:8081"

[prom_http]
listen = "0.0.0.0:9090"

[metrics_http]
listen = "127.0.0.1:9091"

[auth]
enabled = false

# 前端 dist 目录：配置后同一 SQL HTTP 端口同时托管前端（SPA 子路由回退 index.html）。
# 目录需含 index.html；未配置或缺失时仅暴露 API。
# http_web_dir = "web"

# GSE 管理口：配置后经 dataserver 反代 /v1/collect-items 到 GSE 台账，
# 并且保存周期清理会从 GSE 读「live 采集项」（这是按采集项保留期清理的前提）。
# gse_admin_url = "http://127.0.0.1:7101"
# gse-server 开了管理口密码（GSE_SERVER_ADMIN_PASSWORD）时必须一起配上，否则转发与保留清理会 401：
# gse_admin_password = "..."
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpListenConfig {
    pub listen: String,
}

impl Default for HttpListenConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:8081".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub data_path: String,
    pub sql_http: HttpListenConfig,
    pub prom_http: HttpListenConfig,
    pub auth: AuthConfig,
    pub http_web_dir: Option<String>,
    pub gse_admin_url: Option<String>,
    /// gse-server 管理端口密码；空/None 表示对端未开认证。
    ///
    /// dataserver 需要它是因为有两处要读 gse-server 管理口：只读转发 `/v1/agent-specs`
    /// 与保留清理拉 live 采集项。建议用环境变量 `DATASERVER_GSE_ADMIN_PASSWORD`。
    pub gse_admin_password: Option<String>,
    #[serde(default = "default_metrics_http")]
    pub metrics_http: HttpListenConfig,
    #[serde(default = "default_self_metrics_interval")]
    pub self_metrics_interval_secs: u64,
    /// redb 页缓存上限（字节）；0 表示用内置缺省。
    ///
    /// redb 4.x 自己的缺省是 1GiB：容器内存上限 2GiB 时它一个就占掉一半，
    /// 叠加数据文件增长（`ingest/` 去重键）会把进程顶到 OOMKilled。
    #[serde(default = "default_kv_cache_bytes")]
    pub kv_cache_bytes: usize,
    /// `ingest/{record_id}` 去重键保留期（天）；0 表示不清理。
    ///
    /// 去重只防接入重试造成的重复，窗口留几天足够；不清理这些键会单调增长。
    #[serde(default = "default_ingest_dedup_retention_days")]
    pub ingest_dedup_retention_days: u32,
    /// 时序聚合指标的全局保留窗口（天）。0 表示不设窗口（需关闭执行）。
    #[serde(default = "default_ts_retention_days")]
    pub ts_retention_days: u32,
    /// 是否真正拒绝/过滤超出保留窗口的时序点。
    #[serde(default = "default_true")]
    pub ts_retention_enforced: bool,
    /// 时序序列数上限，0 表示不限。
    #[serde(default = "default_ts_cardinality_limit")]
    pub ts_cardinality_limit: usize,
    /// 时序内存预算（字节），0 表示不限。
    #[serde(default)]
    pub ts_memory_limit_bytes: usize,
    /// 时序 WAL 字节上限，0 表示不限。
    #[serde(default)]
    pub ts_wal_size_limit_bytes: usize,
    /// 时序按采集项保留期的清理周期（秒），0 表示不清理。
    #[serde(default = "default_ts_clean_interval")]
    pub ts_clean_interval_secs: u64,
    /// 是否启用 APM（trace 摘要、服务端点半、后续的聚合任务）。
    #[serde(default = "default_true")]
    pub apm_enabled: bool,
    /// 服务端点半保留期（天）。
    #[serde(default = "default_apm_endpoint_retention_days")]
    pub apm_endpoint_retention_days: u32,
    /// RED 与边指标的聚合周期（秒）。
    #[serde(default = "default_apm_agg_interval")]
    pub apm_agg_interval_secs: u64,
    /// APM 明细/摘要/边摘要的默认保留天数。
    #[serde(default = "default_apm_retention_days")]
    pub apm_retention_days_default: u32,
    /// `data_path` 全局容量上限（字节）；0 表示不限。超限时按最久远优先淘汰 APM 数据。
    #[serde(default)]
    pub apm_max_bytes: u64,
    /// APM 保留策略执行周期（秒）。
    #[serde(default = "default_apm_clean_interval")]
    pub apm_clean_interval_secs: u64,
    /// trace 接入限流（每秒批次数）；0 表示不限。超限返回 429 + `unavailable`。
    #[serde(default)]
    pub apm_ingest_max_batches_per_sec: u64,
    /// 只写明细的耗时下限（微秒）；0 表示全部写明细。低于该值的 trace 只写摘要与聚合。
    #[serde(default)]
    pub apm_min_duration_micros_for_detail: i64,
    /// KV 后端：`redb`（本地，缺省）或 `redis`（外置）。
    #[serde(default = "default_kv_backend")]
    pub kv_backend: String,
    /// Redis 连接串（`redis://[:password@]host:port/0`）。
    #[serde(default)]
    pub redis_url: String,
    /// Redis 独立密码；空/None 表示不带 `AUTH`。
    #[serde(default)]
    pub redis_password: Option<String>,
    /// Redis 键前缀，避免与同库其它业务撞键。
    #[serde(default = "default_redis_prefix")]
    pub redis_prefix: String,
    /// 时序后端：`tsink`（本地，缺省）或 `victoriametrics`（外置）。
    #[serde(default = "default_ts_backend")]
    pub ts_backend: String,
    /// VM 集群版 vminsert 基址（单机版与 `vm_select_url` 相同）。
    #[serde(default)]
    pub vm_insert_url: String,
    /// VM 集群版 vmselect 基址。
    #[serde(default)]
    pub vm_select_url: String,
    /// VM 租户号；dataserver 无多租户，固定 `0`。
    #[serde(default = "default_vm_tenant")]
    pub vm_tenant: String,
    /// `true` 表示单机版 VM（不带 `/insert|/select` 前缀）。
    #[serde(default)]
    pub vm_single_node: bool,
    /// VM 单批点数上限。
    #[serde(default = "default_vm_batch_size")]
    pub vm_batch_size: usize,
    /// VM 攒批最长等待（毫秒）。
    #[serde(default = "default_vm_flush_interval_ms")]
    pub vm_flush_interval_ms: u64,
    /// VM 写入队列容量（点数）；满即背压。
    #[serde(default = "default_vm_queue_capacity")]
    pub vm_queue_capacity: usize,
    /// VM 删除鉴权 key（vmselect 的 `-deleteAuthKey`）；空 = 不允许删除。
    #[serde(default)]
    pub vm_delete_auth_key: Option<String>,
    /// 是否接受「按时间窗删除」退化成整序列删除（VM 删除接口不支持 start/end）。
    #[serde(default)]
    pub vm_whole_series_delete: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_path: "./data".to_string(),
            sql_http: HttpListenConfig::default(),
            prom_http: HttpListenConfig {
                listen: "0.0.0.0:9090".to_string(),
            },
            auth: AuthConfig { enabled: false },
            http_web_dir: None,
            gse_admin_url: None,
            gse_admin_password: None,
            metrics_http: default_metrics_http(),
            self_metrics_interval_secs: default_self_metrics_interval(),
            kv_cache_bytes: default_kv_cache_bytes(),
            ingest_dedup_retention_days: default_ingest_dedup_retention_days(),
            ts_retention_days: default_ts_retention_days(),
            ts_retention_enforced: true,
            ts_cardinality_limit: default_ts_cardinality_limit(),
            ts_memory_limit_bytes: 0,
            ts_wal_size_limit_bytes: 0,
            ts_clean_interval_secs: default_ts_clean_interval(),
            apm_enabled: true,
            apm_endpoint_retention_days: default_apm_endpoint_retention_days(),
            apm_agg_interval_secs: default_apm_agg_interval(),
            apm_retention_days_default: default_apm_retention_days(),
            apm_max_bytes: 0,
            apm_clean_interval_secs: default_apm_clean_interval(),
            apm_ingest_max_batches_per_sec: 0,
            apm_min_duration_micros_for_detail: 0,
            kv_backend: default_kv_backend(),
            redis_url: String::new(),
            redis_password: None,
            redis_prefix: default_redis_prefix(),
            ts_backend: default_ts_backend(),
            vm_insert_url: String::new(),
            vm_select_url: String::new(),
            vm_tenant: default_vm_tenant(),
            vm_single_node: false,
            vm_batch_size: default_vm_batch_size(),
            vm_flush_interval_ms: default_vm_flush_interval_ms(),
            vm_queue_capacity: default_vm_queue_capacity(),
            vm_delete_auth_key: None,
            vm_whole_series_delete: false,
        }
    }
}

impl Config {
    /// 从 TOML 字符串解析配置；失败返回 `config_invalid`。
    pub fn from_toml(s: &str) -> Result<Config, crate::DataplaneError> {
        let mut cfg: Config = toml::from_str(s)
            .map_err(|e| crate::DataplaneError::config_invalid(format!("invalid TOML: {e}")))?;
        cfg.normalize();
        Ok(cfg)
    }

    /// 应用环境变量覆盖（`DP_*` 前缀），后写覆盖前写。
    pub fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("DP_DATA_PATH") {
            self.data_path = v;
        }
        if let Ok(v) = std::env::var("DP_SQL_HTTP_LISTEN") {
            self.sql_http.listen = v;
        }
        if let Ok(v) = std::env::var("DP_PROM_HTTP_LISTEN") {
            self.prom_http.listen = v;
        }
        if let Ok(v) = std::env::var("DP_AUTH_ENABLED") {
            self.auth.enabled = v.eq_ignore_ascii_case("true") || v == "1";
        }
        if let Ok(v) = std::env::var("DATASERVER_HTTP_WEB_DIR") {
            self.http_web_dir = nonempty_opt(v);
        }
        if let Ok(v) = std::env::var("DATASERVER_GSE_ADMIN_URL") {
            self.gse_admin_url = nonempty_opt(v);
        }
        if let Ok(v) = std::env::var("DATASERVER_GSE_ADMIN_PASSWORD") {
            // 空串显式表示「对端没开认证」。
            self.gse_admin_password = if v.is_empty() { None } else { Some(v) };
        }
        if let Ok(v) = std::env::var("DP_METRICS_HTTP_LISTEN") {
            self.metrics_http.listen = v;
        }
        if let Ok(v) = std::env::var("DP_SELF_METRICS_INTERVAL") {
            if let Ok(n) = v.parse() {
                self.self_metrics_interval_secs = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_KV_CACHE_BYTES") {
            if let Ok(n) = v.parse() {
                self.kv_cache_bytes = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_INGEST_DEDUP_RETENTION_DAYS") {
            if let Ok(n) = v.parse() {
                self.ingest_dedup_retention_days = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_TS_RETENTION_DAYS") {
            if let Ok(n) = v.parse() {
                self.ts_retention_days = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_TS_RETENTION_ENFORCED") {
            self.ts_retention_enforced = v.eq_ignore_ascii_case("true") || v == "1";
        }
        if let Ok(v) = std::env::var("DATASERVER_TS_CARDINALITY_LIMIT") {
            if let Ok(n) = v.parse() {
                self.ts_cardinality_limit = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_TS_MEMORY_LIMIT_BYTES") {
            if let Ok(n) = v.parse() {
                self.ts_memory_limit_bytes = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_TS_WAL_SIZE_LIMIT_BYTES") {
            if let Ok(n) = v.parse() {
                self.ts_wal_size_limit_bytes = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_APM_ENABLED") {
            self.apm_enabled = v.eq_ignore_ascii_case("true") || v == "1";
        }
        if let Ok(v) = std::env::var("DATASERVER_APM_RETENTION_DAYS") {
            if let Ok(n) = v.parse() {
                self.apm_retention_days_default = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_APM_MAX_BYTES") {
            if let Ok(n) = v.parse() {
                self.apm_max_bytes = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_APM_INGEST_MAX_BATCHES_PER_SEC") {
            if let Ok(n) = v.parse() {
                self.apm_ingest_max_batches_per_sec = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_APM_MIN_DURATION_MICROS_FOR_DETAIL") {
            if let Ok(n) = v.parse() {
                self.apm_min_duration_micros_for_detail = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_APM_CLEAN_INTERVAL") {
            if let Ok(n) = v.parse() {
                self.apm_clean_interval_secs = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_APM_AGG_INTERVAL") {
            if let Ok(n) = v.parse() {
                self.apm_agg_interval_secs = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_APM_ENDPOINT_RETENTION_DAYS") {
            if let Ok(n) = v.parse() {
                self.apm_endpoint_retention_days = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_TS_CLEAN_INTERVAL") {
            if let Ok(n) = v.parse() {
                self.ts_clean_interval_secs = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_KV_BACKEND") {
            self.kv_backend = v;
        }
        if let Ok(v) = std::env::var("DATASERVER_REDIS_URL") {
            self.redis_url = v;
        }
        if let Ok(v) = std::env::var("DATASERVER_REDIS_PASSWORD") {
            // 空串显式表示「Redis 没开密码」。
            self.redis_password = if v.is_empty() { None } else { Some(v) };
        }
        if let Ok(v) = std::env::var("DATASERVER_REDIS_PREFIX") {
            self.redis_prefix = v;
        }
        if let Ok(v) = std::env::var("DATASERVER_TS_BACKEND") {
            self.ts_backend = v;
        }
        if let Ok(v) = std::env::var("DATASERVER_VM_INSERT_URL") {
            self.vm_insert_url = v;
        }
        if let Ok(v) = std::env::var("DATASERVER_VM_SELECT_URL") {
            self.vm_select_url = v;
        }
        if let Ok(v) = std::env::var("DATASERVER_VM_TENANT") {
            self.vm_tenant = v;
        }
        if let Ok(v) = std::env::var("DATASERVER_VM_SINGLE_NODE") {
            self.vm_single_node = v.eq_ignore_ascii_case("true") || v == "1";
        }
        if let Ok(v) = std::env::var("DATASERVER_VM_BATCH_SIZE") {
            if let Ok(n) = v.parse() {
                self.vm_batch_size = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_VM_FLUSH_INTERVAL_MS") {
            if let Ok(n) = v.parse() {
                self.vm_flush_interval_ms = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_VM_QUEUE_CAPACITY") {
            if let Ok(n) = v.parse() {
                self.vm_queue_capacity = n;
            }
        }
        if let Ok(v) = std::env::var("DATASERVER_VM_DELETE_AUTH_KEY") {
            self.vm_delete_auth_key = if v.is_empty() { None } else { Some(v) };
        }
        if let Ok(v) = std::env::var("DATASERVER_VM_WHOLE_SERIES_DELETE") {
            self.vm_whole_series_delete = v.eq_ignore_ascii_case("true") || v == "1";
        }
        self.normalize();
    }

    fn normalize(&mut self) {
        self.http_web_dir = self.http_web_dir.take().and_then(nonempty_opt);
        self.gse_admin_url = self.gse_admin_url.take().and_then(nonempty_opt);
        self.kv_backend = self.kv_backend.trim().to_ascii_lowercase();
        self.ts_backend = self.ts_backend.trim().to_ascii_lowercase();
        if self.redis_prefix.is_empty() {
            self.redis_prefix = default_redis_prefix();
        }
        if self.vm_tenant.is_empty() {
            self.vm_tenant = default_vm_tenant();
        }
    }
}

fn default_kv_backend() -> String {
    "redb".to_string()
}

fn default_ts_backend() -> String {
    "tsink".to_string()
}

fn default_redis_prefix() -> String {
    "dp:".to_string()
}

fn default_vm_tenant() -> String {
    "0".to_string()
}

fn default_vm_batch_size() -> usize {
    5_000
}

fn default_vm_flush_interval_ms() -> u64 {
    200
}

fn default_vm_queue_capacity() -> usize {
    200_000
}

fn default_metrics_http() -> HttpListenConfig {
    HttpListenConfig {
        listen: "127.0.0.1:9091".to_string(),
    }
}

fn default_self_metrics_interval() -> u64 {
    60
}

fn default_kv_cache_bytes() -> usize {
    64 * 1024 * 1024
}

fn default_ingest_dedup_retention_days() -> u32 {
    3
}

fn default_ts_retention_days() -> u32 {
    30
}

fn default_ts_cardinality_limit() -> usize {
    2_000_000
}

fn default_ts_clean_interval() -> u64 {
    3600
}

fn default_true() -> bool {
    true
}

fn default_apm_endpoint_retention_days() -> u32 {
    30
}

fn default_apm_agg_interval() -> u64 {
    60
}

fn default_apm_retention_days() -> u32 {
    3
}

fn default_apm_clean_interval() -> u64 {
    3600
}

fn nonempty_opt(v: String) -> Option<String> {
    let t = v.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// 从可选路径读取配置。`None` 时使用默认配置（含 `./data`）。
/// 调用方负责处理文件读取错误，此处不吞掉 IO 错误。
pub fn load_config(path: Option<&str>) -> Result<Config, crate::DataplaneError> {
    let mut cfg = match path {
        Some(p) => {
            let content = std::fs::read_to_string(p).map_err(|e| {
                crate::DataplaneError::config_invalid(format!("cannot read config file {p}: {e}"))
            })?;
            Config::from_toml(&content)?
        }
        None => Config::default(),
    };
    cfg.apply_env();
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `config.toml.example` 与 [`DEFAULT_CONFIG_TOML`] 必须逐字节一致。
    ///
    /// 两者曾各自漂移：示例文件停留在只有 `data_path`/端口的老版本，APM 与 TS 的开关一个都没写，
    /// 新人照抄示例会得到「APM 关闭且不知道它存在」。用测试把「同源」这件事锁住。
    #[test]
    fn config_example_matches_default_toml() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("config.toml.example");
        // 文件缺失就是失败：原来的 `eprintln!("跳过"); return;` 是一条假绿
        // （搬家/改名后这条测试永久静默通过）。
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("读不到 {}: {e}", path.display()));
        assert_eq!(
            text.trim_end(),
            DEFAULT_CONFIG_TOML.trim_end(),
            "config.toml.example 与 DEFAULT_CONFIG_TOML 不一致：改一处必须同时改另一处"
        );
        // 示例必须能解析（防止写出非法 TOML）。
        Config::from_toml(&text).expect("示例配置必须能解析");
    }

    /// 配置示例漂移守卫：`config.toml.example` 必须覆盖 `Config` 的每个字段、
    /// 以及 `apply_env` 读的每个环境变量名。
    ///
    /// 字段名与 env 名都**从源码本身**提取，不在这里维护第二份清单——第二份清单
    /// 自己就会漂移。新增 Config 字段或新读一个 env 却忘了写进示例，这条就红。
    ///
    /// 背景：示例曾经漏了 `kv_backend`/`redis_*`/`ts_backend`/`vm_*` 共 14 个键，
    /// 靠读示例根本不知道有外置后端这回事（与业务事故无关，纯文档漂移）。
    #[test]
    fn config_example_covers_every_field_and_env_var() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("config.toml.example");
        let example = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("读不到 {}: {e}", path.display()));
        let src = include_str!("config.rs");

        // ---- 1. 字段集合 ----
        let start = src
            .find("pub struct Config {")
            .expect("config.rs 里找不到 `pub struct Config {`");
        let body = &src[start..];
        let body = &body[..body.find("\n}").expect("struct Config 没有结束大括号")];
        let mut field_count = 0usize;
        let mut missing_fields = Vec::new();
        for (idx, _) in body.match_indices("    pub ") {
            let rest = &body[idx + "    pub ".len()..];
            let name = &rest[..rest.find(':').expect("字段声明里没有 `:`")];
            field_count += 1;
            if !example.contains(name) {
                missing_fields.push(name.to_string());
            }
        }
        // 提取逻辑失效（比如有人把字段改成非 `pub`）时不能静默通过。
        assert!(
            field_count >= 30,
            "只从 `struct Config` 提取到 {field_count} 个字段，提取逻辑可能已经失效"
        );
        assert!(
            missing_fields.is_empty(),
            "config.toml.example 缺少这些 Config 字段：{missing_fields:?}"
        );

        // ---- 2. 环境变量名 ----
        let mut missing_envs = Vec::new();
        let mut env_count = 0usize;
        for part in src.split("std::env::var(\"").skip(1) {
            let name = &part[..part.find('"').expect("env 名没有闭合引号")];
            env_count += 1;
            if !example.contains(name) {
                missing_envs.push(name.to_string());
            }
        }
        assert!(
            env_count >= 30,
            "只从 config.rs 提取到 {env_count} 个环境变量名，提取逻辑可能已经失效"
        );
        assert!(
            missing_envs.is_empty(),
            "config.toml.example 没有记录这些环境变量：{missing_envs:?}"
        );
    }

    #[test]
    fn from_toml_reads_web_dir_and_gse_url() {
        let cfg = Config::from_toml(
            r#"
data_path = "./data"
http_web_dir = "web"
gse_admin_url = "http://127.0.0.1:7101"
"#,
        )
        .unwrap();
        assert_eq!(cfg.http_web_dir.as_deref(), Some("web"));
        assert_eq!(cfg.gse_admin_url.as_deref(), Some("http://127.0.0.1:7101"));
    }

    #[test]
    fn empty_web_dir_and_gse_url_become_none() {
        let cfg = Config::from_toml(
            r#"
http_web_dir = ""
gse_admin_url = "   "
"#,
        )
        .unwrap();
        assert!(cfg.http_web_dir.is_none());
        assert!(cfg.gse_admin_url.is_none());
    }

    #[test]
    fn default_metrics_listen_and_interval() {
        let cfg = Config::default();
        assert_eq!(cfg.metrics_http.listen, "127.0.0.1:9091");
        assert_eq!(cfg.self_metrics_interval_secs, 60);
    }

    /// 内存与去重键保留的缺省值：它们直接决定容器会不会 OOM（见 2026-10-08 线上事故）。
    #[test]
    fn default_kv_cache_and_ingest_dedup_window() {
        let cfg = Config::default();
        assert_eq!(cfg.kv_cache_bytes, 64 * 1024 * 1024);
        assert_eq!(cfg.ingest_dedup_retention_days, 3);

        // 两个都可以显式关掉（0 = redb 内置缺省 / 不清理去重键）。
        let cfg =
            Config::from_toml("kv_cache_bytes = 0\ningest_dedup_retention_days = 0\n").unwrap();
        assert_eq!(cfg.kv_cache_bytes, 0);
        assert_eq!(cfg.ingest_dedup_retention_days, 0);
    }

    #[test]
    fn from_toml_reads_metrics_http() {
        let cfg = Config::from_toml(
            "self_metrics_interval_secs = 15\n[metrics_http]\nlisten = \"127.0.0.1:9199\"\n",
        )
        .unwrap();
        assert_eq!(cfg.metrics_http.listen, "127.0.0.1:9199");
        assert_eq!(cfg.self_metrics_interval_secs, 15);
    }
}
