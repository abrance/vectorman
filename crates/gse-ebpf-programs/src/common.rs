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
    helpers::{bpf_get_current_cgroup_id, bpf_get_current_pid_tgid, bpf_probe_read_kernel},
    maps::Array,
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
    let daddr: u32 = unsafe { read_field(sk, offset_daddr) }?;
    let saddr: u32 = unsafe { read_field(sk, offset_rcv_saddr) }?;
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

/// 失败原因默认值（内核态只写枚举，文案在用户态映射）。
pub const NO_REASON: u32 = REASON_NONE;

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
