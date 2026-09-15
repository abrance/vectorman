//! 日志清洗：先按包含/排除正则过滤，再按提取规则写入 labels。

use std::collections::BTreeMap;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 一条标签提取规则。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtractRule {
    /// `regex`（第一捕获组）或 `json`（点分路径）。
    pub kind: String,
    pub expr: String,
    pub label: String,
}

/// 清洗配置。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanConfig {
    #[serde(default)]
    pub include_regex: Option<String>,
    #[serde(default)]
    pub exclude_regex: Option<String>,
    #[serde(default)]
    pub extract: Vec<ExtractRule>,
}

/// 清洗后的行；未通过过滤返回 None。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanedLine {
    pub message: String,
    pub labels: BTreeMap<String, String>,
}

/// 已编译的清洗器，避免逐行重复编译正则。
pub struct Cleaner {
    include: Option<Regex>,
    exclude: Option<Regex>,
    extract: Vec<CompiledRule>,
}

enum CompiledRule {
    Regex { re: Regex, label: String },
    Json { path: Vec<String>, label: String },
}

impl Cleaner {
    /// 编译配置；非法正则被忽略（视为未配置该规则）。
    pub fn new(cfg: &CleanConfig) -> Self {
        Self {
            include: cfg
                .include_regex
                .as_deref()
                .filter(|s| !s.is_empty())
                .and_then(|s| Regex::new(s).ok()),
            exclude: cfg
                .exclude_regex
                .as_deref()
                .filter(|s| !s.is_empty())
                .and_then(|s| Regex::new(s).ok()),
            extract: cfg
                .extract
                .iter()
                .filter_map(|r| match r.kind.as_str() {
                    "regex" => Regex::new(&r.expr).ok().map(|re| CompiledRule::Regex {
                        re,
                        label: r.label.clone(),
                    }),
                    "json" if !r.expr.is_empty() => Some(CompiledRule::Json {
                        path: r.expr.split('.').map(|s| s.to_string()).collect(),
                        label: r.label.clone(),
                    }),
                    _ => None,
                })
                .collect(),
        }
    }

    /// 过滤并提取；通过返回清洗结果，被排除返回 None。
    pub fn clean(&self, line: &str) -> Option<CleanedLine> {
        if let Some(inc) = &self.include {
            if !inc.is_match(line) {
                return None;
            }
        }
        if let Some(exc) = &self.exclude {
            if exc.is_match(line) {
                return None;
            }
        }
        let mut labels = BTreeMap::new();
        let mut parsed_json: Option<Value> = None;
        for rule in &self.extract {
            match rule {
                CompiledRule::Regex { re, label } => {
                    if let Some(caps) = re.captures(line) {
                        if let Some(m) = caps.get(1) {
                            labels.insert(label.clone(), m.as_str().to_string());
                        }
                    }
                }
                CompiledRule::Json { path, label } => {
                    let value = parsed_json.get_or_insert_with(|| {
                        serde_json::from_str::<Value>(line).unwrap_or(Value::Null)
                    });
                    if let Some(v) = json_path(value, path) {
                        let text = match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        labels.insert(label.clone(), text);
                    }
                }
            }
        }
        Some(CleanedLine {
            message: line.to_string(),
            labels,
        })
    }
}

fn json_path<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut cur = value;
    for key in path {
        cur = cur.get(key)?;
    }
    if cur.is_null() {
        None
    } else {
        Some(cur)
    }
}

/// 从整行文本识别 level，大小写无关；识别不到返回 `info`。
pub fn detect_level(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    for (needle, level) in [
        ("fatal", "fatal"),
        ("error", "error"),
        ("warn", "warn"),
        ("debug", "debug"),
        ("info", "info"),
    ] {
        if lower.contains(needle) {
            return level.to_string();
        }
    }
    "info".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(kind: &str, expr: &str, label: &str) -> ExtractRule {
        ExtractRule {
            kind: kind.to_string(),
            expr: expr.to_string(),
            label: label.to_string(),
        }
    }

    #[test]
    fn include_and_exclude_regex_filter_lines() {
        let cleaner = Cleaner::new(&CleanConfig {
            include_regex: Some("keep".to_string()),
            exclude_regex: Some("skip".to_string()),
            extract: vec![],
        });
        assert!(cleaner.clean("keep this").is_some());
        assert!(cleaner.clean("keep skip").is_none());
        assert!(cleaner.clean("drop this").is_none());
    }

    #[test]
    fn extract_regex_and_json_labels() {
        let cleaner = Cleaner::new(&CleanConfig {
            include_regex: None,
            exclude_regex: None,
            extract: vec![
                rule("regex", r#""user":"(\w+)""#, "user"),
                rule("json", "trace.id", "trace_id"),
                rule("json", "missing", "nope"),
            ],
        });
        let out = cleaner
            .clean(r#"{"user":"alice","trace":{"id":"abc"}}"#)
            .expect("kept");
        assert_eq!(out.labels.get("user").map(String::as_str), Some("alice"));
        assert_eq!(out.labels.get("trace_id").map(String::as_str), Some("abc"));
        assert!(!out.labels.contains_key("nope"));

        // 非 JSON 行：json 规则未命中但行仍保留。
        let out = cleaner.clean(r#"not json "user":"bob""#).expect("kept");
        assert_eq!(out.labels.get("user").map(String::as_str), Some("bob"));
    }

    #[test]
    fn detect_level_first_match_wins() {
        assert_eq!(detect_level("FATAL boom"), "fatal");
        assert_eq!(detect_level("some ERROR here"), "error");
        assert_eq!(detect_level("Warning: x"), "warn");
        assert_eq!(detect_level("DEBUG x"), "debug");
        assert_eq!(detect_level("info x"), "info");
        assert_eq!(detect_level("plain message"), "info");
    }
}
