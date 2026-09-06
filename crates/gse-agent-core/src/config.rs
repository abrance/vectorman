/// GSE Agent 配置，支持 TOML 文件 + `GSE_` 前缀环境变量覆盖。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AgentConfig {
    /// Server 地址。
    #[serde(default = "default_server_addr")]
    pub server_addr: String,
    /// agent 唯一标识。
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// 认证 token。
    #[serde(default)]
    pub token: String,
    /// 心跳周期（秒）。
    #[serde(default = "default_interval")]
    pub heartbeat_interval_secs: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            server_addr: default_server_addr(),
            agent_id: default_agent_id(),
            token: String::new(),
            heartbeat_interval_secs: default_interval(),
        }
    }
}

fn default_server_addr() -> String {
    "127.0.0.1:7100".to_string()
}

fn default_agent_id() -> String {
    "agent-1".to_string()
}

fn default_interval() -> u64 {
    30
}

/// 从 TOML 文件加载配置并应用环境变量覆盖；失败返回含路径的错误信息。
pub fn load_config(path: &str) -> Result<AgentConfig, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let mut cfg: AgentConfig = toml::from_str(&raw).map_err(|e| format!("parse {path}: {e}"))?;
    if let Ok(v) = std::env::var("GSE_AGENT_SERVER") {
        cfg.server_addr = v;
    }
    if let Ok(v) = std::env::var("GSE_AGENT_ID") {
        cfg.agent_id = v;
    }
    if let Ok(v) = std::env::var("GSE_AGENT_TOKEN") {
        cfg.token = v;
    }
    if let Ok(v) = std::env::var("GSE_AGENT_HEARTBEAT") {
        if let Ok(secs) = v.parse() {
            cfg.heartbeat_interval_secs = secs;
        }
    }
    Ok(cfg)
}
