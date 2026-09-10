//! 历史作业重做：把来源作业与重做覆盖请求合并为可提交的作业请求。
//!
//! 重做只读取来源作业记录，不修改来源作业；未提供覆盖的字段继承来源作业取值。
//! 合并结果交由 `submit_job_with_source` 复用作业提交的校验与下发语义。

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::ledger::JobRecord;
use crate::server::JobSubmit;

/// 重做请求体；字段全部可选，省略表示继承来源作业。
///
/// `working_dir` 传入空字符串表示清空；`args`/`env` 传入空集合表示清空。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct RerunRequest {
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub interpreter: Option<String>,
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// 以来源作业为默认值合并重做覆盖，产出下游提交请求。
pub fn build_rerun_submit(source: &JobRecord, req: RerunRequest) -> JobSubmit {
    let agent_id = req
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| source.agent_id.clone());

    let working_dir = match req.working_dir {
        Some(dir) if dir.trim().is_empty() => None,
        Some(dir) => Some(dir),
        None => source.working_dir.clone(),
    };

    JobSubmit {
        agent_id,
        interpreter: Some(req.interpreter.unwrap_or_else(|| source.interpreter.clone())),
        script: req.script.unwrap_or_else(|| source.script.clone()),
        args: req.args.unwrap_or_else(|| source.args.clone()),
        env: req.env.unwrap_or_else(|| source.env.clone()),
        working_dir,
        timeout_secs: Some(req.timeout_secs.unwrap_or(source.timeout_secs)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gse_proto::JobStatus;

    fn source() -> JobRecord {
        JobRecord {
            job_id: "job-src".to_string(),
            agent_id: "agent-a".to_string(),
            interpreter: "bash".to_string(),
            script: "echo hi".to_string(),
            args: vec!["-e".to_string()],
            env: BTreeMap::from([("LANG".to_string(), "C".to_string())]),
            working_dir: Some("/tmp".to_string()),
            template_id: Some("tpl-1".to_string()),
            rerun_of: None,
            timeout_secs: 300,
            status: JobStatus::Succeeded,
            exit_code: Some(0),
            signal: None,
            stdout: None,
            stdout_truncated: false,
            stderr: None,
            stderr_truncated: false,
            error: None,
            created_at: "1".to_string(),
            dispatched_at: None,
            started_at: None,
            finished_at: None,
            updated_at: "1".to_string(),
        }
    }

    #[test]
    fn empty_request_inherits_all() {
        let submit = build_rerun_submit(&source(), RerunRequest::default());
        assert_eq!(submit.agent_id, "agent-a");
        assert_eq!(submit.interpreter.as_deref(), Some("bash"));
        assert_eq!(submit.script, "echo hi");
        assert_eq!(submit.args, vec!["-e".to_string()]);
        assert_eq!(submit.env.get("LANG").map(String::as_str), Some("C"));
        assert_eq!(submit.working_dir.as_deref(), Some("/tmp"));
        assert_eq!(submit.timeout_secs, Some(300));
    }

    #[test]
    fn overrides_replace_fields() {
        let req = RerunRequest {
            agent_id: Some(" agent-b ".to_string()),
            interpreter: Some("python3".to_string()),
            script: Some("print(1)".to_string()),
            args: Some(vec!["x".to_string()]),
            env: Some(BTreeMap::from([("MODE".to_string(), "prod".to_string())])),
            working_dir: Some("/srv".to_string()),
            timeout_secs: Some(60),
        };
        let submit = build_rerun_submit(&source(), req);
        assert_eq!(submit.agent_id, "agent-b");
        assert_eq!(submit.interpreter.as_deref(), Some("python3"));
        assert_eq!(submit.script, "print(1)");
        assert_eq!(submit.args, vec!["x".to_string()]);
        assert_eq!(submit.env.get("MODE").map(String::as_str), Some("prod"));
        assert_eq!(submit.working_dir.as_deref(), Some("/srv"));
        assert_eq!(submit.timeout_secs, Some(60));
    }

    #[test]
    fn empty_agent_falls_back_to_source() {
        let req = RerunRequest {
            agent_id: Some("   ".to_string()),
            ..RerunRequest::default()
        };
        assert_eq!(build_rerun_submit(&source(), req).agent_id, "agent-a");
    }

    #[test]
    fn explicit_empty_clears_optional_fields() {
        let req = RerunRequest {
            args: Some(vec![]),
            env: Some(BTreeMap::new()),
            working_dir: Some("   ".to_string()),
            ..RerunRequest::default()
        };
        let submit = build_rerun_submit(&source(), req);
        assert!(submit.args.is_empty());
        assert!(submit.env.is_empty());
        assert_eq!(submit.working_dir, None);
    }
}
