//! tracepoint 字段偏移解析（`/sys/kernel/tracing/events/<cat>/<name>/format`）。
//!
//! 为什么要在运行期读：tracepoint 的字段偏移由 `TP_STRUCT__entry` 决定，会随内核版本变化
//! （例如 `inet_sock_set_state` 在 5.19 追加了 `cookie`），**不能硬编码**。文件不可读时
//! （非 root 又没开 tracing 权限）退回 [`DEFAULTS`]，并在日志里说明用了兜底值。
//!
//! 文件格式（每行一条字段）：
//!
//! ```text
//! field:unsigned short common_type;  offset:0;  size:2;  signed:0;
//! field:const void * skaddr;         offset:8;  size:8;  signed:0;
//! field:__u16 sport;                 offset:24; size:2;  signed:0;
//! field:__u8 saddr[4];               offset:30; size:4;  signed:0;
//! ```
//!
//! 真实文件里分隔符是制表符，解析时按 `;` 切分并 `trim`，两种都吃。

/// 一个字段的偏移（字节）与大小（字节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldOffset {
    pub offset: u32,
    pub size: u32,
}

impl FieldOffset {
    const fn new(offset: u32, size: u32) -> Self {
        Self { offset, size }
    }
}

/// `sock/inet_sock_set_state` 的字段名集合。
pub const SOCK_STATE_FIELDS: [&str; 9] = [
    "oldstate", "newstate", "sport", "dport", "family", "saddr", "daddr", "saddr_v6", "daddr_v6",
];

/// 5.8–6.x 的兜底布局（`include/trace/events/sock.h`）。
///
/// `struct trace_entry`（`common_*`）占 8 字节，其后依次是各字段；`cookie`（5.19 追加）
/// 在最后，因此它之前字段的偏移不受影响。读不到 `format` 文件时用这套值，
/// 偏移一旦不符会表现为「采集不到/记错地址」，此时**更该修权限而不是改这里的数字**。
pub const DEFAULTS: [(&str, FieldOffset); 9] = [
    ("oldstate", FieldOffset::new(16, 4)),
    ("newstate", FieldOffset::new(20, 4)),
    ("sport", FieldOffset::new(24, 2)),
    ("dport", FieldOffset::new(26, 2)),
    ("family", FieldOffset::new(28, 2)),
    ("saddr", FieldOffset::new(30, 4)),
    ("daddr", FieldOffset::new(34, 4)),
    ("saddr_v6", FieldOffset::new(38, 16)),
    ("daddr_v6", FieldOffset::new(54, 16)),
];

/// 解析 `format` 文本，返回「字段名 → 偏移」。
///
/// 解析失败的字段不返回；调用方应检查需要的字段是否齐全（不齐全就别采）。
#[must_use]
pub fn parse(text: &str) -> std::collections::HashMap<String, FieldOffset> {
    let mut out = std::collections::HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("field:") else {
            continue;
        };
        // `field:<类型与名字>;	offset:<n>;	size:<n>;	signed:<n>;`
        let mut parts = rest.split(';');
        let Some(decl) = parts.next() else { continue };
        // 字段名是声明的最后一段；数组会带 `[N]`，这里取到 `[` 之前。
        let Some(raw_name) = decl.split_whitespace().last() else {
            continue;
        };
        let name = raw_name
            .split('[')
            .next()
            .unwrap_or(raw_name)
            .trim()
            .to_string();
        if name.is_empty() {
            continue;
        }
        let mut offset = None;
        let mut size = None;
        for part in parts {
            let part = part.trim();
            if let Some(value) = part.strip_prefix("offset:") {
                offset = value.trim().parse::<u32>().ok();
            } else if let Some(value) = part.strip_prefix("size:") {
                size = value.trim().parse::<u32>().ok();
            }
        }
        if let (Some(offset), Some(size)) = (offset, size) {
            out.insert(name, FieldOffset { offset, size });
        }
    }
    out
}

/// 取需要的字段；任何一个缺失就返回 `Err`（宁可整项不采，也不按错偏移读数）。
pub fn require(
    fields: &std::collections::HashMap<String, FieldOffset>,
    names: &[&str],
) -> Result<std::collections::HashMap<String, FieldOffset>, String> {
    let mut out = std::collections::HashMap::new();
    for name in names {
        let value = fields
            .get(*name)
            .ok_or_else(|| format!("tracepoint format 缺少字段 {name}"))?;
        out.insert((*name).to_string(), *value);
    }
    Ok(out)
}

/// 兜底布局对应的 map（用于 `format` 文件不可读时）。
#[must_use]
pub fn defaults() -> std::collections::HashMap<String, FieldOffset> {
    DEFAULTS
        .iter()
        .map(|(name, offset)| ((*name).to_string(), *offset))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实格式（6.1 内核 `sock/inet_sock_set_state`，含 common 头；5.19 起末尾多 `cookie`）。
    const FIXTURE: &str = r#"
name: inet_sock_set_state
ID: 1075
format:
	field:unsigned short common_type;	offset:0;	size:2;	signed:0;
	field:unsigned char common_flags;	offset:2;	size:1;	signed:0;
	field:unsigned char common_preempt_count;	offset:3;	size:1;	signed:0;
	field:int common_pid;	offset:4;	size:4;	signed:1;

	field:const void * skaddr;	offset:8;	size:8;	signed:0;
	field:int oldstate;	offset:16;	size:4;	signed:1;
	field:int newstate;	offset:20;	size:4;	signed:1;
	field:__u16 sport;	offset:24;	size:2;	signed:0;
	field:__u16 dport;	offset:26;	size:2;	signed:0;
	field:__u16 family;	offset:28;	size:2;	signed:0;
	field:__u8 saddr[4];	offset:30;	size:4;	signed:0;
	field:__u8 daddr[4];	offset:34;	size:4;	signed:0;
	field:__u8 saddr_v6[16];	offset:38;	size:16;	signed:0;
	field:__u8 daddr_v6[16];	offset:54;	size:16;	signed:0;
	field:__u64 cookie;	offset:72;	size:8;	signed:0;

print fmt: "..."
"#;

    #[test]
    fn parses_real_format() {
        let fields = parse(FIXTURE);
        assert_eq!(fields["oldstate"], FieldOffset::new(16, 4));
        assert_eq!(fields["newstate"], FieldOffset::new(20, 4));
        assert_eq!(fields["sport"], FieldOffset::new(24, 2));
        assert_eq!(fields["dport"], FieldOffset::new(26, 2));
        assert_eq!(fields["family"], FieldOffset::new(28, 2));
        assert_eq!(
            fields["saddr"],
            FieldOffset::new(30, 4),
            "数组名字要剥掉 [4]"
        );
        assert_eq!(fields["daddr"], FieldOffset::new(34, 4));
        assert_eq!(fields["saddr_v6"], FieldOffset::new(38, 16));
        assert_eq!(fields["daddr_v6"], FieldOffset::new(54, 16));
        assert_eq!(fields["common_pid"], FieldOffset::new(4, 4));

        let required = require(&fields, &SOCK_STATE_FIELDS).expect("字段齐全");
        assert_eq!(required.len(), 9);
    }

    #[test]
    fn fallback_defaults_match_parsed_fixture() {
        let parsed = require(&parse(FIXTURE), &SOCK_STATE_FIELDS).unwrap();
        let defaults = defaults();
        for name in SOCK_STATE_FIELDS {
            assert_eq!(
                parsed[name], defaults[name],
                "兜底值与真实 6.1 format 不一致：{name}"
            );
        }
    }

    #[test]
    fn missing_field_is_error_and_garbage_is_ignored() {
        let text = "field:int newstate;\toffset:20;\tsize:4;\nsome noise\nfield:no_semicolons\n";
        let fields = parse(text);
        assert_eq!(fields.len(), 1);
        assert!(require(&fields, &SOCK_STATE_FIELDS).is_err());
        assert!(parse("").is_empty());
    }
}
