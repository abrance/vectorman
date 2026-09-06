use std::collections::HashMap;

/// GSE Server 配置，支持 TOML 文件 + `GSE_` 前缀环境变量覆盖。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ServerConfig {
    /// 监听地址。
    #[serde(default = "default_listen")]
    pub listen: String,
    /// 是否启用认证；关闭后任何 agent 可直接接入。
    #[serde(default = "default_true")]
    pub auth_enabled: bool,
    /// agent-id → token 静态表，对应 TOML `[agents]` 段。
    #[serde(default)]
    pub agents: HashMap<String, String>,
    /// agent 期望心跳周期（秒）。
    #[serde(default = "default_interval")]
    pub heartbeat_interval_secs: u64,
    /// 心跳超时窗口（秒），窗口内无消息判离线。
    #[serde(default = "default_timeout")]
    pub heartbeat_timeout_secs: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            auth_enabled: true,
            agents: HashMap::new(),
            heartbeat_interval_secs: default_interval(),
            heartbeat_timeout_secs: default_timeout(),
        }
    }
}

fn default_listen() -> String {
    "0.0.0.0:7100".to_string()
}

fn default_true() -> bool {
    true
}

fn default_interval() -> u64 {
    30
}

fn default_timeout() -> u64 {
    90
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
    if let Ok(v) = std::env::var("GSE_SERVER_HEARTBEAT_TIMEOUT") {
        if let Ok(secs) = v.parse() {
            cfg.heartbeat_timeout_secs = secs;
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
            "GSE_SERVER_HEARTBEAT_TIMEOUT",
        ] {
            std::env::remove_var(key);
        }
        let r = body();
        for key in [
            "GSE_SERVER_LISTEN",
            "GSE_SERVER_AUTH",
            "GSE_SERVER_HEARTBEAT_TIMEOUT",
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
heartbeat_interval_secs = 10
heartbeat_timeout_secs = 30

[agents]
web-01 = "tok-a"
web-02 = "tok-b"
"#,
            );
            let cfg = load_config(&path).expect("parse full toml");
            assert_eq!(cfg.listen, "127.0.0.1:7777");
            assert!(!cfg.auth_enabled);
            assert_eq!(cfg.heartbeat_interval_secs, 10);
            assert_eq!(cfg.heartbeat_timeout_secs, 30);
            assert_eq!(*cfg.agents.get("web-01").unwrap(), "tok-a");
            assert_eq!(*cfg.agents.get("web-02").unwrap(), "tok-b");
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
            assert!(cfg.agents.is_empty());
            assert_eq!(cfg.heartbeat_interval_secs, 30);
            assert_eq!(cfg.heartbeat_timeout_secs, 90);
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
            std::env::set_var("GSE_SERVER_HEARTBEAT_TIMEOUT", "45");
            let cfg = load_config(&path).expect("parse");
            assert_eq!(cfg.listen, "0.0.0.0:9999");
            assert!(!cfg.auth_enabled);
            assert_eq!(cfg.heartbeat_timeout_secs, 45);
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
