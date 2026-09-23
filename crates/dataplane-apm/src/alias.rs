//! 静态服务名映射（`apm_service_alias`）：CRUD、校验与反查缓存。
//!
//! 设计（`/.monkeycode/specs/apm-tracing/requirements.md` Requirement 17 与
//! `/.monkeycode/specs/observability-data-model/design.md`「服务标识与反查」）：
//!
//! - 只用于 eBPF 边与 `unknown-*` 的归一；**不覆盖** OTLP span 自带的 `service`。
//! - 反查优先级：静态映射（同类取 `updated_ts` 最新）> 端点表 > `unknown-<ip>`。
//! - `match_kind` 取值：`process_name` / `process_prefix` / `pod_prefix` / `cidr`。
//! - 写入与删除要立即让反查缓存失效（版本号自增），无需重启。
//!
//! `alias_id` 由 `match_kind:match_value` 派生，使同一匹配条件天然去重（upsert 幂等）。

use std::net::IpAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use dataplane_core::{DataplaneError, SqlValue};
use dataplane_sql::RelationalStore;
use serde::{Deserialize, Serialize};

use crate::tables;

/// 允许的匹配方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AliasMatchKind {
    ProcessName,
    ProcessPrefix,
    PodPrefix,
    Cidr,
}

impl AliasMatchKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProcessName => "process_name",
            Self::ProcessPrefix => "process_prefix",
            Self::PodPrefix => "pod_prefix",
            Self::Cidr => "cidr",
        }
    }

    /// 解析字符串；未知取值返回 `None`。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "process_name" => Some(Self::ProcessName),
            "process_prefix" => Some(Self::ProcessPrefix),
            "pod_prefix" => Some(Self::PodPrefix),
            "cidr" => Some(Self::Cidr),
            _ => None,
        }
    }
}

/// 一条静态映射。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasRecord {
    pub alias_id: String,
    pub match_kind: String,
    pub match_value: String,
    pub service: String,
    pub enabled: bool,
    #[serde(default)]
    pub note: String,
    pub updated_ts: i64,
}

/// 新增/修改请求体（`alias_id` 由服务端派生，忽略客户端传入）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AliasUpsert {
    pub match_kind: String,
    pub match_value: String,
    pub service: String,
    pub enabled: Option<bool>,
    pub note: Option<String>,
}

impl AliasUpsert {
    /// 校验并归一化；非法返回 `invalid_argument`。
    pub fn validate(&self) -> Result<(AliasMatchKind, &str, &str), DataplaneError> {
        let kind = AliasMatchKind::parse(self.match_kind.trim()).ok_or_else(|| {
            DataplaneError::invalid_argument(format!(
                "unsupported match_kind: {} (expected process_name/process_prefix/pod_prefix/cidr)",
                self.match_kind
            ))
        })?;
        let match_value = self.match_value.trim();
        if match_value.is_empty() {
            return Err(DataplaneError::invalid_argument(
                "match_value must not be empty",
            ));
        }
        let service = self.service.trim();
        if service.is_empty() {
            return Err(DataplaneError::invalid_argument(
                "service must not be empty",
            ));
        }
        if kind == AliasMatchKind::Cidr && !is_valid_cidr(match_value) {
            return Err(DataplaneError::invalid_argument(format!(
                "match_value must be a valid CIDR for match_kind=cidr: {match_value}"
            )));
        }
        Ok((kind, match_value, service))
    }
}

/// 派生主键：同一匹配条件只保留一行。
#[must_use]
pub fn alias_id(kind: AliasMatchKind, match_value: &str) -> String {
    format!("{}:{}", kind.as_str(), match_value)
}

/// 校验 CIDR（`addr/len`，长度与地址族匹配）。
#[must_use]
pub fn is_valid_cidr(value: &str) -> bool {
    let Some((addr, len)) = value.split_once('/') else {
        return false;
    };
    let Ok(addr) = IpAddr::from_str(addr.trim()) else {
        return false;
    };
    let Ok(len) = len.trim().parse::<u8>() else {
        return false;
    };
    match addr {
        IpAddr::V4(_) => len <= 32,
        IpAddr::V6(_) => len <= 128,
    }
}

/// 列出映射；可按 `match_kind` 与 `enabled` 过滤。
pub async fn list_aliases(
    sql: &dyn RelationalStore,
    match_kind: Option<&str>,
    enabled: Option<bool>,
) -> Result<Vec<AliasRecord>, DataplaneError> {
    let mut params: Vec<SqlValue> = Vec::new();
    let mut where_sql = String::from(" WHERE 1 = 1");
    if let Some(kind) = match_kind {
        if AliasMatchKind::parse(kind).is_none() {
            return Err(DataplaneError::invalid_argument(format!(
                "unsupported match_kind: {kind}"
            )));
        }
        params.push(SqlValue::Text(kind.to_string()));
        where_sql.push_str(&format!(" AND match_kind = ?{}", params.len()));
    }
    if let Some(enabled) = enabled {
        params.push(SqlValue::Integer(i64::from(enabled)));
        where_sql.push_str(&format!(" AND enabled = ?{}", params.len()));
    }
    let result = sql
        .execute(
            &format!(
                "SELECT alias_id, match_kind, match_value, service, enabled, note, updated_ts
                 FROM {}{where_sql} ORDER BY match_kind, match_value",
                tables::SERVICE_ALIAS
            ),
            &params,
        )
        .await?;
    Ok(result.rows.iter().filter_map(|row| row_from(row)).collect())
}

/// 新建或覆盖一条映射（`enabled` 缺省为 true）。
pub async fn upsert_alias(
    sql: &dyn RelationalStore,
    upsert: &AliasUpsert,
    now_ts: i64,
) -> Result<AliasRecord, DataplaneError> {
    let (kind, match_value, service) = upsert.validate()?;
    let id = alias_id(kind, match_value);
    let enabled = upsert.enabled.unwrap_or(true);
    let note = upsert.note.clone().unwrap_or_default();
    sql.execute(
        &format!(
            "INSERT INTO {} (alias_id, match_kind, match_value, service, enabled, note, updated_ts)
             VALUES (?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(alias_id) DO UPDATE SET
                service = excluded.service,
                enabled = excluded.enabled,
                note = excluded.note,
                updated_ts = excluded.updated_ts",
            tables::SERVICE_ALIAS
        ),
        &[
            SqlValue::Text(id.clone()),
            SqlValue::Text(kind.as_str().to_string()),
            SqlValue::Text(match_value.to_string()),
            SqlValue::Text(service.to_string()),
            SqlValue::Integer(i64::from(enabled)),
            SqlValue::Text(note.clone()),
            SqlValue::Integer(now_ts),
        ],
    )
    .await?;
    Ok(AliasRecord {
        alias_id: id,
        match_kind: kind.as_str().to_string(),
        match_value: match_value.to_string(),
        service: service.to_string(),
        enabled,
        note,
        updated_ts: now_ts,
    })
}

/// 开启/关闭一条映射；返回是否命中。
pub async fn set_alias_enabled(
    sql: &dyn RelationalStore,
    id: &str,
    enabled: bool,
    now_ts: i64,
) -> Result<bool, DataplaneError> {
    sql.execute(
        &format!(
            "UPDATE {} SET enabled = ?1, updated_ts = ?2 WHERE alias_id = ?3",
            tables::SERVICE_ALIAS
        ),
        &[
            SqlValue::Integer(i64::from(enabled)),
            SqlValue::Integer(now_ts),
            SqlValue::Text(id.to_string()),
        ],
    )
    .await?;
    Ok(affected_rows(sql).await? > 0)
}

/// 删除一条映射；返回是否命中。
pub async fn delete_alias(sql: &dyn RelationalStore, id: &str) -> Result<bool, DataplaneError> {
    sql.execute(
        &format!("DELETE FROM {} WHERE alias_id = ?1", tables::SERVICE_ALIAS),
        &[SqlValue::Text(id.to_string())],
    )
    .await?;
    Ok(affected_rows(sql).await? > 0)
}

async fn affected_rows(sql: &dyn RelationalStore) -> Result<u64, DataplaneError> {
    let result = sql.execute("SELECT changes()", &[]).await?;
    Ok(result
        .rows
        .first()
        .and_then(|row| row.first())
        .map_or(0, |value| match value {
            SqlValue::Integer(i) => (*i).max(0) as u64,
            _ => 0,
        }))
}

fn row_from(row: &[SqlValue]) -> Option<AliasRecord> {
    let [SqlValue::Text(alias_id), SqlValue::Text(match_kind), SqlValue::Text(match_value), SqlValue::Text(service), SqlValue::Integer(enabled), SqlValue::Text(note), SqlValue::Integer(updated_ts)] =
        row
    else {
        return None;
    };
    Some(AliasRecord {
        alias_id: alias_id.clone(),
        match_kind: match_kind.clone(),
        match_value: match_value.clone(),
        service: service.clone(),
        enabled: *enabled != 0,
        note: note.clone(),
        updated_ts: *updated_ts,
    })
}

/// 映射快照缓存：版本号变化或 TTL 到期时重新加载。
///
/// 写入方（dataserver 的 CRUD 接口）调用 [`AliasCache::invalidate`]，下一次反查就会
/// 重新读表，因此配置修改无需重启。
#[derive(Debug)]
pub struct AliasCache {
    version: AtomicU64,
    ttl: Duration,
    loaded: Mutex<Option<Loaded>>,
}

#[derive(Debug)]
struct Loaded {
    version: u64,
    at: Instant,
    records: Vec<AliasRecord>,
}

impl Default for AliasCache {
    fn default() -> Self {
        Self::new(60)
    }
}

impl AliasCache {
    #[must_use]
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            version: AtomicU64::new(0),
            ttl: Duration::from_secs(ttl_secs),
            loaded: Mutex::new(None),
        }
    }

    /// 让缓存失效（写入/删除映射后调用）。
    pub fn invalidate(&self) {
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    /// 当前版本号（测试与自监控用）。
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    /// 按优先级匹配：`process_name` → `process_prefix` → `pod_prefix` → `cidr`；
    /// 同类多条取 `updated_ts` 最新。未命中返回 `None`。
    pub async fn resolve(
        &self,
        sql: &dyn RelationalStore,
        host_ip: &str,
        pod_name: &str,
        process_name: &str,
    ) -> Result<Option<String>, DataplaneError> {
        let records = self.snapshot(sql).await?;
        for kind in [
            AliasMatchKind::ProcessName,
            AliasMatchKind::ProcessPrefix,
            AliasMatchKind::PodPrefix,
            AliasMatchKind::Cidr,
        ] {
            let mut best: Option<&AliasRecord> = None;
            for record in records.iter().filter(|r| {
                r.enabled
                    && r.match_kind == kind.as_str()
                    && matches_kind(r, kind, host_ip, pod_name, process_name)
            }) {
                if best.is_none_or(|current| record.updated_ts > current.updated_ts) {
                    best = Some(record);
                }
            }
            if let Some(record) = best {
                return Ok(Some(record.service.clone()));
            }
        }
        Ok(None)
    }

    async fn snapshot(
        &self,
        sql: &dyn RelationalStore,
    ) -> Result<Vec<AliasRecord>, DataplaneError> {
        let version = self.version();
        {
            let guard = self.loaded.lock().map_err(|_| lock_error())?;
            if let Some(loaded) = guard.as_ref() {
                if loaded.version == version && loaded.at.elapsed() < self.ttl {
                    return Ok(loaded.records.clone());
                }
            }
        }
        let records = list_aliases(sql, None, Some(true)).await?;
        let mut guard = self.loaded.lock().map_err(|_| lock_error())?;
        *guard = Some(Loaded {
            version,
            at: Instant::now(),
            records: records.clone(),
        });
        Ok(records)
    }
}

fn matches_kind(
    record: &AliasRecord,
    kind: AliasMatchKind,
    host_ip: &str,
    pod_name: &str,
    process_name: &str,
) -> bool {
    match kind {
        AliasMatchKind::ProcessName => {
            !process_name.is_empty() && record.match_value == process_name
        }
        AliasMatchKind::ProcessPrefix => {
            !process_name.is_empty() && process_name.starts_with(&record.match_value)
        }
        AliasMatchKind::PodPrefix => {
            !pod_name.is_empty() && pod_name.starts_with(&record.match_value)
        }
        AliasMatchKind::Cidr => !host_ip.is_empty() && cidr_contains(&record.match_value, host_ip),
    }
}

/// `cidr` 是否包含 `ip`（仅做前缀长度比较，不要求 `ip` 是网络地址）。
#[must_use]
pub fn cidr_contains(cidr: &str, ip: &str) -> bool {
    let Some((net, len)) = cidr.split_once('/') else {
        return false;
    };
    let (Ok(net), Ok(ip)) = (IpAddr::from_str(net.trim()), IpAddr::from_str(ip.trim())) else {
        return false;
    };
    let Ok(len) = len.trim().parse::<u8>() else {
        return false;
    };
    match (net, ip) {
        (IpAddr::V4(net), IpAddr::V4(ip)) => {
            if len > 32 {
                return false;
            }
            let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
            u32::from(net) & mask == u32::from(ip) & mask
        }
        (IpAddr::V6(net), IpAddr::V6(ip)) => {
            if len > 128 {
                return false;
            }
            let mask = if len == 0 {
                0
            } else {
                u128::MAX << (128 - len)
            };
            u128::from(net) & mask == u128::from(ip) & mask
        }
        _ => false,
    }
}

fn lock_error() -> DataplaneError {
    DataplaneError::new(
        dataplane_core::ErrorCode::QueryFailed,
        "alias cache lock poisoned",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_validation_and_matching() {
        assert!(is_valid_cidr("10.0.0.0/8"));
        assert!(is_valid_cidr("2001:db8::/32"));
        assert!(!is_valid_cidr("10.0.0.0"), "缺前缀长度");
        assert!(!is_valid_cidr("10.0.0.0/33"), "v4 前缀越界");
        assert!(!is_valid_cidr("not-an-ip/8"));

        assert!(cidr_contains("10.0.0.0/8", "10.1.2.3"));
        assert!(!cidr_contains("10.0.0.0/8", "11.1.2.3"));
        assert!(cidr_contains("0.0.0.0/0", "1.2.3.4"), "/0 匹配全部");
        assert!(cidr_contains("2001:db8::/32", "2001:db8::1"));
        assert!(!cidr_contains("2001:db8::/32", "2001:db9::1"));
        assert!(!cidr_contains("10.0.0.0/8", "2001:db8::1"), "地址族不同");
    }

    #[test]
    fn upsert_validation_rejects_bad_input() {
        let bad_kind = AliasUpsert {
            match_kind: "nope".into(),
            match_value: "x".into(),
            service: "svc".into(),
            ..AliasUpsert::default()
        };
        assert_eq!(
            bad_kind.validate().unwrap_err().code.as_str(),
            "invalid_argument"
        );
        for upsert in [
            AliasUpsert {
                match_kind: "process_name".into(),
                match_value: "  ".into(),
                service: "svc".into(),
                ..AliasUpsert::default()
            },
            AliasUpsert {
                match_kind: "process_name".into(),
                match_value: "java".into(),
                service: String::new(),
                ..AliasUpsert::default()
            },
            AliasUpsert {
                match_kind: "cidr".into(),
                match_value: "10.0.0.0".into(),
                service: "svc".into(),
                ..AliasUpsert::default()
            },
        ] {
            assert!(upsert.validate().is_err(), "应拒绝: {upsert:?}");
        }
        assert_eq!(
            alias_id(AliasMatchKind::PodPrefix, "order-api"),
            "pod_prefix:order-api"
        );
    }
}
