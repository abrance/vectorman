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
