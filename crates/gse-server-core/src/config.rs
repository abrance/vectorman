/// GSE Server 配置，支持 TOML 文件 + `GSE_` 前缀环境变量覆盖。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ServerConfig {
    /// 监听地址。
    #[serde(default = "default_listen")]
    pub listen: String,
    /// 是否启用认证；关闭后任何 agent 可直接接入。
    #[serde(default = "default_true")]
    pub auth_enabled: bool,
    /// 台账 sqlite 数据库路径。
    #[serde(default = "default_db")]
    pub db: String,
    /// 是否启用 HTTP 管理端口。
    #[serde(default = "default_true")]
    pub http_enabled: bool,
    /// HTTP 管理端口监听地址，默认仅回环。
    #[serde(default = "default_http_listen")]
    pub http_listen: String,
    /// 数据面探活周期（秒）。
    #[serde(default = "default_dataplane_probe_interval")]
    pub dataplane_probe_interval_secs: u64,
    /// 静态前端目录；配置后同一 HTTP 管理端口同时托管该目录的 dist（SPA 回退 index.html）。
    /// 目录不存在或未配置则不托管静态页面，管理端口仅暴露 API。
    #[serde(default)]
    pub http_web_dir: Option<String>,
    /// agent 期望心跳周期（秒）。
    #[serde(default = "default_interval")]
    pub heartbeat_interval_secs: u64,
    /// 心跳超时窗口（秒），窗口内无消息判离线。
    #[serde(default = "default_timeout")]
    pub heartbeat_timeout_secs: u64,
    /// 是否启用作业执行（dispatch 与 HTTP /jobs）。
    #[serde(default = "default_true")]
    pub jobs_enabled: bool,
    /// 作业默认执行超时（秒）。
    #[serde(default = "default_job_timeout")]
    pub job_default_timeout_secs: u64,
    /// 作业允许的最大执行超时（秒）。
    #[serde(default = "default_job_max_timeout")]
    pub job_max_timeout_secs: u64,
    /// 脚本最大字节数。
    #[serde(default = "default_max_script")]
    pub job_max_script_bytes: usize,
    /// stdout 采集上限（字节），超出截断。
    #[serde(default = "default_stdout_limit")]
    pub job_stdout_limit_bytes: u64,
    /// stderr 采集上限（字节），超出截断。
    #[serde(default = "default_stderr_limit")]
    pub job_stderr_limit_bytes: u64,
    /// 独立 Prometheus 口，默认仅回环。
    #[serde(default = "default_metrics_listen")]
    pub metrics_listen: String,
    /// 单文件大小上限（字节），缺省 64 MiB。
    #[serde(default = "default_max_file")]
    pub job_max_file_bytes: u64,
    /// 临时文件目录；空则使用 `{db}.job-files`。
    #[serde(default)]
    pub job_file_dir: String,
    /// 临时文件保留秒数，缺省 24h。
    #[serde(default = "default_file_retain")]
    pub job_file_retain_secs: u64,
    /// 分块大小（字节），缺省 1 MiB。
    #[serde(default = "default_file_chunk")]
    pub job_file_chunk_bytes: u64,
    /// 过期清理扫描间隔（秒）。
    #[serde(default = "default_file_cleanup")]
    pub job_file_cleanup_interval_secs: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            auth_enabled: true,
            db: default_db(),
            http_enabled: true,
            http_listen: default_http_listen(),
            dataplane_probe_interval_secs: default_dataplane_probe_interval(),
            http_web_dir: None,
            heartbeat_interval_secs: default_interval(),
            heartbeat_timeout_secs: default_timeout(),
            jobs_enabled: true,
            job_default_timeout_secs: default_job_timeout(),
            job_max_timeout_secs: default_job_max_timeout(),
            job_max_script_bytes: default_max_script(),
            job_stdout_limit_bytes: default_stdout_limit(),
            job_stderr_limit_bytes: default_stderr_limit(),
            metrics_listen: default_metrics_listen(),
            job_max_file_bytes: default_max_file(),
            job_file_dir: String::new(),
            job_file_retain_secs: default_file_retain(),
            job_file_chunk_bytes: default_file_chunk(),
            job_file_cleanup_interval_secs: default_file_cleanup(),
        }
    }
}

impl ServerConfig {
    /// 临时文件目录：显式配置优先，否则 `{db}.job-files`。
    pub fn resolved_job_file_dir(&self) -> std::path::PathBuf {
        if !self.job_file_dir.trim().is_empty() {
            return std::path::PathBuf::from(&self.job_file_dir);
        }
        std::path::PathBuf::from(format!("{}.job-files", self.db))
    }
}

fn default_listen() -> String {
    "0.0.0.0:7100".to_string()
}

fn default_true() -> bool {
    true
}

fn default_db() -> String {
    "gse-server.db".to_string()
}

fn default_http_listen() -> String {
    "127.0.0.1:7101".to_string()
}

fn default_dataplane_probe_interval() -> u64 {
    30
}

fn default_interval() -> u64 {
    30
}

fn default_timeout() -> u64 {
    90
}

fn default_job_timeout() -> u64 {
    300
}

fn default_job_max_timeout() -> u64 {
    3600
}

fn default_max_script() -> usize {
    262144
}

fn default_stdout_limit() -> u64 {
    1048576
}

fn default_stderr_limit() -> u64 {
    1048576
}

fn default_metrics_listen() -> String {
    "127.0.0.1:7102".to_string()
}

fn default_max_file() -> u64 {
    67108864
}

fn default_file_retain() -> u64 {
    86400
}

fn default_file_chunk() -> u64 {
    1048576
}

fn default_file_cleanup() -> u64 {
    600
}

/// 从 TOML 文件加载配置并应用环境变量覆盖；失败返回含路径的错误信息。
pub fn load_config(path: &str) -> Result<ServerConfig, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let mut cfg: ServerConfig = toml::from_str(&raw).map_err(|e| format!("parse {path}: {e}"))?;
    if let Ok(v) = std::env::var("GSE_SERVER_LISTEN") {
        cfg.listen = v;
    }
    if let Ok(v) = std::env::var("GSE_SERVER_AUTH") {
        cfg.auth_enabled = v == "1" || v.eq_ignore_ascii_case("true");
    }
    if let Ok(v) = std::env::var("GSE_SERVER_DB") {
        cfg.db = v;
    }
    if let Ok(v) = std::env::var("GSE_SERVER_HTTP_LISTEN") {
        // 设置即启用 HTTP 管理端口。
        cfg.http_enabled = true;
        cfg.http_listen = v;
    }
    if let Ok(v) = std::env::var("GSE_SERVER_HTTP_WEB_DIR") {
        // 显式设空串可关闭静态托管。
        cfg.http_web_dir = if v.is_empty() { None } else { Some(v) };
    }
    if let Ok(v) = std::env::var("GSE_DATAPLANE_PROBE_INTERVAL") {
        if let Ok(secs) = v.parse() {
            cfg.dataplane_probe_interval_secs = secs;
        }
    }
    if let Ok(v) = std::env::var("GSE_SERVER_HEARTBEAT_TIMEOUT") {
        if let Ok(secs) = v.parse() {
            cfg.heartbeat_timeout_secs = secs;
        }
    }
    if let Ok(v) = std::env::var("GSE_SERVER_JOBS") {
        cfg.jobs_enabled = v == "1" || v.eq_ignore_ascii_case("true");
    }
    if let Ok(v) = std::env::var("GSE_SERVER_JOB_DEFAULT_TIMEOUT") {
        if let Ok(secs) = v.parse() {
            cfg.job_default_timeout_secs = secs;
        }
    }
    if let Ok(v) = std::env::var("GSE_SERVER_JOB_MAX_TIMEOUT") {
        if let Ok(secs) = v.parse() {
            cfg.job_max_timeout_secs = secs;
        }
    }
    if let Ok(v) = std::env::var("GSE_SERVER_JOB_MAX_SCRIPT_BYTES") {
        if let Ok(n) = v.parse() {
            cfg.job_max_script_bytes = n;
        }
    }
    if let Ok(v) = std::env::var("GSE_SERVER_JOB_STDOUT_LIMIT") {
        if let Ok(n) = v.parse() {
            cfg.job_stdout_limit_bytes = n;
        }
    }
    if let Ok(v) = std::env::var("GSE_SERVER_JOB_STDERR_LIMIT") {
        if let Ok(n) = v.parse() {
            cfg.job_stderr_limit_bytes = n;
        }
    }
    if let Ok(v) = std::env::var("GSE_SERVER_METRICS_LISTEN") {
        cfg.metrics_listen = v;
    }
    if let Ok(v) = std::env::var("GSE_SERVER_JOB_MAX_FILE") {
        if let Ok(n) = v.parse() {
            cfg.job_max_file_bytes = n;
        }
    }
    if let Ok(v) = std::env::var("GSE_SERVER_JOB_FILE_DIR") {
        cfg.job_file_dir = v;
    }
    if let Ok(v) = std::env::var("GSE_SERVER_JOB_FILE_RETAIN") {
        if let Ok(n) = v.parse() {
            cfg.job_file_retain_secs = n;
        }
    }
    if let Ok(v) = std::env::var("GSE_SERVER_JOB_FILE_CHUNK") {
        if let Ok(n) = v.parse() {
            cfg.job_file_chunk_bytes = n;
        }
    }
    if let Ok(v) = std::env::var("GSE_SERVER_JOB_FILE_CLEANUP") {
        if let Ok(n) = v.parse() {
            cfg.job_file_cleanup_interval_secs = n;
        }
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 串行化环境变量测试，避免并行用例互相覆盖。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_env_clean<R>(body: impl FnOnce() -> R) -> R {
        let _guard = ENV_LOCK.lock().unwrap();
        for key in [
            "GSE_SERVER_LISTEN",
            "GSE_SERVER_AUTH",
            "GSE_SERVER_DB",
            "GSE_SERVER_HTTP_LISTEN",
            "GSE_SERVER_HTTP_WEB_DIR",
            "GSE_DATAPLANE_PROBE_INTERVAL",
            "GSE_SERVER_HEARTBEAT_TIMEOUT",
            "GSE_SERVER_JOBS",
            "GSE_SERVER_JOB_DEFAULT_TIMEOUT",
            "GSE_SERVER_JOB_MAX_TIMEOUT",
            "GSE_SERVER_JOB_MAX_SCRIPT_BYTES",
            "GSE_SERVER_JOB_STDOUT_LIMIT",
            "GSE_SERVER_JOB_STDERR_LIMIT",
            "GSE_SERVER_METRICS_LISTEN",
            "GSE_SERVER_JOB_MAX_FILE",
            "GSE_SERVER_JOB_FILE_DIR",
            "GSE_SERVER_JOB_FILE_RETAIN",
            "GSE_SERVER_JOB_FILE_CHUNK",
            "GSE_SERVER_JOB_FILE_CLEANUP",
        ] {
            std::env::remove_var(key);
        }
        let r = body();
        for key in [
            "GSE_SERVER_LISTEN",
            "GSE_SERVER_AUTH",
            "GSE_SERVER_DB",
            "GSE_SERVER_HTTP_LISTEN",
            "GSE_SERVER_HTTP_WEB_DIR",
            "GSE_DATAPLANE_PROBE_INTERVAL",
            "GSE_SERVER_HEARTBEAT_TIMEOUT",
            "GSE_SERVER_JOBS",
            "GSE_SERVER_JOB_DEFAULT_TIMEOUT",
            "GSE_SERVER_JOB_MAX_TIMEOUT",
            "GSE_SERVER_JOB_MAX_SCRIPT_BYTES",
            "GSE_SERVER_JOB_STDOUT_LIMIT",
            "GSE_SERVER_JOB_STDERR_LIMIT",
            "GSE_SERVER_METRICS_LISTEN",
            "GSE_SERVER_JOB_MAX_FILE",
            "GSE_SERVER_JOB_FILE_DIR",
            "GSE_SERVER_JOB_FILE_RETAIN",
            "GSE_SERVER_JOB_FILE_CHUNK",
            "GSE_SERVER_JOB_FILE_CLEANUP",
        ] {
            std::env::remove_var(key);
        }
        r
    }

    fn write_tmp(dir: &std::path::Path, name: &str, content: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn load_full_toml() {
        with_env_clean(|| {
            let dir = std::env::temp_dir().join(format!("gse-server-cfg-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = write_tmp(
                &dir,
                "full.toml",
                r#"
listen = "127.0.0.1:7777"
auth_enabled = false
db = "/tmp/ledger.db"
http_enabled = true
http_listen = "127.0.0.1:9999"
http_web_dir = "web"
heartbeat_interval_secs = 10
heartbeat_timeout_secs = 30
jobs_enabled = false
job_default_timeout_secs = 120
job_max_timeout_secs = 600
job_max_script_bytes = 1024
job_stdout_limit_bytes = 2048
job_stderr_limit_bytes = 4096

[agents]
web-01 = "tok-a"
web-02 = "tok-b"
"#,
            );
            let cfg = load_config(&path).expect("parse full toml");
            assert_eq!(cfg.listen, "127.0.0.1:7777");
            assert!(!cfg.auth_enabled);
            assert_eq!(cfg.db, "/tmp/ledger.db");
            assert!(cfg.http_enabled);
            assert_eq!(cfg.http_listen, "127.0.0.1:9999");
            assert_eq!(cfg.http_web_dir.as_deref(), Some("web"));
            assert_eq!(cfg.heartbeat_interval_secs, 10);
            assert_eq!(cfg.heartbeat_timeout_secs, 30);
            assert!(!cfg.jobs_enabled);
            assert_eq!(cfg.job_default_timeout_secs, 120);
            assert_eq!(cfg.job_max_timeout_secs, 600);
            assert_eq!(cfg.job_max_script_bytes, 1024);
            assert_eq!(cfg.job_stdout_limit_bytes, 2048);
            assert_eq!(cfg.job_stderr_limit_bytes, 4096);
            assert_eq!(cfg.metrics_listen, "127.0.0.1:7102");
        });
    }

    #[test]
    fn load_absent_fields_fall_back_to_defaults() {
        with_env_clean(|| {
            let dir = std::env::temp_dir().join(format!("gse-server-cfg-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = write_tmp(&dir, "minimal.toml", "# empty config\n");
            let cfg = load_config(&path).expect("parse minimal toml");
            assert_eq!(cfg.listen, "0.0.0.0:7100");
            assert!(cfg.auth_enabled);
            assert_eq!(cfg.db, "gse-server.db");
            assert!(cfg.http_enabled);
            assert_eq!(cfg.http_listen, "127.0.0.1:7101");
            assert!(cfg.http_web_dir.is_none());
            assert_eq!(cfg.heartbeat_interval_secs, 30);
            assert_eq!(cfg.heartbeat_timeout_secs, 90);
            assert!(cfg.jobs_enabled);
            assert_eq!(cfg.job_default_timeout_secs, 300);
            assert_eq!(cfg.job_max_timeout_secs, 3600);
            assert_eq!(cfg.job_max_script_bytes, 262144);
            assert_eq!(cfg.job_stdout_limit_bytes, 1048576);
            assert_eq!(cfg.job_stderr_limit_bytes, 1048576);
            assert_eq!(cfg.metrics_listen, "127.0.0.1:7102");
            assert_eq!(cfg.job_max_file_bytes, 67108864);
            assert!(cfg.job_file_dir.is_empty());
            assert_eq!(cfg.job_file_retain_secs, 86400);
            assert_eq!(cfg.job_file_chunk_bytes, 1048576);
            assert_eq!(cfg.job_file_cleanup_interval_secs, 600);
        });
    }

    #[test]
    fn legacy_agents_section_is_ignored() {
        with_env_clean(|| {
            let dir = std::env::temp_dir().join(format!("gse-server-cfg-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = write_tmp(
                &dir,
                "legacy.toml",
                r#"
listen = "0.0.0.0:7100"
auth_enabled = true

[agents]
web-01 = "tok-a"
"#,
            );
            let cfg = load_config(&path).expect("parse legacy toml");
            assert_eq!(cfg.db, "gse-server.db");
        });
    }

    #[test]
    fn env_overrides_listen_and_auth_and_timeout() {
        with_env_clean(|| {
            let dir = std::env::temp_dir().join(format!("gse-server-cfg-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = write_tmp(&dir, "env.toml", "listen = \"0.0.0.0:7100\"\n");
            std::env::set_var("GSE_SERVER_LISTEN", "0.0.0.0:9999");
            std::env::set_var("GSE_SERVER_AUTH", "false");
            std::env::set_var("GSE_SERVER_DB", "/tmp/ledger.db");
            std::env::set_var("GSE_SERVER_HTTP_LISTEN", "0.0.0.0:7777");
            std::env::set_var("GSE_SERVER_HTTP_WEB_DIR", "/srv/web");
            std::env::set_var("GSE_SERVER_HEARTBEAT_TIMEOUT", "45");
            std::env::set_var("GSE_SERVER_JOBS", "false");
            std::env::set_var("GSE_SERVER_JOB_DEFAULT_TIMEOUT", "60");
            std::env::set_var("GSE_SERVER_JOB_MAX_TIMEOUT", "900");
            std::env::set_var("GSE_SERVER_JOB_MAX_SCRIPT_BYTES", "2048");
            std::env::set_var("GSE_SERVER_JOB_STDOUT_LIMIT", "4096");
            std::env::set_var("GSE_SERVER_JOB_STDERR_LIMIT", "8192");
            std::env::set_var("GSE_SERVER_METRICS_LISTEN", "127.0.0.1:7109");
            let cfg = load_config(&path).expect("parse");
            assert_eq!(cfg.listen, "0.0.0.0:9999");
            assert!(!cfg.auth_enabled);
            assert_eq!(cfg.db, "/tmp/ledger.db");
            assert!(cfg.http_enabled);
            assert_eq!(cfg.http_listen, "0.0.0.0:7777");
            assert_eq!(cfg.http_web_dir.as_deref(), Some("/srv/web"));
            assert_eq!(cfg.heartbeat_timeout_secs, 45);
            assert!(!cfg.jobs_enabled);
            assert_eq!(cfg.job_default_timeout_secs, 60);
            assert_eq!(cfg.job_max_timeout_secs, 900);
            assert_eq!(cfg.job_max_script_bytes, 2048);
            assert_eq!(cfg.job_stdout_limit_bytes, 4096);
            assert_eq!(cfg.job_stderr_limit_bytes, 8192);
            assert_eq!(cfg.metrics_listen, "127.0.0.1:7109");
        });
    }

    #[test]
    fn malformed_toml_returns_error() {
        with_env_clean(|| {
            let dir = std::env::temp_dir().join(format!("gse-server-cfg-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = write_tmp(&dir, "bad.toml", "listen = {\n");
            let err = load_config(&path).expect_err("should fail");
            assert!(
                err.contains("config_invalid") || err.contains("parse"),
                "{err}"
            );
            assert!(err.contains(&path), "{err}");
        });
    }

    #[test]
    fn missing_file_returns_error() {
        with_env_clean(|| {
            let err = load_config("/nonexistent/gse-server.toml").expect_err("should fail");
            assert!(err.contains("cannot read"), "{err}");
            assert!(err.contains("/nonexistent/gse-server.toml"), "{err}");
        });
    }
}
