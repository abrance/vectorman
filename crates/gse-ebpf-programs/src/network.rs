//! `ebpf_network` 采集项的内核态程序。
//!
//! 挂载点与职责：
//!
//! | 挂载点 | 作用 |
//! | --- | --- |
//! | `tracepoint/sock/inet_sock_set_state` | 连接建立（`connections`）、关闭时记存续时长直方图、SYN_SENT→CLOSE 记 `timeout` 失败 |
//! | `kprobe/kretprobe tcp_sendmsg` | 发送字节（返回值 > 0 才计） |
//! | `kprobe/kretprobe tcp_recvmsg` | 接收字节 |
//! | `kprobe/kretprobe tcp_connect` | 主动连接发起与失败（返回负 errno） |
//!
//! 与设计的差异（已同步文档）：
//!
//! - 设计写的是 `kprobe/tcp_close` 取存续时长。这里改用同一个
//!   `inet_sock_set_state` tracepoint 的两端时间差，好处是**不需要从 `struct sock` 里挖
//!   连接键**（`tcp_close` 只有 `sk` 指针），少一处偏移依赖、少一个挂载点。
//! - 设计写的是 `kretprobe/inet_csk_accept` 记被动建立。被动建立已由 tracepoint 的
//!   ESTABLISHED 迁移覆盖，不重复挂。
//! - kprobe 的参数在 kretprobe 里拿不到（返回时寄存器已变），所以用 `ENTRY` map
//!   按 tid 暂存入口拿到的连接键，返回时取出。**kretprobe 未配对时会跳过**，不猜。

#![no_std]
#![no_main]

#[path = "common.rs"]
mod common;

use aya_ebpf::{
    cty::c_void,
    helpers::bpf_ktime_get_ns,
    macros::{kprobe, kretprobe, map, tracepoint},
    maps::{Array, HashMap, PerCpuArray, PerCpuHashMap},
    programs::{ProbeContext, RetProbeContext, TracePointContext},
};
use ebpf_abi::{
    CfgIndex, ConnAggWire, ConnKey, CFG_LEN, REASON_TIMEOUT,
};

/// 连接聚合（per-CPU，用户态差分后写零复位）。
///
/// 容量是占位值：加载时由 `EbpfLoader::map_max_entries("CONN_AGG", cfg.map_max_entries)` 覆盖。
#[map]
static CONN_AGG: PerCpuHashMap<ConnKey, ConnAggWire> =
    PerCpuHashMap::with_max_entries(16384, 0);

/// 建立时刻（纳秒），用于算存续时长；关闭时移除。
#[map]
static CONN_START: HashMap<ConnKey, u64> = HashMap::with_max_entries(16384, 0);

/// kprobe 入口 → kretprobe 返回的键传递（按 tid）。
#[map]
static ENTRY: HashMap<u64, ConnKey> = HashMap::with_max_entries(16384, 0);

/// 运行期参数（结构体偏移、tracepoint 字段偏移、状态常量），由用户态下发。
#[map]
static CFG: Array<u64> = Array::with_max_entries(CFG_LEN, 0);

/// 令牌桶状态（per-CPU：令牌数、上次补充时间、被限流丢弃数）。
#[map]
static RATE: PerCpuArray<u64> = PerCpuArray::with_max_entries(common::rate_slot::LEN, 0);

/// tracepoint 条目缓冲（per-CPU，64 字节）：栈上放不下（见 `common::read_tp_buf` 的说明）。
#[map]
static TPBUF: PerCpuArray<[u64; common::TP_SCRATCH_WORDS]> =
    PerCpuArray::with_max_entries(1, 0);

/// 构造新聚合值的暂存区（per-CPU）：`ConnAggWire` 有 300+ 字节，放栈上会超 512 字节上限。
#[map]
static SCRATCH: PerCpuArray<ConnAggWire> = PerCpuArray::with_max_entries(1, 0);

#[inline(always)]
fn bump<F: FnOnce(&mut ConnAggWire)>(key: &ConnKey, f: F) {
    if let Some(ptr) = CONN_AGG.get_ptr_mut(key) {
        // per-CPU 值：本 CPU 独占，直接读改写（设计里没有用原子指令的必要）。
        unsafe { f(&mut *ptr) };
        return;
    }
    // 新键：先清零、再改、再插入。**在 map 值上做**而不是栈上：
    // `ConnAggWire` 含 32 槽直方图共 300+ 字节，放栈上会触发 BPF 栈超限。
    let Some(scratch) = SCRATCH.get_ptr_mut(0) else {
        return;
    };
    unsafe {
        *scratch = ConnAggWire::default();
        f(&mut *scratch);
        let _ = CONN_AGG.insert(key, &*scratch, 0);
    }
}

/// 连接状态迁移：建连、关闭时长、主动连接超时失败。
///
/// 显式写 `name`/`category`：段名会变成 `tracepoint/sock/inet_sock_set_state`，
/// 与挂载计划一一对应（不带参数时段名只有 `tracepoint`，核对时容易看漏）。
#[tracepoint(name = "inet_sock_set_state", category = "sock")]
fn inet_sock_set_state(ctx: TracePointContext) -> u32 {
    match try_inet_sock_set_state(&ctx) {
        Ok(()) => 0,
        Err(_) => 0,
    }
}

fn try_inet_sock_set_state(ctx: &TracePointContext) -> Result<(), i64> {
    if !common::cfg_ready(&CFG) || !common::rate_allow(&CFG, &RATE) {
        return Ok(());
    }
    let (Some(offset_old), Some(offset_new), Some(offset_sport), Some(offset_dport), Some(offset_family), Some(offset_saddr), Some(offset_daddr)) = (
        common::cfg(&CFG, CfgIndex::TpOldState),
        common::cfg(&CFG, CfgIndex::TpNewState),
        common::cfg(&CFG, CfgIndex::TpSport),
        common::cfg(&CFG, CfgIndex::TpDport),
        common::cfg(&CFG, CfgIndex::TpFamily),
        common::cfg(&CFG, CfgIndex::TpSaddr),
        common::cfg(&CFG, CfgIndex::TpDaddr),
    ) else {
        return Ok(());
    };
    // 整条目一次读进 per-CPU 缓冲：**不能**对 ctx 指针做非常量加法（验证器会拒）。
    if unsafe { common::read_tp_buf(ctx, &TPBUF) }.is_none() {
        return Ok(());
    }
    let Some(family) = common::scratch_u16(&TPBUF, offset_family) else {
        return Ok(());
    };
    if family != common::cfg_u16(&CFG, CfgIndex::FamilyInet).unwrap_or(u16::MAX) {
        return Ok(());
    }
    let (Some(raw_old), Some(raw_new), Some(sport), Some(dport), Some(saddr), Some(daddr)) = (
        common::scratch_u32(&TPBUF, offset_old),
        common::scratch_u32(&TPBUF, offset_new),
        common::scratch_u16(&TPBUF, offset_sport),
        common::scratch_u16(&TPBUF, offset_dport),
        common::scratch_u32(&TPBUF, offset_saddr),
        common::scratch_u32(&TPBUF, offset_daddr),
    ) else {
        return Ok(());
    };
    let old_state = raw_old as i32;
    let new_state = raw_new as i32;
    // tracepoint 里读到的同样是网络序，转主机序后再进键（与 `sock_key` 一致）。
    let saddr = u32::from_be(saddr);
    let daddr = u32::from_be(daddr);

    let key = ConnKey {
        pid: common::current_tgid(),
        cgroup_id: common::current_cgroup_id(),
        saddr,
        daddr,
        sport,
        dport,
        protocol: 6,
    };
    if !common::cfg_bool(&CFG, CfgIndex::IncludeLoopback)
        && (common::is_loopback(key.saddr) || common::is_loopback(key.daddr))
    {
        return Ok(());
    }

    let now = unsafe { bpf_ktime_get_ns() };
    if common::is_state(&CFG, CfgIndex::TcpEstablished, new_state) {
        bump(&key, |value| {
            value.connections = value.connections.saturating_add(1);
        });
        let _ = CONN_START.insert(&key, &now, 0);
        return Ok(());
    }
    if !common::is_state(&CFG, CfgIndex::TcpClose, new_state) {
        return Ok(());
    }

    // 关闭：记存续时长（微秒）。
    if let Some(start) = unsafe { CONN_START.get(&key) }.copied() {
        // 见 `common::ns_to_micros`：用移位替代除法，避免引入 `__multi3` 未定义符号。
        let micros = common::ns_to_micros(now.saturating_sub(start));
        bump(&key, |value| common::record_duration(value, micros));
    }
    let _ = CONN_START.remove(&key);

    // 主动连接从 SYN_SENT 直接进 CLOSE：连接未建立，判为超时失败。
    if common::is_state(&CFG, CfgIndex::TcpSynSent, old_state) {
        bump(&key, |value| {
            value.failures = value.failures.saturating_add(1);
            value.failure_reason = REASON_TIMEOUT;
        });
    }
    Ok(())
}

/// 取出并清除本 tid 暂存的连接键（`HashMap::remove` 不返回值，所以先 `get` 再 `remove`）。
#[inline(always)]
fn take_entry() -> Option<ConnKey> {
    let tid = common::current_tid();
    let key = unsafe { ENTRY.get(&tid) }.copied();
    if key.is_some() {
        let _ = ENTRY.remove(&tid);
    }
    key
}

/// kprobe 入口：把连接键按 tid 暂存，供 kretprobe 使用。
#[inline(always)]
fn stash_entry(ctx: &ProbeContext) {
    if !common::cfg_ready(&CFG) || !common::rate_allow(&CFG, &RATE) {
        return;
    }
    let Some(sk) = ctx.arg::<*const c_void>(0) else {
        return;
    };
    if let Some(key) = unsafe { common::sock_key(sk, &CFG) } {
        let _ = ENTRY.insert(&common::current_tid(), &key, 0);
    }
}

#[kprobe(function = "tcp_sendmsg")]
fn tcp_sendmsg_entry(ctx: ProbeContext) -> u32 {
    stash_entry(&ctx);
    0
}

#[kprobe(function = "tcp_recvmsg")]
fn tcp_recvmsg_entry(ctx: ProbeContext) -> u32 {
    stash_entry(&ctx);
    0
}

#[kprobe(function = "tcp_connect")]
fn tcp_connect_entry(ctx: ProbeContext) -> u32 {
    stash_entry(&ctx);
    0
}

#[kretprobe(function = "tcp_sendmsg")]
fn tcp_sendmsg_ret(ctx: RetProbeContext) -> u32 {
    let Some(key) = take_entry() else {
        return 0;
    };
    // `tcp_sendmsg` 返回 `int`：**必须按 i32 读**。直接读 64 位寄存器会把未定义的高 32 位
    // 当成字节数（实测出现过 4294967285 这种“负数字节”）。
    let ret = ctx.ret::<i32>();
    if ret > 0 {
        bump(&key, |value| {
            value.bytes_sent = value.bytes_sent.saturating_add(ret as u32 as u64);
        });
    }
    0
}

#[kretprobe(function = "tcp_recvmsg")]
fn tcp_recvmsg_ret(ctx: RetProbeContext) -> u32 {
    let Some(key) = take_entry() else {
        return 0;
    };
    // 同上：`tcp_recvmsg` 也返回 `int`。
    let ret = ctx.ret::<i32>();
    if ret > 0 {
        bump(&key, |value| {
            value.bytes_recv = value.bytes_recv.saturating_add(ret as u32 as u64);
        });
    }
    0
}

#[kretprobe(function = "tcp_connect")]
fn tcp_connect_ret(ctx: RetProbeContext) -> u32 {
    let Some(key) = take_entry() else {
        return 0;
    };
    let ret = ctx.ret::<i32>();
    if ret < 0 {
        let reason = ebpf_abi::reason_from_errno(i64::from(ret));
        bump(&key, |value| {
            value.failures = value.failures.saturating_add(1);
            value.failure_reason = reason;
        });
    }
    0
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
