//! spec 应用与 Agent 侧的**可变**运行时状态。
//!
//! 这个模块回答两件事：
//!
//! 1. 收到期望 spec 后，进程内哪些东西要换、哪些东西**不能**换（热加载映射）；
//! 2. 换完之后向上回报什么（`AgentSpecAck`，含 `not_enforced`）。
//!
//! 独立成模块是为了可测：热加载的正确性（哪些重启了、哪些没重启）靠纯逻辑断言，
//! 不必起连接。

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use gse_proto::{
    spec_outcome, AgentSpecAck, AgentSpecPush, AgentSpecWire, SpecItem, SpecParams,
    NOT_ENFORCED_FIELDS,
};
use tokio::sync::{Notify, RwLock};

use crate::collect::{CollectShared, OtlpRuntime};
use crate::job::{JobConfig, JobExecutor};

/// 心跳补报的最大尝试次数。超过就放弃（避免一份快照永远重发），
/// 下一次 spec 变更或重连会重新排一次。
pub const MAX_REPORT_ATTEMPTS: u32 = 5;

/// Agent 进程级可变状态：心跳、作业、采集、重连四条路径共享。
pub struct RuntimeConfig {
    /// 不可变身份与地址（不可下发，只用于上报展示）。
    agent_id: String,
    server_addr: String,
    /// 心跳周期（秒），心跳循环每轮读取。
    heartbeat_interval_secs: AtomicU64,
    /// 当前认证 token；变更时触发重连。
    token: RwLock<String>,
    /// 作业执行器，参数变更后整体重建。
    job_executor: RwLock<JobExecutor>,
    /// 采集侧共享状态（OTLP 参数 + 采集项对齐）。
    collector: Arc<CollectShared>,
    /// 生效快照（敏感字段恒为 `None`，不回声凭据）。
    applied: RwLock<AgentSpecWire>,
    /// 已应用的 revision；空字符串 = 本地文件基线。
    revision: RwLock<String>,
    /// 最近一次应用的回执（含 `outcome` 与 `not_enforced`）。
    ///
    /// 心跳补报必须回放**这一份**，不能临时拼一个：临时拼会把 `outcome` 丢成空串、
    /// 把 `not_enforced` 丢成空数组 —— 于是「Agent 自动拉取」（重连即收敛这条主路径）
    /// 上报的状态里，「未实现字段」的标注就没了，正好违背「不许假装生效」。
    last_ack: RwLock<Option<AgentSpecAck>>,
    /// 剩余补报次数；0 表示不必再报。
    pending_reports: AtomicU32,
    /// token 变更 → 通知心跳循环退出、由 `run` 重连重认证。
    reauth: Notify,
}

impl RuntimeConfig {
    /// 从本地配置构造。`collector` 需已带好启动时的 OTLP 参数。
    pub fn new(cfg: &crate::AgentConfig, collector: Arc<CollectShared>) -> Arc<Self> {
        let params = SpecParams {
            heartbeat_interval_secs: cfg.heartbeat_interval_secs,
            allowed_interpreters: cfg.allowed_interpreters.clone(),
            job_default_interpreter: cfg.job_default_interpreter.clone(),
            max_concurrent_jobs: cfg.max_concurrent_jobs,
            job_work_dir: cfg.job_work_dir.clone(),
            otlp_enabled: cfg.otlp_enabled,
            otlp_listen: cfg.otlp_listen.clone(),
            otlp_max_body_bytes: cfg.otlp_max_body_bytes,
            otlp_token: None,
            otlp_allowed_cidrs: cfg.otlp_allowed_cidrs.clone(),
            token: None,
            cpu_limit_percent: None,
            mem_limit_percent: None,
            log_level: "info".to_string(),
        };
        let mut applied = AgentSpecWire {
            params,
            items: Vec::new(),
        };
        applied.params.token = None;
        applied.params.otlp_token = None;
        Arc::new(Self {
            agent_id: cfg.agent_id.clone(),
            server_addr: cfg.server_addr.clone(),
            heartbeat_interval_secs: AtomicU64::new(cfg.heartbeat_interval_secs.max(1)),
            token: RwLock::new(cfg.token.clone()),
            job_executor: RwLock::new(JobExecutor::new(JobConfig::from_agent(cfg))),
            collector,
            applied: RwLock::new(applied),
            revision: RwLock::new(String::new()),
            last_ack: RwLock::new(None),
            pending_reports: AtomicU32::new(0),
            reauth: Notify::new(),
        })
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn server_addr(&self) -> &str {
        &self.server_addr
    }

    pub fn heartbeat_interval_secs(&self) -> u64 {
        self.heartbeat_interval_secs.load(Ordering::Relaxed)
    }

    /// 采集侧共享状态。
    pub fn collector(&self) -> Arc<CollectShared> {
        self.collector.clone()
    }

    pub async fn token(&self) -> String {
        self.token.read().await.clone()
    }

    /// 取当前作业执行器（参数变更是整体换实例，所以每次取最新的）。
    pub async fn job_executor(&self) -> JobExecutor {
        self.job_executor.read().await.clone()
    }

    /// 重连信号；`run` 侧 `select!` 等它。
    pub async fn wait_reauth(&self) {
        self.reauth.notified().await
    }

    /// 已应用 revision（空 = 本地基线）。
    pub async fn applied_revision(&self) -> String {
        self.revision.read().await.clone()
    }

    /// 生效快照（不含凭据）。
    pub async fn applied_snapshot(&self) -> AgentSpecWire {
        self.applied.read().await.clone()
    }

    /// 记住本次回执，供心跳补报回放。
    async fn remember(&self, ack: &AgentSpecAck) {
        *self.last_ack.write().await = Some(ack.clone());
    }

    /// 排一次心跳补报。下发回执只在「下发那一刻」到达；服务端落库前掉线、
    /// 或本次心跳丢失，都会让它永久停在旧 revision，所以要有兜底。
    pub fn schedule_report(&self) {
        self.pending_reports
            .store(MAX_REPORT_ATTEMPTS, Ordering::Relaxed);
    }

    /// 待补报的生效快照（心跳携带）。剩余次数为 0 时返回 `None`。
    ///
    /// 回放最近一次应用的回执（`outcome` / `not_enforced` / `detail` 都在里面），
    /// 而不是按当前状态临时拼一份 —— 见 `last_ack` 的注释。
    pub async fn pending_ack(&self) -> Option<AgentSpecAck> {
        if self.pending_reports.load(Ordering::Relaxed) == 0 {
            return None;
        }
        self.last_ack.read().await.clone()
    }

    /// 服务端已确认落库：不再补报。
    pub fn confirm_report(&self) {
        self.pending_reports.store(0, Ordering::Relaxed);
    }

    /// 本次补报没被确认：消耗一次尝试机会。
    pub fn decay_report(&self) {
        let _ = self
            .pending_reports
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                Some(n.saturating_sub(1))
            });
    }
}

/// `not_enforced` 的取值规则：**只有本次真的带了值的字段**才算。
///
/// `cpu_limit_percent = None` 就不该出现在清单里 —— 否则页面上会一直挂着一堆
/// 「未实现」提示，连没配过的字段也算。
pub fn not_enforced_fields(params: &SpecParams) -> Vec<String> {
    let mut out = Vec::new();
    for field in NOT_ENFORCED_FIELDS {
        let present = match field {
            "cpu_limit_percent" => params.cpu_limit_percent.is_some(),
            "mem_limit_percent" => params.mem_limit_percent.is_some(),
            // `log_level` 的缺省是 `"info"`，那是「没有意见」而不是「配了个值」——
            // 每台 Agent 都挂一条「未实现」提示会把信号淹掉，所以只标明显偏离缺省的值。
            // 注意这不是「假装生效」：真设了 `debug`/`warn` 照样会被标出来。
            "log_level" => {
                !params.log_level.is_empty() && params.log_level != SpecParams::default().log_level
            }
            _ => false,
        };
        if present {
            out.push(field.to_string());
        }
    }
    out
}

/// 不可变字段防御。
///
/// `server_addr` / `agent_id` 只能改本地文件：改了就要换身份/换地址重连，
/// 而配置中心里改这两项等于「把 Agent 从它自己的管理面里搬走」，没有回滚路径。
///
/// 这里对 JSON 文本做**定点检查**而不是给 `AgentSpecWire` 加 `deny_unknown_fields`：
/// 后者会把将来新增的字段也一起拒掉，旧 Agent 就再也吃不到新版本 server 的配置。
pub fn rejects_immutable_fields(raw: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let mut found: Vec<&str> = Vec::new();
    let mut check = |obj: Option<&serde_json::Map<String, serde_json::Value>>| {
        if let Some(obj) = obj {
            for key in ["server_addr", "agent_id"] {
                if obj.contains_key(key) && !found.contains(&key) {
                    found.push(key);
                }
            }
        }
    };
    check(v.as_object());
    check(v.get("spec").and_then(|s| s.as_object()));
    check(v.pointer("/spec/params").and_then(|p| p.as_object()));
    check(v.get("params").and_then(|p| p.as_object()));
    if found.is_empty() {
        None
    } else {
        Some(format!("不可下发的字段: {}", found.join(", ")))
    }
}

/// 应用一份期望 spec 并回执。
pub async fn apply_push(rt: &Arc<RuntimeConfig>, push: AgentSpecPush) -> AgentSpecAck {
    let Some(spec) = push.spec else {
        // 服务端说「没有期望 spec」：保持本地基线，顺带把现状报上去。
        rt.schedule_report();
        let ack = build_ack(
            rt,
            push.revision,
            spec_outcome::UNCHANGED,
            "无期望 spec，保持本地基线".to_string(),
        )
        .await;
        rt.remember(&ack).await;
        return ack;
    };
    let current = rt.applied_revision().await;
    if !current.is_empty() && current == push.revision {
        // 幂等：不重启任何采集器、不重建执行器、不断连。
        let ack = build_ack(rt, push.revision, spec_outcome::UNCHANGED, String::new()).await;
        rt.remember(&ack).await;
        return ack;
    }
    let not_enforced = not_enforced_fields(&spec.params);
    apply_spec(rt, &spec).await;
    *rt.revision.write().await = push.revision.clone();
    rt.schedule_report();
    let outcome = if not_enforced.is_empty() {
        spec_outcome::APPLIED
    } else {
        spec_outcome::PARTIAL
    };
    let ack = build_ack(rt, push.revision, outcome, String::new()).await;
    rt.remember(&ack).await;
    ack
}

/// 应用本地文件里的参数（`SIGHUP` 重读路径）。
///
/// 采集项不动（本地文件里没有采集项），revision 归零表示「本地基线」——
/// 于是服务端下一次推送（或重连后的自动拉取）仍会按新内容重新应用一遍，
/// 与「下发 > 本地」的优先级一致。
pub async fn apply_local_file(rt: &Arc<RuntimeConfig>, params: SpecParams) -> AgentSpecAck {
    let items = rt.applied.read().await.items.clone();
    let spec = AgentSpecWire { params, items };
    let not_enforced = not_enforced_fields(&spec.params);
    apply_spec(rt, &spec).await;
    *rt.revision.write().await = String::new();
    rt.schedule_report();
    let outcome = if not_enforced.is_empty() {
        spec_outcome::APPLIED
    } else {
        spec_outcome::PARTIAL
    };
    let ack = build_ack(rt, String::new(), outcome, "reloaded from file".to_string()).await;
    rt.remember(&ack).await;
    ack
}

async fn build_ack(
    rt: &Arc<RuntimeConfig>,
    revision: String,
    outcome: &str,
    detail: String,
) -> AgentSpecAck {
    let applied = rt.applied_snapshot().await;
    AgentSpecAck {
        revision,
        outcome: outcome.to_string(),
        not_enforced: not_enforced_fields(&applied.params),
        applied,
        detail,
    }
}

/// 热加载映射的唯一实现。push 与 SIGHUP 两条路径共用，避免行为分叉。
async fn apply_spec(rt: &Arc<RuntimeConfig>, spec: &AgentSpecWire) {
    let params = &spec.params;

    // 心跳周期：心跳循环每轮读，改完下一个周期生效。
    rt.heartbeat_interval_secs
        .store(params.heartbeat_interval_secs.max(1), Ordering::Relaxed);

    // 作业执行器：整体重建。在跑作业仍持旧信号量的许可，瞬时并发可略超新上限
    // （上界 = 在跑作业数）—— 接受，换实例比动态缩容简单且没有卡死风险。
    let job = JobConfig::from_spec_params(params);
    if rt.job_executor.read().await.config() != &job {
        *rt.job_executor.write().await = JobExecutor::new(job);
    }

    // OTLP 进程级参数：变了就要让 `apm_otlp` 采集项重绑 listener。
    let otlp = OtlpRuntime::new(
        params.otlp_enabled,
        params.otlp_listen.clone(),
        params.otlp_max_body_bytes,
        params.otlp_token.clone().unwrap_or_default(),
        params.otlp_allowed_cidrs.clone(),
    );
    let otlp_changed = rt.collector.otlp_runtime().await != otlp;
    if otlp_changed {
        rt.collector.set_otlp_runtime(otlp).await;
    }

    // 采集项整表对齐（含删除/停用 → 停掉采集器）。
    // 先过一遍归一化：空 item_id / 空 kind 的项会让采集器挂在一个无法对齐的键上。
    let items = normalize_items(spec.items.clone());
    rt.collector.apply_spec(items.clone(), otlp_changed).await;

    // token：变更即主动重连重认证（唯一需要断连的字段）。
    if let Some(new_token) = params.token.as_deref() {
        if !new_token.is_empty() && rt.token().await != new_token {
            *rt.token.write().await = new_token.to_string();
            rt.reauth.notify_one();
        }
    }

    // 生效快照：凭据不回声。
    let mut applied_params = params.clone();
    applied_params.token = None;
    applied_params.otlp_token = None;
    *rt.applied.write().await = AgentSpecWire {
        params: applied_params,
        items,
    };
}

/// 从配置文件值构造 `SpecParams`（SIGHUP 重读用）。
pub fn params_from_config(cfg: &crate::AgentConfig) -> SpecParams {
    SpecParams {
        heartbeat_interval_secs: cfg.heartbeat_interval_secs,
        allowed_interpreters: cfg.allowed_interpreters.clone(),
        job_default_interpreter: cfg.job_default_interpreter.clone(),
        max_concurrent_jobs: cfg.max_concurrent_jobs,
        job_work_dir: cfg.job_work_dir.clone(),
        otlp_enabled: cfg.otlp_enabled,
        otlp_listen: cfg.otlp_listen.clone(),
        otlp_max_body_bytes: cfg.otlp_max_body_bytes,
        otlp_token: if cfg.otlp_token.is_empty() {
            None
        } else {
            Some(cfg.otlp_token.clone())
        },
        otlp_allowed_cidrs: cfg.otlp_allowed_cidrs.clone(),
        token: if cfg.token.is_empty() {
            None
        } else {
            Some(cfg.token.clone())
        },
        cpu_limit_percent: None,
        mem_limit_percent: None,
        log_level: "info".to_string(),
    }
}

/// 采集项形状校验（Agent 侧不做业务校验，但要挡住会让采集器崩坏的输入）。
pub fn normalize_items(items: Vec<SpecItem>) -> Vec<SpecItem> {
    items
        .into_iter()
        .filter(|i| !i.item_id.is_empty() && !i.kind.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::CollectShared;

    fn runtime() -> Arc<RuntimeConfig> {
        let cfg = crate::AgentConfig::default();
        let shared = Arc::new(CollectShared::new(cfg.agent_id.clone()));
        RuntimeConfig::new(&cfg, shared)
    }

    fn params(heartbeat: u64) -> SpecParams {
        SpecParams {
            heartbeat_interval_secs: heartbeat,
            ..Default::default()
        }
    }

    fn push(revision: &str, spec: SpecParams, items: Vec<SpecItem>) -> AgentSpecPush {
        AgentSpecPush {
            revision: revision.to_string(),
            spec: Some(AgentSpecWire {
                params: spec,
                items,
            }),
        }
    }

    fn item(id: &str, kind: &str) -> SpecItem {
        SpecItem {
            item_id: id.to_string(),
            name: format!("item {id}"),
            kind: kind.to_string(),
            enabled: true,
            collector: serde_json::json!({"interval_secs": 15}),
            storage: serde_json::json!({"retention_days": 1}),
        }
    }

    #[test]
    fn not_enforced_lists_only_fields_that_carry_a_value() {
        // 一个字段都没配 → 清单为空（页面上不该挂着一堆「未实现」）。
        assert!(not_enforced_fields(&SpecParams::default()).is_empty());

        let p = SpecParams {
            cpu_limit_percent: Some(50),
            ..Default::default()
        };
        assert_eq!(
            not_enforced_fields(&p),
            vec!["cpu_limit_percent".to_string()]
        );

        // 与缺省不同的 log_level 才算「配了个值」。
        let p = SpecParams {
            cpu_limit_percent: Some(50),
            mem_limit_percent: Some(30),
            log_level: "debug".to_string(),
            ..Default::default()
        };
        assert_eq!(not_enforced_fields(&p).len(), 3);
    }

    #[test]
    fn immutable_fields_are_detected_wherever_they_appear() {
        // 合法 payload。
        assert!(rejects_immutable_fields(br#"{"revision":"r","spec":{"params":{}}}"#).is_none());
        // 顶层。
        let raw = br#"{"revision":"r","agent_id":"other","spec":{"params":{}}}"#;
        assert!(rejects_immutable_fields(raw).is_some());
        // spec 层。
        let raw = br#"{"revision":"r","spec":{"agent_id":"other","params":{}}}"#;
        assert!(rejects_immutable_fields(raw).unwrap().contains("agent_id"));
        // params 层。
        let raw = br#"{"revision":"r","spec":{"params":{"server_addr":"1.2.3.4:7100"}}}"#;
        assert!(rejects_immutable_fields(raw)
            .unwrap()
            .contains("server_addr"));
        // 非 JSON：交给下面的类型解码去报错。
        assert!(rejects_immutable_fields(b"not json").is_none());
    }

    #[tokio::test]
    async fn same_revision_is_a_noop() {
        let rt = runtime();
        let first = apply_push(&rt, push("rev-1", params(15), vec![item("i1", "log_file")])).await;
        assert_eq!(first.outcome, spec_outcome::APPLIED);
        assert_eq!(rt.heartbeat_interval_secs(), 15);

        // 同 revision 再来一次：outcome=unchanged，且不排补报（省一次心跳负载）。
        rt.confirm_report();
        let again = apply_push(&rt, push("rev-1", params(99), vec![])).await;
        assert_eq!(again.outcome, spec_outcome::UNCHANGED);
        assert_eq!(
            rt.heartbeat_interval_secs(),
            15,
            "unchanged 时不得改动任何状态"
        );
        assert!(rt.pending_ack().await.is_none(), "unchanged 不该排补报");
    }

    #[tokio::test]
    async fn apply_updates_heartbeat_job_items_and_never_echoes_secrets() {
        let rt = runtime();
        let mut p = params(7);
        p.allowed_interpreters = vec!["python3".to_string()];
        p.job_default_interpreter = "python3".to_string();
        p.max_concurrent_jobs = 3;
        p.otlp_enabled = true;
        p.otlp_listen = "127.0.0.1:14318".to_string();
        p.token = Some("s3cret".to_string());
        p.otlp_token = Some("otlp-secret".to_string());

        let ack = apply_push(&rt, push("rev-2", p, vec![item("i1", "log_file")])).await;
        assert_eq!(ack.outcome, spec_outcome::APPLIED);
        assert_eq!(rt.heartbeat_interval_secs(), 7);
        assert_eq!(rt.job_executor().await.config().max_concurrent_jobs, 3);
        assert_eq!(
            rt.job_executor().await.config().allowed_interpreters,
            vec!["python3".to_string()]
        );
        let otlp = rt.collector().otlp_runtime().await;
        assert!(otlp.enabled);
        assert_eq!(otlp.listen, "127.0.0.1:14318");
        assert_eq!(otlp.token, "otlp-secret");
        // token 变更要触发重连。
        assert_eq!(rt.token().await, "s3cret");
        tokio::time::timeout(std::time::Duration::from_millis(200), rt.wait_reauth())
            .await
            .expect("token 变更必须发出重连信号");

        // 回执与生效快照都不得回声凭据。
        assert!(ack.applied.params.token.is_none());
        assert!(ack.applied.params.otlp_token.is_none());
        let snap = rt.applied_snapshot().await;
        assert!(snap.params.token.is_none());
        assert!(snap.params.otlp_token.is_none());
        assert_eq!(snap.items.len(), 1);
        // 已排补报：服务端掉线也能在下一次心跳拿到现状。
        assert!(rt.pending_ack().await.is_some());
    }

    #[tokio::test]
    async fn outcome_is_partial_when_unimplemented_fields_carry_values() {
        let rt = runtime();
        let mut p = params(30);
        p.cpu_limit_percent = Some(50);
        let ack = apply_push(&rt, push("rev-3", p, vec![])).await;
        assert_eq!(ack.outcome, spec_outcome::PARTIAL);
        assert_eq!(ack.not_enforced, vec!["cpu_limit_percent".to_string()]);
        // 但值要落进生效快照，页面才看得到「收到但没实现」。
        assert_eq!(
            rt.applied_snapshot().await.params.cpu_limit_percent,
            Some(50)
        );
    }

    #[tokio::test]
    async fn empty_push_keeps_local_baseline_and_reports_it() {
        let rt = runtime();
        let ack = apply_push(&rt, AgentSpecPush::default()).await;
        assert_eq!(ack.outcome, spec_outcome::UNCHANGED);
        assert_eq!(ack.revision, "", "无期望 spec 时 revision 为空 = 本地基线");
        assert!(rt.pending_ack().await.is_some(), "首拍要把现状报上去");
    }

    #[tokio::test]
    async fn local_file_reload_resets_revision_and_keeps_items() {
        let rt = runtime();
        apply_push(&rt, push("rev-4", params(15), vec![item("i1", "log_file")])).await;
        assert_eq!(rt.applied_revision().await, "rev-4");

        let ack = apply_local_file(&rt, params(45)).await;
        assert_eq!(ack.revision, "");
        assert_eq!(rt.applied_revision().await, "", "本地基线 revision 归零");
        assert_eq!(rt.heartbeat_interval_secs(), 45);
        assert_eq!(
            rt.applied_snapshot().await.items.len(),
            1,
            "本地文件没有采集项，不该把已有的清掉"
        );
    }

    /// 心跳补报必须回放最近一次回执（含 `outcome` 与 `not_enforced`）。
    ///
    /// 回归：此前 `pending_ack()` 临时拼一份回执，把 `outcome` 丢成空串、
    /// `not_enforced` 丢成空数组 —— 而「Agent 认证后自动拉取」是主路径，
    /// 于是页面上「未实现字段」的标注在真实部署里根本不出现（实测踩到）。
    #[tokio::test]
    async fn heartbeat_report_replays_outcome_and_not_enforced() {
        let rt = runtime();
        let p = SpecParams {
            cpu_limit_percent: Some(80),
            mem_limit_percent: Some(50),
            ..params(15)
        };
        let pushed = apply_push(&rt, push("rev-9", p, vec![item("i1", "log_file")])).await;
        assert_eq!(pushed.outcome, spec_outcome::PARTIAL);

        let reported = rt.pending_ack().await.expect("应排了补报");
        assert_eq!(
            reported.outcome,
            spec_outcome::PARTIAL,
            "补报不得把 outcome 丢掉"
        );
        assert_eq!(
            reported.not_enforced,
            vec![
                "cpu_limit_percent".to_string(),
                "mem_limit_percent".to_string()
            ],
            "补报不得把 not_enforced 丢掉（否则「未实现」标注不显示）"
        );
        assert_eq!(reported.revision, "rev-9");
        assert_eq!(reported.applied.items.len(), 1);

        // 幂等路径（unchanged）也要能补报出正确 outcome。
        let again = apply_push(&rt, push("rev-9", params(15), vec![])).await;
        assert_eq!(again.outcome, spec_outcome::UNCHANGED);
        assert_eq!(
            rt.pending_ack().await.expect("应排了补报").outcome,
            spec_outcome::UNCHANGED
        );

        // 本地重载路径同理。
        let reloaded = apply_local_file(&rt, params(45)).await;
        assert_eq!(reloaded.outcome, spec_outcome::APPLIED);
        assert_eq!(
            rt.pending_ack().await.expect("应排了补报").outcome,
            spec_outcome::APPLIED
        );
    }

    #[tokio::test]
    async fn report_attempts_are_bounded() {
        let rt = runtime();
        apply_push(&rt, push("rev-5", params(30), vec![])).await;
        for _ in 0..MAX_REPORT_ATTEMPTS {
            assert!(rt.pending_ack().await.is_some());
            rt.decay_report();
        }
        assert!(
            rt.pending_ack().await.is_none(),
            "重试次数用尽后必须停手，否则一份旧快照会被永远重发"
        );
    }

    #[test]
    fn job_config_falls_back_on_empty_values() {
        let p = SpecParams {
            allowed_interpreters: vec![],
            job_default_interpreter: String::new(),
            max_concurrent_jobs: 0,
            ..Default::default()
        };
        let job = JobConfig::from_spec_params(&p);
        let default = crate::AgentConfig::default();
        assert_eq!(job.allowed_interpreters, default.allowed_interpreters);
        assert_eq!(job.default_interpreter, default.job_default_interpreter);
        assert_eq!(job.max_concurrent_jobs, 1, "0 并发会让作业永远排不上");
    }
}
