//! `ebpf_process` 采集项的内核态程序：进程生命周期。
//!
//! | 挂载点 | 作用 |
//! | --- | --- |
//! | `tracepoint/sched/sched_process_exec` | `exec` 计数 + 可选的原始事件 |
//! | `tracepoint/sched/sched_process_exit` | `exit` 计数 |
//! | `tracepoint/sched/sched_process_fork` | `fork` 计数（父子关系在原始事件里带 `comm`） |
//!
//! 与设计的差异（已同步文档）：设计写的是从 `bprm` 读 `cmdline` 截断 512 字节。
//! 这里改用助手 `bpf_get_current_comm()` 取 16 字节进程名：**不需要 `bprm`/`task_struct`
//! 偏移**，内核态少一处版本相关依赖；`cmdline` 的完整参数留给 P2（那时再按需引入偏移）。
//!
//! 本采集项不产生 `ebpf_edges`，只产 `ebpf_process_*` 指标与（开启时的）原始事件。

#![no_std]
#![no_main]

#[path = "common.rs"]
mod common;

use aya_ebpf::{
    helpers::bpf_get_current_comm,
    macros::{map, tracepoint},
    maps::{Array, PerCpuHashMap, RingBuf},
    programs::TracePointContext,
};
use ebpf_abi::{
    CfgIndex, ProcAggWire, ProcKey, RawEvent, CFG_LEN, EVENT_KIND_PROCESS_EXEC,
    EVENT_KIND_PROCESS_EXIT, EVENT_KIND_PROCESS_FORK, TASK_COMM_LEN,
};


#[map]
static PROC_AGG: PerCpuHashMap<ProcKey, ProcAggWire> = PerCpuHashMap::with_max_entries(8192, 0);

/// 原始事件环缓冲（容量由加载期 `map_max_entries("EVENTS", cfg.ring_buffer_bytes)` 覆盖）。
#[map]
static EVENTS: RingBuf = RingBuf::with_byte_size(262144, 0);

#[map]
static CFG: Array<u64> = Array::with_max_entries(CFG_LEN, 0);

#[inline(always)]
fn bump<F: FnOnce(&mut ProcAggWire)>(key: &ProcKey, f: F) {
    if let Some(ptr) = PROC_AGG.get_ptr_mut(key) {
        unsafe { f(&mut *ptr) };
        return;
    }
    let mut value = ProcAggWire::default();
    f(&mut value);
    let _ = PROC_AGG.insert(key, &value, 0);
}

/// 进程名（16 字节，含结尾 NUL）。
#[inline(always)]
fn comm() -> [u8; TASK_COMM_LEN] {
    bpf_get_current_comm().unwrap_or([0u8; TASK_COMM_LEN])
}

#[inline(always)]
fn key() -> ProcKey {
    ProcKey {
        pid: common::current_tgid(),
        _pad: 0,
        cgroup_id: common::current_cgroup_id(),
        comm: comm(),
    }
}

/// 抽样后写原始事件；关闭或环形缓冲满时静默丢弃（原始事件是尽力而为）。
#[inline(always)]
fn emit(kind: u32, key: &ProcKey) {
    if !common::cfg_bool(&CFG, CfgIndex::RawEventsEnabled) {
        return;
    }
    let Some(mut entry) = EVENTS.reserve::<RawEvent>(0) else {
        return;
    };
    // `RingBufEntry` deref 到 `MaybeUninit<RawEvent>`，用它的 `write` 写入后 submit。
    entry.write(RawEvent {
        kind,
        pid: key.pid,
        cgroup_id: key.cgroup_id,
        comm: key.comm,
        ..RawEvent::default()
    });
    entry.submit(0);
}

#[tracepoint(name = "sched_process_exec", category = "sched")]
fn sched_process_exec(_ctx: TracePointContext) -> u32 {
    if !common::cfg_ready(&CFG) {
        return 0;
    }
    let key = key();
    bump(&key, |value| {
        value.exec = value.exec.saturating_add(1);
    });
    emit(EVENT_KIND_PROCESS_EXEC, &key);
    0
}

#[tracepoint(name = "sched_process_exit", category = "sched")]
fn sched_process_exit(_ctx: TracePointContext) -> u32 {
    if !common::cfg_ready(&CFG) {
        return 0;
    }
    let key = key();
    bump(&key, |value| {
        value.exit = value.exit.saturating_add(1);
    });
    emit(EVENT_KIND_PROCESS_EXIT, &key);
    0
}

#[tracepoint(name = "sched_process_fork", category = "sched")]
fn sched_process_fork(_ctx: TracePointContext) -> u32 {
    if !common::cfg_ready(&CFG) {
        return 0;
    }
    let key = key();
    bump(&key, |value| {
        value.fork = value.fork.saturating_add(1);
    });
    emit(EVENT_KIND_PROCESS_FORK, &key);
    0
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
