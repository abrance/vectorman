//! GSE 共享数据模型（DTO）：认证、心跳、信令与回执类型。

use std::collections::BTreeMap;

use bytes::Bytes;
use dataplane_core::{DataplaneError, ErrorCode};
use serde::{Deserialize, Serialize};

/// Agent 向 Server 发起的认证请求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthRequest {
    pub agent_id: String,
    pub token: String,
}

/// Server 对认证请求的应答。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthReply {
    pub ok: bool,
    pub reason: Option<String>,
}

/// Agent 周期性上报的心跳。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub agent_id: String,
    pub ts_micros: i64,
}

/// Server 下发给 Agent 的指令。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Command {
    pub id: String,
    pub name: String,
    pub payload: Bytes,
}

/// Agent 对指令的执行回执。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub command_id: String,
    pub ok: bool,
    pub message: Option<String>,
}

/// 作业生命周期状态，serde 使用 snake_case 字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Dispatched,
    Running,
    Succeeded,
    Failed,
    Timeout,
    Rejected,
    Lost,
}

impl JobStatus {
    /// 是否处于终态；终态作业的结果字段不再变更。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Timeout | Self::Rejected | Self::Lost
        )
    }

    /// 稳定字符串，用于持久化与 JSON 载荷。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Dispatched => "dispatched",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::Rejected => "rejected",
            Self::Lost => "lost",
        }
    }

    /// 从稳定字符串解析；未知取值返回 None。
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "dispatched" => Self::Dispatched,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "timeout" => Self::Timeout,
            "rejected" => Self::Rejected,
            "lost" => Self::Lost,
            _ => return None,
        })
    }
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Server → Agent：一次作业下发。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobExec {
    pub job_id: String,
    pub interpreter: String,
    pub script: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    pub timeout_secs: u64,
    pub stdout_limit_bytes: u64,
    pub stderr_limit_bytes: u64,
}

/// Agent → Server：作业受理应答（`job_exec` 的返回值）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobAck {
    pub job_id: String,
    pub accepted: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Agent → Server：作业执行终态回传。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobResult {
    pub job_id: String,
    pub status: JobStatus,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub signal: Option<i32>,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stdout_truncated: bool,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub stderr_truncated: bool,
    #[serde(default)]
    pub started_at_micros: i64,
    #[serde(default)]
    pub finished_at_micros: i64,
    #[serde(default)]
    pub error: Option<String>,
}

/// 跨 RPC 传输的错误载荷，code 为稳定字符串。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GseError {
    pub code: String,
    pub message: String,
}

impl GseError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn from_error(e: &DataplaneError) -> Self {
        Self {
            code: e.code.as_str().to_string(),
            message: e.message.clone(),
        }
    }
}

impl From<DataplaneError> for GseError {
    fn from(e: DataplaneError) -> Self {
        Self {
            code: e.code.as_str().to_string(),
            message: e.message,
        }
    }
}

fn error_code_from_str(s: &str) -> ErrorCode {
    match s {
        "not_found" => ErrorCode::NotFound,
        "invalid_argument" => ErrorCode::InvalidArgument,
        "query_failed" => ErrorCode::QueryFailed,
        "unimplemented" => ErrorCode::Unimplemented,
        "engine_init_failed" => ErrorCode::EngineInitFailed,
        "config_invalid" => ErrorCode::ConfigInvalid,
        _ => ErrorCode::Unavailable,
    }
}

impl From<GseError> for DataplaneError {
    fn from(e: GseError) -> Self {
        DataplaneError {
            code: error_code_from_str(&e.code),
            message: e.message,
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use dataplane_core::{DataplaneError, ErrorCode};

    use super::*;

    fn roundtrip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de> + std::fmt::Debug + PartialEq,
    {
        let bytes = serde_json::to_vec(value).expect("serialize");
        let back: T = serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(&back, value);
        back
    }

    #[test]
    fn auth_request_roundtrip() {
        roundtrip(&AuthRequest {
            agent_id: "web-01".to_string(),
            token: "tok-1".to_string(),
        });
    }

    #[test]
    fn auth_reply_roundtrip_ok() {
        roundtrip(&AuthReply {
            ok: true,
            reason: None,
        });
    }

    #[test]
    fn auth_reply_roundtrip_rejected() {
        roundtrip(&AuthReply {
            ok: false,
            reason: Some("invalid agent_id or token".to_string()),
        });
    }

    #[test]
    fn heartbeat_roundtrip() {
        roundtrip(&Heartbeat {
            agent_id: "web-01".to_string(),
            ts_micros: 1_700_000_000_000_000,
        });
    }

    #[test]
    fn command_roundtrip_preserves_payload() {
        let cmd = Command {
            id: "42".to_string(),
            name: "ping".to_string(),
            payload: Bytes::from_static(b"\x00\x01\x02"),
        };
        let back = roundtrip(&cmd);
        assert_eq!(back.payload.as_ref(), &[0u8, 1, 2]);
    }

    #[test]
    fn command_roundtrip_empty_payload() {
        let cmd = Command {
            id: "7".to_string(),
            name: "noop".to_string(),
            payload: Bytes::new(),
        };
        let back = roundtrip(&cmd);
        assert!(back.payload.is_empty());
    }

    #[test]
    fn receipt_roundtrip() {
        roundtrip(&Receipt {
            command_id: "42".to_string(),
            ok: true,
            message: Some("pong".to_string()),
        });
    }

    #[test]
    fn job_status_strings_roundtrip() {
        let all = [
            (JobStatus::Pending, "pending"),
            (JobStatus::Dispatched, "dispatched"),
            (JobStatus::Running, "running"),
            (JobStatus::Succeeded, "succeeded"),
            (JobStatus::Failed, "failed"),
            (JobStatus::Timeout, "timeout"),
            (JobStatus::Rejected, "rejected"),
            (JobStatus::Lost, "lost"),
        ];
        for (status, text) in all {
            assert_eq!(status.as_str(), text);
            assert_eq!(JobStatus::parse(text), Some(status));
            assert_eq!(roundtrip(&status), status);
        }
        assert_eq!(JobStatus::parse("nonsense"), None);
    }

    #[test]
    fn job_status_terminal_classification() {
        assert!(!JobStatus::Pending.is_terminal());
        assert!(!JobStatus::Dispatched.is_terminal());
        assert!(!JobStatus::Running.is_terminal());
        assert!(JobStatus::Succeeded.is_terminal());
        assert!(JobStatus::Failed.is_terminal());
        assert!(JobStatus::Timeout.is_terminal());
        assert!(JobStatus::Rejected.is_terminal());
        assert!(JobStatus::Lost.is_terminal());
    }

    #[test]
    fn job_exec_roundtrip() {
        let mut env = BTreeMap::new();
        env.insert("LANG".to_string(), "C".to_string());
        roundtrip(&JobExec {
            job_id: "job-1".to_string(),
            interpreter: "bash".to_string(),
            script: "echo hello".to_string(),
            args: vec!["-e".to_string()],
            env,
            working_dir: Some("/tmp".to_string()),
            timeout_secs: 300,
            stdout_limit_bytes: 1_048_576,
            stderr_limit_bytes: 1_048_576,
        });
    }

    #[test]
    fn job_ack_roundtrip() {
        roundtrip(&JobAck {
            job_id: "job-1".to_string(),
            accepted: false,
            reason: Some("busy".to_string()),
        });
    }

    #[test]
    fn job_result_roundtrip() {
        roundtrip(&JobResult {
            job_id: "job-1".to_string(),
            status: JobStatus::Timeout,
            exit_code: None,
            signal: Some(9),
            stdout: "partial".to_string(),
            stdout_truncated: true,
            stderr: String::new(),
            stderr_truncated: false,
            started_at_micros: 1_700_000_000_000_000,
            finished_at_micros: 1_700_000_001_000_000,
            error: Some("timeout".to_string()),
        });
    }

    #[test]
    fn gse_error_roundtrip() {
        roundtrip(&GseError::new("unavailable", "agent offline"));
    }

    #[test]
    fn gse_error_exposes_stable_code() {
        let e = GseError::new("unavailable", "agent offline");
        assert_eq!(e.code, "unavailable");
    }

    #[test]
    fn dataplane_error_to_gse_error_maps_code() {
        let src = DataplaneError {
            code: ErrorCode::NotFound,
            message: "session absent".to_string(),
        };
        let gse: GseError = src.into();
        assert_eq!(gse.code, "not_found");
        assert_eq!(gse.message, "session absent");

        let unk = DataplaneError {
            code: ErrorCode::Unavailable,
            message: "x".to_string(),
        };
        let gse_unk: GseError = unk.into();
        assert_eq!(gse_unk.code, "unavailable");
    }

    #[test]
    fn gse_error_to_dataplane_error_maps_code() {
        let gse = GseError::new("config_invalid", "bad toml");
        let dp: DataplaneError = gse.into();
        assert_eq!(dp.code, ErrorCode::ConfigInvalid);
        assert_eq!(dp.message, "bad toml");

        let unknown = GseError::new("nonsense_code", "z");
        let dp_unknown: DataplaneError = unknown.into();
        assert_eq!(dp_unknown.code, ErrorCode::Unavailable);
    }
}
