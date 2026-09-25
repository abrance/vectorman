//! eBPF 内核态与用户态共享的 ABI：map 值布局、运行期参数索引与常量。
//!
//! 这个 crate **`no_std` 且无依赖**，两侧同时依赖它，避免「内核态结构体与用户态解读不一致」
//! 这类只能靠眼睛发现的错误（设计要求：内核态 map 布局 `#[repr(C)]`）。
//!
//! 两条铁律：
//!
//! 1. 内核态程序里**不硬编码任何内核结构体偏移与 TCP 状态数值**：偏移由用户态从
//!    `/sys/kernel/btf/vmlinux` 解析，状态常量与 tracepoint 字段偏移由用户态从
//!    `/sys/kernel/tracing/events/**/format` 解析，统一经 [`CfgIndex`] 指向的 `Array<u64>` 下发。
//! 2. 布局只在这里定义一次；`#[repr(C)]` + 编译期尺寸断言，改字段会立刻编译失败。

#![no_std]
#![forbid(unsafe_code)]

/// 直方图槽数（与内核态 `[u64; HIST_SLOTS]` 一致）。
pub const HIST_SLOTS: usize = 32;

/// 进程名长度（内核 `TASK_COMM_LEN`）。
pub const TASK_COMM_LEN: usize = 16;

/// 连接键：网络与 TCP 异常共用。
///
/// `saddr`/`daddr` 为 IPv4；IPv6 暂不在 P1 范围（设计里提到单独 map）。
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(C)]
pub struct ConnKey {
    pub pid: u32,
    /// 与 `pid` 一起构成 8 字节对齐；`u64` 在 `u32` 后会自动填充 4 字节。
    pub cgroup_id: u64,
    pub saddr: u32,
    pub daddr: u32,
    pub sport: u16,
    pub dport: u16,
    pub protocol: u8,
}

/// 连接聚合值（内核态 per-CPU map 的值类型）。
///
/// 用户态把它转成带 `Vec` 直方图的视图（`gse-agent-ebpf::aggregate::ConnAgg`）。
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ConnAggWire {
    pub connections: u64,
    pub failures: u64,
    pub failure_reason: u32,
    /// 显式填充：让后面的 `u64` 不依赖编译器默认布局。
    pub _pad: u32,
    pub bytes_sent: u64,
    pub bytes_recv: u64,
    pub duration_sum_us: u64,
    pub duration_max_us: u64,
    pub tcp_retrans: u64,
    pub tcp_resets: u64,
    pub latency_hist: [u64; HIST_SLOTS],
}

impl Default for ConnAggWire {
    fn default() -> Self {
        Self {
            connections: 0,
            failures: 0,
            failure_reason: REASON_NONE,
            _pad: 0,
            bytes_sent: 0,
            bytes_recv: 0,
            duration_sum_us: 0,
            duration_max_us: 0,
            tcp_retrans: 0,
            tcp_resets: 0,
            latency_hist: [0; HIST_SLOTS],
        }
    }
}

/// 进程聚合值（`ebpf_process` 采集项）。
#[derive(Clone, Copy, Default, Debug)]
#[repr(C)]
pub struct ProcAggWire {
    pub exec: u64,
    pub exit: u64,
    pub fork: u64,
}

/// 进程键：`ebpf_process` 采集项；`comm` 是内核的 16 字节进程名。
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(C)]
pub struct ProcKey {
    pub pid: u32,
    pub _pad: u32,
    pub cgroup_id: u64,
    pub comm: [u8; 16],
}

/// 原始事件（`data_type=ebpf`）：统一布局，`kind` 区分事件类型，未用字段置零。
///
/// 原始事件默认关闭、按 `raw_events_sample_ratio` 抽样上行（需求 4.5、10.3）。
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct RawEvent {
    pub kind: u32,
    /// errno（失败类事件）或保留。
    pub code: u32,
    pub pid: u32,
    pub _pad: u32,
    pub timestamp_ns: u64,
    pub cgroup_id: u64,
    pub saddr: u32,
    pub daddr: u32,
    pub sport: u16,
    pub dport: u16,
    pub protocol: u8,
    pub _pad2: [u8; 3],
    pub comm: [u8; 16],
    /// `-1` 表示没有对应的连接键。
    pub conn_pid: u32,
    pub _pad3: u32,
}

/// 原始事件类型。
pub const EVENT_KIND_CONNECT: u32 = 1;
pub const EVENT_KIND_ACCEPT: u32 = 2;
pub const EVENT_KIND_CLOSE: u32 = 3;
pub const EVENT_KIND_PROCESS_EXEC: u32 = 10;
pub const EVENT_KIND_PROCESS_EXIT: u32 = 11;
pub const EVENT_KIND_PROCESS_FORK: u32 = 12;

/// 失败原因枚举：内核态只写枚举值，用户态映射为字符串。
pub const REASON_NONE: u32 = 0;
pub const REASON_REFUSED: u32 = 1;
pub const REASON_TIMEOUT: u32 = 2;
pub const REASON_UNREACHABLE: u32 = 3;
pub const REASON_RESET: u32 = 4;
pub const REASON_OTHER: u32 = 5;

/// errno → 失败原因（内核态只做这个映射，避免状态机判断跑在内核里）。
///
/// 接受 syscall 的原始返回值（负值）或正数 errno：内部取绝对值，调用方不必自己取反。
#[must_use]
pub const fn reason_from_errno(errno: i64) -> u32 {
    match errno.unsigned_abs() {
        111 => REASON_REFUSED,           // ECONNREFUSED
        110 => REASON_TIMEOUT,           // ETIMEDOUT
        104 => REASON_RESET,             // ECONNRESET
        101 | 113 => REASON_UNREACHABLE, // ENETUNREACH / EHOSTUNREACH
        _ => REASON_OTHER,
    }
}

/// `CFG` map（`Array<u64>`）的下标。
///
/// 值分三类：内核结构体字段偏移（bit → byte 由用户态换算）、tracepoint 字段偏移、
/// 语义常量（TCP 状态、失败判定阈值）。**内核态只读，用户态写入**。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum CfgIndex {
    /// 配置格式版本，用户态写 [`CFG_VERSION`]，内核态不匹配则直接返回（避免新旧错配）。
    Version = 0,
    // --- struct sock_common / sock 字段偏移（字节）---
    /// `sock_common.skc_daddr`
    SockDaddr,
    /// `sock_common.skc_rcv_saddr`
    SockRcvSaddr,
    /// `sock_common.skc_dport`（网络序）
    SockDport,
    /// `sock_common.skc_num`（本地端口，主机序）
    SockNum,
    /// `sock_common.skc_family`
    SockFamily,
    /// `sock.sk_protocol`
    SockProtocol,
    // --- socket 家族常量 ---
    FamilyInet,
    FamilyInet6,
    // --- `sock/inet_sock_set_state` tracepoint 字段偏移（字节，含 common 头）---
    TpOldState,
    TpNewState,
    TpSport,
    TpDport,
    TpFamily,
    TpSaddr,
    TpDaddr,
    TpSaddrV6,
    TpDaddrV6,
    // --- TCP 状态常量（来自 `include/net/tcp_states.h`，由用户态填入）---
    TcpEstablished,
    TcpSynSent,
    TcpSynRecv,
    TcpFinWait1,
    TcpFinWait2,
    TcpTimeWait,
    TcpClose,
    TcpCloseWait,
    TcpLastAck,
    TcpListen,
    TcpClosing,
    TcpNewSynRecv,
    // --- 资源与开关 ---
    /// 是否采集回环（0/1）：内核态侧粗过滤，细过滤在用户态。
    IncludeLoopback,
    /// 是否输出原始事件到 RingBuf（0/1）。
    RawEventsEnabled,
    /// 令牌桶速率（每秒事件数）；0 表示不限制。
    RateLimitPerSec,
    /// 令牌桶容量（突发上限）。
    RateLimitBurst,
    /// 单位：`u64` 槽位数量占位，便于后续扩展时保持枚举稳定。
    Reserved,
}

/// `CFG` map 的长度（下标上限，留余量便于向后兼容追加）。
pub const CFG_LEN: u32 = 64;

/// 令牌桶一步：返回「消费后剩余令牌」与「是否放行」。
///
/// 放在共享 crate 里是为了**能在宿主机单测**：内核态只做「读 map → 调用本函数 → 写回」，
/// 决策逻辑（含边界：首次、长时间无事件、零速率、溢出）都在这里验证。
///
/// - `last_ns == 0` 视为首次：直接给满桶（避免启动瞬间按「从时间零点到现在」的超大额度放行）；
/// - 补充速率按 `rate_per_sec` 线性计算，向上取整到整秒之外用整数除法（宁可少补，不多放）；
/// - `rate_per_sec == 0` 由调用方判为「不限制」，不会进到这里。
#[must_use]
pub const fn token_bucket_step(
    tokens: u64,
    last_ns: u64,
    now_ns: u64,
    rate_per_sec: u64,
    burst: u64,
) -> (u64, bool) {
    let burst = if burst == 0 { 1 } else { burst };
    let available = if last_ns == 0 {
        burst
    } else {
        let elapsed_ns = now_ns.saturating_sub(last_ns);
        let refill = elapsed_ns.saturating_mul(rate_per_sec) / 1_000_000_000;
        let filled = tokens.saturating_add(refill);
        // `min` 在 const fn 里还不能用（`Ord` 不是 const trait），手写比较。
        if filled > burst {
            burst
        } else {
            filled
        }
    };
    if available == 0 {
        (0, false)
    } else {
        (available - 1, true)
    }
}

/// 由事件速率上限推导令牌桶容量：速率的 1/10，夹取到 1..=10_000。
///
/// 容量太小会把正常突发（例如连接风暴）当超限丢弃；太大则失去「限流」的意义。
#[must_use]
pub const fn burst_for_rate(rate_per_sec: u64) -> u64 {
    let burst = rate_per_sec / 10;
    if burst == 0 {
        1
    } else if burst > 10_000 {
        10_000
    } else {
        burst
    }
}

/// 当前配置格式版本；用户态与内核态不一致时不采集。
pub const CFG_VERSION: u64 = 1;

/// 直方图槽：把微秒值映射到 `[2^i, 2^(i+1))`。
///
/// 0 映射到槽 0；上界溢出到最后一槽（设计要求：长尾会低估，用户态标注为近似值）。
#[must_use]
pub const fn hist_slot(micros: u64) -> usize {
    if micros == 0 {
        return 0;
    }
    let index = 63 - micros.leading_zeros() as usize;
    if index >= HIST_SLOTS {
        HIST_SLOTS - 1
    } else {
        index
    }
}

/// 槽上界（微秒），用户态用于计算近似 P95。
#[must_use]
pub const fn hist_slot_upper_micros(slot: usize) -> u64 {
    if slot + 1 >= HIST_SLOTS {
        u64::MAX
    } else {
        1u64 << (slot + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, size_of};

    #[test]
    fn wire_layout_is_stable() {
        use core::mem::offset_of;
        // 字段偏移就是内核态 map 值的真实布局，改动即为兼容性破坏。
        assert_eq!(offset_of!(ConnKey, pid), 0);
        assert_eq!(offset_of!(ConnKey, cgroup_id), 8, "u64 对齐填充 4 字节");
        assert_eq!(offset_of!(ConnKey, saddr), 16);
        assert_eq!(offset_of!(ConnKey, daddr), 20);
        assert_eq!(offset_of!(ConnKey, sport), 24);
        assert_eq!(offset_of!(ConnKey, dport), 26);
        assert_eq!(offset_of!(ConnKey, protocol), 28);

        assert_eq!(offset_of!(ConnAggWire, connections), 0);
        assert_eq!(offset_of!(ConnAggWire, failures), 8);
        assert_eq!(offset_of!(ConnAggWire, failure_reason), 16);
        assert_eq!(
            offset_of!(ConnAggWire, bytes_sent),
            24,
            "failure_reason(u32)+显式 pad 之后"
        );
        assert_eq!(offset_of!(ConnAggWire, bytes_recv), 32);
        assert_eq!(offset_of!(ConnAggWire, duration_sum_us), 40);
        assert_eq!(offset_of!(ConnAggWire, duration_max_us), 48);
        assert_eq!(offset_of!(ConnAggWire, tcp_retrans), 56);
        assert_eq!(offset_of!(ConnAggWire, tcp_resets), 64);
        assert_eq!(offset_of!(ConnAggWire, latency_hist), 72);

        assert_eq!(size_of::<ConnKey>(), 32);
        assert_eq!(size_of::<ConnAggWire>(), 72 + HIST_SLOTS * 8);
        assert_eq!(size_of::<ProcAggWire>(), 24);
        assert_eq!(offset_of!(ProcKey, comm), 16);
        assert_eq!(offset_of!(RawEvent, comm), 48);
        assert_eq!(offset_of!(RawEvent, timestamp_ns), 16);
        assert_eq!(align_of::<ConnAggWire>(), 8);
    }

    #[test]
    fn cfg_index_is_contiguous_and_fits() {
        assert_eq!(CfgIndex::Version as u32, 0);
        assert!((CfgIndex::Reserved as u32) < CFG_LEN);
    }

    #[test]
    fn errno_mapping() {
        assert_eq!(reason_from_errno(-111), REASON_REFUSED);
        assert_eq!(reason_from_errno(-110), REASON_TIMEOUT);
        assert_eq!(reason_from_errno(-104), REASON_RESET);
        assert_eq!(reason_from_errno(-101), REASON_UNREACHABLE);
        assert_eq!(reason_from_errno(-113), REASON_UNREACHABLE);
        assert_eq!(reason_from_errno(-5), REASON_OTHER);
        assert_eq!(
            reason_from_errno(111),
            REASON_REFUSED,
            "正数 errno 同样接受"
        );
    }

    #[test]
    fn token_bucket_allows_burst_then_blocks() {
        let rate = 1_000u64; // 每秒 1000 个
        let burst = burst_for_rate(rate);
        assert_eq!(burst, 100);
        // 首次调用直接给满桶。
        let (tokens, allowed) = token_bucket_step(0, 0, 1_000_000_000, rate, burst);
        assert!(allowed);
        assert_eq!(tokens, burst - 1);
        // 同一时刻连续取：取完 burst 个之后开始拒绝。
        let mut tokens_left = tokens;
        let mut allowed_count = 1u64;
        for _ in 0..burst {
            let (next, ok) =
                token_bucket_step(tokens_left, 1_000_000_000, 1_000_000_000, rate, burst);
            tokens_left = next;
            if ok {
                allowed_count += 1;
            }
        }
        assert_eq!(allowed_count, burst, "同一时刻最多放行一桶");
        assert_eq!(tokens_left, 0);
        let (_, blocked) = token_bucket_step(0, 1_000_000_000, 1_000_000_000, rate, burst);
        assert!(!blocked, "没令牌必须拒绝");
    }

    #[test]
    fn token_bucket_refills_with_time() {
        let rate = 1_000u64;
        let burst = burst_for_rate(rate);
        // 空桶等 100ms：补 100 个（1000 * 0.1）。
        let (tokens, allowed) = token_bucket_step(0, 1_000_000_000, 1_100_000_000, rate, burst);
        assert!(allowed);
        assert_eq!(tokens, 99);
        // 等 1 秒：补满但不超容量。
        let (tokens, _) = token_bucket_step(0, 1_000_000_000, 2_000_000_000, rate, burst);
        assert_eq!(tokens, burst - 1);
        // 等很久（时钟回拨用 saturating：`now < last` 时不补也不崩）。
        let (tokens, allowed) = token_bucket_step(5, 2_000_000_000, 1_000_000_000, rate, burst);
        assert!(allowed);
        assert_eq!(tokens, 4);
        // 溢出边界：超大 elapsed 不 panic。
        let (_, allowed) = token_bucket_step(0, 1, u64::MAX, u64::MAX, 10);
        assert!(allowed);
    }

    #[test]
    fn burst_for_rate_edges() {
        assert_eq!(burst_for_rate(0), 1);
        assert_eq!(
            burst_for_rate(5),
            1,
            "不足 10 也给 1，不能是 0（0 会被当作满桶）"
        );
        assert_eq!(burst_for_rate(50_000), 5_000);
        assert_eq!(burst_for_rate(10_000_000), 10_000, "上限 1 万");
    }

    #[test]
    fn histogram_slots() {
        assert_eq!(hist_slot(0), 0);
        assert_eq!(hist_slot(1), 0, "1us 落在 [1,2)");
        assert_eq!(hist_slot(2), 1);
        assert_eq!(hist_slot(1023), 9);
        assert_eq!(hist_slot(1024), 10);
        assert_eq!(hist_slot(u64::MAX), HIST_SLOTS - 1, "溢出进最后一槽");
        assert_eq!(hist_slot_upper_micros(0), 2);
        assert_eq!(hist_slot_upper_micros(HIST_SLOTS - 1), u64::MAX);
    }
}
