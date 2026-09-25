//! 检查点验证：在**特权环境**里真正加载、挂载并读一次 per-CPU 快照。
//!
//! 设计里的「检查点」阶段要求「在 5.8+ 且有 BTF 的 Linux 上加载全部 P1 程序，跑一段受控流量」。
//! 单元测试跑不到这一步（普通用户无法 attach），所以把验证做成一个可以照抄执行的命令：
//!
//! ```bash
//! # 1. 先在本机（或 CI）产出对象文件（需要 bpf-linker + LLVM 21+）
//! scripts/build-ebpf.sh
//!
//! # 2. 用 root（或带 CAP_BPF+CAP_PERFMON）在目标机跑检查点
//! sudo -E cargo run -p gse-agent-ebpf --example checkpoint -- \
//!     --object packaging/ebpf/network.o --kind ebpf_network --seconds 15
//!
//! # 3. 同时制造一点流量（另一个终端）
//! curl -s http://127.0.0.1:8080/ >/dev/null     # 需要一个在监听的本机服务
//! ```
//!
//! 退出码：0 表示前置校验通过且成功读取过快照；2 前置校验失败；3 加载/挂载失败；4 读快照失败。
//! `--include-loopback` 用来让本机自测的流量也能被计入（设计里回环默认丢弃）。

#![cfg_attr(not(unix), allow(dead_code))]

use std::process::ExitCode;
use std::time::Duration;

use gse_agent_ebpf::attach::EbpfItemKind;
use gse_agent_ebpf::cfg::CfgValues;
use gse_agent_ebpf::config::EbpfConfig;
use gse_agent_ebpf::loader::{object_bytes, LoadedItem};
use gse_agent_ebpf::preflight::PreflightEnv;
use gse_agent_ebpf::{btf::Btf, MapSource};

/// 命令行参数（手写解析：检查点工具不值得引 clap）。
struct Args {
    object: Option<String>,
    kind: EbpfItemKind,
    seconds: u64,
    include_loopback: bool,
    interval_millis: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut object = None;
    let mut kind = EbpfItemKind::Network;
    let mut seconds = 15u64;
    let mut include_loopback = true;
    let mut interval_millis = 2_000u64;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--object" => {
                object = Some(args.next().ok_or("--object 需要路径")?);
            }
            "--kind" => {
                let value = args.next().ok_or("--kind 需要取值")?;
                kind = EbpfItemKind::parse(&value)
                    .ok_or_else(|| format!("未知采集项类型：{value}"))?;
            }
            "--seconds" => {
                seconds = args
                    .next()
                    .ok_or("--seconds 需要数值")?
                    .parse()
                    .map_err(|e| format!("--seconds 解析失败：{e}"))?;
            }
            "--interval-millis" => {
                interval_millis = args
                    .next()
                    .ok_or("--interval-millis 需要数值")?
                    .parse()
                    .map_err(|e| format!("--interval-millis 解析失败：{e}"))?;
            }
            "--include-loopback" => include_loopback = true,
            "--no-loopback" => include_loopback = false,
            "--help" | "-h" => {
                println!(
                    "用法：checkpoint [--object <path>] [--kind ebpf_network|ebpf_tcp|ebpf_process] \
                     [--seconds N] [--interval-millis N] [--include-loopback|--no-loopback]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("未知参数：{other}")),
        }
    }
    Ok(Args {
        object,
        kind,
        seconds,
        include_loopback,
        interval_millis,
    })
}

/// 运行期参数：BTF + tracepoint `format`，与 `gse-agent` 走同一套解析（读不到 format 时用兜底布局并提示）。
fn cfg_values(config: &EbpfConfig) -> Result<CfgValues, String> {
    let btf_bytes = std::fs::read("/sys/kernel/btf/vmlinux")
        .map_err(|e| format!("读取 /sys/kernel/btf/vmlinux 失败：{e}"))?;
    let btf = Btf::parse(&btf_bytes)?;
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

fn object_path(args: &Args) -> String {
    args.object
        .clone()
        .unwrap_or_else(|| format!("packaging/ebpf/{}.o", args.kind.object_name()))
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

    // 优先用显式路径；否则用 `build.rs` 内嵌的字节（CI/发布包里的路径）。
    let path = object_path(&args);
    let object = match std::fs::read(&path) {
        Ok(bytes) if !bytes.is_empty() => {
            println!("加载对象文件：{path}（{} 字节）", bytes.len());
            bytes
        }
        _ => {
            let embedded = object_bytes(args.kind).to_vec();
            if embedded.is_empty() {
                eprintln!(
                    "对象文件不存在或为空：{path}；先运行 scripts/build-ebpf.sh（需 bpf-linker + LLVM 21+）"
                );
                return ExitCode::from(3);
            }
            println!("使用内嵌对象文件（{} 字节）", embedded.len());
            embedded
        }
    };

    let loaded = match LoadedItem::load(args.kind, &object, &config, &cfg) {
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
        "开始采集 {} 秒（每 {} ms 读一次快照）；请在此期间制造受控流量",
        args.seconds, args.interval_millis
    );

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("创建运行时失败：{e}");
            return ExitCode::from(3);
        }
    };
    let mut connections = 0u64;
    let mut retrans = 0u64;
    let mut resets = 0u64;
    let mut process_events = 0u64;
    let mut reads = 0u64;
    let deadline = std::time::Instant::now() + Duration::from_secs(args.seconds.max(1));
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(args.interval_millis.max(100)));
        // 进程项的计数在另一张 map 上（`PROC_AGG`），走 `drain_process`；
        // 连接型（network/tcp）走 `drain`。用错接口会「看到 0」而误判成没采到。
        if args.kind == EbpfItemKind::Process {
            match runtime.block_on(async { source.drain_process() }) {
                Ok(Some(rows)) => {
                    reads += 1;
                    for (key, per_cpu) in rows {
                        let (exec, exit, fork) = gse_agent_ebpf::sum_process(&per_cpu);
                        if exec + exit + fork == 0 {
                            continue;
                        }
                        process_events += exec + exit + fork;
                        println!(
                            "进程 pid={} cgroup={} comm={:?} exec={exec} exit={exit} fork={fork}",
                            key.pid,
                            key.cgroup_id,
                            String::from_utf8_lossy(&key.comm)
                                .trim_end_matches('\0')
                                .to_string(),
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
                reads += 1;
                for (key, per_cpu) in rows {
                    let view = gse_agent_ebpf::view_per_cpu(&per_cpu);
                    connections += view.connections;
                    retrans += view.tcp_retrans;
                    resets += view.tcp_resets;
                    println!(
                        "键 pid={} cgroup={} {}:{} -> {}:{} proto={} 连接={} 失败={} 发={} 收={} 重传={}",
                        key.pid,
                        key.cgroup_id,
                        gse_agent_ebpf::aggregate::ipv4_of(key.saddr),
                        key.sport,
                        gse_agent_ebpf::aggregate::ipv4_of(key.daddr),
                        key.dport,
                        key.protocol,
                        view.connections,
                        view.failures,
                        view.bytes_sent,
                        view.bytes_recv,
                        view.tcp_retrans,
                    );
                }
            }
            Err(reason) => {
                eprintln!("读取快照失败：{reason}");
                return ExitCode::from(4);
            }
        }
    }

    println!(
        "读快照 {reads} 次：新建连接 {connections}，重传 {retrans}，RST {resets}，进程事件 {process_events}"
    );
    // 判定标准按采集项区分：重传/RST 本来就稀少，进程事件在空闲机器上也可能为 0，
    // 因此只有 `ebpf_network` 把「一条连接都没采到」当作失败（它最容易踩过滤与偏移问题）。
    match args.kind {
        EbpfItemKind::Network if connections == 0 => {
            eprintln!(
                "警告：全程没有采到连接。检查是否真的产生了流量、是否被回环/端口过滤掉\
                 （自测时加 --include-loopback），以及 tracepoint 字段偏移是否与内核匹配。"
            );
            return ExitCode::from(4);
        }
        _ => {}
    }
    println!("检查点通过：加载、挂载、差分读取都正常");
    ExitCode::SUCCESS
}
