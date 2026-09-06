//! GSE 共享数据模型（DTO）：认证、心跳、信令与回执类型。

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
