//! `agents status`：把「期望 spec（GSE）」与「实际流索引（dataserver）」按
//! `(agent_id, data_type, data_id == item_id)` join，逐行给出 reporting / stale / not_reporting。
//!
//! 设计口径见 `.monkeycode/specs/vmctl-collect-chain/design.md`：
//! - 判定函数是**纯函数**（`now_micros` 由调用方传入），便于边界单测；
//! - 一个采集项可能产出多个 data_type，故判定的粒度是 `(item_id, data_type)` 而不是 item_id。

use serde_json::Value;

use vmctl::{Client, DataClient, Output, Transport};

/// 一个采集项产出的 data_type。依据是各采集器的 push 调用点
/// （`crates/gse-agent-core/src/collect/*.rs`，见 design.md 参考脚注 1）。
///
/// 注意 `ebpf_*` 还会往 `ebpf`（原始事件）写，但那是**可选开关**
/// （`raw_events_enabled`），纳入核验会把「刻意关掉原始事件」误判成采集故障，故不纳入。
pub const KIND_DATA_TYPES: &[(&str, &[&str])] = &[
    ("metrics_host", &["metrics"]),
    ("log_file", &["logs"]),
    ("log_k8s_stdout", &["logs"]),
    ("apm_otlp", &["traces"]),
    // `ebpf_network` 是本家族里**唯一**产出边记录的（`EbpfItemKind::emits_edges`）。
    ("ebpf_network", &["ebpf_edges"]),
    // `ebpf_tcp` **只产出指标**：它与 `ebpf_network` 用不同的 map（`TCP_AGG` vs `CONN_AGG`），
    // 若两边都发边记录，同一个 `record_id` 会被后写的覆盖（sqlite 主键覆盖写）→ 两侧数据互丢。
    // 故它没有 `ebpf_edges`。这条在 2026-10-01 的 cloud3 真集群验收中实测抓到：
    // 初版写成 `ebpf_edges`，导致现网一个健康的 `ebpf_tcp` 采集项被报成 `not_reporting`。
    ("ebpf_tcp", &["metrics"]),
    ("ebpf_process", &["metrics"]),
    ("ebpf_syscall", &["metrics"]),
];

/// 采集项缺 `interval_secs` 时的缺省间隔（秒）。
pub const DEFAULT_INTERVAL_SECS: u64 = 15;
/// 判定阈值下限（秒）：短间隔采集项也不能因为一次抖动被判 stale。
pub const MIN_THRESHOLD_SECS: u64 = 60;
/// 阈值 = max(3 × interval, 60) 秒。
pub const INTERVAL_MULTIPLIER: u64 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Reporting,
    Stale,
    NotReporting,
    Unknown,
    /// kind 不在映射表里（新 kind）—— 不参与退出码判定。
    UnknownKind,
    /// 采集项 `enabled = false` —— 不参与退出码判定。
    Disabled,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reporting => "reporting",
            Self::Stale => "stale",
            Self::NotReporting => "not_reporting",
            Self::Unknown => "unknown",
            Self::UnknownKind => "unknown_kind",
            Self::Disabled => "disabled",
        }
    }

    /// 是否参与「链路是否通」的判定。
    pub fn counts_toward_verdict(self) -> bool {
        matches!(
            self,
            Self::Reporting | Self::Stale | Self::NotReporting | Self::Unknown
        )
    }
}

/// 判定阈值（秒）。
pub fn threshold_secs(interval_secs: u64) -> u64 {
    (INTERVAL_MULTIPLIER * interval_secs).max(MIN_THRESHOLD_SECS)
}

/// `interval_secs` 归一：缺失 / 非数字 / ≤0 一律按 [`DEFAULT_INTERVAL_SECS`]。
pub fn interval_secs_of(collector: Option<&Value>) -> u64 {
    collector
        .and_then(|c| {
            c.get("interval_secs")
                .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        })
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_INTERVAL_SECS)
}

/// 纯函数判定：不读时钟、不发请求。
///
/// `last_seen_micros` 为 `None` 表示没有 stream 记录。
pub fn classify(last_seen_micros: Option<i64>, interval_secs: u64, now_micros: i64) -> Verdict {
    let Some(last_seen) = last_seen_micros else {
        return Verdict::NotReporting;
    };
    let threshold_micros = threshold_secs(interval_secs) as i128 * 1_000_000;
    // 时钟不同步时 last_seen 可能落在未来：那是最新鲜的一种，判 reporting。
    let age = (now_micros as i128) - (last_seen as i128);
    if age <= threshold_micros {
        Verdict::Reporting
    } else {
        Verdict::Stale
    }
}

/// `agents status` 的一行：一个采集项的一个 data_type。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusRow {
    pub item_id: String,
    pub data_type: String,
    pub last_seen_micros: Option<i64>,
    pub accepted: i64,
    pub interval_secs: u64,
    pub verdict: Verdict,
}

/// 需要核验的采集项（从 `desired.spec.items` 展开）。
#[derive(Debug, Clone, PartialEq)]
pub struct ItemSpec {
    pub item_id: String,
    pub kind: String,
    pub enabled: bool,
    pub interval_secs: u64,
}

/// 从 spec 视图的 `desired.spec.items` 展开采集项。
///
/// 以 **`desired`** 为准（期望值），不是 `applied` —— 后者只反映「上次下发成功的内容」，
/// 保存过但没下发时它还是旧的。
pub fn expand_items(spec_view: &Value) -> Vec<ItemSpec> {
    let Some(items) = spec_view
        .pointer("/desired/spec/items")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|it| {
            let item_id = it.get("item_id")?.as_str()?.to_string();
            Some(ItemSpec {
                item_id,
                kind: it
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                enabled: it.get("enabled").and_then(Value::as_bool).unwrap_or(false),
                interval_secs: interval_secs_of(it.get("collector")),
            })
        })
        .collect()
}

/// dataserver `/v1/streams` 的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRow {
    pub agent_id: String,
    pub data_type: String,
    pub data_id: String,
    pub last_seen_micros: i64,
    pub accepted: i64,
}

/// 解析 `/v1/streams` 响应。缺字段的行跳过（不报错）：一条脏 stream 不该让整条命令失败。
pub fn parse_streams(body: &str) -> Option<Vec<StreamRow>> {
    let v: Value = serde_json::from_str(body).ok()?;
    let arr = v.get("streams")?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|s| {
                Some(StreamRow {
                    agent_id: s.get("agent_id")?.as_str()?.to_string(),
                    data_type: s.get("data_type")?.as_str()?.to_string(),
                    data_id: s.get("data_id")?.as_str()?.to_string(),
                    last_seen_micros: s.get("last_seen_micros")?.as_i64()?,
                    accepted: s.get("accepted").and_then(Value::as_i64).unwrap_or(0),
                })
            })
            .collect(),
    )
}

/// 展开 + join + 判定。
///
/// `streams_available = false`（数据面不可达）时所有行的 verdict 为 `Unknown` ——
/// **不能**当成 `NotReporting`：那会把「查不到」误报成「没在采」。
pub fn build_rows(
    items: &[ItemSpec],
    streams: &[StreamRow],
    agent_id: &str,
    now_micros: i64,
    streams_available: bool,
) -> Vec<StatusRow> {
    let mut rows = Vec::new();
    for item in items {
        let Some(types) = KIND_DATA_TYPES
            .iter()
            .find(|(k, _)| *k == item.kind)
            .map(|(_, t)| *t)
        else {
            rows.push(StatusRow {
                item_id: item.item_id.clone(),
                data_type: String::new(),
                last_seen_micros: None,
                accepted: 0,
                interval_secs: item.interval_secs,
                verdict: Verdict::UnknownKind,
            });
            continue;
        };
        for data_type in types {
            // 三键都相等才算这个采集项在该类型上有数据。
            let found = streams.iter().find(|s| {
                s.agent_id == agent_id && s.data_type == *data_type && s.data_id == item.item_id
            });
            let verdict = if !item.enabled {
                Verdict::Disabled
            } else if !streams_available {
                Verdict::Unknown
            } else {
                classify(
                    found.map(|s| s.last_seen_micros),
                    item.interval_secs,
                    now_micros,
                )
            };
            rows.push(StatusRow {
                item_id: item.item_id.clone(),
                data_type: (*data_type).to_string(),
                last_seen_micros: found.map(|s| s.last_seen_micros),
                accepted: found.map(|s| s.accepted).unwrap_or(0),
                interval_secs: item.interval_secs,
                verdict,
            });
        }
    }
    rows
}

/// 人类可读的相对年龄（`12s` / `5m3s` / `2h4m`）。
pub fn relative_age(last_seen_micros: i64, now_micros: i64) -> String {
    let mut secs = ((now_micros as i128) - (last_seen_micros as i128)) / 1_000_000;
    if secs < 0 {
        secs = 0; // 未来时间视为 0（时钟不同步）
    }
    let secs = secs as u64;
    if secs < 60 {
        return format!("{secs}s");
    }
    let (m, s) = (secs / 60, secs % 60);
    if m < 60 {
        return format!("{m}m{s}s");
    }
    let (h, m) = (m / 60, m % 60);
    if h < 24 {
        return format!("{h}h{m}m");
    }
    format!("{}d{}h", h / 24, h % 24)
}

/// `agents status` 渲染结果。
pub struct StatusReport {
    pub stdout: String,
    pub code: u8,
}

/// 渲染并判定退出码。
///
/// 退出码 0 的充要条件：`sync_status == "synced"` **且**全部 enabled 采集项的全部 data_type
/// 均为 `reporting`。
pub fn render_status(
    agent_id: &str,
    spec_sync_status: &str,
    rows: &[StatusRow],
    now_micros: i64,
    streams_note: Option<&str>,
) -> StatusReport {
    let dirty = spec_sync_status != "synced";
    let mut out = String::new();
    out.push_str(&format!("agent_id: {agent_id}\n"));
    let sync_line = if spec_sync_status.is_empty() {
        "spec: unknown".to_string()
    } else if dirty {
        // 未下发时 stream 不可能更新：必须与「已下发未上报」区分开，否则会把
        // 忘掉 apply 误判成采集故障。
        format!("spec: {spec_sync_status} (dirty - spec saved but not applied)")
    } else {
        format!("spec: {spec_sync_status}")
    };
    out.push_str(&format!("{sync_line}\n\n"));

    let body: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                r.item_id.clone(),
                r.data_type.clone(),
                r.interval_secs.to_string(),
                r.last_seen_micros
                    .map(|ls| relative_age(ls, now_micros))
                    .unwrap_or_default(),
                r.accepted.to_string(),
                r.verdict.as_str().to_string(),
            ]
        })
        .collect();
    out.push_str(&crate::spec::render_table(
        &[
            "item_id",
            "data_type",
            "interval",
            "last_seen",
            "accepted",
            "verdict",
        ],
        &body,
    ));

    if let Some(note) = streams_note {
        out.push_str(&format!("\nstreams: {note}\n"));
    }

    let failing: Vec<String> = rows
        .iter()
        .filter(|r| r.verdict.counts_toward_verdict() && r.verdict != Verdict::Reporting)
        .map(|r| {
            let ty = if r.data_type.is_empty() {
                r.verdict.as_str().to_string()
            } else {
                format!("{} ({})", r.item_id, r.data_type)
            };
            format!("{ty}={}", r.verdict.as_str())
        })
        .collect();
    let mut reasons = Vec::new();
    if dirty {
        reasons.push(format!("spec not synced ({spec_sync_status})"));
    }
    if !failing.is_empty() {
        reasons.push(format!("not reporting: {}", failing.join(", ")));
    }
    if reasons.is_empty() {
        out.push_str("\nsummary: ok\n");
        StatusReport {
            stdout: out,
            code: 0,
        }
    } else {
        out.push_str(&format!("\nsummary: {}\n", reasons.join("; ")));
        StatusReport {
            stdout: out,
            code: 1,
        }
    }
}

/// 当前 Unix 微秒。单独成函数是为了让纯函数测试不受影响。
pub fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// `agents status <id>`：GSE 的 spec 与数据面的 streams **并发**取。
///
/// 两个请求相互独立，串行会把延迟相加；为了两个 GET 引 async runtime 不划算
/// （`vmctl` 是纯同步 `ureq` 客户端），`thread::scope` 两行解决。
pub fn run<T: Transport + Sync>(
    client: &Client<'_, T>,
    data: &DataClient<'_, T>,
    agent_id: &str,
) -> Output {
    let now = now_micros();
    let (spec_out, streams_res) = std::thread::scope(|s| {
        let spec_handle = s.spawn(|| client.agents_spec_get(agent_id));
        let streams_handle = s.spawn(|| data.streams());
        (
            spec_handle
                .join()
                .unwrap_or_else(|_| Output::err(1, "spec fetch panicked\n".into())),
            streams_handle
                .join()
                .unwrap_or_else(|_| Err("streams fetch panicked".to_string())),
        )
    });

    if spec_out.code != 0 {
        // 没有 spec 就没有核验主体：不把 streams 的缺失混进错误信息。
        return Output::err(
            1,
            format!(
                "error: cannot read spec for agent {agent_id}: {}\n",
                spec_out.stderr.trim()
            ),
        );
    }
    let spec_view: Value = match serde_json::from_str(&spec_out.stdout) {
        Ok(v) => v,
        Err(e) => return Output::err(1, format!("error: spec response is not valid JSON: {e}\n")),
    };
    if spec_view.pointer("/desired/spec/items").is_none() {
        return Output::err(
            1,
            format!(
                "error: agent {agent_id} has no saved spec (desired is empty); save one with \
                 `vmctl agents spec put {agent_id} -f <spec.json>`\n"
            ),
        );
    }

    let (streams, streams_available, note) = match streams_res {
        Ok((status, body)) if (200..300).contains(&status) => match parse_streams(&body) {
            Some(rows) => (rows, true, None),
            None => (
                Vec::new(),
                false,
                Some("response is not the expected structure".to_string()),
            ),
        },
        Ok((status, body)) => (
            Vec::new(),
            false,
            Some(format!("HTTP {status}: {}", body.trim())),
        ),
        Err(e) => (Vec::new(), false, Some(format!("unreachable: {e}"))),
    };

    let items = expand_items(&spec_view);
    let rows = build_rows(&items, &streams, agent_id, now, streams_available);
    let sync_status = spec_view
        .get("sync_status")
        .and_then(Value::as_str)
        .unwrap_or("");
    let report = render_status(agent_id, sync_status, &rows, now, note.as_deref());
    Output {
        stdout: report.stdout,
        stderr: String::new(),
        code: report.code,
    }
}
