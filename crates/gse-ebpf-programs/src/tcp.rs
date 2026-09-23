//! `ebpf_tcp` 采集项的内核态程序：重传与 RST 计数。
//!
//! | 挂载点 | 作用 |
//! | --- | --- |
//! | `kretprobe/tcp_retransmit_skb` | TCP 重传次数（成功重传才计：返回 0 或 1） |
//! | `kretprobe/tcp_send_active_reset` | 主动 RST 次数 |
//!
//! 与 `network.rs` 使用**独立的 map 与独立的采集项**：两个采集项互不共享 buff，
//! 因此各自差分、各自上报，不会出现「同一批增量被两个采集项各读一次」的重复计数。
//! 本采集项只产出指标（`ebpf_tcp_*`），不产出 `ebpf_edges` 记录。

#![no_std]
#![no_main]

#[path = "common.rs"]
mod common;

use aya_ebpf::{
    cty::c_void,
    macros::{kprobe, kretprobe, map},
    maps::{Array, HashMap, PerCpuHashMap},
    programs::{ProbeContext, RetProbeContext},
};
use ebpf_abi::{ConnAggWire, ConnKey, CFG_LEN};

#[map]
static TCP_AGG: PerCpuHashMap<ConnKey, ConnAggWire> = PerCpuHashMap::with_max_entries(16384, 0);

#[map]
static ENTRY: HashMap<u64, ConnKey> = HashMap::with_max_entries(16384, 0);

#[map]
static CFG: Array<u64> = Array::with_max_entries(CFG_LEN, 0);

#[inline(always)]
fn bump<F: FnOnce(&mut ConnAggWire)>(key: &ConnKey, f: F) {
    if let Some(ptr) = TCP_AGG.get_ptr_mut(key) {
        unsafe { f(&mut *ptr) };
        return;
    }
    let mut value = ConnAggWire::default();
    f(&mut value);
    let _ = TCP_AGG.insert(key, &value, 0);
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

#[inline(always)]
fn stash_entry(ctx: &ProbeContext) {
    if !common::cfg_ready(&CFG) {
        return;
    }
    let Some(sk) = ctx.arg::<*const c_void>(0) else {
        return;
    };
    if let Some(key) = unsafe { common::sock_key(sk, &CFG) } {
        let _ = ENTRY.insert(&common::current_tid(), &key, 0);
    }
}

#[kprobe(function = "tcp_retransmit_skb")]
fn tcp_retransmit_skb_entry(ctx: ProbeContext) -> u32 {
    stash_entry(&ctx);
    0
}

/// `tcp_retransmit_skb` 返回 0 表示重传成功、1 表示队列为空（都不是失败）；
/// 负值是错误，不计重传次数。
#[kretprobe(function = "tcp_retransmit_skb")]
fn tcp_retransmit_skb_ret(ctx: RetProbeContext) -> u32 {
    let Some(key) = take_entry() else {
        return 0;
    };
    if ctx.ret::<i32>() >= 0 {
        bump(&key, |value| {
            value.tcp_retrans = value.tcp_retrans.saturating_add(1);
        });
    }
    0
}

/// `tcp_send_active_reset` 返回 void：入口直接建键计数，不需要 kretprobe 配对。
#[kprobe(function = "tcp_send_active_reset")]
fn tcp_send_active_reset_entry(ctx: ProbeContext) -> u32 {
    if !common::cfg_ready(&CFG) {
        return 0;
    }
    let Some(sk) = ctx.arg::<*const c_void>(0) else {
        return 0;
    };
    if let Some(key) = unsafe { common::sock_key(sk, &CFG) } {
        bump(&key, |value| {
            value.tcp_resets = value.tcp_resets.saturating_add(1);
        });
    }
    0
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
