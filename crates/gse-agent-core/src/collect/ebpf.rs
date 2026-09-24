//! eBPF 采集项（`ebpf_network`/`ebpf_tcp`/`ebpf_process`）。
//!
//! 三层职责：
//!
//! 1. **前置校验**（进程级，一次）：内核 ≥5.8、BTF 可读、root 或 `CAP_BPF`+`CAP_PERFMON`。
//!    失败只降级本项能力：上报 `agent_ebpf_capability` 指标并退出，其它采集项照常。
//! 2. **加载与挂载**：读运行期参数（BTF 偏移 + tracepoint `format` + 状态常量）→ 下发 `CFG`
//!    → 按挂载计划 attach。失败按退避重试（30 秒起、×2、上限 10 分钟）。
//! 3. **采集循环**：差分 per-CPU 快照 → 边记录/指标 → 经既有信道直连 dataserver。

use std::sync::Arc;

use gse_agent_ebpf::{
    attach::EbpfItemKind,
    btf::Btf,
    cfg::CfgValues,
    config::EbpfConfig,
    loader::{object_bytes, LoadedItem},
    preflight::{PreflightEnv, PreflightReport},
    run_loop, run_process_loop, EbpfSink, EbpfStats,
};

use crate::collect::CollectShared;

/// tracepoint `format` 文件路径（`inet_sock_set_state`）。
pub const FORMAT_PATH: &str = "/sys/kernel/tracing/events/sock/inet_sock_set_state/format";
/// 老内核/老工具链的 debugfs 路径。
pub const FORMAT_PATH_DEBUGFS: &str =
    "/sys/kernel/debug/tracing/events/sock/inet_sock_set_state/format";
/// BTF 路径。
pub const BTF_PATH: &str = "/sys/kernel/btf/vmlinux";

/// 把 eBPF 采集结果交给既有采集信道（`CollectShared::push` 做组批与重试）。
struct SharedSink {
    shared: Arc<CollectShared>,
}

impl EbpfSink for SharedSink {
    fn edges(&self, item_id: &str, records: Vec<serde_json::Value>) {
        push(&self.shared, "ebpf_edges", item_id, records);
    }

    fn metrics(&self, item_id: &str, records: Vec<serde_json::Value>) {
        push(&self.shared, "metrics", item_id, records);
    }

    fn raw_events(&self, item_id: &str, records: Vec<serde_json::Value>) {
        push(&self.shared, "ebpf", item_id, records);
    }
}

/// `push` 是 async，采集循环是同步上下文，这里用阻塞提交（队列是无界的，不会卡采集）。
fn push(
    shared: &Arc<CollectShared>,
    data_type: &str,
    item_id: &str,
    records: Vec<serde_json::Value>,
) {
    if records.is_empty() {
        return;
    }
    let shared = shared.clone();
    let data_type = data_type.to_string();
    let item_id = item_id.to_string();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            shared.push(&data_type, &item_id, records).await;
        });
    } else {
        eprintln!("gse-agent: ebpf push outside runtime; {data_type} records dropped");
    }
}

/// 采集项入口。
pub async fn run_item(
    shared: Arc<CollectShared>,
    item_id: String,
    kind: EbpfItemKind,
    collector: serde_json::Value,
) {
    let config = EbpfConfig::from_value(&collector);
    let report = PreflightEnv::detect().check();
    let sink: Arc<dyn EbpfSink> = Arc::new(SharedSink {
        shared: shared.clone(),
    });
    let stats = Arc::new(EbpfStats::default());

    if !report.ok() {
        // 前置校验失败：只降级本项，Agent 其它能力不受影响。
        eprintln!(
            "gse-agent: ebpf item {item_id} disabled: {}; {}",
            report.reason.clone().unwrap_or_default(),
            report.advice.clone().unwrap_or_default()
        );
        sink.metrics(
            &item_id,
            vec![gse_agent_ebpf::capability_metric(
                &shared.agent_id,
                &report,
                &item_id,
            )],
        );
        return;
    }

    // 可用性上报一次（链路页据此区分「eBPF 不可用」与「没有数据」）。
    sink.metrics(
        &item_id,
        vec![gse_agent_ebpf::capability_metric(
            &shared.agent_id,
            &report,
            &item_id,
        )],
    );

    let mut backoff = gse_agent_ebpf::Backoff::new();
    loop {
        match load(kind, &config) {
            Ok(loaded) => {
                backoff.succeeded();
                run_loaded(
                    loaded,
                    sink.clone(),
                    config.clone(),
                    item_id.clone(),
                    shared.agent_id.clone(),
                    report.clone(),
                    stats.clone(),
                )
                .await;
                // 采集循环只在任务被 abort 时结束；正常路径不会走到这里。
                eprintln!("gse-agent: ebpf item {item_id} loop exited unexpectedly");
                return;
            }
            Err(reason) => {
                let wait = backoff.failed();
                eprintln!(
                    "gse-agent: ebpf item {item_id} load failed (attempt {}): {reason}; retry in {}s",
                    backoff.attempts(),
                    wait.as_secs()
                );
                tokio::time::sleep(wait).await;
            }
        }
    }
}

/// 加载并挂载，返回可读取 map 的采集项。
fn load(kind: EbpfItemKind, config: &EbpfConfig) -> Result<LoadedItem, String> {
    let cfg_values = resolve_cfg(config)?;
    let object = object_bytes(kind);
    LoadedItem::load(kind, object, config, &cfg_values)
}

/// 运行期参数：BTF 偏移 + tracepoint `format` 字段偏移 + TCP 状态常量。
///
/// 任何一项取不到都返回 `Err`（内核态不按猜测值跑）。
fn resolve_cfg(config: &EbpfConfig) -> Result<CfgValues, String> {
    let btf =
        Btf::parse(&std::fs::read(BTF_PATH).map_err(|e| format!("读取 {BTF_PATH} 失败：{e}"))?)?;
    let format_text = match std::fs::read_to_string(FORMAT_PATH) {
        Ok(text) => Some(text),
        Err(e) => {
            // debugfs 兜底；再不行就用文档化布局并记 warn（不是致命错误，但要说清楚）。
            match std::fs::read_to_string(FORMAT_PATH_DEBUGFS) {
                Ok(text) => Some(text),
                Err(e2) => {
                    eprintln!(
                        "gse-agent: 读取 tracepoint format 失败（{FORMAT_PATH}: {e}; {FORMAT_PATH_DEBUGFS}: {e2}），改用兜底布局"
                    );
                    None
                }
            }
        }
    };
    CfgValues::build(&btf, format_text.as_deref(), config)
}

/// 跑采集循环。
async fn run_loaded(
    loaded: LoadedItem,
    sink: Arc<dyn EbpfSink>,
    config: EbpfConfig,
    item_id: String,
    agent_id: String,
    report: PreflightReport,
    stats: Arc<EbpfStats>,
) {
    let kind = loaded.kind;
    let source = match loaded.into_map_source() {
        Ok(source) => source,
        Err(reason) => {
            eprintln!("gse-agent: ebpf item {item_id} map unavailable: {reason}");
            return;
        }
    };
    match kind {
        EbpfItemKind::Process => {
            let _ = report;
            run_process_loop(source, sink, config, item_id, agent_id, stats).await;
        }
        // network/tcp 走连接差分循环；`ebpf_tcp` 只出指标（边记录由 `emits_edges` 决定）。
        _ => {
            run_loop(
                Box::new(source),
                sink,
                config,
                item_id,
                agent_id,
                report,
                stats,
            )
            .await;
        }
    }
}
