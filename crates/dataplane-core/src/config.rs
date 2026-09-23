use serde::{Deserialize, Serialize};

/// 默认配置文件内容，供 `config.toml.example` 使用。
pub const DEFAULT_CONFIG_TOML: &str = r#"data_path = "./data"
self_metrics_interval_secs = 60

# 时序聚合指标的全局保留窗口（天）与是否执行；每个采集项更短的保留期由定时
# 删除任务（ts_clean_interval_secs）补齐。
ts_retention_days = 30
ts_retention_enforced = true
ts_cardinality_limit = 2000000
# ts_memory_limit_bytes = 0
# ts_wal_size_limit_bytes = 0
ts_clean_interval_secs = 3600
apm_enabled = true
apm_endpoint_retention_days = 30
apm_agg_interval_secs = 60
apm_retention_days_default = 3
# 全局容量上限（字节）；0 = 不限。超限时按最久远优先淘汰 APM 数据。
apm_max_bytes = 0
apm_clean_interval_secs = 3600
# trace 接入限流（每秒批次数）；0 = 不限。
apm_ingest_max_batches_per_sec = 0
# 只写明细的耗时下限（微秒）；0 = 全部写明细。
apm_min_duration_micros_for_detail = 0

[sql_http]
listen = "0.0.0.0:8081"

[prom_http]
listen = "0.0.0.0:9090"

[metrics_http]
listen = "127.0.0.1:9091"

[auth]
enabled = false
# http_web_dir = "web"
# gse_admin_url = "http://127.0.0.1:7101"
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
    #[serde(default = "default_metrics_http")]
    pub metrics_http: HttpListenConfig,
    #[serde(default = "default_self_metrics_interval")]
    pub self_metrics_interval_secs: u64,
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
            metrics_http: default_metrics_http(),
            self_metrics_interval_secs: default_self_metrics_interval(),
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
        if let Ok(v) = std::env::var("DP_METRICS_HTTP_LISTEN") {
            self.metrics_http.listen = v;
        }
        if let Ok(v) = std::env::var("DP_SELF_METRICS_INTERVAL") {
            if let Ok(n) = v.parse() {
                self.self_metrics_interval_secs = n;
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
        self.normalize();
    }

    fn normalize(&mut self) {
        self.http_web_dir = self.http_web_dir.take().and_then(nonempty_opt);
        self.gse_admin_url = self.gse_admin_url.take().and_then(nonempty_opt);
    }
}

fn default_metrics_http() -> HttpListenConfig {
    HttpListenConfig {
        listen: "127.0.0.1:9091".to_string(),
    }
}

fn default_self_metrics_interval() -> u64 {
    60
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
