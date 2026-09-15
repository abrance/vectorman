use serde::{Deserialize, Serialize};

/// 默认配置文件内容，供 `config.toml.example` 使用。
pub const DEFAULT_CONFIG_TOML: &str = r#"data_path = "./data"

[sql_http]
listen = "0.0.0.0:8081"

[prom_http]
listen = "0.0.0.0:9090"

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
        self.normalize();
    }

    fn normalize(&mut self) {
        self.http_web_dir = self.http_web_dir.take().and_then(nonempty_opt);
        self.gse_admin_url = self.gse_admin_url.take().and_then(nonempty_opt);
    }
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
}
