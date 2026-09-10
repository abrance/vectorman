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
    /// 允许执行的解释器白名单。
    #[serde(default = "default_interpreters")]
    pub allowed_interpreters: Vec<String>,
    /// 未指定解释器时使用的默认解释器。
    #[serde(default = "default_job_interpreter")]
    pub job_default_interpreter: String,
    /// 单个 Agent 并发运行作业上限。
    #[serde(default = "default_max_jobs")]
    pub max_concurrent_jobs: usize,
    /// 作业临时脚本目录；为空时使用系统临时目录。
    #[serde(default)]
    pub job_work_dir: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            server_addr: default_server_addr(),
            agent_id: default_agent_id(),
            token: String::new(),
            heartbeat_interval_secs: default_interval(),
            allowed_interpreters: default_interpreters(),
            job_default_interpreter: default_job_interpreter(),
            max_concurrent_jobs: default_max_jobs(),
            job_work_dir: None,
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

fn default_interpreters() -> Vec<String> {
    vec!["bash".to_string(), "sh".to_string(), "python3".to_string()]
}

fn default_job_interpreter() -> String {
    "bash".to_string()
}

fn default_max_jobs() -> usize {
    1
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
    if let Ok(v) = std::env::var("GSE_AGENT_INTERPRETERS") {
        let list: Vec<String> = v
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !list.is_empty() {
            cfg.allowed_interpreters = list;
        }
    }
    if let Ok(v) = std::env::var("GSE_AGENT_JOB_INTERPRETER") {
        cfg.job_default_interpreter = v;
    }
    if let Ok(v) = std::env::var("GSE_AGENT_MAX_JOBS") {
        if let Ok(n) = v.parse() {
            cfg.max_concurrent_jobs = n;
        }
    }
    if let Ok(v) = std::env::var("GSE_AGENT_JOB_WORK_DIR") {
        cfg.job_work_dir = if v.is_empty() { None } else { Some(v) };
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
            "GSE_AGENT_SERVER",
            "GSE_AGENT_ID",
            "GSE_AGENT_TOKEN",
            "GSE_AGENT_HEARTBEAT",
            "GSE_AGENT_INTERPRETERS",
            "GSE_AGENT_JOB_INTERPRETER",
            "GSE_AGENT_MAX_JOBS",
            "GSE_AGENT_JOB_WORK_DIR",
        ] {
            std::env::remove_var(key);
        }
        let r = body();
        for key in [
            "GSE_AGENT_SERVER",
            "GSE_AGENT_ID",
            "GSE_AGENT_TOKEN",
            "GSE_AGENT_HEARTBEAT",
            "GSE_AGENT_INTERPRETERS",
            "GSE_AGENT_JOB_INTERPRETER",
            "GSE_AGENT_MAX_JOBS",
            "GSE_AGENT_JOB_WORK_DIR",
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
            let dir = std::env::temp_dir().join(format!("gse-agent-cfg-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = write_tmp(
                &dir,
                "full.toml",
                r#"
server_addr = "10.0.0.2:7100"
agent_id = "web-02"
token = "tok-b"
heartbeat_interval_secs = 10
"#,
            );
            let cfg = load_config(&path).expect("parse full toml");
            assert_eq!(cfg.server_addr, "10.0.0.2:7100");
            assert_eq!(cfg.agent_id, "web-02");
            assert_eq!(cfg.token, "tok-b");
            assert_eq!(cfg.heartbeat_interval_secs, 10);
        });
    }

    #[test]
    fn load_absent_fields_fall_back_to_defaults() {
        with_env_clean(|| {
            let dir = std::env::temp_dir().join(format!("gse-agent-cfg-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = write_tmp(&dir, "minimal.toml", "# empty config\n");
            let cfg = load_config(&path).expect("parse minimal toml");
            assert_eq!(cfg.server_addr, "127.0.0.1:7100");
            assert_eq!(cfg.agent_id, "agent-1");
            assert!(cfg.token.is_empty());
            assert_eq!(cfg.heartbeat_interval_secs, 30);
            assert_eq!(
                cfg.allowed_interpreters,
                vec!["bash".to_string(), "sh".to_string(), "python3".to_string()]
            );
            assert_eq!(cfg.job_default_interpreter, "bash");
            assert_eq!(cfg.max_concurrent_jobs, 1);
            assert!(cfg.job_work_dir.is_none());
        });
    }

    #[test]
    fn env_overrides_all_fields() {
        with_env_clean(|| {
            let dir = std::env::temp_dir().join(format!("gse-agent-cfg-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = write_tmp(&dir, "env.toml", "# empty config\n");
            std::env::set_var("GSE_AGENT_SERVER", "10.1.0.9:7100");
            std::env::set_var("GSE_AGENT_ID", "env-agent");
            std::env::set_var("GSE_AGENT_TOKEN", "env-tok");
            std::env::set_var("GSE_AGENT_HEARTBEAT", "7");
            std::env::set_var("GSE_AGENT_INTERPRETERS", "bash, python3");
            std::env::set_var("GSE_AGENT_JOB_INTERPRETER", "python3");
            std::env::set_var("GSE_AGENT_MAX_JOBS", "4");
            std::env::set_var("GSE_AGENT_JOB_WORK_DIR", "/srv/jobs");
            let cfg = load_config(&path).expect("parse");
            assert_eq!(cfg.server_addr, "10.1.0.9:7100");
            assert_eq!(cfg.agent_id, "env-agent");
            assert_eq!(cfg.token, "env-tok");
            assert_eq!(cfg.heartbeat_interval_secs, 7);
            assert_eq!(
                cfg.allowed_interpreters,
                vec!["bash".to_string(), "python3".to_string()]
            );
            assert_eq!(cfg.job_default_interpreter, "python3");
            assert_eq!(cfg.max_concurrent_jobs, 4);
            assert_eq!(cfg.job_work_dir.as_deref(), Some("/srv/jobs"));
        });
    }

    #[test]
    fn malformed_toml_returns_error() {
        with_env_clean(|| {
            let dir = std::env::temp_dir().join(format!("gse-agent-cfg-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = write_tmp(&dir, "bad.toml", "server_addr = [\n");
            let err = load_config(&path).expect_err("should fail");
            assert!(err.contains("parse"), "{err}");
            assert!(err.contains(&path), "{err}");
        });
    }

    #[test]
    fn missing_file_returns_error() {
        with_env_clean(|| {
            let err = load_config("/nonexistent/gse-agent.toml").expect_err("should fail");
            assert!(err.contains("cannot read"), "{err}");
        });
    }
}
