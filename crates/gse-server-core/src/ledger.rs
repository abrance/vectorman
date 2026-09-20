//! 台账（Ledger）：SQLite 资产的语义化封装。
//!
//! hosts / access_points / agents / agent_configs / jobs 均为 sqlite 持久化表，
//! 以自然键为主键幂等 upsert；本模块向会话层与 HTTP 层提供统一的增删改查。

use std::collections::BTreeMap;

use dataplane_core::{DataplaneError, SqlValue};
use dataplane_sql::{RelationalStore, SqliteRelationalStore};
use gse_proto::{FileEndpoint, GseError, JobResult, JobStatus};
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

/// 数据面服务登记（运维手工 upsert，探活维护 status）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataplaneService {
    pub service_id: String,
    pub ingest_url: String,
    pub query_url: String,
    /// unknown / online / offline。
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub last_seen_at: Option<String>,
    #[serde(default)]
    pub registered_at: String,
}

/// 采集项：以 `item_id` 为主键，`agent_ids` 为目标 Agent 列表。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectItem {
    pub item_id: String,
    pub agent_ids: Vec<String>,
    pub name: String,
    /// metrics_host | log_file | log_k8s_stdout。
    pub kind: String,
    pub enabled: bool,
    pub collector: serde_json::Value,
    pub storage: serde_json::Value,
    #[serde(default)]
    pub updated_at: String,
}

impl CollectItem {
    /// 转为跨 RPC 下发的 DTO（不含本地 `updated_at`）。
    pub fn to_proto(&self) -> gse_proto::CollectItem {
        gse_proto::CollectItem {
            item_id: self.item_id.clone(),
            agent_ids: self.agent_ids.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
            enabled: self.enabled,
            collector: self.collector.clone(),
            storage: self.storage.clone(),
        }
    }
}

/// 作业持久化记录；字段与前端 `Job` 类型对齐。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRecord {
    pub job_id: String,
    pub agent_id: String,
    pub interpreter: String,
    pub script: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    /// 由模板提交时记录的来源模板；手工提交为 None。
    #[serde(default)]
    pub template_id: Option<String>,
    /// 重做时记录的来源作业 job_id；非重做作业为 None。
    #[serde(default)]
    pub rerun_of: Option<String>,
    pub timeout_secs: u64,
    pub status: JobStatus,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub signal: Option<i32>,
    #[serde(default)]
    pub stdout: Option<String>,
    #[serde(default)]
    pub stdout_truncated: bool,
    #[serde(default)]
    pub stderr: Option<String>,
    #[serde(default)]
    pub stderr_truncated: bool,
    #[serde(default)]
    pub error: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub dispatched_at: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    pub updated_at: String,
    /// script（缺省）或 file_transfer。
    #[serde(default = "default_job_kind")]
    pub kind: String,
    #[serde(default)]
    pub source: Option<FileEndpoint>,
    #[serde(default)]
    pub destination: Option<FileEndpoint>,
    #[serde(default)]
    pub source_agent_id: Option<String>,
    #[serde(default)]
    pub dest_agent_id: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub file_bytes: Option<i64>,
    #[serde(default)]
    pub file_sha256: Option<String>,
    #[serde(default)]
    pub file_id: Option<String>,
}

/// 新作业的提交参数；由 server 层在受理时构造。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NewJob {
    pub job_id: String,
    pub agent_id: String,
    pub interpreter: String,
    pub script: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub working_dir: Option<String>,
    pub template_id: Option<String>,
    pub rerun_of: Option<String>,
    pub timeout_secs: u64,
    pub created_at: String,
    pub kind: String,
    pub source: Option<FileEndpoint>,
    pub destination: Option<FileEndpoint>,
    pub source_agent_id: Option<String>,
    pub dest_agent_id: Option<String>,
    pub file_name: Option<String>,
    pub file_bytes: Option<i64>,
    pub file_sha256: Option<String>,
    pub file_id: Option<String>,
}

fn default_job_kind() -> String {
    "script".to_string()
}

/// 作业模板；由 server 层在受理创建时构造。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobTemplate {
    pub template_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub interpreter: String,
    pub script: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    pub timeout_secs: u64,
    pub created_at: String,
    pub updated_at: String,
}

/// 创建/更新作业模板的输入字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewJobTemplate {
    pub name: String,
    pub description: Option<String>,
    pub interpreter: String,
    pub script: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub working_dir: Option<String>,
    pub timeout_secs: u64,
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
            "CREATE TABLE IF NOT EXISTS jobs (
                job_id            TEXT PRIMARY KEY,
                agent_id          TEXT NOT NULL,
                interpreter       TEXT NOT NULL,
                script            TEXT NOT NULL,
                args              TEXT NOT NULL DEFAULT '[]',
                env               TEXT NOT NULL DEFAULT '{}',
                working_dir       TEXT,
                template_id       TEXT,
                rerun_of          TEXT,
                timeout_secs      INTEGER NOT NULL,
                status            TEXT NOT NULL,
                exit_code         INTEGER,
                signal            INTEGER,
                stdout            TEXT,
                stdout_truncated  INTEGER NOT NULL DEFAULT 0,
                stderr            TEXT,
                stderr_truncated  INTEGER NOT NULL DEFAULT 0,
                error             TEXT,
                created_at        TEXT NOT NULL,
                dispatched_at     TEXT,
                started_at        TEXT,
                finished_at       TEXT,
                updated_at        TEXT NOT NULL,
                kind              TEXT NOT NULL DEFAULT 'script',
                source_json       TEXT,
                dest_json         TEXT,
                file_name         TEXT,
                file_bytes        INTEGER,
                file_sha256       TEXT,
                file_id           TEXT,
                source_agent_id   TEXT,
                dest_agent_id     TEXT
            )",
            "CREATE INDEX IF NOT EXISTS idx_jobs_agent ON jobs(agent_id, created_at)",
            "CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status)",
            "CREATE TABLE IF NOT EXISTS job_templates (
                template_id    TEXT PRIMARY KEY,
                name           TEXT NOT NULL UNIQUE,
                description    TEXT,
                interpreter    TEXT NOT NULL,
                script         TEXT NOT NULL,
                args           TEXT NOT NULL DEFAULT '[]',
                env            TEXT NOT NULL DEFAULT '{}',
                working_dir    TEXT,
                timeout_secs   INTEGER NOT NULL,
                created_at     TEXT NOT NULL,
                updated_at     TEXT NOT NULL
            )",
            "CREATE INDEX IF NOT EXISTS idx_templates_name ON job_templates(name)",
            "CREATE TABLE IF NOT EXISTS dataplane_services (
                service_id    TEXT PRIMARY KEY,
                ingest_url    TEXT NOT NULL,
                query_url     TEXT NOT NULL,
                status        TEXT NOT NULL DEFAULT 'unknown',
                last_seen_at  TEXT,
                registered_at TEXT NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS collect_items (
                item_id        TEXT PRIMARY KEY,
                agent_ids      TEXT NOT NULL,
                name           TEXT NOT NULL,
                kind           TEXT NOT NULL,
                enabled        INTEGER NOT NULL DEFAULT 1,
                collector_json TEXT NOT NULL,
                storage_json   TEXT NOT NULL,
                updated_at     TEXT NOT NULL
            )",
        ];
        for sql in ddl {
            self.store
                .execute(sql, &[])
                .await
                .map_err(|e| format!("init ledger: {}", e.message))?;
        }
        self.migrate_jobs_template_id().await?;
        self.migrate_jobs_rerun_of().await?;
        self.migrate_jobs_file_transfer().await?;
        Ok(())
    }

    /// 兼容旧库：`jobs` 表缺失 `template_id` 列时补齐，重复调用幂等。
    async fn migrate_jobs_template_id(&self) -> Result<(), String> {
        let info = self
            .store
            .execute("PRAGMA table_info(jobs)", &[])
            .await
            .map_err(|e| format!("init ledger: {}", e.message))?;
        let present = info
            .rows
            .iter()
            .any(|row| field_text(&info.columns, row, "name") == "template_id");
        if !present {
            self.store
                .execute("ALTER TABLE jobs ADD COLUMN template_id TEXT", &[])
                .await
                .map_err(|e| format!("init ledger: {}", e.message))?;
        }
        Ok(())
    }

    /// 兼容旧库：`jobs` 表缺失 `rerun_of` 列时补齐，重复调用幂等。
    async fn migrate_jobs_rerun_of(&self) -> Result<(), String> {
        let info = self
            .store
            .execute("PRAGMA table_info(jobs)", &[])
            .await
            .map_err(|e| format!("init ledger: {}", e.message))?;
        let present = info
            .rows
            .iter()
            .any(|row| field_text(&info.columns, row, "name") == "rerun_of");
        if !present {
            self.store
                .execute("ALTER TABLE jobs ADD COLUMN rerun_of TEXT", &[])
                .await
                .map_err(|e| format!("init ledger: {}", e.message))?;
        }
        Ok(())
    }

    /// 兼容旧库：补齐文件传输列，重复调用幂等。
    async fn migrate_jobs_file_transfer(&self) -> Result<(), String> {
        let info = self
            .store
            .execute("PRAGMA table_info(jobs)", &[])
            .await
            .map_err(|e| format!("init ledger: {}", e.message))?;
        let present: std::collections::HashSet<String> = info
            .rows
            .iter()
            .map(|row| field_text(&info.columns, row, "name"))
            .collect();
        let wanted = [
            ("kind", "TEXT NOT NULL DEFAULT 'script'"),
            ("source_json", "TEXT"),
            ("dest_json", "TEXT"),
            ("file_name", "TEXT"),
            ("file_bytes", "INTEGER"),
            ("file_sha256", "TEXT"),
            ("file_id", "TEXT"),
            ("source_agent_id", "TEXT"),
            ("dest_agent_id", "TEXT"),
        ];
        for (name, decl) in wanted {
            if present.contains(name) {
                continue;
            }
            self.store
                .execute(&format!("ALTER TABLE jobs ADD COLUMN {name} {decl}"), &[])
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

    // ---- dataplane_services ----

    /// 以 service_id 为主键幂等登记数据面；重置为 `unknown` 等待探活。
    pub async fn upsert_dataplane(&self, d: &DataplaneService) -> Result<(), GseError> {
        let sql = "INSERT INTO dataplane_services (service_id, ingest_url, query_url, status, last_seen_at, registered_at)
                   VALUES (?, ?, ?, 'unknown', ?, ?)
                   ON CONFLICT(service_id) DO UPDATE SET
                     ingest_url = excluded.ingest_url,
                     query_url = excluded.query_url,
                     status = 'unknown',
                     last_seen_at = excluded.last_seen_at,
                     registered_at = excluded.registered_at";
        let params = vec![
            text(&d.service_id),
            text(&d.ingest_url),
            text(&d.query_url),
            opt_text(d.last_seen_at.as_deref()),
            text(&d.registered_at),
        ];
        self.execute(sql, &params).await?;
        Ok(())
    }

    pub async fn list_dataplanes(&self) -> Result<Vec<DataplaneService>, GseError> {
        let res = self
            .execute("SELECT * FROM dataplane_services ORDER BY service_id", &[])
            .await?;
        Ok(res
            .rows
            .iter()
            .map(|row| row_to_dataplane(&res.columns, row))
            .collect())
    }

    pub async fn get_dataplane(
        &self,
        service_id: &str,
    ) -> Result<Option<DataplaneService>, GseError> {
        let res = self
            .execute(
                "SELECT * FROM dataplane_services WHERE service_id = ?",
                &[text(service_id)],
            )
            .await?;
        Ok(res
            .rows
            .first()
            .map(|row| row_to_dataplane(&res.columns, row)))
    }

    pub async fn delete_dataplane(&self, service_id: &str) -> Result<(), GseError> {
        self.execute(
            "DELETE FROM dataplane_services WHERE service_id = ?",
            &[text(service_id)],
        )
        .await?;
        Ok(())
    }

    /// 探活回写：`online` 记录 last_seen_at，其余置 `offline`。
    pub async fn set_dataplane_status(
        &self,
        service_id: &str,
        status: &str,
        last_seen_at: Option<&str>,
    ) -> Result<(), GseError> {
        self.execute(
            "UPDATE dataplane_services SET status = ?, last_seen_at = ? WHERE service_id = ?",
            &[text(status), opt_text(last_seen_at), text(service_id)],
        )
        .await?;
        Ok(())
    }

    /// 在 `online` 集合中按 `service_id` 字典序取模选一条 `ingest_url`。
    pub async fn pick_ingest_url(&self, agent_id: &str) -> Result<Option<String>, GseError> {
        let all = self.list_dataplanes().await?;
        let online: Vec<DataplaneService> =
            all.into_iter().filter(|d| d.status == "online").collect();
        if online.is_empty() {
            return Ok(None);
        }
        let idx = (hash_agent(agent_id) as usize) % online.len();
        Ok(Some(online[idx].ingest_url.clone()))
    }

    // ---- collect_items ----

    /// 以 `item_id` 为主键幂等保存采集项。
    pub async fn upsert_collect_item(&self, item: &CollectItem) -> Result<(), GseError> {
        let agent_ids = serde_json::to_string(&item.agent_ids)
            .map_err(|e| GseError::new("invalid_argument", e.to_string()))?;
        let collector = serde_json::to_string(&item.collector)
            .map_err(|e| GseError::new("invalid_argument", e.to_string()))?;
        let storage = serde_json::to_string(&item.storage)
            .map_err(|e| GseError::new("invalid_argument", e.to_string()))?;
        let sql = "INSERT INTO collect_items (item_id, agent_ids, name, kind, enabled, collector_json, storage_json, updated_at)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(item_id) DO UPDATE SET
                     agent_ids = excluded.agent_ids,
                     name = excluded.name,
                     kind = excluded.kind,
                     enabled = excluded.enabled,
                     collector_json = excluded.collector_json,
                     storage_json = excluded.storage_json,
                     updated_at = excluded.updated_at";
        self.execute(
            sql,
            &[
                text(&item.item_id),
                SqlValue::Text(agent_ids),
                text(&item.name),
                text(&item.kind),
                bool_int(item.enabled),
                SqlValue::Text(collector),
                SqlValue::Text(storage),
                text(&item.updated_at),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn get_collect_item(&self, item_id: &str) -> Result<Option<CollectItem>, GseError> {
        let res = self
            .execute(
                "SELECT * FROM collect_items WHERE item_id = ?",
                &[text(item_id)],
            )
            .await?;
        Ok(res
            .rows
            .first()
            .map(|row| row_to_collect_item(&res.columns, row)))
    }

    /// 列出采集项，最新更新在前；`agent_id` 存在时仅返回目标列表包含它的项。
    pub async fn list_collect_items(
        &self,
        agent_id: Option<&str>,
    ) -> Result<Vec<CollectItem>, GseError> {
        let res = self
            .execute("SELECT * FROM collect_items ORDER BY updated_at DESC", &[])
            .await?;
        Ok(res
            .rows
            .iter()
            .map(|row| row_to_collect_item(&res.columns, row))
            .filter(|item| match agent_id {
                Some(id) => item.agent_ids.iter().any(|a| a == id),
                None => true,
            })
            .collect())
    }

    /// 删除采集项；返回是否存在并删除。
    pub async fn delete_collect_item(&self, item_id: &str) -> Result<bool, GseError> {
        if self.get_collect_item(item_id).await?.is_none() {
            return Ok(false);
        }
        self.execute(
            "DELETE FROM collect_items WHERE item_id = ?",
            &[text(item_id)],
        )
        .await?;
        Ok(true)
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

    // ---- jobs ----

    /// 插入一条 `pending` 作业。
    pub async fn insert_job(&self, job: &NewJob) -> Result<(), GseError> {
        let args = serde_json::to_string(&job.args)
            .map_err(|e| GseError::new("invalid_argument", e.to_string()))?;
        let env = serde_json::to_string(&job.env)
            .map_err(|e| GseError::new("invalid_argument", e.to_string()))?;
        let kind = if job.kind.trim().is_empty() {
            "script"
        } else {
            job.kind.as_str()
        };
        let source_json = opt_json(&job.source)?;
        let dest_json = opt_json(&job.destination)?;
        let sql = "INSERT INTO jobs (
                       job_id, agent_id, interpreter, script, args, env, working_dir, template_id,
                       rerun_of,
                       timeout_secs, status, stdout_truncated, stderr_truncated,
                       created_at, updated_at,
                       kind, source_json, dest_json, file_name, file_bytes, file_sha256, file_id,
                       source_agent_id, dest_agent_id
                   ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', 0, 0, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
        self.execute(
            sql,
            &[
                text(&job.job_id),
                text(&job.agent_id),
                text(&job.interpreter),
                text(&job.script),
                SqlValue::Text(args),
                SqlValue::Text(env),
                job.working_dir
                    .as_deref()
                    .map(text)
                    .unwrap_or(SqlValue::Null),
                job.template_id
                    .as_deref()
                    .map(text)
                    .unwrap_or(SqlValue::Null),
                job.rerun_of.as_deref().map(text).unwrap_or(SqlValue::Null),
                SqlValue::Integer(job.timeout_secs as i64),
                text(&job.created_at),
                text(&job.created_at),
                text(kind),
                source_json,
                dest_json,
                job.file_name.as_deref().map(text).unwrap_or(SqlValue::Null),
                job.file_bytes
                    .map(SqlValue::Integer)
                    .unwrap_or(SqlValue::Null),
                job.file_sha256
                    .as_deref()
                    .map(text)
                    .unwrap_or(SqlValue::Null),
                job.file_id.as_deref().map(text).unwrap_or(SqlValue::Null),
                job.source_agent_id
                    .as_deref()
                    .map(text)
                    .unwrap_or(SqlValue::Null),
                job.dest_agent_id
                    .as_deref()
                    .map(text)
                    .unwrap_or(SqlValue::Null),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn get_job(&self, job_id: &str) -> Result<Option<JobRecord>, GseError> {
        let res = self
            .execute("SELECT * FROM jobs WHERE job_id = ?", &[text(job_id)])
            .await?;
        Ok(res.rows.first().map(|row| row_to_job(&res.columns, row)))
    }

    /// 列出作业，可按 agent_id 与状态筛选；limit 默认 200，上限 1000。
    pub async fn list_jobs(
        &self,
        agent_id: Option<&str>,
        status: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<JobRecord>, GseError> {
        let mut sql = String::from("SELECT * FROM jobs");
        let mut conditions: Vec<&str> = Vec::new();
        let mut params: Vec<SqlValue> = Vec::new();
        if let Some(a) = agent_id {
            conditions.push("(agent_id = ? OR source_agent_id = ? OR dest_agent_id = ?)");
            params.push(text(a));
            params.push(text(a));
            params.push(text(a));
        }
        if let Some(s) = status {
            conditions.push("status = ?");
            params.push(text(s));
        }
        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT ?");
        params.push(SqlValue::Integer(limit.unwrap_or(200).clamp(1, 1000)));
        let res = self.execute(&sql, &params).await?;
        Ok(res
            .rows
            .iter()
            .map(|row| row_to_job(&res.columns, row))
            .collect())
    }

    /// 标记受理成功进入运行态；已终态作业不变。
    pub async fn mark_running(&self, job_id: &str, started_at: &str) -> Result<(), GseError> {
        let sql = "UPDATE jobs SET status = 'running', started_at = ?, updated_at = ?
                   WHERE job_id = ? AND status NOT IN ('succeeded', 'failed', 'timeout', 'rejected', 'lost')";
        self.execute(sql, &[text(started_at), text(started_at), text(job_id)])
            .await?;
        Ok(())
    }

    /// 标记文件作业已进入后台编排；已终态作业不变。
    pub async fn mark_dispatched(&self, job_id: &str, dispatched_at: &str) -> Result<(), GseError> {
        let sql = "UPDATE jobs SET status = 'dispatched', dispatched_at = COALESCE(dispatched_at, ?), updated_at = ?
                   WHERE job_id = ? AND status NOT IN ('succeeded', 'failed', 'timeout', 'rejected', 'lost')";
        self.execute(
            sql,
            &[text(dispatched_at), text(dispatched_at), text(job_id)],
        )
        .await?;
        Ok(())
    }

    /// 写入作业终态；已终态作业保留首次结果。
    pub async fn finish_job(&self, result: &JobResult, finished_at: &str) -> Result<(), GseError> {
        let started_at = if result.started_at_micros > 0 {
            SqlValue::Text(result.started_at_micros.to_string())
        } else {
            SqlValue::Null
        };
        let sql = "UPDATE jobs SET
                       status = ?,
                       exit_code = ?,
                       signal = ?,
                       stdout = ?,
                       stdout_truncated = ?,
                       stderr = ?,
                       stderr_truncated = ?,
                       error = ?,
                       started_at = COALESCE(?, started_at),
                       finished_at = ?,
                       updated_at = ?
                   WHERE job_id = ?
                     AND status NOT IN ('succeeded', 'failed', 'timeout', 'rejected', 'lost')";
        self.execute(
            sql,
            &[
                text(result.status.as_str()),
                opt_i32(result.exit_code),
                opt_i32(result.signal),
                SqlValue::Text(result.stdout.clone()),
                bool_int(result.stdout_truncated),
                SqlValue::Text(result.stderr.clone()),
                bool_int(result.stderr_truncated),
                result.error.as_deref().map(text).unwrap_or(SqlValue::Null),
                started_at,
                text(finished_at),
                text(finished_at),
                text(&result.job_id),
            ],
        )
        .await?;
        Ok(())
    }

    /// 回写文件作业结果字段；可在终态前后调用。
    pub async fn set_job_file_meta(
        &self,
        job_id: &str,
        file_name: Option<&str>,
        file_bytes: Option<i64>,
        file_sha256: Option<&str>,
        file_id: Option<&str>,
    ) -> Result<(), GseError> {
        let sql = "UPDATE jobs SET file_name = ?, file_bytes = ?, file_sha256 = ?, file_id = ?, updated_at = ?
                   WHERE job_id = ?";
        self.execute(
            sql,
            &[
                file_name.map(text).unwrap_or(SqlValue::Null),
                file_bytes.map(SqlValue::Integer).unwrap_or(SqlValue::Null),
                file_sha256.map(text).unwrap_or(SqlValue::Null),
                file_id.map(text).unwrap_or(SqlValue::Null),
                text(&ledger_stamp()),
                text(job_id),
            ],
        )
        .await?;
        Ok(())
    }

    /// 标记作业被 Agent 拒绝受理；已终态作业不变。
    pub async fn mark_rejected(&self, job_id: &str, reason: &str) -> Result<(), GseError> {
        let sql = "UPDATE jobs SET status = 'rejected', error = ?, updated_at = ?
                   WHERE job_id = ? AND status NOT IN ('succeeded', 'failed', 'timeout', 'rejected', 'lost')";
        self.execute(sql, &[text(reason), text(&ledger_stamp()), text(job_id)])
            .await?;
        Ok(())
    }

    /// 将会话离线 Agent 的在途作业标记为 lost。
    pub async fn mark_lost_by_agent(&self, agent_id: &str) -> Result<(), GseError> {
        let sql = "UPDATE jobs SET status = 'lost', error = COALESCE(error, 'agent offline'), updated_at = ?
                   WHERE (agent_id = ? OR source_agent_id = ? OR dest_agent_id = ?)
                     AND status IN ('pending', 'dispatched', 'running')";
        let stamp = text(&ledger_stamp());
        self.execute(
            sql,
            &[stamp, text(agent_id), text(agent_id), text(agent_id)],
        )
        .await?;
        Ok(())
    }

    /// 服务启动时将所有在途作业标记为 lost。
    pub async fn mark_lost_inflight_on_startup(&self) -> Result<(), GseError> {
        let sql = "UPDATE jobs SET status = 'lost', error = COALESCE(error, 'server restarted'), updated_at = ?
                   WHERE status IN ('pending', 'dispatched', 'running')";
        self.execute(sql, &[text(&ledger_stamp())]).await?;
        Ok(())
    }

    // ---- job_templates ----

    /// 插入模板；名称冲突映射为 `already_exists`。
    pub async fn insert_template(
        &self,
        template_id: &str,
        t: &NewJobTemplate,
        now: &str,
    ) -> Result<(), GseError> {
        let args = serde_json::to_string(&t.args)
            .map_err(|e| GseError::new("invalid_argument", e.to_string()))?;
        let env = serde_json::to_string(&t.env)
            .map_err(|e| GseError::new("invalid_argument", e.to_string()))?;
        let sql = "INSERT INTO job_templates (
                       template_id, name, description, interpreter, script, args, env,
                       working_dir, timeout_secs, created_at, updated_at
                   ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
        let res = self
            .execute(
                sql,
                &[
                    text(template_id),
                    text(&t.name),
                    opt_text(t.description.as_deref()),
                    text(&t.interpreter),
                    text(&t.script),
                    SqlValue::Text(args),
                    SqlValue::Text(env),
                    opt_text(t.working_dir.as_deref()),
                    SqlValue::Integer(t.timeout_secs as i64),
                    text(now),
                    text(now),
                ],
            )
            .await;
        map_unique_conflict(res)
    }

    pub async fn get_template(&self, template_id: &str) -> Result<Option<JobTemplate>, GseError> {
        let res = self
            .execute(
                "SELECT * FROM job_templates WHERE template_id = ?",
                &[text(template_id)],
            )
            .await?;
        Ok(res
            .rows
            .first()
            .map(|row| row_to_template(&res.columns, row)))
    }

    pub async fn get_template_by_name(&self, name: &str) -> Result<Option<JobTemplate>, GseError> {
        let res = self
            .execute("SELECT * FROM job_templates WHERE name = ?", &[text(name)])
            .await?;
        Ok(res
            .rows
            .first()
            .map(|row| row_to_template(&res.columns, row)))
    }

    /// 列出模板，可按名称模糊筛选；limit 默认 200，上限 1000。
    pub async fn list_templates(
        &self,
        name: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<JobTemplate>, GseError> {
        let mut sql = String::from("SELECT * FROM job_templates");
        let mut params: Vec<SqlValue> = Vec::new();
        if let Some(n) = name {
            sql.push_str(" WHERE name LIKE ?");
            params.push(text(&format!("%{n}%")));
        }
        sql.push_str(" ORDER BY updated_at DESC LIMIT ?");
        params.push(SqlValue::Integer(limit.unwrap_or(200).clamp(1, 1000)));
        let res = self.execute(&sql, &params).await?;
        Ok(res
            .rows
            .iter()
            .map(|row| row_to_template(&res.columns, row))
            .collect())
    }

    /// 更新模板；名称冲突映射为 `already_exists`。
    pub async fn update_template(
        &self,
        template_id: &str,
        t: &NewJobTemplate,
        now: &str,
    ) -> Result<(), GseError> {
        let args = serde_json::to_string(&t.args)
            .map_err(|e| GseError::new("invalid_argument", e.to_string()))?;
        let env = serde_json::to_string(&t.env)
            .map_err(|e| GseError::new("invalid_argument", e.to_string()))?;
        let sql = "UPDATE job_templates SET
                       name = ?, description = ?, interpreter = ?, script = ?, args = ?, env = ?,
                       working_dir = ?, timeout_secs = ?, updated_at = ?
                   WHERE template_id = ?";
        let res = self
            .execute(
                sql,
                &[
                    text(&t.name),
                    opt_text(t.description.as_deref()),
                    text(&t.interpreter),
                    text(&t.script),
                    SqlValue::Text(args),
                    SqlValue::Text(env),
                    opt_text(t.working_dir.as_deref()),
                    SqlValue::Integer(t.timeout_secs as i64),
                    text(now),
                    text(template_id),
                ],
            )
            .await;
        map_unique_conflict(res)
    }

    /// 删除模板；返回是否存在并删除。
    pub async fn delete_template(&self, template_id: &str) -> Result<bool, GseError> {
        if self.get_template(template_id).await?.is_none() {
            return Ok(false);
        }
        self.execute(
            "DELETE FROM job_templates WHERE template_id = ?",
            &[text(template_id)],
        )
        .await?;
        Ok(true)
    }
}

// ---- SqlValue 构造与行转换辅助 ----

fn text(v: &str) -> SqlValue {
    SqlValue::Text(v.to_string())
}

fn opt_int(v: Option<i64>) -> SqlValue {
    v.map(SqlValue::Integer).unwrap_or(SqlValue::Null)
}

fn opt_text(v: Option<&str>) -> SqlValue {
    v.map(text).unwrap_or(SqlValue::Null)
}

/// 将唯一约束冲突（名称重复）转换为 `already_exists`，其余错误原样返回。
fn map_unique_conflict(res: Result<dataplane_core::SqlResult, GseError>) -> Result<(), GseError> {
    match res {
        Ok(_) => Ok(()),
        Err(e) if e.message.to_ascii_uppercase().contains("UNIQUE") => {
            Err(GseError::new("already_exists", e.message))
        }
        Err(e) => Err(e),
    }
}

fn opt_i32(v: Option<i32>) -> SqlValue {
    v.map(|i| SqlValue::Integer(i64::from(i)))
        .unwrap_or(SqlValue::Null)
}

/// 对 `agent_id` 字节做一次 `DefaultHasher`。
fn hash_agent(agent_id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    agent_id.as_bytes().hash(&mut hasher);
    hasher.finish()
}

fn bool_int(v: bool) -> SqlValue {
    SqlValue::Integer(if v { 1 } else { 0 })
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

fn opt_json<T: serde::Serialize>(v: &Option<T>) -> Result<SqlValue, GseError> {
    match v {
        None => Ok(SqlValue::Null),
        Some(val) => serde_json::to_string(val)
            .map(SqlValue::Text)
            .map_err(|e| GseError::new("invalid_argument", e.to_string())),
    }
}

fn parse_endpoint(raw: Option<String>) -> Option<FileEndpoint> {
    let s = raw.filter(|v| !v.is_empty())?;
    serde_json::from_str(&s).ok()
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

fn row_to_dataplane(columns: &[String], row: &[SqlValue]) -> DataplaneService {
    DataplaneService {
        service_id: field_text(columns, row, "service_id"),
        ingest_url: field_text(columns, row, "ingest_url"),
        query_url: field_text(columns, row, "query_url"),
        status: field_text(columns, row, "status"),
        last_seen_at: field_opt_text(columns, row, "last_seen_at"),
        registered_at: field_text(columns, row, "registered_at"),
    }
}

fn row_to_collect_item(columns: &[String], row: &[SqlValue]) -> CollectItem {
    let agent_ids: Vec<String> =
        serde_json::from_str(&field_text(columns, row, "agent_ids")).unwrap_or_default();
    let collector: serde_json::Value =
        serde_json::from_str(&field_text(columns, row, "collector_json"))
            .unwrap_or(serde_json::Value::Null);
    let storage: serde_json::Value =
        serde_json::from_str(&field_text(columns, row, "storage_json"))
            .unwrap_or(serde_json::Value::Null);
    CollectItem {
        item_id: field_text(columns, row, "item_id"),
        agent_ids,
        name: field_text(columns, row, "name"),
        kind: field_text(columns, row, "kind"),
        enabled: field_i64(columns, row, "enabled") != 0,
        collector,
        storage,
        updated_at: field_text(columns, row, "updated_at"),
    }
}

fn row_to_job(columns: &[String], row: &[SqlValue]) -> JobRecord {
    let status = JobStatus::parse(&field_text(columns, row, "status")).unwrap_or(JobStatus::Lost);
    let args: Vec<String> =
        serde_json::from_str(&field_text(columns, row, "args")).unwrap_or_default();
    let env: BTreeMap<String, String> =
        serde_json::from_str(&field_text(columns, row, "env")).unwrap_or_default();
    JobRecord {
        job_id: field_text(columns, row, "job_id"),
        agent_id: field_text(columns, row, "agent_id"),
        interpreter: field_text(columns, row, "interpreter"),
        script: field_text(columns, row, "script"),
        args,
        env,
        working_dir: field_opt_text(columns, row, "working_dir"),
        template_id: field_opt_text(columns, row, "template_id"),
        rerun_of: field_opt_text(columns, row, "rerun_of"),
        timeout_secs: field_i64(columns, row, "timeout_secs").max(0) as u64,
        status,
        exit_code: field_opt_i64(columns, row, "exit_code").map(|v| v as i32),
        signal: field_opt_i64(columns, row, "signal").map(|v| v as i32),
        stdout: field_opt_text(columns, row, "stdout"),
        stdout_truncated: field_i64(columns, row, "stdout_truncated") != 0,
        stderr: field_opt_text(columns, row, "stderr"),
        stderr_truncated: field_i64(columns, row, "stderr_truncated") != 0,
        error: field_opt_text(columns, row, "error"),
        created_at: field_text(columns, row, "created_at"),
        dispatched_at: field_opt_text(columns, row, "dispatched_at"),
        started_at: field_opt_text(columns, row, "started_at"),
        finished_at: field_opt_text(columns, row, "finished_at"),
        updated_at: field_text(columns, row, "updated_at"),
        kind: {
            let k = field_text(columns, row, "kind");
            if k.is_empty() {
                default_job_kind()
            } else {
                k
            }
        },
        source: parse_endpoint(field_opt_text(columns, row, "source_json")),
        destination: parse_endpoint(field_opt_text(columns, row, "dest_json")),
        source_agent_id: field_opt_text(columns, row, "source_agent_id"),
        dest_agent_id: field_opt_text(columns, row, "dest_agent_id"),
        file_name: field_opt_text(columns, row, "file_name"),
        file_bytes: field_opt_i64(columns, row, "file_bytes"),
        file_sha256: field_opt_text(columns, row, "file_sha256"),
        file_id: field_opt_text(columns, row, "file_id"),
    }
}

fn row_to_template(columns: &[String], row: &[SqlValue]) -> JobTemplate {
    let args: Vec<String> =
        serde_json::from_str(&field_text(columns, row, "args")).unwrap_or_default();
    let env: BTreeMap<String, String> =
        serde_json::from_str(&field_text(columns, row, "env")).unwrap_or_default();
    JobTemplate {
        template_id: field_text(columns, row, "template_id"),
        name: field_text(columns, row, "name"),
        description: field_opt_text(columns, row, "description"),
        interpreter: field_text(columns, row, "interpreter"),
        script: field_text(columns, row, "script"),
        args,
        env,
        working_dir: field_opt_text(columns, row, "working_dir"),
        timeout_secs: field_i64(columns, row, "timeout_secs").max(0) as u64,
        created_at: field_text(columns, row, "created_at"),
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

    fn dataplane(id: &str, ingest_url: &str, status: &str) -> DataplaneService {
        DataplaneService {
            service_id: id.to_string(),
            ingest_url: ingest_url.to_string(),
            query_url: ingest_url.to_string(),
            status: status.to_string(),
            last_seen_at: None,
            registered_at: ledger_stamp(),
        }
    }

    #[tokio::test]
    async fn dataplane_upsert_resets_status_and_crud() {
        let ledger = fresh_ledger("dp-crud").await;
        ledger
            .upsert_dataplane(&dataplane("ds-1", "http://10.0.0.5:8081", "online"))
            .await
            .expect("upsert");
        let got = ledger.get_dataplane("ds-1").await.unwrap().unwrap();
        assert_eq!(got.status, "unknown");
        assert_eq!(got.ingest_url, "http://10.0.0.5:8081");
        assert_eq!(ledger.list_dataplanes().await.unwrap().len(), 1);

        ledger
            .set_dataplane_status("ds-1", "online", Some("t1"))
            .await
            .unwrap();
        let got = ledger.get_dataplane("ds-1").await.unwrap().unwrap();
        assert_eq!(got.status, "online");
        assert_eq!(got.last_seen_at.as_deref(), Some("t1"));

        ledger.delete_dataplane("ds-1").await.unwrap();
        assert!(ledger.get_dataplane("ds-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn pick_ingest_url_uses_online_sorted_subset() {
        let ledger = fresh_ledger("dp-pick").await;
        ledger
            .upsert_dataplane(&dataplane("b", "http://b:8081", "unknown"))
            .await
            .unwrap();
        ledger
            .upsert_dataplane(&dataplane("a", "http://a:8081", "unknown"))
            .await
            .unwrap();

        // 无 online 实例 -> None。
        assert!(ledger.pick_ingest_url("agent-1").await.unwrap().is_none());

        ledger
            .set_dataplane_status("a", "online", Some("t"))
            .await
            .unwrap();
        ledger
            .set_dataplane_status("b", "online", Some("t"))
            .await
            .unwrap();
        let first = ledger.pick_ingest_url("agent-1").await.unwrap();
        let second = ledger.pick_ingest_url("agent-1").await.unwrap();
        assert_eq!(first, second);
        assert!(first.is_some());

        // 只留一台时所有 agent 都落到该台。
        ledger
            .set_dataplane_status("a", "offline", None)
            .await
            .unwrap();
        assert_eq!(
            ledger.pick_ingest_url("agent-1").await.unwrap().as_deref(),
            Some("http://b:8081")
        );
        assert_eq!(
            ledger.pick_ingest_url("agent-2").await.unwrap().as_deref(),
            Some("http://b:8081")
        );
    }

    fn collect_item(id: &str, agents: &[&str]) -> CollectItem {
        CollectItem {
            item_id: id.to_string(),
            agent_ids: agents.iter().map(|a| a.to_string()).collect(),
            name: format!("item-{id}"),
            kind: "metrics_host".to_string(),
            enabled: true,
            collector: serde_json::json!({"interval_secs": 15}),
            storage: serde_json::json!({"retention_days": 1}),
            updated_at: ledger_stamp(),
        }
    }

    #[tokio::test]
    async fn collect_item_crud_and_agent_filter() {
        let ledger = fresh_ledger("collect-crud").await;
        ledger
            .upsert_collect_item(&collect_item("i-1", &["a-1", "a-2"]))
            .await
            .unwrap();
        ledger
            .upsert_collect_item(&collect_item("i-2", &["a-2"]))
            .await
            .unwrap();

        let got = ledger.get_collect_item("i-1").await.unwrap().unwrap();
        assert_eq!(got.agent_ids, vec!["a-1", "a-2"]);
        assert_eq!(got.collector["interval_secs"], 15);
        assert!(got.enabled);

        // 目标过滤：a-1 只命中 i-1，a-2 命中两条，a-3 为空。
        assert_eq!(
            ledger.list_collect_items(Some("a-1")).await.unwrap().len(),
            1
        );
        assert_eq!(
            ledger.list_collect_items(Some("a-2")).await.unwrap().len(),
            2
        );
        assert!(ledger
            .list_collect_items(Some("a-3"))
            .await
            .unwrap()
            .is_empty());
        assert_eq!(ledger.list_collect_items(None).await.unwrap().len(), 2);

        // 关闭开关后仍在列表中（前端需要显示禁用的项）。
        let mut disabled = collect_item("i-2", &["a-2"]);
        disabled.enabled = false;
        ledger.upsert_collect_item(&disabled).await.unwrap();
        assert!(
            !ledger
                .get_collect_item("i-2")
                .await
                .unwrap()
                .unwrap()
                .enabled
        );

        assert!(ledger.delete_collect_item("i-1").await.unwrap());
        assert!(!ledger.delete_collect_item("i-1").await.unwrap());
        assert!(ledger.get_collect_item("i-1").await.unwrap().is_none());
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

    fn new_job(id: &str, agent: &str) -> NewJob {
        NewJob {
            job_id: id.to_string(),
            agent_id: agent.to_string(),
            interpreter: "bash".to_string(),
            script: "echo hi".to_string(),
            args: vec!["-e".to_string()],
            env: BTreeMap::from([("LANG".to_string(), "C".to_string())]),
            working_dir: Some("/tmp".to_string()),
            template_id: None,
            rerun_of: None,
            timeout_secs: 300,
            created_at: ledger_stamp(),
            ..Default::default()
        }
    }

    fn job_result(id: &str, status: JobStatus) -> JobResult {
        JobResult {
            job_id: id.to_string(),
            status,
            exit_code: Some(0),
            signal: None,
            stdout: "hi\n".to_string(),
            stdout_truncated: false,
            stderr: String::new(),
            stderr_truncated: false,
            started_at_micros: 1_000,
            finished_at_micros: 2_000,
            error: None,
        }
    }

    #[tokio::test]
    async fn jobs_crud_roundtrip_and_filters() {
        let ledger = fresh_ledger("jobs-crud").await;
        ledger
            .insert_job(&new_job("j-1", "a-1"))
            .await
            .expect("insert 1");
        ledger
            .insert_job(&new_job("j-2", "a-2"))
            .await
            .expect("insert 2");

        let got = ledger.get_job("j-1").await.expect("get").expect("exists");
        assert_eq!(got.status, JobStatus::Pending);
        assert_eq!(got.interpreter, "bash");
        assert_eq!(got.args, vec!["-e".to_string()]);
        assert_eq!(got.env.get("LANG").map(String::as_str), Some("C"));
        assert_eq!(got.working_dir.as_deref(), Some("/tmp"));
        assert_eq!(got.timeout_secs, 300);

        assert_eq!(
            ledger.list_jobs(None, None, None).await.expect("all").len(),
            2
        );
        assert_eq!(
            ledger
                .list_jobs(Some("a-1"), None, None)
                .await
                .expect("by agent")
                .len(),
            1
        );
        assert_eq!(
            ledger
                .list_jobs(None, Some("pending"), None)
                .await
                .expect("by status")
                .len(),
            2
        );
        assert!(ledger
            .list_jobs(None, Some("succeeded"), None)
            .await
            .expect("by status")
            .is_empty());
    }

    #[tokio::test]
    async fn mark_running_then_finish_persists_result() {
        let ledger = fresh_ledger("jobs-run").await;
        ledger
            .insert_job(&new_job("j-1", "a-1"))
            .await
            .expect("insert");

        ledger.mark_running("j-1", "1500").await.expect("running");
        let running = ledger.get_job("j-1").await.expect("get").expect("exists");
        assert_eq!(running.status, JobStatus::Running);
        assert_eq!(running.started_at.as_deref(), Some("1500"));

        ledger
            .finish_job(&job_result("j-1", JobStatus::Succeeded), "2000")
            .await
            .expect("finish");
        let done = ledger.get_job("j-1").await.expect("get").expect("exists");
        assert_eq!(done.status, JobStatus::Succeeded);
        assert_eq!(done.exit_code, Some(0));
        assert_eq!(done.stdout.as_deref(), Some("hi\n"));
        assert_eq!(done.finished_at.as_deref(), Some("2000"));
        // Agent 微秒时间覆盖 started_at。
        assert_eq!(done.started_at.as_deref(), Some("1000"));
    }

    #[tokio::test]
    async fn terminal_job_is_immutable() {
        let ledger = fresh_ledger("jobs-immutable").await;
        ledger
            .insert_job(&new_job("j-1", "a-1"))
            .await
            .expect("insert");
        ledger
            .finish_job(&job_result("j-1", JobStatus::Succeeded), "2000")
            .await
            .expect("finish");

        // 后续 running / finish / rejected / lost 均不得覆盖首次终态。
        ledger.mark_running("j-1", "3000").await.expect("running");
        ledger
            .finish_job(&job_result("j-1", JobStatus::Failed), "4000")
            .await
            .expect("finish again");
        ledger.mark_rejected("j-1", "busy").await.expect("rejected");
        ledger.mark_lost_by_agent("a-1").await.expect("lost");

        let final_job = ledger.get_job("j-1").await.expect("get").expect("exists");
        assert_eq!(final_job.status, JobStatus::Succeeded);
        assert_eq!(final_job.finished_at.as_deref(), Some("2000"));
    }

    #[tokio::test]
    async fn mark_rejected_records_reason() {
        let ledger = fresh_ledger("jobs-reject").await;
        ledger
            .insert_job(&new_job("j-1", "a-1"))
            .await
            .expect("insert");
        ledger.mark_rejected("j-1", "busy").await.expect("reject");
        let rejected = ledger.get_job("j-1").await.expect("get").expect("exists");
        assert_eq!(rejected.status, JobStatus::Rejected);
        assert_eq!(rejected.error.as_deref(), Some("busy"));
    }

    #[tokio::test]
    async fn mark_lost_by_agent_only_affects_that_agent() {
        let ledger = fresh_ledger("jobs-lost-agent").await;
        ledger
            .insert_job(&new_job("j-1", "a-1"))
            .await
            .expect("insert 1");
        ledger
            .insert_job(&new_job("j-2", "a-2"))
            .await
            .expect("insert 2");
        ledger.mark_running("j-2", "10").await.expect("running");

        ledger.mark_lost_by_agent("a-1").await.expect("lost");
        assert_eq!(
            ledger
                .get_job("j-1")
                .await
                .expect("get")
                .expect("exists")
                .status,
            JobStatus::Lost
        );
        assert_eq!(
            ledger
                .get_job("j-2")
                .await
                .expect("get")
                .expect("exists")
                .status,
            JobStatus::Running
        );
    }

    #[tokio::test]
    async fn startup_recovery_marks_inflight_lost_only() {
        let ledger = fresh_ledger("jobs-recovery").await;
        ledger
            .insert_job(&new_job("j-1", "a-1"))
            .await
            .expect("insert 1");
        ledger
            .insert_job(&new_job("j-2", "a-2"))
            .await
            .expect("insert 2");
        ledger
            .finish_job(&job_result("j-2", JobStatus::Succeeded), "5")
            .await
            .expect("finish");

        ledger
            .mark_lost_inflight_on_startup()
            .await
            .expect("recovery");
        assert_eq!(
            ledger
                .get_job("j-1")
                .await
                .expect("get")
                .expect("exists")
                .status,
            JobStatus::Lost
        );
        assert_eq!(
            ledger
                .get_job("j-2")
                .await
                .expect("get")
                .expect("exists")
                .status,
            JobStatus::Succeeded
        );
    }

    fn new_template(name: &str) -> NewJobTemplate {
        NewJobTemplate {
            name: name.to_string(),
            description: Some("demo".to_string()),
            interpreter: "bash".to_string(),
            script: "echo ${WHO}".to_string(),
            args: vec!["-x".to_string()],
            env: BTreeMap::from([("LANG".to_string(), "C".to_string())]),
            working_dir: Some("/tmp".to_string()),
            timeout_secs: 60,
        }
    }

    #[tokio::test]
    async fn template_crud_roundtrip() {
        let ledger = fresh_ledger("tpl-crud").await;
        ledger
            .insert_template("tpl-1", &new_template("greet"), "100")
            .await
            .expect("insert");
        let got = ledger
            .get_template("tpl-1")
            .await
            .expect("get")
            .expect("exists");
        assert_eq!(got.name, "greet");
        assert_eq!(got.script, "echo ${WHO}");
        assert_eq!(got.args, vec!["-x".to_string()]);
        assert_eq!(got.env.get("LANG").map(String::as_str), Some("C"));
        assert_eq!(got.timeout_secs, 60);
        assert_eq!(got.created_at, "100");

        assert_eq!(
            ledger
                .get_template_by_name("greet")
                .await
                .expect("by name")
                .expect("exists")
                .template_id,
            "tpl-1"
        );
        assert!(ledger
            .get_template_by_name("missing")
            .await
            .expect("by name")
            .is_none());

        let mut updated = new_template("greet");
        updated.description = None;
        updated.timeout_secs = 90;
        ledger
            .update_template("tpl-1", &updated, "200")
            .await
            .expect("update");
        let after = ledger
            .get_template("tpl-1")
            .await
            .expect("get")
            .expect("exists");
        assert_eq!(after.timeout_secs, 90);
        assert_eq!(after.description, None);
        assert_eq!(after.updated_at, "200");

        assert_eq!(
            ledger.list_templates(None, None).await.expect("list").len(),
            1
        );
        assert!(ledger.delete_template("tpl-1").await.expect("delete"));
        assert!(!ledger.delete_template("tpl-1").await.expect("re-delete"));
        assert!(ledger.get_template("tpl-1").await.expect("get").is_none());
    }

    #[tokio::test]
    async fn template_name_is_unique() {
        let ledger = fresh_ledger("tpl-unique").await;
        ledger
            .insert_template("tpl-1", &new_template("greet"), "1")
            .await
            .expect("insert 1");
        let err = ledger
            .insert_template("tpl-2", &new_template("greet"), "1")
            .await
            .expect_err("duplicate name");
        assert_eq!(err.code, "already_exists");

        ledger
            .insert_template("tpl-2", &new_template("other"), "1")
            .await
            .expect("insert 2");
        let err = ledger
            .update_template("tpl-2", &new_template("greet"), "2")
            .await
            .expect_err("rename to duplicate");
        assert_eq!(err.code, "already_exists");
    }

    #[tokio::test]
    async fn template_list_filters_by_name_and_limit() {
        let ledger = fresh_ledger("tpl-filter").await;
        ledger
            .insert_template("tpl-1", &new_template("deploy-web"), "1")
            .await
            .expect("insert 1");
        ledger
            .insert_template("tpl-2", &new_template("deploy-db"), "2")
            .await
            .expect("insert 2");
        ledger
            .insert_template("tpl-3", &new_template("cleanup"), "3")
            .await
            .expect("insert 3");

        let deploy = ledger
            .list_templates(Some("deploy"), None)
            .await
            .expect("filter");
        assert_eq!(deploy.len(), 2);
        assert_eq!(
            ledger
                .list_templates(None, Some(1))
                .await
                .expect("limit")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn job_records_template_source() {
        let ledger = fresh_ledger("jobs-template").await;
        let mut job = new_job("j-1", "a-1");
        job.template_id = Some("tpl-1".to_string());
        ledger.insert_job(&job).await.expect("insert");
        let got = ledger.get_job("j-1").await.expect("get").expect("exists");
        assert_eq!(got.template_id.as_deref(), Some("tpl-1"));
        assert_eq!(
            ledger.get_job("j-1").await.expect("get").expect("exists"),
            got
        );
    }

    #[tokio::test]
    async fn job_records_rerun_source() {
        let ledger = fresh_ledger("jobs-rerun").await;
        let plain = new_job("j-plain", "a-1");
        ledger.insert_job(&plain).await.expect("insert plain");
        assert_eq!(
            ledger
                .get_job("j-plain")
                .await
                .expect("get")
                .expect("exists")
                .rerun_of,
            None
        );

        let mut rerun = new_job("j-rerun", "a-1");
        rerun.rerun_of = Some("j-plain".to_string());
        ledger.insert_job(&rerun).await.expect("insert rerun");
        assert_eq!(
            ledger
                .get_job("j-rerun")
                .await
                .expect("get")
                .expect("exists")
                .rerun_of
                .as_deref(),
            Some("j-plain")
        );
    }

    #[tokio::test]
    async fn file_job_filters_and_lost_by_source_or_dest() {
        let ledger = fresh_ledger("jobs-file").await;
        let mut job = new_job("j-ft", "dst-1");
        job.kind = "file_transfer".to_string();
        job.source = Some(FileEndpoint::Agent {
            agent_id: "src-1".to_string(),
            path: "/tmp/a".to_string(),
        });
        job.destination = Some(FileEndpoint::Agent {
            agent_id: "dst-1".to_string(),
            path: "/tmp/b".to_string(),
        });
        job.source_agent_id = Some("src-1".to_string());
        job.dest_agent_id = Some("dst-1".to_string());
        ledger.insert_job(&job).await.expect("insert");

        let got = ledger.get_job("j-ft").await.expect("get").expect("exists");
        assert_eq!(got.kind, "file_transfer");
        assert_eq!(got.source_agent_id.as_deref(), Some("src-1"));
        assert_eq!(got.dest_agent_id.as_deref(), Some("dst-1"));

        assert_eq!(
            ledger
                .list_jobs(Some("src-1"), None, None)
                .await
                .expect("src")
                .len(),
            1
        );
        assert_eq!(
            ledger
                .list_jobs(Some("dst-1"), None, None)
                .await
                .expect("dst")
                .len(),
            1
        );
        assert!(ledger
            .list_jobs(Some("other"), None, None)
            .await
            .expect("other")
            .is_empty());

        ledger.mark_lost_by_agent("src-1").await.expect("lost");
        let lost = ledger.get_job("j-ft").await.expect("get").expect("exists");
        assert_eq!(lost.status, JobStatus::Lost);
    }
}
