use serde::{Deserialize, Serialize};

/// 默认配置文件内容，供 `config.toml.example` 使用。
pub const DEFAULT_CONFIG_TOML: &str = r#"data_path = "./data"

[sql_http]
listen = "0.0.0.0:8081"

[prom_http]
listen = "0.0.0.0:9090"

[auth]
enabled = false
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
        }
    }
}

impl Config {
    /// 从 TOML 字符串解析配置；失败返回 `config_invalid`。
    pub fn from_toml(s: &str) -> Result<Config, crate::DataplaneError> {
        let cfg: Config = toml::from_str(s)
            .map_err(|e| crate::DataplaneError::config_invalid(format!("invalid TOML: {e}")))?;
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
