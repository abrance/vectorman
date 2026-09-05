use serde::{Deserialize, Serialize};

/// 稳定错误码，跨 HTTP、trait 与 CLI 使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 对象或键不存在。
    NotFound,
    /// JSON/SQL/PromQL 无法解析或缺字段。
    InvalidArgument,
    /// SQL 或 Prom 查询执行失败。
    QueryFailed,
    /// 占位 adapter 或未实现的 Prom 函数。
    Unimplemented,
    /// 启动时引擎打不开。
    EngineInitFailed,
    /// TOML 缺失或无法解析。
    ConfigInvalid,
    /// 对端不可达（dpc 使用）。
    Unavailable,
}

impl ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::NotFound => "not_found",
            ErrorCode::InvalidArgument => "invalid_argument",
            ErrorCode::QueryFailed => "query_failed",
            ErrorCode::Unimplemented => "unimplemented",
            ErrorCode::EngineInitFailed => "engine_init_failed",
            ErrorCode::ConfigInvalid => "config_invalid",
            ErrorCode::Unavailable => "unavailable",
        }
    }
}

/// 统一错误类型。`code` 是稳定机器可读标识，`message` 供展示。
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct DataplaneError {
    pub code: ErrorCode,
    pub message: String,
}

impl DataplaneError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    pub fn query_failed(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::QueryFailed, message)
    }

    pub fn unimplemented(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unimplemented, message)
    }

    pub fn config_invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ConfigInvalid, message)
    }
}

impl From<std::io::Error> for DataplaneError {
    fn from(e: std::io::Error) -> Self {
        Self::new(ErrorCode::QueryFailed, e.to_string())
    }
}
