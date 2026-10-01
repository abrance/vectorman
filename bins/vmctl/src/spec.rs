//! spec 视图的解析与渲染：`agents specs` / `agents spec get` / `agents spec put`。
//!
//! 设计口径（`.monkeycode/specs/vmctl-collect-chain/design.md` P3）：
//! **解析失败一律退回透传**。这两个命令的第一职责是「拿到正文」，表格只是增强 ——
//! 把「服务端字段改名」变成 CLI 退出码 1，会让运维在升级期彻底失去这两个命令。

use serde_json::Value;

/// `-f <path>` 或 `--json '<json>'` 二选一的请求体来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecSource {
    File(String),
    Inline(String),
}

/// 读请求体：**不做字段裁剪、不补默认值**。
///
/// 服务端 `SpecParamsInput` 用 `double_option` 区分「字段缺失 = 不修改」与「显式 null = 清空」。
/// CLI 若照自己的默认值补全，会把「不修改」变成「改成默认」——典型后果是
/// `allowed_interpreters` 被重置成默认集，Agent 上原有解释器配置静默丢失。
pub fn read_spec_body(source: &SpecSource) -> Result<String, String> {
    let (text, origin) = match source {
        SpecSource::File(path) => (
            std::fs::read_to_string(path).map_err(|e| format!("read spec file {path}: {e}"))?,
            path.clone(),
        ),
        SpecSource::Inline(json) => (json.clone(), "--json".to_string()),
    };
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| format!("parse spec from {origin}: not valid JSON: {e}"))?;
    if !value.is_object() {
        return Err(format!(
            "parse spec from {origin}: expected a JSON object, got {}",
            json_type_name(&value)
        ));
    }
    Ok(value.to_string())
}

/// 从 `-f` / `--json` 两个可选参数里选出唯一来源；都给或都不给都报错。
pub fn pick_source(file: Option<String>, json: Option<String>) -> Result<SpecSource, String> {
    match (file, json) {
        (Some(_), Some(_)) => Err("-f and --json are mutually exclusive; give exactly one".into()),
        (Some(path), None) => Ok(SpecSource::File(path)),
        (None, Some(text)) => Ok(SpecSource::Inline(text)),
        (None, None) => Err("missing request body: give -f <path> or --json '<json>'".into()),
    }
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_str()
}

fn opt_str(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => String::new(),
    }
}

/// `agents specs --table` 的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecsRow {
    pub agent_id: String,
    pub session_state: String,
    pub sync_status: String,
    /// `desired.spec.items` 条数；无期望时为空。
    pub items: String,
    /// `reported_at`；无上报时为空。
    pub reported_at: String,
}

/// 解析 `GET /api/gse/agent-specs` 的响应。**不是对象数组就返回 `None`** → 调用方透传原正文。
pub fn agent_specs_rows(body: &str) -> Option<Vec<SpecsRow>> {
    let value: Value = serde_json::from_str(body).ok()?;
    let arr = value.as_array()?;
    // agent_id 是唯一必需字段；缺失说明不是预期结构。
    let mut rows = Vec::with_capacity(arr.len());
    for v in arr {
        let agent_id = str_at(v, &["agent_id"])?;
        rows.push(SpecsRow {
            agent_id: agent_id.to_string(),
            session_state: opt_str(v.get("session_state")),
            sync_status: opt_str(v.get("sync_status")),
            items: v
                .pointer("/desired/spec/items")
                .and_then(Value::as_array)
                .map(|a| a.len().to_string())
                .unwrap_or_default(),
            reported_at: opt_str(v.get("reported_at")),
        });
    }
    Some(rows)
}

/// `agents spec get --table` 的关键字段（服务端 `AgentSpecView`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecGetRow {
    pub agent_id: String,
    pub session_state: String,
    pub sync_status: String,
    pub desired_revision: String,
    pub applied_revision: String,
    pub applied_outcome: String,
    pub item_count: String,
    pub not_enforced: String,
    pub diff_empty: String,
    pub updated_at: String,
    pub reported_at: String,
}

/// 解析单台 Agent 的 spec 视图。`desired` / `applied` / `diff` 三者都可能为 `null`。
pub fn agent_spec_get_row(body: &str) -> Option<SpecGetRow> {
    let v: Value = serde_json::from_str(body).ok()?;
    let agent_id = str_at(&v, &["agent_id"])?;
    let not_enforced = v
        .pointer("/applied/not_enforced")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    // `diff` 为 null 表示未曾比较过（无生效值）；是空对象表示「比过且无差异」。
    let diff_empty = match v.get("diff") {
        None | Some(Value::Null) => String::new(),
        Some(d) => {
            let empty = d
                .pointer("/items")
                .map(|items| {
                    ["added", "removed", "changed"].iter().all(|k| {
                        items
                            .get(k)
                            .and_then(Value::as_array)
                            .is_none_or(|a| a.is_empty())
                    })
                })
                .unwrap_or(true)
                && d.get("params")
                    .and_then(Value::as_object)
                    .is_none_or(|p| p.is_empty());
            if empty {
                "empty".to_string()
            } else {
                "diff".to_string()
            }
        }
    };
    Some(SpecGetRow {
        agent_id: agent_id.to_string(),
        session_state: opt_str(v.get("session_state")),
        sync_status: opt_str(v.get("sync_status")),
        desired_revision: opt_str(v.pointer("/desired/revision")),
        applied_revision: opt_str(v.pointer("/applied/revision")),
        applied_outcome: opt_str(v.pointer("/applied/outcome")),
        item_count: v
            .pointer("/desired/spec/items")
            .and_then(Value::as_array)
            .map(|a| a.len().to_string())
            .unwrap_or_default(),
        not_enforced,
        diff_empty,
        updated_at: opt_str(v.get("updated_at")),
        reported_at: opt_str(v.get("reported_at")),
    })
}

/// 按空格填充到固定列宽（不用 `\t`：终端宽度不一致会错列）。
pub fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }
    }
    let mut out = String::new();
    let line = |cells: Vec<String>| {
        let padded: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| pad(c, widths.get(i).copied().unwrap_or(0)))
            .collect();
        format!("{}\n", padded.join("  ").trim_end())
    };
    out.push_str(&line(headers.iter().map(|h| h.to_string()).collect()));
    for row in rows {
        out.push_str(&line(row.clone()));
    }
    out
}

fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        return s.to_string();
    }
    format!("{s}{}", " ".repeat(width - len))
}

pub fn render_specs_table(rows: &[SpecsRow]) -> String {
    let body: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                r.agent_id.clone(),
                r.session_state.clone(),
                r.sync_status.clone(),
                r.items.clone(),
                r.reported_at.clone(),
            ]
        })
        .collect();
    render_table(
        &[
            "agent_id",
            "session_state",
            "sync_status",
            "items",
            "reported_at",
        ],
        &body,
    )
}

pub fn render_spec_get_table(row: &SpecGetRow) -> String {
    // 键值对形态比横向表格好读：字段名长短差异大，且值里有逗号。
    let pairs = [
        ("agent_id", row.agent_id.as_str()),
        ("session_state", row.session_state.as_str()),
        ("sync_status", row.sync_status.as_str()),
        ("desired.revision", row.desired_revision.as_str()),
        ("applied.revision", row.applied_revision.as_str()),
        ("applied.outcome", row.applied_outcome.as_str()),
        ("items", row.item_count.as_str()),
        ("not_enforced", row.not_enforced.as_str()),
        ("diff", row.diff_empty.as_str()),
        ("updated_at", row.updated_at.as_str()),
        ("reported_at", row.reported_at.as_str()),
    ];
    let starts_empty = |v: &str| v.trim().is_empty();
    let head: Vec<usize> = pairs
        .iter()
        .filter(|(k, _)| !starts_empty(k))
        .map(|(k, _)| k.len())
        .collect();
    let key_width = head.into_iter().max().unwrap_or(0);
    let mut out = String::new();
    for (k, v) in pairs {
        out.push_str(&format!("{}: {}\n", pad(k, key_width), v));
    }
    out
}
