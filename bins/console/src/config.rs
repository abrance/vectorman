use std::path::Path;

/// Console 服务配置：TOML + `CONSOLE_` 环境变量覆盖。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ConsoleConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_web_dir")]
    pub web_dir: String,
    #[serde(default = "default_data_file")]
    pub data_file: String,
}

impl Default for ConsoleConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            web_dir: default_web_dir(),
            data_file: default_data_file(),
        }
    }
}

fn default_listen() -> String {
    "0.0.0.0:7200".to_string()
}

fn default_web_dir() -> String {
    "web".to_string()
}

fn default_data_file() -> String {
    "apps.json".to_string()
}

/// 文件存在则解析；缺失则用默认值。解析失败返回说明。
pub fn load_config(path: &str) -> Result<ConsoleConfig, String> {
    let mut cfg = if Path::new(path).is_file() {
        let raw = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        toml::from_str(&raw).map_err(|e| format!("parse {path}: {e}"))?
    } else {
        ConsoleConfig::default()
    };
    if let Ok(v) = std::env::var("CONSOLE_LISTEN") {
        cfg.listen = v;
    }
    if let Ok(v) = std::env::var("CONSOLE_WEB_DIR") {
        cfg.web_dir = v;
    }
    if let Ok(v) = std::env::var("CONSOLE_DATA_FILE") {
        cfg.data_file = v;
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_uses_defaults() {
        std::env::remove_var("CONSOLE_LISTEN");
        std::env::remove_var("CONSOLE_WEB_DIR");
        std::env::remove_var("CONSOLE_DATA_FILE");
        let cfg = load_config("/tmp/console-missing-config-does-not-exist.toml").expect("load");
        assert_eq!(cfg.listen, "0.0.0.0:7200");
        assert_eq!(cfg.web_dir, "web");
        assert_eq!(cfg.data_file, "apps.json");
    }

    #[test]
    fn invalid_toml_is_config_invalid() {
        let dir = std::env::temp_dir().join(format!("console-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.toml");
        std::fs::write(&path, "listen = {\n").unwrap();
        let err = load_config(path.to_str().unwrap()).expect_err("invalid");
        assert!(err.contains("parse"), "{err}");
    }
}
