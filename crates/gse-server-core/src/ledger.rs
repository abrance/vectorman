//! 台账（Ledger）：四张 sqlite 资产的语义化封装。
//!
//! hosts / access_points / agents / agent_configs 均为 sqlite 持久化表，
//! 以自然键为主键幂等 upsert；本模块向会话层与 HTTP 层提供统一的增删改查。

use dataplane_core::{DataplaneError, SqlValue};
use dataplane_sql::{RelationalStore, SqliteRelationalStore};
use gse_proto::GseError;
use serde::{Deserialize, Serialize};

/// 主机资产。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    pub host_id: String,
    pub inner_ip: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub os_type: String,
    #[serde(default)]
    pub os_version: String,
    #[serde(default)]
    pub cpu_spec: String,
    #[serde(default)]
    pub mem_spec: String,
    #[serde(default)]
    pub created_at: String,
}

/// 接入点（Server 通信地址登记）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessPoint {
    pub id: String,
    pub name: String,
    pub server_ip: String,
    pub rpc_port: i32,
    #[serde(default)]
    pub file_port: Option<i32>,
    #[serde(default)]
    pub data_port: Option<i32>,
    #[serde(default)]
    pub created_at: String,
}

/// Agent 实例（预登记 + 运行状态）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub agent_id: String,
    pub host_id: String,
    #[serde(default)]
    pub access_point_id: Option<String>,
    pub token: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub install_path: String,
    /// online / offline / unknown。
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub last_heartbeat_at: Option<String>,
    #[serde(default)]
    pub registered_at: String,
}

/// Agent 运行时配置（本期仅存储与查询）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    pub agent_id: String,
    pub host_id: String,
    #[serde(default)]
    pub cpu_limit_percent: Option<i64>,
    #[serde(default)]
    pub mem_limit_percent: Option<i64>,
    #[serde(default)]
    pub log_level: String,
    #[serde(default)]
    pub updated_at: String,
}

/// 台账访问入口。
pub struct Ledger {
    store: SqliteRelationalStore,
}

impl Ledger {
    /// 打开或创建 sqlite 台账库；失败返回含路径原因。
    pub fn new(db_path: &str) -> Result<Self, String> {
        let store = SqliteRelationalStore::new(db_path)
            .map_err(|e| format!("open ledger {db_path}: {}", e.message))?;
        Ok(Self { store })
    }

    /// 幂等建表。
    pub async fn init(&self) -> Result<(), String> {
        let ddl = [
            "CREATE TABLE IF NOT EXISTS hosts (
                host_id    TEXT PRIMARY KEY,
                inner_ip   TEXT NOT NULL,
                hostname   TEXT NOT NULL DEFAULT '',
                os_type    TEXT NOT NULL DEFAULT '',
                os_version TEXT NOT NULL DEFAULT '',
                cpu_spec   TEXT NOT NULL DEFAULT '',
                mem_spec   TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS access_points (
                id         TEXT PRIMARY KEY,
                name       TEXT NOT NULL,
                server_ip  TEXT NOT NULL,
                rpc_port   INTEGER NOT NULL,
                file_port  INTEGER,
                data_port  INTEGER,
                created_at TEXT NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS agents (
                agent_id         TEXT PRIMARY KEY,
                host_id          TEXT NOT NULL,
                access_point_id  TEXT,
                token            TEXT NOT NULL,
                version          TEXT NOT NULL DEFAULT '',
                install_path     TEXT NOT NULL DEFAULT '',
                status           TEXT NOT NULL DEFAULT 'unknown',
                last_heartbeat_at TEXT,
                registered_at    TEXT NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS agent_configs (
                agent_id           TEXT PRIMARY KEY,
                host_id            TEXT NOT NULL,
                cpu_limit_percent  INTEGER,
                mem_limit_percent  INTEGER,
                log_level          TEXT NOT NULL DEFAULT 'info',
                updated_at         TEXT NOT NULL
            )",
        ];
        for sql in ddl {
            self.store
                .execute(sql, &[])
                .await
                .map_err(|e| format!("init ledger: {}", e.message))?;
        }
        Ok(())
    }

    async fn execute(
        &self,
        sql: &str,
        params: &[SqlValue],
    ) -> Result<dataplane_core::SqlResult, GseError> {
        self.store
            .execute(sql, params)
            .await
            .map_err(|e: DataplaneError| GseError::from(e))
    }

    // ---- hosts ----

    /// 以 host_id 为主键幂等登记主机。
    pub async fn upsert_host(&self, h: &Host) -> Result<(), GseError> {
        let sql = "INSERT INTO hosts (host_id, inner_ip, hostname, os_type, os_version, cpu_spec, mem_spec, created_at)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(host_id) DO UPDATE SET
                     inner_ip = excluded.inner_ip,
                     hostname = excluded.hostname,
                     os_type = excluded.os_type,
                     os_version = excluded.os_version,
                     cpu_spec = excluded.cpu_spec,
                     mem_spec = excluded.mem_spec";
        self.execute(sql, &sql_params_host(h)).await?;
        Ok(())
    }

    pub async fn get_host(&self, id: &str) -> Result<Option<Host>, GseError> {
        let res = self
            .execute("SELECT * FROM hosts WHERE host_id = ?", &[text(id)])
            .await?;
        Ok(res.rows.first().map(|row| row_to_host(&res.columns, row)))
    }

    pub async fn list_hosts(&self) -> Result<Vec<Host>, GseError> {
        let res = self
            .execute("SELECT * FROM hosts ORDER BY host_id", &[])
            .await?;
        Ok(res
            .rows
            .iter()
            .map(|row| row_to_host(&res.columns, row))
            .collect())
    }

    pub async fn remove_host(&self, id: &str) -> Result<(), GseError> {
        self.execute("DELETE FROM hosts WHERE host_id = ?", &[text(id)])
            .await?;
        Ok(())
    }

    // ---- access_points ----

    /// 以 id 为主键幂等登记接入点。
    pub async fn upsert_access_point(&self, ap: &AccessPoint) -> Result<(), GseError> {
        let sql = "INSERT INTO access_points (id, name, server_ip, rpc_port, file_port, data_port, created_at)
                   VALUES (?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(id) DO UPDATE SET
                     name = excluded.name,
                     server_ip = excluded.server_ip,
                     rpc_port = excluded.rpc_port,
                     file_port = excluded.file_port,
                     data_port = excluded.data_port";
        let params = vec![
            text(&ap.id),
            text(&ap.name),
            text(&ap.server_ip),
            SqlValue::Integer(i64::from(ap.rpc_port)),
            opt_i32(ap.file_port),
            opt_i32(ap.data_port),
            text(&ap.created_at),
        ];
        self.execute(sql, &params).await?;
        Ok(())
    }

    pub async fn get_access_point(&self, id: &str) -> Result<Option<AccessPoint>, GseError> {
        let res = self
            .execute("SELECT * FROM access_points WHERE id = ?", &[text(id)])
            .await?;
        Ok(res
            .rows
            .first()
            .map(|row| row_to_access_point(&res.columns, row)))
    }

    pub async fn list_access_points(&self) -> Result<Vec<AccessPoint>, GseError> {
        let res = self
            .execute("SELECT * FROM access_points ORDER BY id", &[])
            .await?;
        Ok(res
            .rows
            .iter()
            .map(|row| row_to_access_point(&res.columns, row))
            .collect())
    }

    pub async fn remove_access_point(&self, id: &str) -> Result<(), GseError> {
        self.execute("DELETE FROM access_points WHERE id = ?", &[text(id)])
            .await?;
        Ok(())
    }

    // ---- agents ----

    /// 以 agent_id 为主键幂等登记或更新 Agent。
    pub async fn upsert_agent(&self, a: &Agent) -> Result<(), GseError> {
        let sql = "INSERT INTO agents (agent_id, host_id, access_point_id, token, version, install_path, status, last_heartbeat_at, registered_at)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(agent_id) DO UPDATE SET
                     host_id = excluded.host_id,
                     access_point_id = excluded.access_point_id,
                     token = excluded.token,
                     version = excluded.version,
                     install_path = excluded.install_path,
                     status = excluded.status,
                     last_heartbeat_at = excluded.last_heartbeat_at";
        self.execute(sql, &sql_params_agent(a)).await?;
        Ok(())
    }

    pub async fn get_agent(&self, id: &str) -> Result<Option<Agent>, GseError> {
        let res = self
            .execute("SELECT * FROM agents WHERE agent_id = ?", &[text(id)])
            .await?;
        Ok(res.rows.first().map(|row| row_to_agent(&res.columns, row)))
    }

    pub async fn list_agents(&self) -> Result<Vec<Agent>, GseError> {
        let res = self
            .execute("SELECT * FROM agents ORDER BY agent_id", &[])
            .await?;
        Ok(res
            .rows
            .iter()
            .map(|row| row_to_agent(&res.columns, row))
            .collect())
    }

    pub async fn remove_agent(&self, id: &str) -> Result<(), GseError> {
        self.execute("DELETE FROM agents WHERE agent_id = ?", &[text(id)])
            .await?;
        Ok(())
    }

    // ---- agents 运行态 ----

    /// 仅从 agents 表校验 agent-id 与 token 一致性。
    pub async fn check_auth(&self, agent_id: &str, token: &str) -> Result<bool, GseError> {
        let res = self
            .execute(
                "SELECT 1 FROM agents WHERE agent_id = ? AND token = ?",
                &[text(agent_id), text(token)],
            )
            .await?;
        Ok(!res.rows.is_empty())
    }

    /// 认证成功后置在线并记录最后心跳。
    pub async fn mark_online(&self, agent_id: &str, now: &str) -> Result<(), GseError> {
        self.execute(
            "UPDATE agents SET status = 'online', last_heartbeat_at = ? WHERE agent_id = ?",
            &[text(now), text(agent_id)],
        )
        .await?;
        Ok(())
    }

    /// 心跳回写最后心跳时间。
    pub async fn mark_heartbeat(&self, agent_id: &str, now: &str) -> Result<(), GseError> {
        self.execute(
            "UPDATE agents SET last_heartbeat_at = ? WHERE agent_id = ?",
            &[text(now), text(agent_id)],
        )
        .await?;
        Ok(())
    }

    /// 会话推进至离线时置状态离线。
    pub async fn mark_offline(&self, agent_id: &str) -> Result<(), GseError> {
        self.execute(
            "UPDATE agents SET status = 'offline' WHERE agent_id = ?",
            &[text(agent_id)],
        )
        .await?;
        Ok(())
    }

    // ---- agent_configs ----

    /// 以 agent_id 为主键幂等保存运行时配置。
    pub async fn upsert_agent_config(&self, c: &AgentConfig) -> Result<(), GseError> {
        let sql = "INSERT INTO agent_configs (agent_id, host_id, cpu_limit_percent, mem_limit_percent, log_level, updated_at)
                   VALUES (?, ?, ?, ?, ?, ?)
                   ON CONFLICT(agent_id) DO UPDATE SET
                     host_id = excluded.host_id,
                     cpu_limit_percent = excluded.cpu_limit_percent,
                     mem_limit_percent = excluded.mem_limit_percent,
                     log_level = excluded.log_level,
                     updated_at = excluded.updated_at";
        let params = vec![
            text(&c.agent_id),
            text(&c.host_id),
            opt_int(c.cpu_limit_percent),
            opt_int(c.mem_limit_percent),
            text(&c.log_level),
            text(&c.updated_at),
        ];
        self.execute(sql, &params).await?;
        Ok(())
    }

    pub async fn get_agent_config(&self, agent_id: &str) -> Result<Option<AgentConfig>, GseError> {
        let res = self
            .execute(
                "SELECT * FROM agent_configs WHERE agent_id = ?",
                &[text(agent_id)],
            )
            .await?;
        Ok(res
            .rows
            .first()
            .map(|row| row_to_agent_config(&res.columns, row)))
    }

    pub async fn list_agent_configs(&self) -> Result<Vec<AgentConfig>, GseError> {
        let res = self
            .execute("SELECT * FROM agent_configs ORDER BY agent_id", &[])
            .await?;
        Ok(res
            .rows
            .iter()
            .map(|row| row_to_agent_config(&res.columns, row))
            .collect())
    }

    /// 删除 Agent 运行时配置（删除 Agent 时级联清理用）。
    pub async fn remove_agent_config(&self, agent_id: &str) -> Result<(), GseError> {
        self.execute(
            "DELETE FROM agent_configs WHERE agent_id = ?",
            &[text(agent_id)],
        )
        .await?;
        Ok(())
    }
}

// ---- SqlValue 构造与行转换辅助 ----

fn text(v: &str) -> SqlValue {
    SqlValue::Text(v.to_string())
}

fn opt_int(v: Option<i64>) -> SqlValue {
    v.map(SqlValue::Integer).unwrap_or(SqlValue::Null)
}

fn opt_i32(v: Option<i32>) -> SqlValue {
    v.map(|i| SqlValue::Integer(i64::from(i)))
        .unwrap_or(SqlValue::Null)
}

fn sql_params_host(h: &Host) -> Vec<SqlValue> {
    vec![
        text(&h.host_id),
        text(&h.inner_ip),
        text(&h.hostname),
        text(&h.os_type),
        text(&h.os_version),
        text(&h.cpu_spec),
        text(&h.mem_spec),
        text(&h.created_at),
    ]
}

fn sql_params_agent(a: &Agent) -> Vec<SqlValue> {
    let access_point_id = a
        .access_point_id
        .as_deref()
        .map(text)
        .unwrap_or(SqlValue::Null);
    let last_heartbeat_at = a
        .last_heartbeat_at
        .as_deref()
        .map(text)
        .unwrap_or(SqlValue::Null);
    vec![
        text(&a.agent_id),
        text(&a.host_id),
        access_point_id,
        text(&a.token),
        text(&a.version),
        text(&a.install_path),
        text(&a.status),
        last_heartbeat_at,
        text(&a.registered_at),
    ]
}

fn field<'a>(columns: &[String], row: &'a [SqlValue], name: &str) -> &'a SqlValue {
    let idx = columns
        .iter()
        .position(|c| c == name)
        .expect("column present");
    &row[idx]
}

fn field_text(columns: &[String], row: &[SqlValue], name: &str) -> String {
    match field(columns, row, name) {
        SqlValue::Text(s) => s.clone(),
        _ => String::new(),
    }
}

fn field_opt_text(columns: &[String], row: &[SqlValue], name: &str) -> Option<String> {
    match field(columns, row, name) {
        SqlValue::Null => None,
        SqlValue::Text(s) => Some(s.clone()),
        _ => None,
    }
}

fn field_i64(columns: &[String], row: &[SqlValue], name: &str) -> i64 {
    match field(columns, row, name) {
        SqlValue::Integer(i) => *i,
        _ => 0,
    }
}

fn field_opt_i64(columns: &[String], row: &[SqlValue], name: &str) -> Option<i64> {
    match field(columns, row, name) {
        SqlValue::Null => None,
        SqlValue::Integer(i) => Some(*i),
        _ => None,
    }
}

fn row_to_host(columns: &[String], row: &[SqlValue]) -> Host {
    Host {
        host_id: field_text(columns, row, "host_id"),
        inner_ip: field_text(columns, row, "inner_ip"),
        hostname: field_text(columns, row, "hostname"),
        os_type: field_text(columns, row, "os_type"),
        os_version: field_text(columns, row, "os_version"),
        cpu_spec: field_text(columns, row, "cpu_spec"),
        mem_spec: field_text(columns, row, "mem_spec"),
        created_at: field_text(columns, row, "created_at"),
    }
}

fn row_to_access_point(columns: &[String], row: &[SqlValue]) -> AccessPoint {
    AccessPoint {
        id: field_text(columns, row, "id"),
        name: field_text(columns, row, "name"),
        server_ip: field_text(columns, row, "server_ip"),
        rpc_port: field_i64(columns, row, "rpc_port") as i32,
        file_port: field_opt_i64(columns, row, "file_port").map(|v| v as i32),
        data_port: field_opt_i64(columns, row, "data_port").map(|v| v as i32),
        created_at: field_text(columns, row, "created_at"),
    }
}

fn row_to_agent(columns: &[String], row: &[SqlValue]) -> Agent {
    Agent {
        agent_id: field_text(columns, row, "agent_id"),
        host_id: field_text(columns, row, "host_id"),
        access_point_id: field_opt_text(columns, row, "access_point_id"),
        token: field_text(columns, row, "token"),
        version: field_text(columns, row, "version"),
        install_path: field_text(columns, row, "install_path"),
        status: field_text(columns, row, "status"),
        last_heartbeat_at: field_opt_text(columns, row, "last_heartbeat_at"),
        registered_at: field_text(columns, row, "registered_at"),
    }
}

fn row_to_agent_config(columns: &[String], row: &[SqlValue]) -> AgentConfig {
    AgentConfig {
        agent_id: field_text(columns, row, "agent_id"),
        host_id: field_text(columns, row, "host_id"),
        cpu_limit_percent: field_opt_i64(columns, row, "cpu_limit_percent"),
        mem_limit_percent: field_opt_i64(columns, row, "mem_limit_percent"),
        log_level: field_text(columns, row, "log_level"),
        updated_at: field_text(columns, row, "updated_at"),
    }
}

fn seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// 生成唯一时间戳字符串（微秒 + 序列号）。
pub fn ledger_stamp() -> String {
    format!("{}-{}", crate::session::now_micros(), seq())
}

#[cfg(test)]
fn test_db(name: &str) -> String {
    std::env::temp_dir()
        .join(format!("gse-ledger-{}-{name}.db", std::process::id()))
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(id: &str) -> Host {
        Host {
            host_id: id.to_string(),
            inner_ip: "10.0.0.1".to_string(),
            hostname: "web-1".to_string(),
            os_type: "linux".to_string(),
            os_version: "ubuntu-24.04".to_string(),
            cpu_spec: "8c".to_string(),
            mem_spec: "16g".to_string(),
            created_at: ledger_stamp(),
        }
    }

    fn agent(id: &str, token: &str) -> Agent {
        Agent {
            agent_id: id.to_string(),
            host_id: "h-1".to_string(),
            access_point_id: None,
            token: token.to_string(),
            version: "0.1.0".to_string(),
            install_path: "/opt/gse".to_string(),
            status: "unknown".to_string(),
            last_heartbeat_at: None,
            registered_at: ledger_stamp(),
        }
    }

    async fn fresh_ledger(name: &str) -> Ledger {
        let db = test_db(name);
        let ledger = Ledger::new(&db).expect("open");
        ledger.init().await.expect("init");
        ledger
    }

    #[tokio::test]
    async fn init_is_idempotent() {
        let ledger = fresh_ledger("init-idemp").await;
        ledger.init().await.expect("second init");
    }

    #[tokio::test]
    async fn host_crud_roundtrip() {
        let ledger = fresh_ledger("host-crud").await;
        ledger.upsert_host(&host("h-1")).await.expect("upsert");
        let got = ledger.get_host("h-1").await.expect("get").expect("exists");
        assert_eq!(got.inner_ip, "10.0.0.1");
        assert_eq!(got.hostname, "web-1");
        assert_eq!(ledger.list_hosts().await.expect("list").len(), 1);
        ledger.remove_host("h-1").await.expect("remove");
        assert!(ledger.get_host("h-1").await.expect("get").is_none());
    }

    #[tokio::test]
    async fn host_upsert_overwrites_same_pk() {
        let ledger = fresh_ledger("host-upsert").await;
        ledger.upsert_host(&host("h-1")).await.expect("first");
        let mut h2 = host("h-1");
        h2.inner_ip = "10.0.0.2".to_string();
        ledger.upsert_host(&h2).await.expect("second");
        let got = ledger.get_host("h-1").await.expect("get").expect("exists");
        assert_eq!(got.inner_ip, "10.0.0.2");
        assert_eq!(ledger.list_hosts().await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn access_point_crud_roundtrip() {
        let ledger = fresh_ledger("ap-crud").await;
        let ap = AccessPoint {
            id: "ap-1".to_string(),
            name: "main".to_string(),
            server_ip: "192.168.1.10".to_string(),
            rpc_port: 7100,
            file_port: Some(7102),
            data_port: None,
            created_at: ledger_stamp(),
        };
        ledger.upsert_access_point(&ap).await.expect("upsert");
        let got = ledger
            .get_access_point("ap-1")
            .await
            .expect("get")
            .expect("exists");
        assert_eq!(got.rpc_port, 7100);
        assert_eq!(got.file_port, Some(7102));
        assert_eq!(got.data_port, None);
        ledger.upsert_access_point(&ap).await.expect("re-upsert");
        assert_eq!(ledger.list_access_points().await.expect("list").len(), 1);
        ledger.remove_access_point("ap-1").await.expect("remove");
        assert!(ledger
            .get_access_point("ap-1")
            .await
            .expect("get")
            .is_none());
    }

    #[tokio::test]
    async fn agent_crud_roundtrip() {
        let ledger = fresh_ledger("agent-crud").await;
        ledger
            .upsert_agent(&agent("a-1", "tok-a"))
            .await
            .expect("upsert");
        let got = ledger.get_agent("a-1").await.expect("get").expect("exists");
        assert_eq!(got.token, "tok-a");
        assert_eq!(got.status, "unknown");
        ledger
            .upsert_agent(&agent("a-1", "tok-b"))
            .await
            .expect("update");
        assert_eq!(
            ledger
                .get_agent("a-1")
                .await
                .expect("get")
                .expect("exists")
                .token,
            "tok-b"
        );
        assert_eq!(ledger.list_agents().await.expect("list").len(), 1);
        ledger.remove_agent("a-1").await.expect("remove");
        assert!(ledger.get_agent("a-1").await.expect("get").is_none());
    }

    #[tokio::test]
    async fn agent_config_crud_roundtrip() {
        let ledger = fresh_ledger("acfg-crud").await;
        let cfg = AgentConfig {
            agent_id: "a-1".to_string(),
            host_id: "h-1".to_string(),
            cpu_limit_percent: Some(50),
            mem_limit_percent: None,
            log_level: "warn".to_string(),
            updated_at: ledger_stamp(),
        };
        ledger.upsert_agent_config(&cfg).await.expect("upsert");
        let got = ledger
            .get_agent_config("a-1")
            .await
            .expect("get")
            .expect("exists");
        assert_eq!(got.cpu_limit_percent, Some(50));
        assert_eq!(got.mem_limit_percent, None);
        assert_eq!(got.log_level, "warn");
        ledger.upsert_agent_config(&cfg).await.expect("re-upsert");
        assert_eq!(ledger.list_agent_configs().await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn check_auth_three_states() {
        let ledger = fresh_ledger("auth").await;
        ledger
            .upsert_agent(&agent("a-1", "tok-a"))
            .await
            .expect("register");
        assert!(ledger
            .check_auth("a-1", "tok-a")
            .await
            .expect("registered ok"));
        assert!(!ledger.check_auth("a-1", "wrong").await.expect("bad token"));
        assert!(!ledger
            .check_auth("ghost", "tok-a")
            .await
            .expect("unknown id"));
    }

    #[tokio::test]
    async fn runtime_state_transitions() {
        let ledger = fresh_ledger("runtime").await;
        ledger
            .upsert_agent(&agent("a-1", "tok-a"))
            .await
            .expect("register");
        assert_eq!(
            ledger
                .get_agent("a-1")
                .await
                .expect("get")
                .expect("exists")
                .status,
            "unknown"
        );
        ledger.mark_online("a-1", "100").await.expect("online");
        let online = ledger.get_agent("a-1").await.expect("get").expect("exists");
        assert_eq!(online.status, "online");
        assert_eq!(online.last_heartbeat_at.as_deref(), Some("100"));
        ledger
            .mark_heartbeat("a-1", "200")
            .await
            .expect("heartbeat");
        assert_eq!(
            ledger
                .get_agent("a-1")
                .await
                .expect("get")
                .expect("exists")
                .last_heartbeat_at
                .as_deref(),
            Some("200")
        );
        ledger.mark_offline("a-1").await.expect("offline");
        let offline = ledger.get_agent("a-1").await.expect("get").expect("exists");
        assert_eq!(offline.status, "offline");
        assert_eq!(offline.last_heartbeat_at.as_deref(), Some("200"));
    }
}
