//! per-Agent spec 的纯逻辑：revision 指纹与逐字段 diff。
//!
//! 单独成模块是为了可测：`revision` 决定「重复保存是否幂等」「下发是否白跑」，
//! `diff` 决定页面上「哪个字段没生效」，两者都不该埋在 sqlite 读写里。

use std::collections::BTreeMap;

use gse_proto::{AgentSpecWire, SpecItem};
use serde::{Deserialize, Serialize};

/// revision 取 sha256 前 16 位十六进制（碰撞概率足够低，且便于人眼比对与日志）。
const REV_LEN: usize = 16;

/// spec 的内容指纹。**同一份 spec 必得同一 revision**：
/// 依赖 `AgentSpecWire` 字段顺序固定、`serde_json` 默认 `Map` 按键排序。
pub fn revision(spec: &AgentSpecWire) -> Result<String, String> {
    let json = serde_json::to_string(spec).map_err(|e| format!("encode spec: {e}"))?;
    let hex = crate::hashutil::sha256_hex(json.as_bytes());
    Ok(hex[..REV_LEN].to_string())
}

/// 单个字段的期望值与生效值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldPair {
    pub desired: serde_json::Value,
    pub applied: serde_json::Value,
}

/// 采集项按 `item_id` 的增 / 删 / 改。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ItemDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

/// 期望 spec 与生效 spec 的差异。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SpecDiff {
    /// `params` 逐字段：字段名 → {desired, applied}。
    pub params: BTreeMap<String, FieldPair>,
    /// `items` 按 `item_id`。
    pub items: ItemDiff,
}

impl SpecDiff {
    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
            && self.items.added.is_empty()
            && self.items.removed.is_empty()
            && self.items.changed.is_empty()
    }
}

/// 不应出现在 diff（以及页面上）的字段。
fn is_secret(key: &str) -> bool {
    matches!(key, "token" | "otlp_token")
}

/// 逐字段 diff。
///
/// **只比非敏感字段**：`token` / `otlp_token` 恒不入 diff —— 凭据不上页面，
/// 「是否已下发」由 revision（它包含敏感字段）间接表达。
pub fn diff(desired: &AgentSpecWire, applied: &AgentSpecWire) -> Result<SpecDiff, String> {
    let dv = serde_json::to_value(&desired.params).map_err(|e| format!("encode params: {e}"))?;
    let av = serde_json::to_value(&applied.params).map_err(|e| format!("encode params: {e}"))?;
    let (Some(dm), Some(am)) = (dv.as_object(), av.as_object()) else {
        return Err("encode params: params 必须序列化为 JSON 对象".to_string());
    };
    let mut params = BTreeMap::new();
    for (key, d) in dm {
        if is_secret(key) {
            continue;
        }
        // applied 里缺 key 视作 null：旧 Agent 可能不回声新增字段。
        let a = am.get(key).cloned().unwrap_or(serde_json::Value::Null);
        if *d != a {
            params.insert(
                key.clone(),
                FieldPair {
                    desired: d.clone(),
                    applied: a,
                },
            );
        }
    }

    let by_id = |items: &[SpecItem]| -> BTreeMap<String, String> {
        items
            .iter()
            .map(|i| {
                (
                    i.item_id.clone(),
                    serde_json::to_string(i).unwrap_or_default(),
                )
            })
            .collect()
    };
    let did = by_id(&desired.items);
    let aid = by_id(&applied.items);
    let mut items = ItemDiff::default();
    for (id, d) in &did {
        match aid.get(id) {
            None => items.added.push(id.clone()),
            Some(a) if a != d => items.changed.push(id.clone()),
            Some(_) => {}
        }
    }
    for id in aid.keys() {
        if !did.contains_key(id) {
            items.removed.push(id.clone());
        }
    }

    Ok(SpecDiff { params, items })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gse_proto::SpecParams;

    fn item(id: &str) -> SpecItem {
        SpecItem {
            item_id: id.to_string(),
            name: format!("item {id}"),
            kind: "log_file".to_string(),
            enabled: true,
            collector: serde_json::json!({"path_patterns": ["/var/log/*.log"]}),
            storage: serde_json::json!({"retention_days": 7}),
        }
    }

    fn spec() -> AgentSpecWire {
        AgentSpecWire {
            params: SpecParams {
                heartbeat_interval_secs: 30,
                allowed_interpreters: vec!["bash".to_string()],
                job_default_interpreter: "bash".to_string(),
                max_concurrent_jobs: 1,
                job_work_dir: None,
                otlp_enabled: false,
                otlp_listen: "0.0.0.0:4318".to_string(),
                otlp_max_body_bytes: 8 * 1024 * 1024,
                otlp_token: None,
                otlp_allowed_cidrs: vec![],
                token: None,
                cpu_limit_percent: None,
                mem_limit_percent: None,
                log_level: "info".to_string(),
            },
            items: vec![item("i1")],
        }
    }

    #[test]
    fn revision_is_stable_and_content_addressed() {
        let a = revision(&spec()).expect("rev");
        let b = revision(&spec()).expect("rev");
        assert_eq!(a, b);
        assert_eq!(a.len(), REV_LEN);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));

        // 任何一处内容变化都要换 revision（否则下发会被判成 unchanged）。
        let mut other = spec();
        other.params.heartbeat_interval_secs = 31;
        assert_ne!(revision(&other).expect("rev"), a);

        let mut other = spec();
        other.items[0].enabled = false;
        assert_ne!(revision(&other).expect("rev"), a);

        // 敏感字段参与哈希：轮换 token 必须被视为一次变更。
        let mut other = spec();
        other.params.token = Some("new".to_string());
        assert_ne!(revision(&other).expect("rev"), a);
    }

    #[test]
    fn identical_spec_has_empty_diff() {
        let d = diff(&spec(), &spec()).expect("diff");
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn diff_reports_changed_and_missing_params() {
        // applied 是旧 Agent 报上来的：心跳周期不同、且完全没有 max_concurrent_jobs 字段。
        let applied = AgentSpecWire {
            params: SpecParams {
                heartbeat_interval_secs: 60,
                ..spec().params.clone()
            },
            ..spec()
        };
        let d = diff(&spec(), &applied).expect("diff");
        assert_eq!(
            d.params.get("heartbeat_interval_secs").map(|p| &p.desired),
            Some(&serde_json::json!(30))
        );
        assert_eq!(
            d.params.get("heartbeat_interval_secs").map(|p| &p.applied),
            Some(&serde_json::json!(60))
        );
        assert_eq!(d.params.len(), 1, "{:?}", d.params);
    }

    #[test]
    fn diff_ignores_secrets() {
        // 期望里带了 token，生效回执里恒为 None —— 不能因此报「token 没生效」。
        let mut desired = spec();
        desired.params.token = Some("t".to_string());
        desired.params.otlp_token = Some("o".to_string());
        let d = diff(&desired, &spec()).expect("diff");
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn diff_groups_items_by_item_id() {
        let mut desired = spec();
        desired.items.push(item("i2")); // added
        let mut applied = spec();
        applied.items.push(item("i3")); // removed
        applied.items[0].enabled = false; // changed
        let d = diff(&desired, &applied).expect("diff");
        assert_eq!(d.items.added, vec!["i2".to_string()]);
        assert_eq!(d.items.removed, vec!["i3".to_string()]);
        assert_eq!(d.items.changed, vec!["i1".to_string()]);
    }

    #[test]
    fn item_order_does_not_produce_diff() {
        // 采集项是数组：顺序不同不该算变更（否则每次下发都白跑）。
        let mut desired = spec();
        desired.items.push(item("i2"));
        let mut applied = spec();
        applied.items.insert(0, item("i2"));
        let d = diff(&desired, &applied).expect("diff");
        assert!(d.is_empty(), "{d:?}");
    }
}
