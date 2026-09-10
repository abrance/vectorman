//! 作业模板领域逻辑：占位符提取、校验与展开。
//!
//! 模板仅描述作业请求，展开发生在 Server 侧；Agent 不感知模板。
//! 复用作业约束（脚本字节数、超时范围、解释器默认 bash），保证展开结果可直接下发。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

use gse_proto::GseError;
use serde::{Deserialize, Serialize};

use crate::config::ServerConfig;
use crate::ledger::{JobTemplate, NewJobTemplate};
use crate::server::JobSubmit;

static TPL_SEQ: AtomicU64 = AtomicU64::new(0);

/// 生成模板 ID：`tpl-{unix_micros}-{seq}`，进程内序号保证同微秒内不冲突。
pub fn new_template_id() -> String {
    let seq = TPL_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("tpl-{}-{}", crate::session::now_micros(), seq)
}

/// 创建/更新模板的请求体；`interpreter`/`timeout_secs` 省略时套用服务端默认。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateInput {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub interpreter: Option<String>,
    #[serde(default)]
    pub script: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// 占位符展开后的可下发参数（不含 `agent_id`，由调用方在提交时指定）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedJob {
    pub interpreter: String,
    pub script: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub working_dir: Option<String>,
    pub timeout_secs: u64,
}

impl TemplateInput {
    /// 从模板记录构造校验/展开输入。
    pub fn from_template(t: &JobTemplate) -> Self {
        Self {
            name: t.name.clone(),
            description: t.description.clone(),
            interpreter: Some(t.interpreter.clone()),
            script: t.script.clone(),
            args: t.args.clone(),
            env: t.env.clone(),
            working_dir: t.working_dir.clone(),
            timeout_secs: Some(t.timeout_secs),
        }
    }

    /// 解析 interpreter：空则默认 `bash`（与 `submit_job` 一致）。
    pub fn resolved_interpreter(&self) -> String {
        self.interpreter
            .as_deref()
            .map(str::trim)
            .filter(|i| !i.is_empty())
            .unwrap_or("bash")
            .to_string()
    }

    /// 解析 timeout：省略则用服务端默认值。
    pub fn resolved_timeout(&self, cfg: &ServerConfig) -> u64 {
        self.timeout_secs.unwrap_or(cfg.job_default_timeout_secs)
    }

    /// 校验名称/脚本/超时与占位符合法性。
    pub fn validate(&self, cfg: &ServerConfig) -> Result<(), GseError> {
        if self.name.trim().is_empty() {
            return Err(GseError::new("invalid_argument", "name required"));
        }
        if self.script.trim().is_empty() {
            return Err(GseError::new("invalid_argument", "script required"));
        }
        if self.script.len() > cfg.job_max_script_bytes {
            return Err(GseError::new(
                "invalid_argument",
                format!("script exceeds {} bytes", cfg.job_max_script_bytes),
            ));
        }
        let timeout = self.resolved_timeout(cfg);
        if timeout == 0 || timeout > cfg.job_max_timeout_secs {
            return Err(GseError::new(
                "invalid_argument",
                format!("timeout_secs must be within 1..={}", cfg.job_max_timeout_secs),
            ));
        }
        validate_placeholders(self)?;
        Ok(())
    }

    /// 转换为台账写入结构。
    pub fn to_new_template(&self, cfg: &ServerConfig) -> NewJobTemplate {
        NewJobTemplate {
            name: self.name.trim().to_string(),
            description: self
                .description
                .clone()
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty()),
            interpreter: self.resolved_interpreter(),
            script: self.script.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            working_dir: self.working_dir.clone(),
            timeout_secs: self.resolved_timeout(cfg),
        }
    }
}

impl ExpandedJob {
    /// 拼装为 `submit_job` 的请求体。
    pub fn into_submit(self, agent_id: impl Into<String>) -> JobSubmit {
        JobSubmit {
            agent_id: agent_id.into(),
            interpreter: Some(self.interpreter),
            script: self.script,
            args: self.args,
            env: self.env,
            working_dir: self.working_dir,
            timeout_secs: Some(self.timeout_secs),
        }
    }
}

/// 扫描模板中所有占位符来源（脚本、参数、env 值、工作目录），收集变量名。
/// 出现非法名称或未闭合占位符时返回 `invalid_argument`。
pub fn extract_variables(input: &TemplateInput) -> Result<BTreeSet<String>, GseError> {
    let mut names = BTreeSet::new();
    collect_placeholders(&input.script, &mut names)?;
    for arg in &input.args {
        collect_placeholders(arg, &mut names)?;
    }
    for value in input.env.values() {
        collect_placeholders(value, &mut names)?;
    }
    if let Some(dir) = &input.working_dir {
        collect_placeholders(dir, &mut names)?;
    }
    Ok(names)
}

/// 校验所有 `${...}` 名称合法且闭合；等价于 `extract_variables` 的副作用版本。
pub fn validate_placeholders(input: &TemplateInput) -> Result<(), GseError> {
    extract_variables(input).map(|_| ())
}

/// 展开模板：替换所有声明变量，缺失取值拒绝，额外变量忽略；并复核作业约束。
pub fn expand(
    input: &TemplateInput,
    vars: &BTreeMap<String, String>,
    cfg: &ServerConfig,
) -> Result<ExpandedJob, GseError> {
    input.validate(cfg)?;

    let script = substitute(&input.script, vars)?;
    if script.len() > cfg.job_max_script_bytes {
        return Err(GseError::new(
            "invalid_argument",
            format!("script exceeds {} bytes", cfg.job_max_script_bytes),
        ));
    }
    let mut args = Vec::with_capacity(input.args.len());
    for arg in &input.args {
        args.push(substitute(arg, vars)?);
    }
    let mut env = BTreeMap::new();
    for (key, value) in &input.env {
        env.insert(key.clone(), substitute(value, vars)?);
    }
    let working_dir = match &input.working_dir {
        Some(dir) => Some(substitute(dir, vars)?),
        None => None,
    };
    let timeout = input.resolved_timeout(cfg);
    if timeout == 0 || timeout > cfg.job_max_timeout_secs {
        return Err(GseError::new(
            "invalid_argument",
            format!("timeout_secs must be within 1..={}", cfg.job_max_timeout_secs),
        ));
    }

    Ok(ExpandedJob {
        interpreter: input.resolved_interpreter(),
        script,
        args,
        env,
        working_dir,
        timeout_secs: timeout,
    })
}

/// `${name}` 名称规则：首字符为字母或下划线，其余为字母、数字或下划线。
fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn collect_placeholders(text: &str, names: &mut BTreeSet<String>) -> Result<(), GseError> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            let start = i + 2;
            let rel = text[start..].find('}').ok_or_else(|| {
                GseError::new("invalid_argument", "unterminated placeholder")
            })?;
            let name = &text[start..start + rel];
            if !is_valid_name(name) {
                return Err(GseError::new(
                    "invalid_argument",
                    format!("invalid placeholder name ${{{name}}}"),
                ));
            }
            names.insert(name.to_string());
            i = start + rel + 1;
        } else {
            i += 1;
        }
    }
    Ok(())
}

fn substitute(text: &str, vars: &BTreeMap<String, String>) -> Result<String, GseError> {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            let start = i + 2;
            let rel = text[start..].find('}').ok_or_else(|| {
                GseError::new("invalid_argument", "unterminated placeholder")
            })?;
            let name = &text[start..start + rel];
            let value = vars.get(name).ok_or_else(|| {
                GseError::new("invalid_argument", format!("missing variable {name}"))
            })?;
            out.push_str(value);
            i = start + rel + 1;
        } else {
            let next = text[i..]
                .find("${")
                .map(|rel| i + rel)
                .unwrap_or(bytes.len());
            out.push_str(&text[i..next]);
            i = next;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ServerConfig {
        ServerConfig::default()
    }

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn input(script: &str) -> TemplateInput {
        TemplateInput {
            name: "t".to_string(),
            description: None,
            interpreter: None,
            script: script.to_string(),
            args: vec![],
            env: BTreeMap::new(),
            working_dir: None,
            timeout_secs: None,
        }
    }

    #[test]
    fn extract_covers_all_sources() {
        let mut t = input("echo ${svc}");
        t.args = vec!["--tag=${tag}".to_string()];
        t.env = BTreeMap::from([
            ("A".to_string(), "${a}".to_string()),
            ("B".to_string(), "literal".to_string()),
        ]);
        t.working_dir = Some("/var/log/${svc}".to_string());
        let names = extract_variables(&t).expect("extract");
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            vec!["a".to_string(), "svc".to_string(), "tag".to_string()]
        );
    }

    #[test]
    fn extract_returns_empty_without_vars() {
        let t = input("echo hello");
        assert!(extract_variables(&t).expect("extract").is_empty());
    }

    #[test]
    fn invalid_placeholder_name_rejected() {
        let t = input("echo ${1bad}");
        let err = validate_placeholders(&t).expect_err("invalid");
        assert_eq!(err.code, "invalid_argument");
    }

    #[test]
    fn unterminated_placeholder_rejected() {
        let t = input("echo ${oops");
        let err = validate_placeholders(&t).expect_err("unterminated");
        assert_eq!(err.code, "invalid_argument");
    }

    #[test]
    fn expand_replaces_all_and_ignores_extras() {
        let mut t = input("run ${svc} in ${dir}");
        t.args = vec!["--name=${svc}".to_string()];
        t.env = BTreeMap::from([("SVC".to_string(), "${svc}".to_string())]);
        t.working_dir = Some("/srv/${dir}".to_string());
        let out = expand(
            &t,
            &vars(&[("svc", "nginx"), ("dir", "etc"), ("extra", "x")]),
            &config(),
        )
        .expect("expand");
        assert_eq!(out.script, "run nginx in etc");
        assert_eq!(out.args, vec!["--name=nginx".to_string()]);
        assert_eq!(out.env.get("SVC").map(String::as_str), Some("nginx"));
        assert_eq!(out.working_dir.as_deref(), Some("/srv/etc"));
        assert_eq!(out.interpreter, "bash");
        assert!(!out.script.contains("${"));
    }

    #[test]
    fn expand_rejects_missing_variable() {
        let t = input("run ${svc}");
        let err = expand(&t, &vars(&[]), &config()).expect_err("missing");
        assert_eq!(err.code, "invalid_argument");
    }

    #[test]
    fn expand_rejects_script_over_limit() {
        let mut t = input("echo ${x}");
        t.script = format!("echo ${{x}}{}", "a".repeat(config().job_max_script_bytes));
        let err = expand(&t, &vars(&[("x", "1")]), &config()).expect_err("too long");
        assert_eq!(err.code, "invalid_argument");
    }

    #[test]
    fn expand_rejects_timeout_over_limit() {
        let mut t = input("echo hi");
        t.timeout_secs = Some(config().job_max_timeout_secs + 1);
        let err = expand(&t, &vars(&[]), &config()).expect_err("timeout");
        assert_eq!(err.code, "invalid_argument");
    }

    #[test]
    fn validate_requires_name_and_script() {
        let mut t = input("echo hi");
        t.name = "  ".to_string();
        assert_eq!(
            t.validate(&config()).expect_err("name").code,
            "invalid_argument"
        );
        let t = input("   ");
        assert_eq!(
            t.validate(&config()).expect_err("script").code,
            "invalid_argument"
        );
    }
}
