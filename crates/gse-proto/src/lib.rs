//! GSE 共享数据模型（DTO）：认证、心跳、信令与回执类型。

use bytes::Bytes;
use dataplane_core::{DataplaneError, ErrorCode};
use serde::{Deserialize, Serialize};

/// Agent 向 Server 发起的认证请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthRequest {
    pub agent_id: String,
    pub token: String,
}

/// Server 对认证请求的应答。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthReply {
    pub ok: bool,
    pub reason: Option<String>,
}

/// Agent 周期性上报的心跳。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub agent_id: String,
    pub ts_micros: i64,
}

/// Server 下发给 Agent 的指令。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Command {
    pub id: String,
    pub name: String,
    pub payload: Bytes,
}

/// Agent 对指令的执行回执。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub command_id: String,
    pub ok: bool,
    pub message: Option<String>,
}

/// 跨 RPC 传输的错误载荷，code 为稳定字符串。
#[derive(Debug, Clone, Serialize, Deserialize)]
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
