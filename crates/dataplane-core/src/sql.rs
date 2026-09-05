use serde::{Deserialize, Serialize};

use crate::error::{DataplaneError, ErrorCode};

/// 单个 SQL 值，跨 HTTP JSON 与引擎使用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

/// 一次 SQL 执行的返回：列名 + 行数据。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SqlResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
}

/// 将 JSON 参数值映射为 `SqlValue`。
pub fn json_to_sql_value(v: serde_json::Value) -> Result<SqlValue, DataplaneError> {
    match v {
        serde_json::Value::Null => Ok(SqlValue::Null),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(SqlValue::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(SqlValue::Real(f))
            } else {
                Err(DataplaneError::new(ErrorCode::InvalidArgument, "unsupported number parameter"))
            }
        }
        serde_json::Value::String(s) => Ok(SqlValue::Text(s)),
        serde_json::Value::Bool(b) => Ok(SqlValue::Integer(i64::from(b))),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => Err(DataplaneError::new(
            ErrorCode::InvalidArgument,
            "parameter must be null, number, string, or blob object",
        )),
    }
}

/// 将请求参数数组转为 `SqlValue` 列表。支持 `{"b64":"..."}` 形式的 blob。
pub fn json_params_to_sql_values(params: &[serde_json::Value]) -> Result<Vec<SqlValue>, DataplaneError> {
    params
        .iter()
        .map(|v| match v {
            serde_json::Value::Object(map) if map.len() == 1 && map.contains_key("b64") => {
                let b64 = map["b64"].as_str().ok_or_else(|| {
                    DataplaneError::new(ErrorCode::InvalidArgument, "blob b64 field must be a string")
                })?;
                use base64::Engine;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| DataplaneError::new(ErrorCode::InvalidArgument, format!("invalid base64: {e}")))?;
                Ok(SqlValue::Blob(bytes))
            }
            serde_json::Value::Object(_) => Err(DataplaneError::new(
                ErrorCode::InvalidArgument,
                "parameter object must be blob with b64 field",
            )),
            other => json_to_sql_value(other.clone()),
        })
        .collect()
}

/// 将 `SqlValue` 转为 JSON 值，供 HTTP 响应序列化。
pub fn sql_value_to_json(v: &SqlValue) -> serde_json::Value {
    match v {
        SqlValue::Null => serde_json::Value::Null,
        SqlValue::Integer(i) => serde_json::Value::Number((*i).into()),
        SqlValue::Real(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        SqlValue::Text(s) => serde_json::Value::String(s.clone()),
        SqlValue::Blob(b) => {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(b);
            serde_json::json!({ "b64": b64 })
        }
    }
}