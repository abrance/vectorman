use std::process::ExitCode;

use clap::{Parser, Subcommand};
use vmctl::{
    Client, DataClient, JobRerunSpec, JobSubmitSpec, UreqTransport, WaitPolicy, DEFAULT_BASE_URL,
    DEFAULT_DATA_URL,
};

mod doctor;
mod spec;
mod status;

#[derive(Parser)]
#[command(
    name = "vmctl",
    version = vectorman_version::VERSION,
    about = "gse-server HTTP 客户端：节点只读查询与作业提交"
)]
struct Cli {
    /// gse-server HTTP 根地址
    #[arg(long, default_value = DEFAULT_BASE_URL)]
    url: String,

    /// gse-server 管理口密码（配了 GSE_SERVER_ADMIN_PASSWORD 时必填）
    #[arg(long)]
    password: Option<String>,

    /// dataserver SQL 口根地址（仅 `agents status` / `agents doctor` 使用）
    #[arg(long, default_value = DEFAULT_DATA_URL)]
    data_url: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// GET /health
    Health,
    /// Host 台账只读
    Hosts {
        #[command(subcommand)]
        cmd: HostsCmd,
    },
    /// Agent 台账只读
    Agents {
        #[command(subcommand)]
        cmd: AgentsCmd,
    },
    /// 作业提交、查询与重做
    Jobs {
        #[command(subcommand)]
        cmd: Box<JobsCmd>,
    },
}

#[derive(Subcommand)]
enum HostsCmd {
    List,
    Get { host_id: String },
}

#[derive(Subcommand)]
enum AgentsCmd {
    List,
    Get {
        agent_id: String,
    },
    /// 全部 Agent 的 spec 总览（期望 + 生效 + sync_status）
    Specs {
        /// 表格输出（`agent_id` / `session_state` / `sync_status` / `items` / `reported_at`）
        #[arg(long)]
        table: bool,
        /// 只输出该 Agent（本地过滤）
        #[arg(long)]
        agent_id: Option<String>,
    },
    /// 单台 Agent 的 spec 读取 / 保存 / 下发
    Spec {
        #[command(subcommand)]
        cmd: SpecCmd,
    },
    /// 逐采集项 × 逐 data_type 核验「采上来了没有」
    Status {
        agent_id: String,
        /// 表格输出（默认即为表格，此参数只为口径统一）
        #[arg(long)]
        table: bool,
    },
    /// 链路聚合诊断：台账 / spec / 采集项 / eBPF 能力 / 数据面连通性
    Doctor {
        agent_id: String,
    },
}

#[derive(Subcommand)]
enum SpecCmd {
    Get {
        agent_id: String,
        /// 表格输出
        #[arg(long)]
        table: bool,
    },
    /// 保存期望 spec（只写台账，不推送）
    Put {
        agent_id: String,
        /// 从文件读请求体
        #[arg(long, short = 'f')]
        file: Option<String>,
        /// 直接给出请求体
        #[arg(long)]
        json: Option<String>,
    },
    /// 下发整份 spec 到该 Agent
    Apply { agent_id: String },
}

#[derive(Subcommand)]
enum JobsCmd {
    List {
        #[arg(long)]
        agent_id: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        limit: Option<i64>,
    },
    Get {
        job_id: String,
    },
    Submit {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        agent_id: Option<String>,
        #[arg(long)]
        script_file: Option<String>,
        #[arg(long)]
        interpreter: Option<String>,
        #[arg(long = "arg")]
        args: Vec<String>,
        #[arg(long = "env")]
        env: Vec<String>,
        #[arg(long)]
        working_dir: Option<String>,
        #[arg(long)]
        timeout_secs: Option<u64>,
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        from_agent: Option<String>,
        #[arg(long)]
        from_path: Option<String>,
        #[arg(long)]
        to_agent: Option<String>,
        #[arg(long)]
        to_path: Option<String>,
        #[arg(long)]
        upload: Option<String>,
        /// `--kind agent_upgrade`：目标机上已就位的二进制路径。
        #[arg(long)]
        binary_path: Option<String>,
        /// `--kind agent_upgrade`：期望的 sha256（64 位十六进制）。
        #[arg(long)]
        sha256: Option<String>,
    },
    Rerun {
        job_id: String,
        #[arg(long)]
        agent_id: Option<String>,
        #[arg(long)]
        script_file: Option<String>,
        #[arg(long)]
        interpreter: Option<String>,
        #[arg(long = "arg")]
        args: Vec<String>,
        #[arg(long = "env")]
        env: Vec<String>,
        #[arg(long)]
        working_dir: Option<String>,
        #[arg(long)]
        timeout_secs: Option<u64>,
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        dest_path: Option<String>,
    },
    /// Server 临时文件
    Files {
        #[command(subcommand)]
        cmd: JobFilesCmd,
    },
}

#[derive(Subcommand)]
enum JobFilesCmd {
    List,
    Upload {
        #[arg(long)]
        file: String,
    },
    Download {
        file_id: String,
        #[arg(long)]
        output: String,
    },
    Delete {
        file_id: String,
    },
}

/// `agents specs`：透传或表格；解析失败退回透传（design P3）。
fn cmd_agents_specs<T: vmctl::Transport>(
    client: &Client<'_, T>,
    table: bool,
    filter: Option<&str>,
) -> vmctl::Output {
    let out = client.agents_specs();
    if out.code != 0 || !table {
        if out.code == 0 {
            if let Some(id) = filter {
                return match spec::agent_specs_rows(&out.stdout) {
                    Some(rows) => {
                        let want: Vec<spec::SpecsRow> =
                            rows.into_iter().filter(|r| r.agent_id == id).collect();
                        vmctl::Output::ok(spec::render_specs_table(&want))
                    }
                    None => vmctl::Output::err(
                        0,
                        "warning: response is not the expected structure; showing full body\n"
                            .to_string(),
                    ),
                };
            }
        }
        return out;
    }
    let Some(rows) = spec::agent_specs_rows(&out.stdout) else {
        // 不是预期结构 → 透传原正文，退出码 0。
        return out;
    };
    let wanted: Vec<spec::SpecsRow> = match filter {
        Some(id) => rows.into_iter().filter(|r| r.agent_id == id).collect(),
        None => rows,
    };
    if filter.is_some() && wanted.is_empty() {
        return vmctl::Output::ok(format!(
            "no agent matched --agent-id {}\n",
            filter.unwrap_or_default()
        ));
    }
    vmctl::Output::ok(spec::render_specs_table(&wanted))
}

/// `agents spec get`：透传或表格；解析失败退回透传。
fn cmd_spec_get<T: vmctl::Transport>(
    client: &Client<'_, T>,
    agent_id: &str,
    table: bool,
) -> vmctl::Output {
    let out = client.agents_spec_get(agent_id);
    if out.code != 0 || !table {
        return out;
    }
    match spec::agent_spec_get_row(&out.stdout) {
        Some(row) => vmctl::Output::ok(spec::render_spec_get_table(&row)),
        None => out,
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // 环境变量兜底：密码不该出现在命令行（`ps` 可见）与 shell 历史里。
    let password = cli
        .password
        .or_else(|| std::env::var("VECTORMAN_PASSWORD").ok());
    let transport = UreqTransport::with_password(password);
    let client = Client {
        base_url: cli.url,
        transport: &transport,
        wait: WaitPolicy::default(),
    };
    let data = DataClient {
        base_url: cli.data_url,
        transport: &transport,
    };
    let out = match cli.command {
        Command::Health => client.health(),
        Command::Hosts { cmd } => match cmd {
            HostsCmd::List => client.hosts_list(),
            HostsCmd::Get { host_id } => client.hosts_get(&host_id),
        },
        Command::Agents { cmd } => match cmd {
            AgentsCmd::List => client.agents_list(),
            AgentsCmd::Get { agent_id } => client.agents_get(&agent_id),
            AgentsCmd::Specs { table, agent_id } => {
                cmd_agents_specs(&client, table, agent_id.as_deref())
            }
            AgentsCmd::Spec { cmd } => match cmd {
                SpecCmd::Get { agent_id, table } => cmd_spec_get(&client, &agent_id, table),
                SpecCmd::Put {
                    agent_id,
                    file,
                    json,
                } => match spec::pick_source(file, json).and_then(|s| spec::read_spec_body(&s)) {
                    Ok(body) => client.agents_spec_put(&agent_id, &body),
                    Err(e) => vmctl::Output::err(1, format!("error: {e}\n")),
                },
                SpecCmd::Apply { agent_id } => client.agents_spec_apply(&agent_id),
            },
            AgentsCmd::Status { agent_id, .. } => status::run(&client, &data, &agent_id),
            AgentsCmd::Doctor { agent_id } => doctor::run(&client, &data, &agent_id),
        },
        Command::Jobs { cmd } => match *cmd {
            JobsCmd::List {
                agent_id,
                status,
                limit,
            } => client.jobs_list(agent_id.as_deref(), status.as_deref(), limit),
            JobsCmd::Get { job_id } => client.jobs_get(&job_id),
            JobsCmd::Submit {
                kind,
                agent_id,
                script_file,
                interpreter,
                args,
                env,
                working_dir,
                timeout_secs,
                wait,
                from_agent,
                from_path,
                to_agent,
                to_path,
                upload,
                binary_path,
                sha256,
            } => client.jobs_submit(&JobSubmitSpec {
                kind: kind.unwrap_or_default(),
                agent_id: agent_id.unwrap_or_default(),
                script_file: script_file.unwrap_or_default(),
                interpreter,
                args,
                env,
                working_dir,
                timeout_secs,
                wait,
                from_agent,
                from_path,
                to_agent,
                to_path,
                upload,
                binary_path,
                sha256,
            }),
            JobsCmd::Rerun {
                job_id,
                agent_id,
                script_file,
                interpreter,
                args,
                env,
                working_dir,
                timeout_secs,
                wait,
                dest_path,
            } => client.jobs_rerun(&JobRerunSpec {
                job_id,
                agent_id,
                script_file,
                interpreter,
                args,
                env,
                working_dir,
                timeout_secs,
                wait,
                dest_path,
            }),
            JobsCmd::Files { cmd } => match cmd {
                JobFilesCmd::List => client.jobs_files_list(),
                JobFilesCmd::Upload { file } => client.jobs_files_upload(&file),
                JobFilesCmd::Download { file_id, output } => {
                    client.jobs_files_download(&file_id, &output)
                }
                JobFilesCmd::Delete { file_id } => client.jobs_files_delete(&file_id),
            },
        },
    };
    if !out.stdout.is_empty() {
        print!("{}", out.stdout);
        if !out.stdout.ends_with('\n') {
            println!();
        }
    }
    if !out.stderr.is_empty() {
        eprint!("{}", out.stderr);
        if !out.stderr.ends_with('\n') {
            eprintln!();
        }
    }
    ExitCode::from(out.code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vmctl::test_support::Mock;

    /// 真实夹具（2026-09-30 从本机 gse-server `PUT /api/gse/agents/agent-1/spec` 实测抓取）。
    /// 用真响应而不是手写夹具：本仓库已被手写夹具掩盖过两次字段漂移。
    const REAL_SPEC_VIEW: &str = r#"{
      "agent_id":"agent-1","host_id":"host-1","session_state":"absent",
      "sync_status":"unknown","updated_at":"1790831429653846-7","reported_at":null,
      "desired":{"revision":"6c4338bcb6ca2a38","spec":{"params":{
        "heartbeat_interval_secs":30,"allowed_interpreters":["sh","bash"],
        "job_default_interpreter":"sh","max_concurrent_jobs":4,"job_work_dir":null,
        "otlp_enabled":false,"otlp_listen":"127.0.0.1:4317","otlp_max_body_bytes":1048576,
        "otlp_token":"","otlp_allowed_cidrs":[],"token":"***","cpu_limit_percent":null,
        "mem_limit_percent":null,"log_level":"info"},
        "items":[
          {"item_id":"i-metrics","name":"host metrics","kind":"metrics_host","enabled":true,
           "collector":{"interval_secs":15},"storage":{"retention_days":7}},
          {"item_id":"i-ebpf-proc","name":"ebpf process","kind":"ebpf_process","enabled":true,
           "collector":{"interval_secs":30},"storage":{"retention_days":3}},
          {"item_id":"i-logfile","name":"app log","kind":"log_file","enabled":false,
           "collector":{"interval_secs":15,"path_patterns":["/var/log/app/*.log"]},
           "storage":{"retention_days":1}}]}},
      "applied":null,"diff":null}"#;

    const REAL_SPECS_LIST: &str = r#"[{"agent_id":"agent-1","host_id":"host-1","session_state":"absent",
      "sync_status":"unknown","updated_at":"1790831429653846-7","reported_at":null,
      "desired":{"revision":"6c4338bcb6ca2a38","spec":{"params":{},"items":[{},{},{}]}},
      "applied":null,"diff":null},
      {"agent_id":"agent-2","host_id":"host-2","session_state":"absent","sync_status":"unknown",
      "updated_at":null,"reported_at":null,"desired":null,"applied":null,"diff":null}]"#;

    /// 该 Agent 有会话口径的列表响应（`session_state` 只在列表端点上）。
    const REAL_AGENTS_LIST: &str = r#"[{"agent_id":"agent-1","host_id":"host-1","token":"tok-1-rotated",
      "version":"1.3.6","install_path":"","status":"unknown","last_heartbeat_at":null,
      "registered_at":"1790831424046442-3","session_state":"absent","job_channel_available":false}]"#;

    const REAL_AGENT_ONE: &str = r#"{"agent_id":"agent-1","host_id":"host-1","access_point_id":null,
      "token":"tok-1-rotated","version":"1.3.6","install_path":"","status":"unknown",
      "last_heartbeat_at":null,"registered_at":"1790831424046442-3"}"#;

    fn ok(body: &str) -> Result<(u16, String), String> {
        Ok((200, body.to_string()))
    }

    fn client_of<'a>(m: &'a Mock) -> Client<'a, Mock> {
        Client {
            base_url: "http://127.0.0.1:7101/".into(),
            transport: m,
            wait: WaitPolicy::default(),
        }
    }

    fn data_of<'a>(m: &'a Mock) -> DataClient<'a, Mock> {
        DataClient {
            base_url: "http://127.0.0.1:8081/".into(),
            transport: m,
        }
    }

    // ---- T1: `--help` 层级（需求 R1）----

    #[test]
    fn top_level_has_exactly_four_subcommands() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let mut names: Vec<&str> = cmd.get_subcommands().map(|c| c.get_name()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["agents", "health", "hosts", "jobs"],
            "新增顶层子命令 = 违反 R1（agent 单数会与 agents 撞名）"
        );
    }

    #[test]
    fn agents_has_six_subcommands_and_spec_has_three() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let agents = cmd
            .get_subcommands()
            .find(|c| c.get_name() == "agents")
            .expect("agents");
        let mut names: Vec<&str> = agents.get_subcommands().map(|c| c.get_name()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["doctor", "get", "list", "spec", "specs", "status"]
        );
        let spec = agents
            .get_subcommands()
            .find(|c| c.get_name() == "spec")
            .expect("spec");
        let mut names: Vec<&str> = spec.get_subcommands().map(|c| c.get_name()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["apply", "get", "put"]);
    }

    // ---- T2: spec 透传类（需求 R3/R4）----

    #[test]
    fn put_rejects_both_and_neither_source_without_network() {
        for (file, json) in [(None, None), (Some("a.json".into()), Some("{}".into()))] {
            let mock = Mock::new(vec![]);
            let c = client_of(&mock);
            let err = spec::pick_source(file, json).and_then(|s| spec::read_spec_body(&s));
            assert!(err.is_err(), "expected local error");
            assert_eq!(mock.call_count(), 0, "must not touch the network");
            let _ = &c;
        }
    }

    #[test]
    fn put_rejects_non_object_and_missing_file() {
        let arr = spec::read_spec_body(&spec::SpecSource::Inline("[1,2]".into())).unwrap_err();
        assert!(arr.contains("expected a JSON object"), "{arr}");
        assert!(arr.contains("array"), "{arr}");

        let scalar = spec::read_spec_body(&spec::SpecSource::Inline("42".into())).unwrap_err();
        assert!(scalar.contains("expected a JSON object"), "{scalar}");

        let missing =
            spec::read_spec_body(&spec::SpecSource::File("/nope/absent.json".into())).unwrap_err();
        assert!(missing.contains("/nope/absent.json"), "{missing}");
    }

    #[test]
    fn put_forwards_body_verbatim_without_defaults() {
        // 只给部分 params：CLI 不得补全（服务端用 double_option 区分「缺失=不修改」）。
        let body = r#"{"params":{"otlp_enabled":true},"items":[]}"#;
        let got = spec::read_spec_body(&spec::SpecSource::Inline(body.into())).expect("ok");
        let v: serde_json::Value = serde_json::from_str(&got).expect("json");
        let params = v.get("params").expect("params").as_object().expect("obj");
        assert_eq!(
            params.len(),
            1,
            "CLI 补了默认值 -> allowed_interpreters 会被重置，Agent 配置静默丢失"
        );
    }

    #[test]
    fn put_body_never_echoes_token_on_error() {
        // 错误信息里不得回显请求体（可能含 token / otlp_token）。
        let body = r#"{"params":{"token":"s3cret"},"items":[]}"#;
        let out = client_of(&Mock::new(vec![Ok((
            400,
            r#"{"code":"invalid_argument","error":"bad"}"#.to_string(),
        ))]))
        .agents_spec_put("agent-1", body);
        assert_eq!(out.code, 1);
        assert!(
            !out.stderr.contains("s3cret"),
            "token leaked: {}",
            out.stderr
        );
        assert!(out.stderr.contains("bad"));
    }

    #[test]
    fn spec_put_uses_put_method_and_encoded_path() {
        let mock = Mock::new(vec![ok(REAL_SPEC_VIEW)]);
        let c = client_of(&mock);
        let out = c.agents_spec_put("agent 1", r#"{"params":{},"items":[]}"#);
        assert_eq!(out.code, 0);
        let calls = mock.calls();
        assert_eq!(calls[0].0, "PUT");
        assert_eq!(
            calls[0].1,
            "http://127.0.0.1:7101/api/gse/agents/agent%201/spec"
        );
    }

    #[test]
    fn spec_apply_404_and_409_exit_one_with_server_body() {
        for (status, body) in [
            (
                404,
                r#"{"code":"not_found","error":"agent ghost 没有 spec"}"#,
            ),
            (
                409,
                r#"{"code":"agent_offline","error":"agent agent-1 无会话"}"#,
            ),
        ] {
            let out = client_of(&Mock::new(vec![Ok((status, body.to_string()))]))
                .agents_spec_apply("agent-1");
            assert_eq!(out.code, 1);
            assert!(out.stdout.is_empty(), "errors go to stderr");
            assert!(
                out.stderr.contains("error"),
                "server body must pass through"
            );
        }
    }

    #[test]
    fn specs_table_falls_back_to_passthrough_on_unexpected_shape() {
        // 数组缺 agent_id -> 不是预期结构 -> 透传原正文，退出码 0。
        let weird = r#"[{"nope":1}]"#;
        let out = cmd_agents_specs(&client_of(&Mock::new(vec![ok(weird)])), true, None);
        assert_eq!(out.code, 0);
        assert_eq!(out.stdout, weird);

        let not_json = "not json at all";
        let out = cmd_agents_specs(&client_of(&Mock::new(vec![ok(not_json)])), true, None);
        assert_eq!(out.code, 0);
        assert_eq!(out.stdout, not_json);
    }

    #[test]
    fn specs_table_renders_real_fixture() {
        let rows = spec::agent_specs_rows(REAL_SPECS_LIST).expect("parsed");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].items, "3", "desired.spec.items 条数");
        assert_eq!(rows[1].items, "", "desired=null 时 items 列为空，不是 0");
        let text = spec::render_specs_table(&rows);
        assert!(text.contains("agent-1") && text.contains("agent-2"));
        assert!(text.contains("session_state"));
    }

    #[test]
    fn spec_get_table_survives_null_applied_and_diff() {
        let row = spec::agent_spec_get_row(REAL_SPEC_VIEW).expect("parsed");
        assert_eq!(row.desired_revision, "6c4338bcb6ca2a38");
        assert_eq!(row.applied_revision, "", "applied=null -> 空");
        assert_eq!(row.diff_empty, "", "diff=null -> 空，不是 empty");
        assert_eq!(row.item_count, "3");
        let text = spec::render_spec_get_table(&row);
        assert!(text.contains("desired.revision"));
    }

    // ---- T3: status 判定（需求 R5）----

    #[test]
    fn kind_data_types_covers_all_eight_kinds() {
        let kinds = [
            "metrics_host",
            "log_file",
            "log_k8s_stdout",
            "apm_otlp",
            "ebpf_network",
            "ebpf_tcp",
            "ebpf_process",
            "ebpf_syscall",
        ];
        for k in kinds {
            assert!(
                status::KIND_DATA_TYPES.iter().any(|(x, _)| *x == k),
                "kind {k} 缺映射"
            );
        }
        let f = |k: &str| {
            status::KIND_DATA_TYPES
                .iter()
                .find(|(x, _)| *x == k)
                .map(|(_, t)| t.to_vec())
                .unwrap_or_default()
        };
        assert_eq!(f("metrics_host"), vec!["metrics"]);
        assert_eq!(f("log_file"), vec!["logs"]);
        assert_eq!(f("log_k8s_stdout"), vec!["logs"]);
        assert_eq!(f("apm_otlp"), vec!["traces"]);
        assert_eq!(f("ebpf_network"), vec!["ebpf_edges"]);
        assert_eq!(f("ebpf_tcp"), vec!["ebpf_edges"]);
        assert_eq!(f("ebpf_process"), vec!["metrics"]);
        assert_eq!(f("ebpf_syscall"), vec!["metrics"]);
    }

    #[test]
    fn classify_boundaries() {
        let now = 10_000_000_000_i64; // 10000s
        let interval = 15_u64; // threshold = max(45,60) = 60s
        assert_eq!(status::threshold_secs(interval), 60);
        assert_eq!(status::threshold_secs(1000), 3000);

        // 无 stream
        assert_eq!(
            status::classify(None, interval, now),
            status::Verdict::NotReporting
        );
        // 恰好等于阈值
        assert_eq!(
            status::classify(Some(now - 60_000_000), interval, now),
            status::Verdict::Reporting
        );
        // 阈值 +1µs
        assert_eq!(
            status::classify(Some(now - 60_000_001), interval, now),
            status::Verdict::Stale
        );
        // 阈值 -1µs
        assert_eq!(
            status::classify(Some(now - 59_999_999), interval, now),
            status::Verdict::Reporting
        );
        // 未来时间（时钟不同步）-> 最新鲜
        assert_eq!(
            status::classify(Some(now + 5_000_000), interval, now),
            status::Verdict::Reporting
        );
        // 长间隔采集项阈值按 3x
        assert_eq!(
            status::classify(Some(now - 200_000_000), 100, now),
            status::Verdict::Reporting
        );
        assert_eq!(
            status::classify(Some(now - 301_000_000), 100, now),
            status::Verdict::Stale
        );
    }

    #[test]
    fn interval_secs_normalises_bad_values_to_default() {
        use serde_json::json;
        assert_eq!(status::interval_secs_of(None), 15);
        assert_eq!(status::interval_secs_of(Some(&json!({}))), 15);
        assert_eq!(
            status::interval_secs_of(Some(&json!({"interval_secs": 0}))),
            15
        );
        assert_eq!(
            status::interval_secs_of(Some(&json!({"interval_secs": -1}))),
            15
        );
        assert_eq!(
            status::interval_secs_of(Some(&json!({"interval_secs": "60"}))),
            60
        );
        assert_eq!(
            status::interval_secs_of(Some(&json!({"interval_secs": 30}))),
            30
        );
    }

    #[test]
    fn builds_one_row_per_data_type_and_skips_disabled_from_verdict() {
        let view: serde_json::Value = serde_json::from_str(REAL_SPEC_VIEW).expect("json");
        let items = status::expand_items(&view);
        assert_eq!(items.len(), 3);
        let now = 1_000_000_000_000_i64;
        let streams = vec![status::StreamRow {
            agent_id: "agent-1".into(),
            data_type: "metrics".into(),
            data_id: "i-metrics".into(),
            last_seen_micros: now - 1_000_000,
            accepted: 7,
        }];
        let rows = status::build_rows(&items, &streams, "agent-1", now, true);
        assert_eq!(rows.len(), 3, "三个采集项各一行");
        let metrics = rows.iter().find(|r| r.item_id == "i-metrics").expect("row");
        assert_eq!(metrics.verdict, status::Verdict::Reporting);
        assert_eq!(metrics.accepted, 7);
        let disabled = rows.iter().find(|r| r.item_id == "i-logfile").expect("row");
        assert_eq!(disabled.verdict, status::Verdict::Disabled);
        assert!(!disabled.verdict.counts_toward_verdict());
        let never = rows
            .iter()
            .find(|r| r.item_id == "i-ebpf-proc")
            .expect("row");
        assert_eq!(never.verdict, status::Verdict::NotReporting);
    }

    #[test]
    fn unreachable_streams_is_unknown_not_not_reporting() {
        let view: serde_json::Value = serde_json::from_str(REAL_SPEC_VIEW).expect("json");
        let items = status::expand_items(&view);
        let rows = status::build_rows(&items, &[], "agent-1", 1_000_000, false);
        for r in rows
            .iter()
            .filter(|r| r.verdict != status::Verdict::Disabled)
        {
            assert_eq!(
                r.verdict,
                status::Verdict::Unknown,
                "查不到 != 没在采：数据面不可达必须是 unknown"
            );
        }
    }

    #[test]
    fn unknown_kind_is_reported_but_does_not_fail() {
        let view = serde_json::json!({
            "desired": {"spec": {"items": [
                {"item_id": "i-new", "kind": "brand_new_kind", "enabled": true, "collector": {}}
            ]}}
        });
        let items = status::expand_items(&view);
        let rows = status::build_rows(&items, &[], "a", 0, true);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].verdict, status::Verdict::UnknownKind);
        assert!(
            !rows[0].verdict.counts_toward_verdict(),
            "新 kind 不该让旧 CLI 失败"
        );
    }

    #[test]
    fn status_is_dirty_when_spec_not_synced() {
        let items = Vec::new();
        let r = status::render_status("a-1", "unknown", &items, 0, None);
        assert_eq!(r.code, 1);
        assert!(r.stdout.contains("dirty"), "{}", r.stdout);
        assert!(r.stdout.contains("not applied"));

        let r = status::render_status("a-1", "synced", &items, 0, None);
        assert_eq!(r.code, 0, "synced + 无 enabled 项 = 通");
        assert!(r.stdout.contains("summary: ok"));
    }

    #[test]
    fn status_renders_both_item_id_and_data_type() {
        // ebpf_process 与 metrics_host 都是 metrics：只打 data_type 两行看起来一样。
        let rows = vec![
            status::StatusRow {
                item_id: "i-metrics".into(),
                data_type: "metrics".into(),
                last_seen_micros: Some(0),
                accepted: 1,
                interval_secs: 15,
                verdict: status::Verdict::Reporting,
            },
            status::StatusRow {
                item_id: "i-ebpf-proc".into(),
                data_type: "metrics".into(),
                last_seen_micros: Some(0),
                accepted: 1,
                interval_secs: 30,
                verdict: status::Verdict::Reporting,
            },
        ];
        let r = status::render_status("a", "synced", &rows, 0, None);
        assert_eq!(r.code, 0);
        assert!(r.stdout.contains("i-metrics") && r.stdout.contains("i-ebpf-proc"));
    }

    #[test]
    fn relative_age_formats() {
        assert_eq!(status::relative_age(0, 5_000_000), "5s");
        assert_eq!(status::relative_age(0, 65_000_000), "1m5s");
        assert_eq!(status::relative_age(0, 3_725_000_000), "1h2m");
        assert_eq!(status::relative_age(0, 90_000_000_000), "1d1h");
        assert_eq!(status::relative_age(9_999_999, 0), "0s", "未来时间归 0");
    }

    // ---- T4: doctor（需求 R6）----

    #[test]
    fn doctor_short_circuits_when_agent_missing() {
        // 段 1 的两个端点（单台 + 列表）并发发出；单台 404 -> 立即结束。
        // 单台端点 404；其余路由都配好，用来证明「一个都没被请求」。
        let mock = Mock::routed(vec![
            (
                "/api/gse/agents/ghost",
                Ok((404, r#"{"code":"not_found"}"#.to_string())),
            ),
            ("/api/gse/agents", ok(REAL_AGENTS_LIST)),
            ("/agents/ghost/spec", ok(REAL_SPEC_VIEW)),
            ("/v1/streams", ok(r#"{"streams":[]}"#)),
            ("/v1/ebpf/capability", ok(r#"{"agents":[],"reported":0}"#)),
            ("/health", ok(r#"{"status":"ok"}"#)),
        ]);
        let out = doctor::run(&client_of(&mock), &data_of(&mock), "ghost");
        assert_eq!(out.code, 1);
        assert_eq!(
            mock.call_count(),
            2,
            "Agent 404 后不得再请求 spec / streams / capability / health"
        );
        assert!(out.stderr.contains("cannot read agent ghost"));
    }

    #[test]
    fn doctor_never_writes() {
        // 全绿路径也不得出现 POST / PUT。
        let mock = Mock::new(vec![
            ok(REAL_AGENT_ONE),                  // get_agent
            ok(REAL_AGENTS_LIST),                // list_agents
            ok(REAL_SPEC_VIEW),                  // spec
            ok(r#"{"streams":[]}"#),             // streams
            ok(r#"{"agents":[],"reported":0}"#), // capability
            ok(r#"{"status":"ok"}"#),            // health
        ]);
        let out = doctor::run(&client_of(&mock), &data_of(&mock), "agent-1");
        for (method, url, _) in mock.calls() {
            assert_eq!(method, "GET", "doctor 是只读诊断，{} {}", method, url);
        }
        let _ = out;
    }

    #[test]
    fn doctor_unknown_sections_do_not_abort() {
        let mock = Mock::routed(vec![
            ("/api/gse/agents/agent-1", ok(REAL_AGENT_ONE)),
            ("/api/gse/agents", ok(REAL_AGENTS_LIST)),
            ("/spec", ok(REAL_SPEC_VIEW)),
            ("/v1/streams", Err("connection refused".to_string())),
            ("/v1/ebpf/capability", Err("connection refused".to_string())),
            ("/health", Err("connection refused".to_string())),
        ]);
        let out = doctor::run(&client_of(&mock), &data_of(&mock), "agent-1");
        assert_eq!(out.code, 1);
        assert!(out.stdout.contains("unreachable"), "{}", out.stdout);
        assert!(out.stdout.contains("summary:"));
        // 其余段仍要完整输出
        assert!(out.stdout.contains("agent ledger"));
        assert!(out.stdout.contains("spec"));
    }

    #[test]
    fn doctor_does_not_leak_plaintext_token() {
        // 实测：GET /api/gse/agents* 返回**明文 token**，doctor 不得透传。
        let mock = Mock::routed(vec![
            ("/api/gse/agents/agent-1", ok(REAL_AGENT_ONE)),
            ("/api/gse/agents", ok(REAL_AGENTS_LIST)),
            ("/spec", ok(REAL_SPEC_VIEW)),
            ("/v1/streams", ok(r#"{"streams":[]}"#)),
            ("/v1/ebpf/capability", ok(r#"{"agents":[],"reported":0}"#)),
            ("/health", ok(r#"{"status":"ok"}"#)),
        ]);
        let out = doctor::run(&client_of(&mock), &data_of(&mock), "agent-1");
        assert!(
            !out.stdout.contains("tok-1-rotated") && !out.stderr.contains("tok-1-rotated"),
            "明文 token 泄漏到输出：{}",
            out.stdout
        );
        assert!(!out.stdout.contains("\"token\""));
    }

    #[test]
    fn doctor_takes_session_state_from_list_not_single_agent() {
        // 单台端点没有 session_state；只用它会把「在线」误判成「不在线」。
        let online_list = REAL_AGENTS_LIST
            .replace("\"absent\"", "\"online\"")
            .replace(
                "\"job_channel_available\":false",
                "\"job_channel_available\":true",
            );
        let mock = Mock::routed(vec![
            ("/api/gse/agents/agent-1", ok(REAL_AGENT_ONE)),
            ("/api/gse/agents", ok(&online_list)),
            ("/spec", ok(REAL_SPEC_VIEW)),
            ("/v1/streams", ok(r#"{"streams":[]}"#)),
            ("/v1/ebpf/capability", ok(r#"{"agents":[],"reported":0}"#)),
            ("/health", ok(r#"{"status":"ok"}"#)),
        ]);
        let out = doctor::run(&client_of(&mock), &data_of(&mock), "agent-1");
        assert!(
            out.stdout.contains("session_state: online"),
            "{}",
            out.stdout
        );
        assert!(out.stdout.contains("job_channel_available: true"));
    }
}
