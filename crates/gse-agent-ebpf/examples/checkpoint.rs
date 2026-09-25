//! 检查点验证：在**特权环境**里真正加载、挂载、差分成边记录，并可上报到 dataserver。
//!
//! 设计里的「检查点」要求「在 5.8+ 且有 BTF 的 Linux 上加载全部 P1 程序，跑一段受控流量」。
//! 单元测试跑不到这一步（普通用户无法 attach），所以把验证做成一个可以照抄执行的命令：
//!
//! ```bash
//! # 1. 产出对象文件（本机或从 CI artifact 取）
//! scripts/build-ebpf.sh
//!
//! # 2. 在目标机用 root 跑（会真的 attach，然后按周期差分）
//! sudo -E cargo run -p gse-agent-ebpf --example checkpoint -- \
//!     --kind ebpf_network --seconds 15
//!
//! # 3. 另一个终端制造受控流量（本机自测要带 --include-loopback，回环默认丢弃）
//! curl -s http://127.0.0.1:8080/ >/dev/null
//!
//! # 4. 可选：跑通闭环（内核 → 用户态 → dataserver → 查询）
//! sudo -E cargo run -p gse-agent-ebpf --example checkpoint -- \
//!     --kind ebpf_network --seconds 10 --ingest-url http://127.0.0.1:8081
//! curl -s -X POST http://127.0.0.1:8081/v1/edges/search \
//!     -H 'content-type: application/json' -d '{"source":"ebpf","limit":3}'
//! ```
//!
//! 退出码：0 通过；1 参数错；2 前置校验/参数解析失败；3 加载挂载失败；4 读快照失败或
//! `ebpf_network` 全程没采到连接；5 上报失败。
//!
//! 记录的构造路径与 `run_loop`/`run_process_loop` **完全一致**（同一批函数），
//! 所以这里跑通就等于采集项的采集段跑通；差别只有「没有走 GSE 下发采集项」这一层。

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gse_agent_ebpf::attach::EbpfItemKind;
use gse_agent_ebpf::cfg::CfgValues;
use gse_agent_ebpf::cgroup::{describe, ProcessResolver};
use gse_agent_ebpf::config::EbpfConfig;
use gse_agent_ebpf::loader::{object_bytes, LoadedItem};
use gse_agent_ebpf::preflight::PreflightEnv;
use gse_agent_ebpf::{btf::Btf, MapSource};
use gse_agent_ebpf::{
    bucket_start, diff, edge_record, is_empty, raw_event_record, sum_process, ConnAgg,
    ProcessContext,
};

/// 单批上报的记录数：接入端有请求体上限（默认 2 MiB），几千条边一次发会被拒。
const INGEST_BATCH: usize = 500;

/// 边记录的桶宽（秒），与采集项默认值一致。
const BUCKET_SECS: i64 = 10;

/// 命令行参数（手写解析：检查点工具不值得引 clap）。
struct Args {
    object: Option<String>,
    kind: EbpfItemKind,
    seconds: u64,
    interval_millis: u64,
    include_loopback: bool,
    raw_events: bool,
    ingest_url: Option<String>,
    agent_id: String,
    item_id: String,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        object: None,
        kind: EbpfItemKind::Network,
        seconds: 15,
        interval_millis: 2_000,
        include_loopback: true,
        raw_events: false,
        ingest_url: None,
        agent_id: "agent-checkpoint".to_string(),
        item_id: "item-ebpf".to_string(),
    };
    let mut rest = std::env::args().skip(1);
    while let Some(arg) = rest.next() {
        let mut next = |name: &str| rest.next().ok_or_else(|| format!("{name} 需要取值"));
        match arg.as_str() {
            "--object" => args.object = Some(next("--object")?),
            "--kind" => {
                let value = next("--kind")?;
                args.kind = EbpfItemKind::parse(&value)
                    .ok_or_else(|| format!("未知采集项类型：{value}"))?;
            }
            "--seconds" => {
                args.seconds = next("--seconds")?
                    .parse()
                    .map_err(|e| format!("--seconds 解析失败：{e}"))?;
            }
            "--interval-millis" => {
                args.interval_millis = next("--interval-millis")?
                    .parse()
                    .map_err(|e| format!("--interval-millis 解析失败：{e}"))?;
            }
            "--ingest-url" => args.ingest_url = Some(next("--ingest-url")?),
            "--agent-id" => args.agent_id = next("--agent-id")?,
            "--item-id" => args.item_id = next("--item-id")?,
            "--include-loopback" => args.include_loopback = true,
            "--no-loopback" => args.include_loopback = false,
            "--raw-events" => args.raw_events = true,
            "--help" | "-h" => {
                println!(
                    "用法：checkpoint [--object <path>] \
                     [--kind ebpf_network|ebpf_tcp|ebpf_process] [--seconds N] \
                     [--interval-millis N] [--include-loopback|--no-loopback] [--raw-events]\n\
                     上报（跑通闭环）：[--ingest-url http://host:port] [--agent-id id] [--item-id id]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("未知参数：{other}")),
        }
    }
    Ok(args)
}

/// 当前 Unix 微秒。
fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// 运行期参数：BTF + tracepoint `format`，与 `gse-agent` 走同一套解析。
fn cfg_values(config: &EbpfConfig) -> Result<CfgValues, String> {
    let btf = Btf::parse(
        &std::fs::read("/sys/kernel/btf/vmlinux")
            .map_err(|e| format!("读取 /sys/kernel/btf/vmlinux 失败：{e}"))?,
    )?;
    let format_text =
        std::fs::read_to_string("/sys/kernel/tracing/events/sock/inet_sock_set_state/format")
            .or_else(|_| {
                std::fs::read_to_string(
                    "/sys/kernel/debug/tracing/events/sock/inet_sock_set_state/format",
                )
            })
            .ok();
    if format_text.is_none() {
        eprintln!(
            "提示：tracepoint format 读不到，使用文档化兜底布局（生产环境应修 tracing 权限）"
        );
    }
    CfgValues::build(&btf, format_text.as_deref(), config)
}

/// 加载对象：优先显式路径，否则用 `build.rs` 内嵌的字节。
fn load_object(args: &Args) -> Result<Vec<u8>, String> {
    let path = args
        .object
        .clone()
        .unwrap_or_else(|| format!("packaging/ebpf/{}.o", args.kind.object_name()));
    match std::fs::read(&path) {
        Ok(bytes) if !bytes.is_empty() => {
            println!("加载对象文件：{path}（{} 字节）", bytes.len());
            Ok(bytes)
        }
        _ => {
            let embedded = object_bytes(args.kind).to_vec();
            if embedded.is_empty() {
                return Err(format!(
                    "对象文件不存在或为空：{path}；先运行 scripts/build-ebpf.sh（需 bpf-linker）"
                ));
            }
            println!("使用内嵌对象文件（{} 字节）", embedded.len());
            Ok(embedded)
        }
    }
}

/// 极简 HTTP POST（不为检查点工具引 HTTP 依赖：只发 JSON、读响应）。
fn post_json(url: &str, body: &str) -> Result<String, String> {
    use std::io::{Read, Write};
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("只支持 http://：{url}"))?;
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    let mut stream = std::net::TcpStream::connect(authority)
        .map_err(|e| format!("连接 {authority} 失败：{e}"))?;
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("写请求失败：{e}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| format!("读响应失败：{e}"))?;
    Ok(response
        .split_once("\r\n\r\n")
        .map_or(response.clone(), |(_, body)| body.to_string()))
}

/// 进程计数聚合键：`(pid, cgroup_id, comm)`。
type ProcSeenKey = (u32, u64, [u8; ebpf_abi::TASK_COMM_LEN]);

/// 采集统计（打印与判定用）。
#[derive(Default)]
struct Totals {
    reads: u64,
    connections: u64,
    retrans: u64,
    resets: u64,
    process_events: u64,
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(reason) => {
            eprintln!("参数错误：{reason}");
            return ExitCode::from(1);
        }
    };

    let report = PreflightEnv::detect().check();
    println!(
        "前置校验：kernel={} ({}) btf={} capability={}",
        report.kernel_release, report.kernel_ok, report.btf_ok, report.capability_ok
    );
    if !report.ok() {
        eprintln!(
            "前置校验失败：{}；建议：{}",
            report.reason.unwrap_or_default(),
            report.advice.unwrap_or_default()
        );
        return ExitCode::from(2);
    }

    let config = EbpfConfig {
        include_loopback: args.include_loopback,
        flush_interval_secs: 2,
        raw_events_enabled: args.raw_events,
        ..EbpfConfig::default()
    };
    let cfg = match cfg_values(&config) {
        Ok(cfg) => cfg,
        Err(reason) => {
            eprintln!("运行期参数解析失败：{reason}");
            return ExitCode::from(2);
        }
    };
    println!(
        "CFG：sock 偏移 daddr={} rcv_saddr={} dport={} num={} family={} protocol={}",
        cfg.get(ebpf_abi::CfgIndex::SockDaddr),
        cfg.get(ebpf_abi::CfgIndex::SockRcvSaddr),
        cfg.get(ebpf_abi::CfgIndex::SockDport),
        cfg.get(ebpf_abi::CfgIndex::SockNum),
        cfg.get(ebpf_abi::CfgIndex::SockFamily),
        cfg.get(ebpf_abi::CfgIndex::SockProtocol),
    );

    let object = match load_object(&args) {
        Ok(object) => object,
        Err(reason) => {
            eprintln!("{reason}");
            return ExitCode::from(3);
        }
    };
    // 检查点工具不带 k8s 凭据，Pod 名反查不启用（退回 uid）。
    let loaded = match LoadedItem::load(args.kind, &object, &config, &cfg, None) {
        Ok(loaded) => loaded,
        Err(reason) => {
            eprintln!("加载/挂载失败：{reason}");
            return ExitCode::from(3);
        }
    };
    println!("已挂载程序：{:?}", loaded.attached());
    let mut source = match loaded.into_map_source() {
        Ok(source) => source,
        Err(reason) => {
            eprintln!("聚合 map 不可用：{reason}");
            return ExitCode::from(3);
        }
    };
    println!(
        "开始采集 {} 秒（每 {} ms 一次快照）；请在此期间制造受控流量",
        args.seconds, args.interval_millis
    );

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("创建运行时失败：{e}");
            return ExitCode::from(3);
        }
    };

    // 容器/Pod 反查（需求 11.1）：按 pid 读 `/proc/<pid>/cgroup`。
    let mut resolver = ProcessResolver::new(60, 4096);
    let mut totals = Totals::default();
    let mut pending_edges: Vec<serde_json::Value> = Vec::new();
    let mut pending_metrics: Vec<serde_json::Value> = Vec::new();
    // 差分基准：**只上报本周期真实增量**（与 `run_loop` 相同语义）。
    let mut previous: BTreeMap<ebpf_abi::ConnKey, ConnAgg> = BTreeMap::new();
    let mut process_totals: BTreeMap<ProcSeenKey, (u64, u64, u64)> = BTreeMap::new();

    let deadline = std::time::Instant::now() + Duration::from_secs(args.seconds.max(1));
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(args.interval_millis.max(100)));
        // 进程项的计数在 `PROC_AGG` 上，必须走 `drain_process`；用错接口会「看到 0」而误判。
        if args.kind == EbpfItemKind::Process {
            match runtime.block_on(async { source.drain_process() }) {
                Ok(Some(rows)) => {
                    totals.reads += 1;
                    for (key, per_cpu) in rows {
                        let counts = sum_process(&per_cpu);
                        if counts == (0, 0, 0) {
                            continue;
                        }
                        totals.process_events += counts.0 + counts.1 + counts.2;
                        let entry = process_totals
                            .entry((key.pid, key.cgroup_id, key.comm))
                            .or_insert((0, 0, 0));
                        entry.0 += counts.0;
                        entry.1 += counts.1;
                        entry.2 += counts.2;
                        let context = resolver.resolve(key.pid);
                        println!(
                            "进程 pid={} cgroup={} comm={:?} exec={} exit={} fork={} 上下文={}",
                            key.pid,
                            key.cgroup_id,
                            String::from_utf8_lossy(&key.comm).trim_end_matches('\0'),
                            counts.0,
                            counts.1,
                            counts.2,
                            context
                                .as_ref()
                                .map(describe)
                                .unwrap_or_else(|| "未反查到".to_string()),
                        );
                    }
                }
                Ok(None) => {}
                Err(reason) => {
                    eprintln!("读取进程快照失败：{reason}");
                    return ExitCode::from(4);
                }
            }
            continue;
        }

        match runtime.block_on(async { source.drain() }) {
            Ok(rows) => {
                totals.reads += 1;
                let bucket = bucket_start(now_micros(), BUCKET_SECS);
                for (key, per_cpu) in rows {
                    let view = gse_agent_ebpf::view_per_cpu(&per_cpu);
                    let delta = diff(previous.get(&key), &view);
                    previous.insert(key, view);
                    if is_empty(&delta) {
                        continue;
                    }
                    totals.connections += delta.connections;
                    totals.retrans += delta.tcp_retrans;
                    totals.resets += delta.tcp_resets;
                    let context = resolver.resolve(key.pid).map(|info| {
                        let pod_name = info.pod_label();
                        ProcessContext {
                            process_name: info.process_name,
                            container_id: info.container_id,
                            pod_name,
                        }
                    });
                    // 与 `run_loop` 完全相同的记录构造路径（服务名留空，由 dataserver 反查）。
                    if let Some(record) = edge_record(
                        &args.agent_id,
                        &key,
                        &delta,
                        bucket,
                        BUCKET_SECS,
                        context.as_ref(),
                    ) {
                        if let Ok(value) = serde_json::to_value(&record) {
                            pending_edges.push(value);
                        }
                    }
                    println!(
                        "键 pid={} cgroup={} {}:{} -> {}:{} proto={} 连接={} 失败={} 发={} 收={} 重传={} 上下文={}",
                        key.pid,
                        key.cgroup_id,
                        gse_agent_ebpf::aggregate::ipv4_of(key.saddr),
                        key.sport,
                        gse_agent_ebpf::aggregate::ipv4_of(key.daddr),
                        key.dport,
                        key.protocol,
                        delta.connections,
                        delta.failures,
                        delta.bytes_sent,
                        delta.bytes_recv,
                        delta.tcp_retrans,
                        context
                            .as_ref()
                            .map(|c| describe(&gse_agent_ebpf::cgroup::ContainerInfo {
                                container_id: c.container_id.clone(),
                                pod_uid: c.pod_name.clone(),
                                pod_name: String::new(),
                                process_name: c.process_name.clone(),
                            }))
                            .unwrap_or_else(|| "未反查到".to_string()),
                    );
                }
                // 限流丢弃计数（需求 12.3 的可观测项）。
                let limited = source.take_rate_limit_drops();
                if limited > 0 {
                    println!("本周期被限流丢弃 {limited} 个事件");
                }
            }
            Err(reason) => {
                eprintln!("读取快照失败：{reason}");
                return ExitCode::from(4);
            }
        }
    }

    println!(
        "读快照 {} 次：新建连接 {}，重传 {}，RST {}，进程事件 {}；反查缓存 {} 个 pid",
        totals.reads,
        totals.connections,
        totals.retrans,
        totals.resets,
        totals.process_events,
        resolver.cached()
    );

    // 原始事件（`data_type=ebpf`）：只在开启时内核态才写，这里一次读走。
    let mut raw_records: Vec<serde_json::Value> = Vec::new();
    if args.raw_events {
        // 单调时钟 → 墙上时钟（与采集循环同一套换算）。
        let offset = gse_agent_ebpf::monotonic_to_unix_offset_micros().unwrap_or_else(now_micros);
        for event in source.drain_raw_events() {
            raw_records.push(raw_event_record(&args.agent_id, &event, offset));
        }
        println!("读到原始事件 {} 条", raw_records.len());
    }

    // 能力状态 + 进程指标：与 `run_item`/`run_process_loop` 同样的记录形状。
    pending_metrics.push(gse_agent_ebpf::capability_metric(
        &args.agent_id,
        &report,
        &args.item_id,
    ));
    if args.kind == EbpfItemKind::Process {
        let bucket = bucket_start(now_micros(), 60);
        let mut emitted = 0usize;
        for ((pid, cgroup_id, comm), (exec, exit, fork)) in &process_totals {
            let comm = String::from_utf8_lossy(comm)
                .trim_end_matches('\0')
                .to_string();
            let context = resolver.resolve(*pid);
            for (measurement, value) in [
                ("ebpf_process_exec_total", *exec),
                ("ebpf_process_exit_total", *exit),
                ("ebpf_process_fork_total", *fork),
            ] {
                if value == 0 {
                    continue;
                }
                emitted += 1;
                pending_metrics.push(serde_json::json!({
                    "record_id": format!("{}-{pid}-{bucket}-{measurement}", args.agent_id),
                    "timestamp": bucket,
                    "measurement": measurement,
                    "tags": {
                        "agent_id": args.agent_id,
                        "pid": pid.to_string(),
                        "cgroup_id": cgroup_id.to_string(),
                        "process_name": comm.clone(),
                        "container_id": context.as_ref().map(|c| c.container_id.clone()).unwrap_or_default(),
                        "pod_uid": context.as_ref().map(|c| c.pod_uid.clone()).unwrap_or_default(),
                    },
                    "field_name": "value",
                    "field_value": value as f64,
                }));
            }
        }
        println!("构造进程指标 {emitted} 条");
    }

    // 判定标准按采集项区分：重传/RST 本来就稀少、进程事件在空闲机器上也可能为 0，
    // 因此只有 `ebpf_network` 把「一条连接都没采到」当作失败（它最容易踩过滤与偏移问题）。
    if args.kind == EbpfItemKind::Network && totals.connections == 0 {
        eprintln!(
            "警告：全程没有采到连接。检查是否真的产生了流量、是否被回环/端口过滤掉\
             （自测时加 --include-loopback），以及 tracepoint 字段偏移是否与内核匹配。"
        );
        return ExitCode::from(4);
    }

    // 可选：按 Agent 的信封格式上报，跑通「内核 → 用户态 → dataserver → 查询」闭环。
    if let Some(url) = &args.ingest_url {
        let ingest = |name: &str,
                      data_type: &str,
                      records: Vec<serde_json::Value>|
         -> Result<String, String> {
            let envelope = serde_json::json!({
                "batch_id": format!("{}-checkpoint-{name}", args.agent_id),
                "data_type": data_type,
                "data_id": args.item_id,
                "agent_id": args.agent_id,
                "host_id": "checkpoint-host",
                "sent_at_micros": now_micros(),
                "records": records,
            });
            post_json(
                &format!("{}/v1/ingest", url.trim_end_matches('/')),
                &envelope.to_string(),
            )
        };

        if pending_edges.is_empty() {
            println!("没有边记录可上报（{} 采集项不产生边）", args.kind.as_str());
        } else {
            let total = pending_edges.len();
            let mut sent = 0usize;
            for (index, chunk) in pending_edges.chunks(INGEST_BATCH).enumerate() {
                match ingest(&format!("edges-{index}"), "ebpf_edges", chunk.to_vec()) {
                    Ok(body) => {
                        sent += chunk.len();
                        println!("边记录批次 {index}：{} 条 → {body}", chunk.len());
                    }
                    Err(reason) => {
                        eprintln!("边记录批次 {index} 上报失败：{reason}");
                        return ExitCode::from(5);
                    }
                }
            }
            println!("已上报边记录 {sent}/{total} 条");
        }

        if !pending_metrics.is_empty() {
            match ingest("metrics", "metrics", pending_metrics.clone()) {
                Ok(body) => println!("指标上报 {} 条 → {body}", pending_metrics.len()),
                Err(reason) => eprintln!("指标上报失败：{reason}"),
            }
        }
        if !raw_records.is_empty() {
            match ingest("events", "ebpf", raw_records.clone()) {
                Ok(body) => println!("原始事件上报 {} 条 → {body}", raw_records.len()),
                Err(reason) => eprintln!("原始事件上报失败：{reason}"),
            }
        }
    }

    println!("检查点通过：加载、挂载、差分读取都正常");
    ExitCode::SUCCESS
}
