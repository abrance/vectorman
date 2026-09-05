use async_trait::async_trait;

use crate::error::{DataplaneError, ErrorCode};

/// 请求元信息，供鉴权层检查。
#[derive(Debug, Clone, Default)]
pub struct RequestMeta {
    pub peer_addr: Option<String>,
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
}

/// 鉴权中间件接口。apiserver 在两个 HTTP 端口的最外层调用。
#[async_trait]
pub trait AuthN: Send + Sync {
    async fn check(&self, req: &RequestMeta) -> Result<(), DataplaneError>;
}

/// v1 默认鉴权实现：直接放行。
pub struct NoopAuth;

#[async_trait]
impl AuthN for NoopAuth {
    async fn check(&self, _req: &RequestMeta) -> Result<(), DataplaneError> {
        Ok(())
    }
}

/// 静态 token 鉴权（v1 预留，未启用）。
pub struct TokenAuth {
    pub expected: String,
}

#[async_trait]
impl AuthN for TokenAuth {
    async fn check(&self, req: &RequestMeta) -> Result<(), DataplaneError> {
        let found = req.headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("authorization") && *v == format!("Bearer {}", self.expected)
        });
        if found {
            Ok(())
        } else {
            Err(DataplaneError::new(
                ErrorCode::InvalidArgument,
                "missing or invalid bearer token",
            ))
        }
    }
}
