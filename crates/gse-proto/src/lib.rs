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
    /// 尚未上报的升级结果（有则带一次，server 收到后 agent 标记已上报）。
    ///
    /// 为什么走心跳：升级过程中 agent 会被重启，作业结果通道那时已断，
    /// 结果只能由新启动的 agent 在后续心跳里补报。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgrade_result: Option<UpgradeReport>,
    /// 生效 spec 的**一次性补报**：连接后的首拍、以及每次应用 spec 后的下一拍各带一次，
    /// 服务端确认（`HeartbeatReply::spec_synced`）后由 agent 清除。
    ///
    /// 为什么要有它：下发回执只在「下发那一刻」到达，若 agent 在服务端落库前掉线、
    /// 或 agent 侧被人工改回，服务端就永久停在旧 revision。心跳补报是漂移检测的兜底。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<AgentSpecAck>,
}

/// 升级结果的上行表示（与 agent 侧 `UpgradeResult` 字段对应）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpgradeReport {
    pub started_at: String,
    pub finished_at: String,
    pub from_version: String,
    pub to_version: String,
    pub from_sha256: String,
    pub to_sha256: String,
    /// `succeeded` / `rolled_back` / `failed`。
    pub outcome: String,
    #[serde(default)]
    pub detail: String,
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
    /// 作业类型：`script`（默认，执行 `script` 字段）或 `agent_upgrade`
    /// （自更新：`script` 字段携带 `AgentUpgradeSpec` 的 JSON）。
    /// 缺省为 `script`，使旧 server 发的报文在旧/新 agent 上都按原语义执行。
    #[serde(default = "default_job_kind")]
    pub kind: String,
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

/// `JobExec::kind` 的默认值（向后兼容：无 kind 即普通脚本作业）。
pub fn default_job_kind() -> String {
    "script".to_string()
}

/// `agent_upgrade` 作业的载荷（放在 `JobExec.script` 里，JSON）。
///
/// 二进制由调用方先前用 `file_transfer` 落到 `binary_path`；
/// 本结构只描述「要换成哪个二进制」以及校验值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentUpgradeSpec {
    /// 已在目标机上、准备启用的新二进制绝对路径。
    pub binary_path: String,
    /// 期望的 sha256（不匹配则拒绝执行 —— 防半截传输或投毒）。
    pub sha256: String,
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

/// 文件传输端点：Agent 本机路径或 Server 临时文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileEndpoint {
    Agent {
        agent_id: String,
        path: String,
    },
    ServerTemp {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_id: Option<String>,
    },
}

impl FileEndpoint {
    pub fn agent_id(&self) -> Option<&str> {
        match self {
            Self::Agent { agent_id, .. } => Some(agent_id.as_str()),
            Self::ServerTemp { .. } => None,
        }
    }

    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Agent { path, .. } => Some(path.as_str()),
            Self::ServerTemp { .. } => None,
        }
    }

    pub fn file_id(&self) -> Option<&str> {
        match self {
            Self::ServerTemp { file_id } => file_id.as_deref().filter(|s| !s.is_empty()),
            Self::Agent { .. } => None,
        }
    }
}

/// Server → Agent：按偏移读取普通文件一块。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileReadReq {
    pub job_id: String,
    pub path: String,
    pub offset: u64,
    pub length: u64,
    #[serde(default)]
    pub max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileReadReply {
    pub job_id: String,
    pub size: u64,
    pub offset: u64,
    pub eof: bool,
    #[serde(default)]
    pub data_b64: String,
    #[serde(default)]
    pub chunk_sha256: String,
    /// 仅 eof=true 时填写整文件 SHA-256。
    #[serde(default)]
    pub file_sha256: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Server → Agent：按偏移写入；eof=true 且最终路径不存在时 rename 就位。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileWriteReq {
    pub job_id: String,
    pub path: String,
    pub offset: u64,
    pub eof: bool,
    #[serde(default)]
    pub data_b64: String,
    #[serde(default)]
    pub chunk_sha256: String,
    /// eof=true 时由 Server 带上源文件 SHA-256，Agent 比对。
    #[serde(default)]
    pub file_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileWriteReply {
    pub job_id: String,
    pub written: u64,
    pub eof: bool,
    #[serde(default)]
    pub file_sha256: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// 跨 RPC 传输的错误载荷，code 为稳定字符串。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GseError {
    pub code: String,
    pub message: String,
}

/// Agent → Server：拉取本 Agent 应写入的数据面地址。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataplaneAddrRequest {
    pub agent_id: String,
}

/// Server → Agent：数据面接入地址与主机归属。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataplaneAddrReply {
    pub ok: bool,
    #[serde(default)]
    pub ingest_url: Option<String>,
    #[serde(default)]
    pub host_id: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// 单个采集项：GSE 按 `agent_ids` 过滤后下发，Agent 按 `item_id` 对齐采集器。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectItem {
    pub item_id: String,
    pub agent_ids: Vec<String>,
    pub name: String,
    /// metrics_host | log_file | log_k8s_stdout | apm_otlp。
    pub kind: String,
    pub enabled: bool,
    pub collector: serde_json::Value,
    pub storage: serde_json::Value,
}

/// Server → Agent / Agent 拉取：过滤后的采集项整表（可空）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CollectItemsReply {
    #[serde(default)]
    pub items: Vec<CollectItem>,
}

/// 心跳应答：告诉 Agent 心跳里那份 spec 补报是否已被服务端落库。
///
/// `false` 时 Agent 下一拍继续携带（上限 N 次），否则「发一次丢了」会让服务端
/// 永久停在旧 revision（`sync_status = stale`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HeartbeatReply {
    #[serde(default)]
    pub spec_synced: bool,
}

// ---------- per-Agent spec ----------

/// 本期**没有真实实现**、只记录与上报的 `params` 字段名。
/// Agent 用它构造 `AgentSpecAck::not_enforced`，服务端与前端据此标注「未实现（仅记录）」。
/// 升级路径：CPU/内存限制落到作业子进程 `setrlimit`，`log_level` 需要先给 Agent 一个日志级别门控。
pub const NOT_ENFORCED_FIELDS: [&str; 3] =
    ["cpu_limit_percent", "mem_limit_percent", "log_level"];

/// `AgentSpecAck::outcome` 的取值。两侧与测试共用这些常量，避免字符串写错。
pub mod spec_outcome {
    /// 字段全部生效（`not_enforced` 为空）。
    pub const APPLIED: &str = "applied";
    /// revision 与已应用值相同，什么都没做。
    pub const UNCHANGED: &str = "unchanged";
    /// 生效，但存在 `not_enforced` 字段。
    pub const PARTIAL: &str = "partial";
    /// 拒绝（含不可变字段变更）。
    pub const REJECTED: &str = "rejected";
}

/// Agent 运行参数。
///
/// 下行为期望值（敏感字段为 `Some`），上行为生效值（敏感字段恒为 `None`，不回声凭据）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpecParams {
    #[serde(default)]
    pub heartbeat_interval_secs: u64,
    #[serde(default)]
    pub allowed_interpreters: Vec<String>,
    #[serde(default)]
    pub job_default_interpreter: String,
    #[serde(default)]
    pub max_concurrent_jobs: usize,
    #[serde(default)]
    pub job_work_dir: Option<String>,
    #[serde(default)]
    pub otlp_enabled: bool,
    #[serde(default)]
    pub otlp_listen: String,
    #[serde(default)]
    pub otlp_max_body_bytes: usize,
    /// 敏感：仅下行为 `Some`，Agent 回执恒为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_token: Option<String>,
    #[serde(default)]
    pub otlp_allowed_cidrs: Vec<String>,
    /// 敏感：仅下行为 `Some`，Agent 回执恒为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// 本期仅记录（见 `NOT_ENFORCED_FIELDS`）。
    #[serde(default)]
    pub cpu_limit_percent: Option<i64>,
    /// 本期仅记录。
    #[serde(default)]
    pub mem_limit_percent: Option<i64>,
    /// 本期仅记录。
    #[serde(default)]
    pub log_level: String,
}

/// 手写 `Default`：**不能是 `String::default()` / `0` 那一套**。
///
/// 理由具体：`heartbeat_interval_secs = 0` 会同时被本项目的下发校验（> 0）判非法，
/// 也会让 Agent 退化成零间隔心跳；`allowed_interpreters` 为空会让所有作业被拒；
/// `otlp_listen` 为空会让 OTLP 采集器绑不上。缺省值必须与
/// `crates/gse-agent-core/src/config.rs` 的 `AgentConfig::default()` 一致。
impl Default for SpecParams {
    fn default() -> Self {
        Self {
            heartbeat_interval_secs: 30,
            allowed_interpreters: vec!["bash".to_string(), "sh".to_string(), "python3".to_string()],
            job_default_interpreter: "bash".to_string(),
            max_concurrent_jobs: 1,
            job_work_dir: None,
            otlp_enabled: false,
            otlp_listen: "0.0.0.0:4318".to_string(),
            otlp_max_body_bytes: 8 * 1024 * 1024,
            otlp_token: None,
            otlp_allowed_cidrs: Vec::new(),
            token: None,
            cpu_limit_percent: None,
            mem_limit_percent: None,
            log_level: "info".to_string(),
        }
    }
}

/// 一条采集项。归属由所在的 spec 决定，**没有 `agent_ids`**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpecItem {
    pub item_id: String,
    pub name: String,
    /// metrics_host | log_file | log_k8s_stdout | apm_otlp | ebpf_network | ebpf_tcp | ebpf_process | ebpf_syscall。
    pub kind: String,
    pub enabled: bool,
    pub collector: serde_json::Value,
    pub storage: serde_json::Value,
}

/// 一台 Agent 的完整期望状态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AgentSpecWire {
    #[serde(default)]
    pub params: SpecParams,
    #[serde(default)]
    pub items: Vec<SpecItem>,
}

/// Server → Agent（推送与拉取应答共用）。`revision` 为空表示无期望 spec。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AgentSpecPush {
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub spec: Option<AgentSpecWire>,
}

/// Agent → Server 的应用回执。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSpecAck {
    pub revision: String,
    /// 取值见 [`spec_outcome`]。
    pub outcome: String,
    pub applied: AgentSpecWire,
    /// 收到但未真实实现的字段名，见 [`NOT_ENFORCED_FIELDS`]。
    #[serde(default)]
    pub not_enforced: Vec<String>,
    #[serde(default)]
    pub detail: String,
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
    fn dataplane_addr_roundtrip() {
        roundtrip(&DataplaneAddrRequest {
            agent_id: "web-01".to_string(),
        });
        roundtrip(&DataplaneAddrReply {
            ok: true,
            ingest_url: Some("http://10.0.0.5:8081".to_string()),
            host_id: Some("h-1".to_string()),
            reason: None,
        });
        roundtrip(&DataplaneAddrReply {
            ok: false,
            ingest_url: None,
            host_id: None,
            reason: Some("no online dataplane".to_string()),
        });
    }

    #[test]
    fn collect_items_roundtrip() {
        let item = CollectItem {
            item_id: "item-1".to_string(),
            agent_ids: vec!["a-1".to_string(), "a-2".to_string()],
            name: "cpu".to_string(),
            kind: "metrics_host".to_string(),
            enabled: true,
            collector: serde_json::json!({"interval_secs": 15}),
            storage: serde_json::json!({"retention_days": 1}),
        };
        roundtrip(&item);
        roundtrip(&CollectItemsReply { items: vec![item] });
        roundtrip(&CollectItemsReply::default());
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
            upgrade_result: None,
            spec: None,
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
            kind: "script".to_string(),
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

    #[test]
    fn file_endpoint_roundtrip() {
        roundtrip(&FileEndpoint::Agent {
            agent_id: "web-01".to_string(),
            path: "/var/log/app.log".to_string(),
        });
        roundtrip(&FileEndpoint::ServerTemp {
            file_id: Some("file-1".to_string()),
        });
        roundtrip(&FileEndpoint::ServerTemp { file_id: None });
    }

    #[test]
    fn file_read_write_roundtrip() {
        roundtrip(&FileReadReq {
            job_id: "job-1".to_string(),
            path: "/tmp/a".to_string(),
            offset: 0,
            length: 1024,
            max_bytes: 64,
        });
        roundtrip(&FileReadReply {
            job_id: "job-1".to_string(),
            size: 4,
            offset: 0,
            eof: true,
            data_b64: "YWJjZA==".to_string(),
            chunk_sha256: "ab".to_string(),
            file_sha256: Some("cd".to_string()),
            error: None,
        });
        roundtrip(&FileWriteReq {
            job_id: "job-1".to_string(),
            path: "/tmp/b".to_string(),
            offset: 0,
            eof: true,
            data_b64: "YWJjZA==".to_string(),
            chunk_sha256: "ab".to_string(),
            file_sha256: Some("cd".to_string()),
        });
        roundtrip(&FileWriteReply {
            job_id: "job-1".to_string(),
            written: 4,
            eof: true,
            file_sha256: Some("cd".to_string()),
            error: None,
        });
    }

    // ---- Agent spec ----

    #[test]
    fn spec_params_secrets_are_omitted_when_none() {
        // Agent 回执里的生效值不回声凭据：None 时 JSON 里必须**没有**这两个 key，
        // 否则「没值」与「值为空串」在两端分不清。
        let params = SpecParams {
            heartbeat_interval_secs: 30,
            token: None,
            otlp_token: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&params).expect("encode");
        assert!(!json.contains("\"token\""), "{json}");
        assert!(!json.contains("otlp_token"), "{json}");
        // 下行可以带凭据。
        let with_secret = SpecParams {
            token: Some("t".into()),
            otlp_token: Some("o".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&with_secret).expect("encode");
        assert!(json.contains("\"token\":\"t\""), "{json}");
        roundtrip(&with_secret);
        roundtrip(&params);
    }

    #[test]
    fn agent_spec_wire_roundtrips_and_is_stable() {
        let wire = AgentSpecWire {
            params: SpecParams {
                heartbeat_interval_secs: 15,
                allowed_interpreters: vec!["bash".into()],
                job_default_interpreter: "bash".into(),
                max_concurrent_jobs: 2,
                job_work_dir: None,
                otlp_enabled: true,
                otlp_listen: "0.0.0.0:4318".into(),
                otlp_max_body_bytes: 1024,
                otlp_token: None,
                otlp_allowed_cidrs: vec![],
                token: None,
                cpu_limit_percent: Some(50),
                mem_limit_percent: None,
                log_level: "info".into(),
            },
            items: vec![SpecItem {
                item_id: "i1".into(),
                name: "app log".into(),
                kind: "log_file".into(),
                enabled: true,
                collector: serde_json::json!({"path_patterns": ["/var/log/*.log"]}),
                storage: serde_json::json!({"retention_days": 7}),
            }],
        };
        roundtrip(&wire);
        // revision 是 spec JSON 的哈希 —— 同一份 wire 两次编码必须逐字节相同。
        assert_eq!(
            serde_json::to_string(&wire).expect("encode"),
            serde_json::to_string(&wire).expect("encode")
        );
        // collector / storage 是自由 JSON：键的插入顺序不得影响编码结果
        // （serde_json 默认 Map 是 BTreeMap，按键排序），否则 revision 会飘。
        let a = serde_json::json!({"b": 1, "a": 2});
        let b = serde_json::json!({"a": 2, "b": 1});
        assert_eq!(
            serde_json::to_string(&a).expect("encode"),
            serde_json::to_string(&b).expect("encode")
        );
    }

    #[test]
    fn empty_spec_push_means_no_desired_spec() {
        let push = AgentSpecPush::default();
        assert!(push.revision.is_empty());
        assert!(push.spec.is_none());
        let back = roundtrip(&push);
        assert_eq!(back.revision, "");
        // 空 revision 的语义是「无期望 spec，Agent 保持本地基线」。
        assert!(back.spec.is_none());
    }

    #[test]
    fn agent_spec_ack_roundtrips() {
        let ack = AgentSpecAck {
            revision: "ab12".into(),
            outcome: spec_outcome::PARTIAL.into(),
            applied: AgentSpecWire::default(),
            not_enforced: NOT_ENFORCED_FIELDS.iter().map(|s| s.to_string()).collect(),
            detail: String::new(),
        };
        let back = roundtrip(&ack);
        assert_eq!(back.outcome, "partial");
        assert_eq!(back.not_enforced.len(), 3);
    }

    #[test]
    fn heartbeat_spec_field_is_optional_on_the_wire() {
        let hb = Heartbeat {
            agent_id: "a-1".into(),
            ts_micros: 1,
            upgrade_result: None,
            spec: None,
        };
        let json = serde_json::to_string(&hb).expect("encode");
        assert!(!json.contains("\"spec\""), "{json}");
        roundtrip(&hb);
        // 旧 agent 发的报文没有 spec 字段 —— 必须能解出来（向后兼容）。
        let legacy: Heartbeat = serde_json::from_str(
            r#"{"agent_id":"a-1","ts_micros":7}"#,
        )
        .expect("decode legacy");
        assert!(legacy.spec.is_none());
        assert!(legacy.upgrade_result.is_none());
        // 带补报的心跳也要能往返。
        roundtrip(&Heartbeat {
            spec: Some(AgentSpecAck {
                revision: "ab12".into(),
                outcome: spec_outcome::APPLIED.into(),
                applied: AgentSpecWire::default(),
                not_enforced: vec![],
                detail: "ok".into(),
            }),
            ..hb
        });
    }

    #[test]
    fn spec_params_default_is_usable() {
        // 缺省值必须是「能直接下发的合法配置」，否则迁移与「无期望 spec」路径会推出一份坏配置。
        let p = SpecParams::default();
        assert_eq!(p.heartbeat_interval_secs, 30);
        assert!(!p.allowed_interpreters.is_empty());
        assert_eq!(p.job_default_interpreter, "bash");
        assert_eq!(p.max_concurrent_jobs, 1);
        assert_eq!(p.otlp_listen, "0.0.0.0:4318");
        assert_eq!(p.otlp_max_body_bytes, 8 * 1024 * 1024);
        assert_eq!(p.log_level, "info");
    }

    #[test]
    fn heartbeat_reply_defaults_to_not_synced() {
        let reply: HeartbeatReply = serde_json::from_str("{}").expect("decode");
        assert!(!reply.spec_synced, "缺字段时保守取「未确认」，让 agent 继续补报");
    }
}

#[cfg(test)]
mod job_kind_compat_tests {
    use super::*;

    fn exec_json(kind_field: &str) -> String {
        format!(
            r#"{{"job_id":"j1"{kind_field},"interpreter":"bash","script":"echo hi",
                "timeout_secs":30,"stdout_limit_bytes":1024,"stderr_limit_bytes":1024}}"#
        )
    }

    #[test]
    fn missing_kind_defaults_to_script() {
        // 旧 server 发的报文没有 kind 字段 —— 必须按普通脚本作业处理。
        let e: JobExec = serde_json::from_str(&exec_json("")).expect("decode");
        assert_eq!(e.kind, "script");
    }

    #[test]
    fn explicit_kind_is_preserved() {
        let e: JobExec =
            serde_json::from_str(&exec_json(r#","kind":"agent_upgrade""#)).expect("decode");
        assert_eq!(e.kind, "agent_upgrade");
    }

    #[test]
    fn heartbeat_without_upgrade_result_still_parses() {
        // 旧 agent 不发该字段；旧 server 也不能因为缺字段而报错。
        let hb: Heartbeat =
            serde_json::from_str(r#"{"agent_id":"a","ts_micros":1}"#).expect("decode");
        assert!(hb.upgrade_result.is_none());
    }

    #[test]
    fn heartbeat_omits_absent_upgrade_result_on_encode() {
        let hb = Heartbeat {
            agent_id: "a".to_string(),
            ts_micros: 1,
            upgrade_result: None,
            spec: None,
        };
        let json = serde_json::to_string(&hb).expect("encode");
        assert!(!json.contains("upgrade_result"), "无结果时不应出现在报文里");
    }

    #[test]
    fn heartbeat_carries_upgrade_result_when_present() {
        let hb = Heartbeat {
            agent_id: "a".to_string(),
            ts_micros: 1,
            upgrade_result: Some(UpgradeReport {
                started_at: "t0".into(),
                finished_at: "t1".into(),
                from_version: "1.1.0".into(),
                to_version: "1.2.0".into(),
                from_sha256: "a".into(),
                to_sha256: "b".into(),
                outcome: "succeeded".into(),
                detail: String::new(),
            }),
            spec: None,
        };
        let json = serde_json::to_string(&hb).expect("encode");
        let back: Heartbeat = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, hb);
    }

    #[test]
    fn agent_upgrade_spec_roundtrips() {
        let spec = AgentUpgradeSpec {
            binary_path: "/tmp/gse-agent-new".to_string(),
            sha256: "56ca0df6".to_string(),
        };
        let s = serde_json::to_string(&spec).expect("encode");
        let back: AgentUpgradeSpec = serde_json::from_str(&s).expect("decode");
        assert_eq!(back, spec);
    }
}
