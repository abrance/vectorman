//! `agents doctor <id>`：把链路每一段聚合到一条命令里。
//!
//! 设计口径（`.monkeycode/specs/vmctl-collect-chain/design.md`）：
//! - **只读**：任何 `POST` / `PUT` 都不发（有单测断言）；
//! - 段 1（Agent 台账）404 立即短路 —— 无主体可诊断；
//! - 其余段失败只标 `unknown` 并记进 summary，**不中断**整条命令。

use serde_json::Value;
use vmctl::{Client, DataClient, Output, Transport};

use crate::status;

/// 一线一段的结果。
struct Section {
    title: &'static str,
    lines: Vec<String>,
}

impl Section {
    fn unknown(title: &'static str, why: String) -> Self {
        Self {
            title,
            lines: vec![format!("unknown: {why}")],
        }
    }
}

/// `agents doctor <id>`。
pub fn run<T: Transport + Sync>(
    client: &Client<'_, T>,
    data: &DataClient<'_, T>,
    agent_id: &str,
) -> Output {
    let now = status::now_micros();

    // ---- 段 1：Agent 台账（**先查，404 短路**）----
    //
    // 两个端点都要：`GET /api/gse/agents/{id}` 返回的是裸 `Agent`（台账字段 + token），
    // **不含 `session_state`**；会话口径只在列表端点上。没有会话就不知道「能不能下发」，
    // 而会话与本仓库踩过的「心跳新鲜但连接已死」是两件事，不能不查。
    //
    // 这里的顺序是刻意的：先确认主体存在，再把它余五个请求并发发出。
    // 若先并发再判，Agent 不存在时依旧会打出 6 个请求（实测定过），白花四次往返。
    let (agent_out, agents_list_out) = std::thread::scope(|s| {
        let one = s.spawn(|| client.agents_get(agent_id));
        let list = s.spawn(|| client.agents_list());
        (
            one.join()
                .unwrap_or_else(|_| Output::err(1, "agent fetch panicked\n".into())),
            list.join()
                .unwrap_or_else(|_| Output::err(1, "agent list panicked\n".into())),
        )
    });
    if agent_out.code != 0 {
        return Output::err(
            1,
            format!(
                "error: cannot read agent {agent_id} from ledger: {}\n",
                agent_out.stderr.trim()
            ),
        );
    }
    let agent: Value = match serde_json::from_str(&agent_out.stdout) {
        Ok(v) => v,
        Err(e) => return Output::err(1, format!("error: agent response is not valid JSON: {e}\n")),
    };
    let session_state = session_state_of(&agents_list_out.stdout, agent_id)
        .map(|(state, _)| state)
        .unwrap_or_default();
    let heartbeat_status = agent
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let job_channel = session_state_of(&agents_list_out.stdout, agent_id)
        .map(|(_, avail)| avail.to_string())
        .unwrap_or_default();
    let mut sections = vec![Section {
        title: "agent ledger",
        lines: vec![
            kv("agent_id", agent_id),
            kv("host_id", s(&agent, "host_id")),
            kv("version", s(&agent, "version")),
            // 心跳口径与会话口径**都**打出来：心跳新鲜但会话已死是本仓库踩过的坑。
            kv("status (heartbeat)", heartbeat_status),
            kv("session_state", &session_state),
            kv("job_channel_available", &job_channel),
            kv("last_heartbeat_at", s(&agent, "last_heartbeat_at")),
        ],
    }];

    // ---- 段 2-5：四个独立读，并发 ----
    let (spec_out, streams_res, caps_res, health_res) = std::thread::scope(|s| {
        let spec_handle = s.spawn(|| client.agents_spec_get(agent_id));
        let streams_handle = s.spawn(|| data.streams());
        let caps_handle = s.spawn(|| data.ebpf_capability());
        let health_handle = s.spawn(|| data.health());
        (
            spec_handle
                .join()
                .unwrap_or_else(|_| Output::err(1, "spec fetch panicked\n".into())),
            streams_handle
                .join()
                .unwrap_or_else(|_| Err("streams fetch panicked".to_string())),
            caps_handle
                .join()
                .unwrap_or_else(|_| Err("capability fetch panicked".to_string())),
            health_handle
                .join()
                .unwrap_or_else(|_| Err("health fetch panicked".to_string())),
        )
    });

    // ---- 段 2：spec 同步 ----
    let mut sync_ok = None;
    if spec_out.code != 0 {
        sections.push(Section::unknown(
            "spec",
            format!("not readable: {}", spec_out.stderr.trim()),
        ));
    } else {
        match serde_json::from_str::<Value>(&spec_out.stdout) {
            Ok(v) => {
                let sync_status = v.get("sync_status").and_then(Value::as_str).unwrap_or("");
                let not_enforced = v
                    .pointer("/applied/not_enforced")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .filter(|x| !x.is_empty())
                    .unwrap_or_else(|| "none".to_string());
                sync_ok = Some(sync_status == "synced");
                sections.push(Section {
                    title: "spec",
                    lines: vec![
                        kv("sync_status", sync_status),
                        kv("desired.revision", ptr(&v, "/desired/revision")),
                        kv("applied.revision", ptr(&v, "/applied/revision")),
                        kv("applied.outcome", ptr(&v, "/applied/outcome")),
                        kv("not_enforced", not_enforced),
                    ],
                });
            }
            Err(e) => sections.push(Section::unknown("spec", format!("invalid JSON: {e}"))),
        }
    }

    // ---- 段 3：采集项核验（复用 status 的判定）----
    let spec_view: Option<Value> = serde_json::from_str(&spec_out.stdout).ok();
    let (streams, streams_available, streams_note) = match streams_res {
        Ok((code, body)) if (200..300).contains(&code) => match status::parse_streams(&body) {
            Some(rows) => (rows, true, None),
            None => (
                Vec::new(),
                false,
                Some("response not the expected structure".to_string()),
            ),
        },
        Ok((code, body)) => (
            Vec::new(),
            false,
            Some(format!("HTTP {code}: {}", body.trim())),
        ),
        Err(e) => (Vec::new(), false, Some(format!("unreachable: {e}"))),
    };
    let mut collect_ok = None;
    match spec_view.as_ref() {
        Some(v) if v.pointer("/desired/spec/items").is_some() => {
            let items = status::expand_items(v);
            let rows = status::build_rows(&items, &streams, agent_id, now, streams_available);
            let body: Vec<Vec<String>> = rows
                .iter()
                .map(|r| {
                    vec![
                        r.item_id.clone(),
                        r.data_type.clone(),
                        r.last_seen_micros
                            .map(|ls| status::relative_age(ls, now))
                            .unwrap_or_default(),
                        r.accepted.to_string(),
                        r.verdict.as_str().to_string(),
                    ]
                })
                .collect();
            let bad: Vec<String> = rows
                .iter()
                .filter(|r| {
                    r.verdict.counts_toward_verdict() && r.verdict != status::Verdict::Reporting
                })
                .map(|r| format!("{} ({})={}", r.item_id, r.data_type, r.verdict.as_str()))
                .collect();
            collect_ok = Some(bad.is_empty() && streams_available);
            let mut lines = vec![crate::spec::render_table(
                &["item_id", "data_type", "last_seen", "accepted", "verdict"],
                &body,
            )
            .trim_end()
            .to_string()];
            if let Some(note) = streams_note.as_deref() {
                lines.push(kv("streams", note));
            }
            if !bad.is_empty() {
                lines.push(kv("not reporting", bad.join(", ")));
            }
            sections.push(Section {
                title: "collect items",
                lines,
            });
        }
        _ => sections.push(Section::unknown(
            "collect items",
            "no saved spec (desired is empty)".to_string(),
        )),
    }

    // ---- 段 4：eBPF 能力 ----
    match caps_res {
        Ok((code, body)) if (200..300).contains(&code) => {
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let reported = v.get("reported").and_then(Value::as_i64).unwrap_or(0);
            let unavailable: Vec<String> = v
                .get("agents")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter(|x| x.get("available").and_then(Value::as_bool) != Some(true))
                        .map(|x| {
                            let id = x.get("agent_id").and_then(Value::as_str).unwrap_or("?");
                            let why = x
                                .get("reason")
                                .and_then(Value::as_str)
                                .unwrap_or("no reason reported");
                            format!("{id}: {why}")
                        })
                        .collect()
                })
                .unwrap_or_default();
            let lines = vec![
                kv("reported", reported.to_string()),
                kv(
                    "unavailable",
                    &if unavailable.is_empty() {
                        "none".to_string()
                    } else {
                        unavailable.join("; ")
                    },
                ),
            ];
            sections.push(Section {
                title: "ebpf capability",
                lines,
                // 能力报告是「有没有 eBPF 采集项」的前提，不是链路通断的必要条件，
                // 故不计入退出码（unknown 语义）。
            });
        }
        Ok((code, body)) => sections.push(Section::unknown(
            "ebpf capability",
            format!("HTTP {code}: {}", body.trim()),
        )),
        Err(e) => sections.push(Section::unknown(
            "ebpf capability",
            format!("unreachable: {e}"),
        )),
    }

    // ---- 段 5：数据面连通性 ----
    let health_ok = match health_res {
        Ok((code, _)) if (200..300).contains(&code) => Some(true),
        Ok((code, _)) => {
            sections.push(Section {
                title: "dataplane health",
                lines: vec![kv("HTTP", code.to_string())],
            });
            Some(false)
        }
        Err(e) => {
            sections.push(Section::unknown(
                "dataplane health",
                format!("unreachable: {e}"),
            ));
            None
        }
    };

    // ---- 汇总 ----
    let mut out = String::new();
    for sec in &sections {
        out.push_str(&format!("== {} ==\n", sec.title));
        for line in &sec.lines {
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    let mut reasons: Vec<String> = Vec::new();
    if session_state != "online" {
        reasons.push(format!("session_state={session_state} (expected online)"));
    }
    if health_ok != Some(true) {
        reasons.push("dataplane health failed or unknown".to_string());
    }
    if sync_ok != Some(true) {
        reasons.push(format!(
            "spec not synced ({})",
            spec_view
                .as_ref()
                .and_then(|v| v.get("sync_status"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ));
    }
    if collect_ok != Some(true) {
        reasons.push("collect items not all reporting".to_string());
    }
    let code = if reasons.is_empty() { 0 } else { 1 };
    if reasons.is_empty() {
        out.push_str("summary: ok\n");
    } else {
        out.push_str(&format!("summary: {}\n", reasons.join("; ")));
    }
    Output {
        stdout: out,
        stderr: String::new(),
        code,
    }
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn ptr(v: &Value, path: &str) -> String {
    v.pointer(path)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn kv(k: &str, v: impl AsRef<str>) -> String {
    format!("{k}: {}", v.as_ref())
}

/// 从 `GET /api/gse/agents` 列表里取该 Agent 的 `(session_state, job_channel_available)`。
///
/// 列表端点才有会话口径；单台端口返回裸 `Agent`。解析失败返回 `None` —— 调用方按空值处理，
/// 不把「查不到」当成「offline」。
fn session_state_of(list_body: &str, agent_id: &str) -> Option<(String, bool)> {
    let v: Value = serde_json::from_str(list_body).ok()?;
    let arr = v.as_array()?;
    let found = arr
        .iter()
        .find(|a| a.get("agent_id").and_then(Value::as_str) == Some(agent_id))?;
    Some((
        found
            .get("session_state")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        found
            .get("job_channel_available")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    ))
}
