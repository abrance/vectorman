//! APM 与 eBPF 观测数据的 sqlite 观察表：建表、版本检查与表常量。
//!
//! 对应设计：
//!
//! - `/.monkeycode/specs/observability-data-model/design.md`「sqlite 观测表」定义
//!   `obs_schema_meta`、`apm_trace_summary`、`apm_service_endpoint`、`apm_service_alias`。
//! - `/.monkeycode/specs/apm-tracing/design.md`「Data Models」定义 `apm_edge_summary`
//!   与 `apm_trace_summary.max_end_ts`。
//!
//! `RelationalStore` 只有 `execute(sql, params)` 且拒绝多语句，因此建表逐条执行。
//! 版本策略：`obs_schema_meta` 的 `schema_version` 高于本代码支持的版本时以
//! `config_invalid` 退出（不做自动降级）；低于或缺失时按当前版本补齐。

use std::time::{SystemTime, UNIX_EPOCH};

use dataplane_core::{DataplaneError, ErrorCode, SqlValue};
use dataplane_sql::RelationalStore;

pub mod accumulator;
pub mod aggregator;
pub mod edge;
pub mod query;
pub mod red;

pub mod endpoint;
pub mod sink;
#[cfg(test)]
mod tests;

pub use accumulator::{ApmSinkConfig, TraceSummaryAccumulator};
pub use aggregator::{AggReport, ApmAggregator};
pub use edge::{EdgeAccumulator, ServiceResolver};
pub use endpoint::{Endpoint, EndpointRegistry};
pub use query::{
    get_trace, list_services, search_edges, search_traces, EdgeSearchPage, EdgeSearchQuery,
    ServiceRow, TraceDetail, TraceSearchPage, TraceSearchQuery, TraceSummary,
};
pub use red::{ClosedSamples, RedSamples};
pub use sink::{ApmSink, FlushReport};

/// 当前 Unix 微秒。
#[must_use]
pub fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// 本代码支持的观测表 schema 版本。
///
/// v1 → v2：`apm_trace_summary` 增加 `root_start_ts`。摘要按「根 span 取 `start_ts`
/// 最小者」维护，而根可能比普通 span 晚到、也可能跨多次 flush 才到，因此必须把
/// 当前根的开始时间也存下来，否则无法在后续 flush 中比较出更早的根。
pub const SCHEMA_VERSION: i64 = 2;

/// 版本迁移：`(目标版本, 语句)`，仅在当前版本低于目标版本时执行。
///
/// 新建库的 `CREATE TABLE` 已包含新列，因此迁移语句对「缺少该列」与「已有该列」
/// 两种情况都要能容忍（重复列错误被忽略）。
pub const MIGRATIONS: &[(i64, &str)] = &[(
    2,
    "ALTER TABLE apm_trace_summary ADD COLUMN root_start_ts INTEGER NOT NULL DEFAULT 0",
)];

/// 版本行的键名。
pub const SCHEMA_VERSION_KEY: &str = "schema_version";

/// 表名常量（供查询与测试引用，避免散落字符串）。
pub mod tables {
    pub const META: &str = "obs_schema_meta";
    pub const TRACE_SUMMARY: &str = "apm_trace_summary";
    pub const EDGE_SUMMARY: &str = "apm_edge_summary";
    pub const SERVICE_ENDPOINT: &str = "apm_service_endpoint";
    pub const SERVICE_ALIAS: &str = "apm_service_alias";
}

/// 建表与索引语句，按执行顺序排列。
///
/// 列定义与设计文档一致；`DEFAULT` 值保证 upsert 时不必为可空列传参。
pub const DDL: &[&str] = &[
    // 版本元数据。
    "CREATE TABLE IF NOT EXISTS obs_schema_meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    )",
    // trace 摘要：start_ts 为全部 span 的最小 start，max_end_ts 为最大 end，
    // duration_micros 由两者相减重算（乱序到达时取极值）。
    "CREATE TABLE IF NOT EXISTS apm_trace_summary (
        trace_id         TEXT PRIMARY KEY,
        start_ts         INTEGER NOT NULL,
        max_end_ts       INTEGER NOT NULL DEFAULT 0,
        duration_micros  INTEGER NOT NULL,
        root_service     TEXT NOT NULL,
        root_operation   TEXT NOT NULL,
        root_start_ts    INTEGER NOT NULL DEFAULT 0,
        span_count       INTEGER NOT NULL,
        error_count      INTEGER NOT NULL,
        status           TEXT NOT NULL,
        services_json    TEXT NOT NULL,
        collector        TEXT NOT NULL,
        agent_id         TEXT NOT NULL,
        host_id          TEXT NOT NULL DEFAULT '',
        data_id          TEXT NOT NULL,
        updated_ts       INTEGER NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS apm_trace_summary_start ON apm_trace_summary(start_ts)",
    "CREATE INDEX IF NOT EXISTS apm_trace_summary_duration ON apm_trace_summary(duration_micros)",
    "CREATE INDEX IF NOT EXISTS apm_trace_summary_root ON apm_trace_summary(root_service, root_operation)",
    // 边摘要：span 配对产生的分钟级聚合，供 apm_edge_* 指标与边列表使用。
    "CREATE TABLE IF NOT EXISTS apm_edge_summary (
        bucket_start    INTEGER NOT NULL,
        src_service     TEXT NOT NULL,
        dst_service     TEXT NOT NULL,
        span_kind       TEXT NOT NULL,
        calls           INTEGER NOT NULL,
        errors          INTEGER NOT NULL,
        duration_sum    INTEGER NOT NULL,
        duration_max    INTEGER NOT NULL,
        agent_id        TEXT NOT NULL DEFAULT '',
        data_id         TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (bucket_start, src_service, dst_service, span_kind, agent_id)
    )",
    "CREATE INDEX IF NOT EXISTS apm_edge_summary_svc ON apm_edge_summary(src_service, dst_service, bucket_start)",
    // 端点表：OTLP resource 自动登记，供 eBPF 边与未识别 IP 反查服务名。
    "CREATE TABLE IF NOT EXISTS apm_service_endpoint (
        service         TEXT NOT NULL,
        instance_id     TEXT NOT NULL,
        pod_name        TEXT NOT NULL DEFAULT '',
        node_name       TEXT NOT NULL DEFAULT '',
        host_ip         TEXT NOT NULL DEFAULT '',
        listen_port     INTEGER NOT NULL DEFAULT 0,
        collector       TEXT NOT NULL,
        first_seen_ts   INTEGER NOT NULL,
        last_seen_ts    INTEGER NOT NULL,
        PRIMARY KEY (service, instance_id)
    )",
    "CREATE INDEX IF NOT EXISTS apm_service_endpoint_ip_port ON apm_service_endpoint(host_ip, listen_port)",
    // 静态服务名映射：前端维护，反查优先级高于端点表。
    "CREATE TABLE IF NOT EXISTS apm_service_alias (
        alias_id      TEXT PRIMARY KEY,
        match_kind    TEXT NOT NULL,
        match_value   TEXT NOT NULL,
        service       TEXT NOT NULL,
        enabled       INTEGER NOT NULL DEFAULT 1,
        note          TEXT NOT NULL DEFAULT '',
        updated_ts    INTEGER NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS apm_service_alias_match ON apm_service_alias(match_kind, match_value)",
];

/// 建表并校验版本；可在每次进程启动时调用，幂等。
///
/// 每次启动都执行全部 `CREATE TABLE IF NOT EXISTS`，因为 sqlite 没有独立的
/// 迁移记录；后续版本新增表时只需在 [`DDL`] 末尾追加并提升 [`SCHEMA_VERSION`]。
pub async fn bootstrap(sql: &dyn RelationalStore) -> Result<(), DataplaneError> {
    for statement in DDL {
        sql.execute(statement, &[]).await?;
    }

    let current = read_version(sql).await?;
    match current {
        Some(v) if v > SCHEMA_VERSION => Err(DataplaneError::new(
            ErrorCode::ConfigInvalid,
            format!(
                "obs schema version {v} is newer than supported {SCHEMA_VERSION}; \
                 upgrade dataserver instead of downgrading the database"
            ),
        )),
        Some(v) if v < SCHEMA_VERSION => {
            for (target, statement) in MIGRATIONS.iter().filter(|(t, _)| *t > v) {
                apply_migration(sql, *target, statement).await?;
            }
            write_version(sql, SCHEMA_VERSION).await
        }
        Some(_) => Ok(()),
        None => write_version(sql, SCHEMA_VERSION).await,
    }
}

/// 执行一条迁移语句；「重复列」等已应用过的错误视为成功。
async fn apply_migration(
    sql: &dyn RelationalStore,
    target: i64,
    statement: &str,
) -> Result<(), DataplaneError> {
    if let Err(e) = sql.execute(statement, &[]).await {
        let already_applied = e.message.contains("duplicate column name");
        if !already_applied {
            return Err(DataplaneError::new(
                ErrorCode::ConfigInvalid,
                format!("obs schema migration to v{target} failed: {}", e.message),
            ));
        }
    }
    Ok(())
}

/// 写入/覆盖版本行。
async fn write_version(sql: &dyn RelationalStore, version: i64) -> Result<(), DataplaneError> {
    sql.execute(
        &format!(
            "INSERT INTO {} (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            tables::META
        ),
        &[
            SqlValue::Text(SCHEMA_VERSION_KEY.to_string()),
            SqlValue::Text(version.to_string()),
        ],
    )
    .await?;
    Ok(())
}

/// 读取 `schema_version`；缺失返回 `None`。
pub async fn read_version(sql: &dyn RelationalStore) -> Result<Option<i64>, DataplaneError> {
    let result = sql
        .execute(
            &format!("SELECT value FROM {} WHERE key = ?1", tables::META),
            &[SqlValue::Text(SCHEMA_VERSION_KEY.to_string())],
        )
        .await?;
    let Some(row) = result.rows.first() else {
        return Ok(None);
    };
    let Some(value) = row.first() else {
        return Ok(None);
    };
    let parsed = match value {
        SqlValue::Integer(i) => *i,
        SqlValue::Text(s) => s.trim().parse::<i64>().map_err(|e| {
            DataplaneError::new(
                ErrorCode::ConfigInvalid,
                format!("invalid {SCHEMA_VERSION_KEY} value {s:?}: {e}"),
            )
        })?,
        other => {
            return Err(DataplaneError::new(
                ErrorCode::ConfigInvalid,
                format!("unexpected {SCHEMA_VERSION_KEY} type: {other:?}"),
            ))
        }
    };
    Ok(Some(parsed))
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    use dataplane_sql::SqliteRelationalStore;

    fn store(dir: &std::path::Path) -> SqliteRelationalStore {
        SqliteRelationalStore::new(dir.join("sql.sqlite")).unwrap()
    }

    async fn table_exists(sql: &dyn RelationalStore, name: &str) -> bool {
        let result = sql
            .execute(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
                &[SqlValue::Text(name.to_string())],
            )
            .await
            .unwrap();
        !result.rows.is_empty()
    }

    async fn index_exists(sql: &dyn RelationalStore, name: &str) -> bool {
        let result = sql
            .execute(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND name = ?1",
                &[SqlValue::Text(name.to_string())],
            )
            .await
            .unwrap();
        !result.rows.is_empty()
    }

    #[tokio::test]
    async fn bootstrap_creates_all_tables_and_version() {
        let dir = tempfile::tempdir().unwrap();
        let sql = store(dir.path());
        bootstrap(&sql).await.unwrap();

        for name in [
            tables::META,
            tables::TRACE_SUMMARY,
            tables::EDGE_SUMMARY,
            tables::SERVICE_ENDPOINT,
            tables::SERVICE_ALIAS,
        ] {
            assert!(table_exists(&sql, name).await, "缺少表 {name}");
        }
        for name in [
            "apm_trace_summary_start",
            "apm_trace_summary_duration",
            "apm_trace_summary_root",
            "apm_edge_summary_svc",
            "apm_service_endpoint_ip_port",
            "apm_service_alias_match",
        ] {
            assert!(index_exists(&sql, name).await, "缺少索引 {name}");
        }
        assert_eq!(read_version(&sql).await.unwrap(), Some(SCHEMA_VERSION));
    }

    #[tokio::test]
    async fn bootstrap_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let sql = store(dir.path());
        bootstrap(&sql).await.unwrap();
        bootstrap(&sql).await.unwrap();

        let result = sql
            .execute(&format!("SELECT COUNT(*) FROM {}", tables::META), &[])
            .await
            .unwrap();
        assert_eq!(result.rows[0][0], SqlValue::Integer(1), "版本行只应有一行");
    }

    #[tokio::test]
    async fn trace_summary_accepts_upsert_with_out_of_order_extremes() {
        let dir = tempfile::tempdir().unwrap();
        let sql = store(dir.path());
        bootstrap(&sql).await.unwrap();

        // 先写入较晚的 span 摘要，再写入较早的，模拟乱序到达后的极值合并。
        for (start_ts, max_end_ts) in [(2_000i64, 3_000i64), (1_000i64, 2_500i64)] {
            sql.execute(
                "INSERT INTO apm_trace_summary (trace_id, start_ts, max_end_ts, duration_micros,
                    root_service, root_operation, span_count, error_count, status, services_json,
                    collector, agent_id, host_id, data_id, updated_ts)
                 VALUES ('t1', ?1, ?2, ?2 - ?1, 'svc', 'op', 1, 0, 'ok', '[\"svc\"]', 'otlp', 'a1', '', 'item', 0)
                 ON CONFLICT(trace_id) DO UPDATE SET
                    start_ts = MIN(apm_trace_summary.start_ts, excluded.start_ts),
                    max_end_ts = MAX(apm_trace_summary.max_end_ts, excluded.max_end_ts),
                    duration_micros = MAX(apm_trace_summary.max_end_ts, excluded.max_end_ts)
                        - MIN(apm_trace_summary.start_ts, excluded.start_ts),
                    span_count = apm_trace_summary.span_count + excluded.span_count",
                &[SqlValue::Integer(start_ts), SqlValue::Integer(max_end_ts)],
            )
            .await
            .unwrap();
        }

        let result = sql
            .execute(
                "SELECT start_ts, max_end_ts, duration_micros, span_count FROM apm_trace_summary WHERE trace_id = 't1'",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(
            result.rows[0],
            vec![
                SqlValue::Integer(1_000),
                SqlValue::Integer(3_000),
                SqlValue::Integer(2_000),
                SqlValue::Integer(2),
            ]
        );
    }

    #[tokio::test]
    async fn edge_summary_primary_key_conflict_is_upsertable() {
        let dir = tempfile::tempdir().unwrap();
        let sql = store(dir.path());
        bootstrap(&sql).await.unwrap();
        let insert = "INSERT INTO apm_edge_summary
            (bucket_start, src_service, dst_service, span_kind, calls, errors, duration_sum, duration_max)
            VALUES (60, 'gw', 'order', 'server', 1, 0, 100, 100)
            ON CONFLICT(bucket_start, src_service, dst_service, span_kind, agent_id)
            DO UPDATE SET calls = apm_edge_summary.calls + excluded.calls";
        sql.execute(insert, &[]).await.unwrap();
        sql.execute(insert, &[]).await.unwrap();
        let result = sql
            .execute("SELECT calls FROM apm_edge_summary", &[])
            .await
            .unwrap();
        assert_eq!(result.rows[0][0], SqlValue::Integer(2));
    }

    #[tokio::test]
    async fn newer_schema_version_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let sql = store(dir.path());
        bootstrap(&sql).await.unwrap();
        sql.execute(
            &format!("UPDATE {} SET value = ?1 WHERE key = ?2", tables::META),
            &[
                SqlValue::Text((SCHEMA_VERSION + 1).to_string()),
                SqlValue::Text(SCHEMA_VERSION_KEY.to_string()),
            ],
        )
        .await
        .unwrap();

        let err = bootstrap(&sql).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid);
        assert!(
            err.message.contains("newer than supported"),
            "unexpected message: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn corrupt_version_value_is_config_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let sql = store(dir.path());
        bootstrap(&sql).await.unwrap();
        sql.execute(
            &format!("UPDATE {} SET value = 'x' WHERE key = ?1", tables::META),
            &[SqlValue::Text(SCHEMA_VERSION_KEY.to_string())],
        )
        .await
        .unwrap();

        let err = bootstrap(&sql).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid);
    }
}
