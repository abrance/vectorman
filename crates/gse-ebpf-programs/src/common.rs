//! 内核态程序共用部分：CFG 读取、连接键拼装、per-CPU 累加。
//!
//! 设计约束（`/.monkeycode/specs/ebpf-observability/design.md` Pitfalls）：
//!
//! - **内核态不硬编码内核结构体偏移与 TCP 状态数值**：偏移来自用户态解析的
//!   `/sys/kernel/btf/vmlinux`，tracepoint 字段偏移与 TCP 状态常量来自用户态解析的
//!   tracepoint `format` 文件与 `include/net/tcp_states.h` 对应值，统一经 `CFG` map 下发。
//!   任一项读不到就**不采集**，绝不按猜测值跑。
//! - per-CPU map 的值由本 CPU 独占，直接读改写；用户态读差分后写零复位。
//!
//! 三个 bin 各自 `#[path = "common.rs"] mod common;`，因此每个 bin 只用到这里的一部分函数。

#![allow(dead_code)]

use aya_ebpf::{
    cty::{c_int, c_void},
    helpers::{
        bpf_get_current_cgroup_id, bpf_get_current_pid_tgid, bpf_ktime_get_ns,
        bpf_probe_read_kernel,
    },
    maps::{Array, PerCpuArray},
};
use ebpf_abi::{CfgIndex, ConnAggWire, ConnKey, CFG_VERSION, REASON_NONE};

/// 从 `CFG` map 读一个参数；未写入的槽返回 `None`。
#[inline(always)]
pub fn cfg(map: &Array<u64>, index: CfgIndex) -> Option<u64> {
    map.get(index as u32).copied()
}

/// 读参数并夹取为 `T`（`u16`/`u32`/`i32` 这些字段偏移与状态值）。
#[inline(always)]
pub fn cfg_u16(map: &Array<u64>, index: CfgIndex) -> Option<u16> {
    cfg(map, index).and_then(|v| u16::try_from(v).ok())
}

#[inline(always)]
pub fn cfg_u32(map: &Array<u64>, index: CfgIndex) -> Option<u32> {
    cfg(map, index).and_then(|v| u32::try_from(v).ok())
}

#[inline(always)]
pub fn cfg_i32(map: &Array<u64>, index: CfgIndex) -> Option<i32> {
    cfg(map, index).and_then(|v| i32::try_from(v).ok())
}

#[inline(always)]
pub fn cfg_bool(map: &Array<u64>, index: CfgIndex) -> bool {
    cfg(map, index).is_some_and(|v| v != 0)
}

/// 配置版本不一致时直接不采集（避免用户态与内核态布局错配）。
#[inline(always)]
pub fn cfg_ready(map: &Array<u64>) -> bool {
    cfg(map, CfgIndex::Version) == Some(CFG_VERSION)
}

/// tracepoint 条目缓冲的字数（`[u64; 8]` = 64 字节）。
pub const TP_SCRATCH_WORDS: usize = 8;

/// tracepoint 条目的栈缓冲长度。
///
/// 只需要覆盖到 `daddr`（5.8–6.x 上偏移 34..38），取 64 字节留余量；
/// **不能取太大**：BPF 每个程序栈上限 512 字节，缓冲会叠加在已有局部变量之上。
pub const TP_BUF_LEN: usize = TP_SCRATCH_WORDS * 8;

/// 把 tracepoint 条目整体读进栈缓冲。
///
/// **为什么必须这样**：字段偏移由用户态下发（设计里「内核态不硬编码偏移」），
/// 于是 `ctx + 偏移` 是**非常量**指针运算，验证器直接拒绝：
/// `math between ctx pointer and register with unbounded min value is not allowed`。
/// 改为「ctx → 固定长度栈缓冲」一次拷贝（不涉及 ctx 指针运算），再从缓冲里按掩码后的
/// 有界偏移取值 —— 验证器能证明索引落在缓冲内，于是放行。
/// 缓冲放在 **per-CPU map** 而不是栈上：BPF 每程序栈上限 512 字节，
/// 而 tracepoint 处理函数里已经有一个插入路径要用的聚合值（300+ 字节），
/// 再加缓冲会直接触发 LLVM 的 `BPF stack limit is exceeded`。
pub unsafe fn read_tp_buf(
    ctx: &aya_ebpf::programs::TracePointContext,
    scratch: &aya_ebpf::maps::PerCpuArray<[u64; TP_SCRATCH_WORDS]>,
) -> Option<()> {
    let Some(ptr) = scratch.get_ptr_mut(0) else {
        return None;
    };
    let dst = unsafe { core::slice::from_raw_parts_mut(ptr.cast::<u8>(), TP_BUF_LEN) };
    // `EbpfContext::as_ptr` 需要通过 trait 调用（aya 里它是 trait 方法）。
    let src = aya_ebpf::EbpfContext::as_ptr(ctx).cast::<u8>();
    unsafe { aya_ebpf::helpers::bpf_probe_read_kernel_buf(src, dst) }.ok()?;
    Some(())
}

/// 从 per-CPU 缓冲里按掩码后的有界偏移读字节。
#[inline(always)]
pub fn scratch_u8(
    scratch: &aya_ebpf::maps::PerCpuArray<[u64; TP_SCRATCH_WORDS]>,
    index: usize,
) -> Option<u8> {
    let ptr = scratch.get_ptr_mut(0)?;
    let byte = unsafe { ptr.cast::<u8>().add(index & (TP_BUF_LEN - 1)) };
    Some(unsafe { *byte })
}

/// 从 per-CPU 缓冲读 u16：偏移按 2 字节对齐掩码，保证 `offset + 1 < TP_BUF_LEN`。
#[inline(always)]
pub fn scratch_u16(
    scratch: &aya_ebpf::maps::PerCpuArray<[u64; TP_SCRATCH_WORDS]>,
    offset: u64,
) -> Option<u16> {
    let index = (offset as usize) & (TP_BUF_LEN - 2);
    let low = scratch_u8(scratch, index)?;
    let high = scratch_u8(scratch, index + 1)?;
    Some(u16::from_ne_bytes([low, high]))
}

/// 从 per-CPU 缓冲读 u32：偏移按 4 字节对齐掩码，保证 `offset + 3 < TP_BUF_LEN`。
#[inline(always)]
pub fn scratch_u32(
    scratch: &aya_ebpf::maps::PerCpuArray<[u64; TP_SCRATCH_WORDS]>,
    offset: u64,
) -> Option<u32> {
    let index = (offset as usize) & (TP_BUF_LEN - 4);
    let b0 = scratch_u8(scratch, index)?;
    let b1 = scratch_u8(scratch, index + 1)?;
    let b2 = scratch_u8(scratch, index + 2)?;
    let b3 = scratch_u8(scratch, index + 3)?;
    Some(u32::from_ne_bytes([b0, b1, b2, b3]))
}

/// 从 per-CPU 缓冲读 u64：偏移按 8 字节对齐掩码，保证 `offset + 7 < TP_BUF_LEN`。
///
/// `sys_exit_*` 的 `ret` 是 `long`（64 位）：**必须按 64 位读**。只读低 32 位时，
/// 大返回值（例如 `read` 一次读出 >2GiB）会看起来像负数、被误判成失败。
#[inline(always)]
pub fn scratch_u64(
    scratch: &aya_ebpf::maps::PerCpuArray<[u64; TP_SCRATCH_WORDS]>,
    offset: u64,
) -> Option<u64> {
    let index = (offset as usize) & (TP_BUF_LEN - 8);
    let low = scratch_u32(scratch, index as u64)?;
    let high = scratch_u32(scratch, (index + 4) as u64)?;
    Some(u64::from(low) | (u64::from(high) << 32))
}

/// 从内核地址读一个字段（`bpf_probe_read_kernel` 封装）。
///
/// # Safety
///
/// `base` 必须指向内核态可读内存；`offset` 来自用户态的 BTF 解析结果。
#[inline(always)]
pub unsafe fn read_field<T: Copy>(base: *const c_void, offset: u64) -> Option<T> {
    let ptr = unsafe { (base as *const u8).add(offset as usize) } as *const T;
    unsafe { bpf_probe_read_kernel(ptr) }.ok()
}

/// 当前 tid（内核态用 tid 关联 kprobe 与 kretprobe），高 32 位是 tgid。
#[inline(always)]
pub fn current_pid_tgid() -> u64 {
    bpf_get_current_pid_tgid()
}

#[inline(always)]
pub fn current_tid() -> u64 {
    current_pid_tgid() & 0xffff_ffff
}

#[inline(always)]
pub fn current_tgid() -> u32 {
    (current_pid_tgid() >> 32) as u32
}

#[inline(always)]
pub fn current_cgroup_id() -> u64 {
    unsafe { bpf_get_current_cgroup_id() }
}

/// 用 `struct sock *` 拼连接键。
///
/// 只支持 IPv4（P1 范围；IPv6 走单独 map，设计里已标注）。`skc_dport` 是网络序、
/// `skc_num` 是主机序——两者都按内核里的字节序读回后再归一，避免端口错位。
///
/// # Safety
///
/// `sk` 必须是内核态 `struct sock *`。
#[inline(always)]
pub unsafe fn sock_key(sk: *const c_void, cfg_map: &Array<u64>) -> Option<ConnKey> {
    let (Some(offset_daddr), Some(offset_rcv_saddr), Some(offset_dport), Some(offset_num), Some(offset_family), Some(offset_protocol)) = (
        cfg(cfg_map, CfgIndex::SockDaddr),
        cfg(cfg_map, CfgIndex::SockRcvSaddr),
        cfg(cfg_map, CfgIndex::SockDport),
        cfg(cfg_map, CfgIndex::SockNum),
        cfg(cfg_map, CfgIndex::SockFamily),
        cfg(cfg_map, CfgIndex::SockProtocol),
    ) else {
        return None;
    };
    let family: u16 = unsafe { read_field(sk, offset_family) }?;
    if family != cfg_u16(cfg_map, CfgIndex::FamilyInet)? {
        return None;
    }
    // `skc_daddr`/`skc_rcv_saddr` 是 `__be32`（网络序）。统一转成主机序再进键，
    // 这样用户态格式化不需要关心字节序（否则 127.0.0.1 会显示成 1.0.0.127 —— 实测踩过）。
    let daddr: u32 = u32::from_be(unsafe { read_field(sk, offset_daddr) }?);
    let saddr: u32 = u32::from_be(unsafe { read_field(sk, offset_rcv_saddr) }?);
    let dport_be: u16 = unsafe { read_field(sk, offset_dport) }?;
    let sport: u16 = unsafe { read_field(sk, offset_num) }?;
    let protocol: u8 = unsafe { read_field(sk, offset_protocol) }?;
    Some(ConnKey {
        pid: current_tgid(),
        cgroup_id: current_cgroup_id(),
        saddr,
        daddr,
        sport,
        dport: u16::from_be(dport_be),
        protocol,
    })
}

/// 回环判定（IPv4 的 127.0.0.0/8）。内核态侧粗过滤，细过滤仍在用户态。
#[inline(always)]
pub fn is_loopback(addr: u32) -> bool {
    addr & 0xff00_0000 == 0x7f00_0000
}

/// 按 `CFG` 的 TCP 状态常量判断：`state == CFG[TcpXxx]`。
#[inline(always)]
pub fn is_state(cfg_map: &Array<u64>, index: CfgIndex, state: c_int) -> bool {
    cfg_i32(cfg_map, index) == Some(state)
}

/// 读 `struct sock *` 的进程归属（pid/cgroup），失败返回 `(0, 0)`。
///
/// kprobe 入口与 tracepoint 里 `sk` 属于**当前进程**还是对端，取决于钩子位置：
/// 这里只记录当前进程，用户态按需反查。
#[inline(always)]
pub fn current_owner() -> (u32, u64) {
    (current_tgid(), current_cgroup_id())
}

/// 令牌桶状态在 `PerCpuArray` 里的下标。
pub mod rate_slot {
    /// 剩余令牌。
    pub const TOKENS: u32 = 0;
    /// 上次补充时间（纳秒），0 表示首次。
    pub const LAST_NS: u32 = 1;
    /// 累计被限流丢弃的事件数（用户态读走并复位）。
    pub const DROPPED: u32 = 2;
    /// 数组长度。
    pub const LEN: u32 = 3;
}

/// 令牌桶放行判定（需求 9.5、12.1-12.2）。
///
/// 内核态不做 CPU 百分比测量（拿不到），限流口径是**每秒事件数 + 突发容量**；
/// 决策函数在 `ebpf-abi` 里，宿主侧有单测（含首次、补充、时钟回拨、溢出边界）。
/// 被拒绝的事件在这里累加计数，用户态周期性读走并复位（对应 `agent_ebpf_rate_limited_total`）。
///
/// **只做移位与乘法**：BPF 目标没有 64 位除法指令，`u64 / 常量` 会引用未定义的 `__multi3`
/// （compiler_builtins），对象能编出来但 aya 加载时在函数重定位阶段直接失败。
#[inline(always)]
pub fn rate_allow(cfg_map: &Array<u64>, rate: &PerCpuArray<u64>) -> bool {
    let tokens_per_tick = cfg(cfg_map, CfgIndex::RateLimitTokensPerTick).unwrap_or(0);
    if tokens_per_tick == 0 {
        // 0 表示不限制：保持「配置成 0 就不限流」的直觉，避免误配把采集整体掐死。
        return true;
    }
    let burst = cfg(cfg_map, CfgIndex::RateLimitBurst).unwrap_or(1);
    let (Some(tokens_ptr), Some(last_ptr), Some(dropped_ptr)) = (
        rate.get_ptr_mut(rate_slot::TOKENS),
        rate.get_ptr_mut(rate_slot::LAST_NS),
        rate.get_ptr_mut(rate_slot::DROPPED),
    ) else {
        // 拿不到限流状态时**放行**：宁可采多，也不要因为缺 map 变成不采集。
        return true;
    };
    let tokens = unsafe { *tokens_ptr };
    let last_ns = unsafe { *last_ptr };
    let now_ns = unsafe { bpf_ktime_get_ns() };
    let (next_tokens, allowed) =
        ebpf_abi::token_bucket_step(tokens, last_ns, now_ns, tokens_per_tick, burst);
    unsafe {
        *tokens_ptr = next_tokens;
        *last_ptr = now_ns;
        if !allowed {
            *dropped_ptr = (*dropped_ptr).saturating_add(1);
        }
    }
    allowed
}

/// 失败原因默认值（内核态只写枚举，文案在用户态映射）。
pub const NO_REASON: u32 = REASON_NONE;

/// 纳秒 → 微秒：用 `>> 10`（≈ /1024）而不是 `/1000`。
///
/// 偏差约 2.4%，而直方图按 2 的幂分槽、前端 P95 本身就是近似值，因此可接受；
/// 换来的是内核态不出现常量除法（见 [`rate_allow`] 的说明）。**这是刻意的取舍，不是笔误。**
#[inline(always)]
pub const fn ns_to_micros(ns: u64) -> u64 {
    ns >> 10
}

/// 历史槽：把微秒时长记入 log2 直方图。
#[inline(always)]
pub fn record_duration(value: &mut ConnAggWire, micros: u64) {
    value.duration_sum_us = value.duration_sum_us.saturating_add(micros);
    if micros > value.duration_max_us {
        value.duration_max_us = micros;
    }
    let slot = ebpf_abi::hist_slot(micros);
    value.latency_hist[slot] = value.latency_hist[slot].saturating_add(1);
}
