//! `ebpf_syscall` 采集项的内核态程序：文件与 syscall 延迟（需求 6）。
//!
//! | 挂载点 | 作用 |
//! | --- | --- |
//! | `tracepoint/syscalls/sys_enter_{openat,read,write,fsync}` | 记下开始时间（+ `openat` 的 `filename` 用户指针） |
//! | `tracepoint/syscalls/sys_exit_{openat,read,write,fsync}` | 算耗时入直方图槽；返回值为负时按 `errno` 计数 |
//!
//! 设计要点：
//!
//! - **偏移全部来自 `format`**：`sys_exit_*` 的 `ret` 偏移与 `sys_enter_openat` 的 `filename`
//!   偏移都由用户态解析后经 `CFG` 下发，内核态不硬编码（与 network/tcp 同一口径）。
//! - **路径只在慢调用时读**（需求 6.4）：入口只存 `filename` 指针，出口判定超过
//!   `slow_threshold_micros` 才用 `bpf_probe_read_user_str_bytes` 读进 `SLOW_IO` 事件，
//!   且截断到 [`SLOW_IO_PATH_LEN`]。路径不进聚合 map，避免「默认上行全量路径」。
//! - **慢事件单独一个 RingBuf**：路径 256 字节，塞进 `RawEvent` 会让每次进程/连接事件的
//!   拷贝都变大（那些事件比慢调用频繁得多）。
//! - `errno` 用**正数**入 `SYSCALL_ERR` 键（内核态写 `-ret`）。
//!
//! 时间换算用 `>> 10`（`ns → µs`）：内核态不能出现 64 位常量除法，否则 LLVM 会引入
//! 未定义的 `__multi3`（踩过），偏差约 2.4%，与连接延迟同一口径。

#![no_std]
#![no_main]

#[path = "common.rs"]
mod common;

use aya_ebpf::{
    helpers::{bpf_get_current_comm, bpf_ktime_get_ns, bpf_probe_read_user_str_bytes},
    macros::{map, tracepoint},
    maps::{Array, HashMap, PerCpuArray, PerCpuHashMap, RingBuf},
    programs::TracePointContext,
};
use ebpf_abi::{
    hist_slot, CfgIndex, SlowIoEvent, SyscallAggWire, SyscallErrKey, SyscallKey, CFG_LEN,
    SLOW_IO_PATH_LEN, SYSCALL_OP_FSYNC, SYSCALL_OP_OPENAT, SYSCALL_OP_READ, SYSCALL_OP_WRITE,
    TASK_COMM_LEN,
};

/// syscall 延迟聚合（per-CPU；键带 `comm`，见 `SyscallKey`）。
#[map]
static SYSCALL_AGG: PerCpuHashMap<SyscallKey, SyscallAggWire> =
    PerCpuHashMap::with_max_entries(4096, 0);

/// 错误码计数：`(op, errno, comm) → 次数`。
#[map]
static SYSCALL_ERR: PerCpuHashMap<SyscallErrKey, u64> = PerCpuHashMap::with_max_entries(4096, 0);

/// 入口暂存（进程可能被抢占、同一 tid 上 syscall 不嵌套，按 tid 存即可）。
///
/// `filename` 是 `sys_enter_openat` 的**用户指针**：出口时若判定为慢调用再读字符串，
/// 此时调用方内存仍未被回收（同一个 syscall 尚未返回）。
#[derive(Clone, Copy, Default)]
#[repr(C)]
struct Pending {
    op: u32,
    _pad: u32,
    ts_ns: u64,
    filename: u64,
    cgroup_id: u64,
    comm: [u8; TASK_COMM_LEN],
}

#[map]
static PENDING: HashMap<u64, Pending> = HashMap::with_max_entries(8192, 0);

/// 慢调用事件环缓冲（容量由加载期覆盖为 `ring_buffer_bytes`）。
#[map]
static SLOW_IO: RingBuf = RingBuf::with_byte_size(262144, 0);

#[map]
static CFG: Array<u64> = Array::with_max_entries(CFG_LEN, 0);

/// 令牌桶状态（per-CPU：令牌数、上次补充时间、被限流丢弃数）。
#[map]
static RATE: PerCpuArray<u64> = PerCpuArray::with_max_entries(common::rate_slot::LEN, 0);

/// 新键写入用的暂存区（per-CPU）：`SyscallAggWire` 有 280+ 字节，放栈上会触发 BPF 栈超限。
#[map]
static SCRATCH: PerCpuArray<SyscallAggWire> = PerCpuArray::with_max_entries(1, 0);

/// tracepoint 条目缓冲（见 `common::read_tp_buf`：不能对 ctx 做非常量偏移运算）。
#[map]
static TPBUF: PerCpuArray<[u64; common::TP_SCRATCH_WORDS]> =
    PerCpuArray::with_max_entries(1, 0);

#[inline(always)]
fn comm() -> [u8; TASK_COMM_LEN] {
    bpf_get_current_comm().unwrap_or([0u8; TASK_COMM_LEN])
}

/// 纳秒 → 微秒（`>> 10`，见模块文档）。
#[inline(always)]
fn ns_to_micros(ns: u64) -> u64 {
    ns >> 10
}

#[inline(always)]
fn bump<F: FnOnce(&mut SyscallAggWire)>(key: &SyscallKey, f: F) {
    if let Some(ptr) = SYSCALL_AGG.get_ptr_mut(key) {
        unsafe { f(&mut *ptr) };
        return;
    }
    let Some(scratch) = SCRATCH.get_ptr_mut(0) else {
        return;
    };
    unsafe {
        *scratch = SyscallAggWire::default();
        f(&mut *scratch);
        let _ = SYSCALL_AGG.insert(key, &*scratch, 0);
    }
}

/// 错误码计数（同样的新键在 per-CPU map 上直接插入 `1`；已存在则累加）。
#[inline(always)]
fn bump_err(key: &SyscallErrKey) {
    if let Some(ptr) = SYSCALL_ERR.get_ptr_mut(key) {
        unsafe { *ptr = (*ptr).saturating_add(1) };
        return;
    }
    let _ = SYSCALL_ERR.insert(key, &1u64, 0);
}

/// 入口：存下开始时间（与 `openat` 的路径指针）。
#[inline(always)]
fn enter(ctx: &TracePointContext, op: u32) {
    if !common::cfg_ready(&CFG) || !common::rate_allow(&CFG, &RATE) {
        return;
    }
    let mut filename = 0u64;
    if op == SYSCALL_OP_OPENAT {
        // `filename` 偏移来自 `sys_enter_openat` 的 `format`。槽为 0 表示用户态没拿到该字段
        // （内核版本差异）——此时**继续采集延迟**，只是慢事件里没有路径（不猜偏移）。
        if let Some(offset) = common::cfg(&CFG, CfgIndex::SysOpenatFilename) {
            if offset != 0 && unsafe { common::read_tp_buf(ctx, &TPBUF) }.is_some() {
                filename = common::scratch_u64(&TPBUF, offset).unwrap_or(0);
            }
        }
    }
    let entry = Pending {
        op,
        _pad: 0,
        ts_ns: unsafe { bpf_ktime_get_ns() },
        filename,
        cgroup_id: common::current_cgroup_id(),
        comm: comm(),
    };
    let _ = PENDING.insert(&common::current_tid(), &entry, 0);
}

/// 出口：算耗时、计错误、必要时推慢事件。
#[inline(always)]
fn exit(ctx: &TracePointContext, op: u32) {
    if !common::cfg_ready(&CFG) {
        return;
    }
    let tid = common::current_tid();
    let pending = unsafe { PENDING.get(&tid) }.copied();
    if pending.is_some() {
        let _ = PENDING.remove(&tid);
    }
    let Some(pending) = pending else {
        return;
    };
    // `ret` 是 `long`：必须按 64 位读（只读低 32 位会把大返回值当成负数）。
    if unsafe { common::read_tp_buf(ctx, &TPBUF) }.is_none() {
        return;
    }
    let Some(ret_offset) = common::cfg(&CFG, CfgIndex::SysExitRet) else {
        return;
    };
    let ret = common::scratch_u64(&TPBUF, ret_offset).unwrap_or(0) as i64;
    let now_ns = unsafe { bpf_ktime_get_ns() };
    let duration_us = ns_to_micros(now_ns.saturating_sub(pending.ts_ns));
    let errno = if ret < 0 { ret.unsigned_abs() as u32 } else { 0 };

    let key = SyscallKey {
        pid: common::current_tgid(),
        _pad: 0,
        cgroup_id: pending.cgroup_id,
        op,
        _pad2: 0,
        comm: pending.comm,
    };
    bump(&key, |value| {
        value.calls = value.calls.saturating_add(1);
        value.duration_sum_us = value.duration_sum_us.saturating_add(duration_us);
        if duration_us > value.duration_max_us {
            value.duration_max_us = duration_us;
        }
        let slot = hist_slot(duration_us);
        if slot < value.hist.len() {
            value.hist[slot] = value.hist[slot].saturating_add(1);
        }
        if errno != 0 {
            value.errors = value.errors.saturating_add(1);
        }
    });

    if errno != 0 {
        bump_err(&SyscallErrKey { errno, op });
    }

    // 慢调用：超过阈值才读路径并推事件（需求 6.4、6.5）。
    if !common::cfg_bool(&CFG, CfgIndex::RawEventsEnabled) {
        return;
    }
    let threshold = common::cfg(&CFG, CfgIndex::SlowThresholdMicros).unwrap_or(u64::MAX);
    if duration_us < threshold {
        return;
    }
    emit_slow(op, &pending, duration_us, errno, now_ns);
}

/// 慢调用事件：**就地**填 ringbuf 条目，避免 300+ 字节的结构体临时量上栈。
#[inline(always)]
fn emit_slow(op: u32, pending: &Pending, duration_us: u64, errno: u32, now_ns: u64) {
    let Some(mut entry) = SLOW_IO.reserve::<SlowIoEvent>(0) else {
        return;
    };
    let dst = entry.as_mut_ptr();
    unsafe {
        (*dst).op = op;
        (*dst).path_len = 0;
        (*dst).pid = common::current_tgid();
        (*dst).errno = errno;
        (*dst).timestamp_ns = now_ns;
        (*dst).cgroup_id = pending.cgroup_id;
        (*dst).duration_us = duration_us;
        (*dst).comm = pending.comm;
        (*dst).path = [0u8; SLOW_IO_PATH_LEN];
    }
    if pending.filename != 0 {
        let mut len = 0u32;
        unsafe {
            let path = core::slice::from_raw_parts_mut(
                (*dst).path.as_mut_ptr(),
                SLOW_IO_PATH_LEN,
            );
            if let Ok(bytes) = bpf_probe_read_user_str_bytes(pending.filename as *const u8, path) {
                len = bytes.len() as u32;
            }
        }
        unsafe { (*dst).path_len = len };
    }
    entry.submit(0);
}

#[tracepoint(name = "sys_enter_openat", category = "syscalls")]
fn sys_enter_openat(ctx: TracePointContext) -> u32 {
    enter(&ctx, SYSCALL_OP_OPENAT);
    0
}

#[tracepoint(name = "sys_exit_openat", category = "syscalls")]
fn sys_exit_openat(ctx: TracePointContext) -> u32 {
    exit(&ctx, SYSCALL_OP_OPENAT);
    0
}

#[tracepoint(name = "sys_enter_read", category = "syscalls")]
fn sys_enter_read(ctx: TracePointContext) -> u32 {
    enter(&ctx, SYSCALL_OP_READ);
    0
}

#[tracepoint(name = "sys_exit_read", category = "syscalls")]
fn sys_exit_read(ctx: TracePointContext) -> u32 {
    exit(&ctx, SYSCALL_OP_READ);
    0
}

#[tracepoint(name = "sys_enter_write", category = "syscalls")]
fn sys_enter_write(ctx: TracePointContext) -> u32 {
    enter(&ctx, SYSCALL_OP_WRITE);
    0
}

#[tracepoint(name = "sys_exit_write", category = "syscalls")]
fn sys_exit_write(ctx: TracePointContext) -> u32 {
    exit(&ctx, SYSCALL_OP_WRITE);
    0
}

#[tracepoint(name = "sys_enter_fsync", category = "syscalls")]
fn sys_enter_fsync(ctx: TracePointContext) -> u32 {
    enter(&ctx, SYSCALL_OP_FSYNC);
    0
}

#[tracepoint(name = "sys_exit_fsync", category = "syscalls")]
fn sys_exit_fsync(ctx: TracePointContext) -> u32 {
    exit(&ctx, SYSCALL_OP_FSYNC);
    0
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
