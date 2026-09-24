//! 组装并下发内核态运行期参数（`CFG: Array<u64>`）。
//!
//! 内核态程序不硬编码内核结构体偏移与 TCP 状态值，这些值在这里算好：
//!
//! | 来源 | 提供什么 |
//! | --- | --- |
//! | `/sys/kernel/btf/vmlinux`（[`crate::btf`]） | `sock_common`/`sock` 字段字节偏移 |
//! | tracepoint `format`（[`crate::tracepoint_format`]） | `inet_sock_set_state` 字段偏移 |
//! | 本模块常量 | TCP 状态值、地址家族、开关 |
//!
//! 任一项取不到就返回 `Err`：**宁可这一项不采，也不按猜测值跑**（写错偏移会静默记到错误的
//! 地址上，比不采集更难查）。

use crate::btf::Btf;
use crate::config::EbpfConfig;
use crate::tracepoint_format::{self, FieldOffset};
use ebpf_abi::{CfgIndex, CFG_LEN, CFG_VERSION};

/// `AF_INET`（`include/linux/socket.h`）。
pub const AF_INET: u64 = 2;
/// `AF_INET6`。
pub const AF_INET6: u64 = 10;

/// TCP 状态值（`include/net/tcp_states.h`）。
///
/// 这些是**用户态**的常量：内核态只做 `state == CFG[TcpXxx]` 的比较，因此换内核或状态值
/// 变化只需改这里，不用重编 `.o`。取值与 Linux 数十年来的 ABI 一致。
pub const TCP_ESTABLISHED: u64 = 1;
pub const TCP_SYN_SENT: u64 = 2;
pub const TCP_SYN_RECV: u64 = 3;
pub const TCP_FIN_WAIT1: u64 = 4;
pub const TCP_FIN_WAIT2: u64 = 5;
pub const TCP_TIME_WAIT: u64 = 6;
pub const TCP_CLOSE: u64 = 7;
pub const TCP_CLOSE_WAIT: u64 = 8;
pub const TCP_LAST_ACK: u64 = 9;
pub const TCP_LISTEN: u64 = 10;
pub const TCP_CLOSING: u64 = 11;
pub const TCP_NEW_SYN_RECV: u64 = 12;

/// 内核态用到的 `struct sock` 字段（BTF 类型名, 成员名, CFG 下标）。
const SOCK_FIELDS: [(&str, &str, CfgIndex); 6] = [
    ("sock_common", "skc_daddr", CfgIndex::SockDaddr),
    ("sock_common", "skc_rcv_saddr", CfgIndex::SockRcvSaddr),
    ("sock_common", "skc_dport", CfgIndex::SockDport),
    ("sock_common", "skc_num", CfgIndex::SockNum),
    ("sock_common", "skc_family", CfgIndex::SockFamily),
    ("sock", "sk_protocol", CfgIndex::SockProtocol),
];

/// tracepoint 字段名 → CFG 下标。
const TRACEPOINT_FIELDS: [(&str, CfgIndex); 7] = [
    ("oldstate", CfgIndex::TpOldState),
    ("newstate", CfgIndex::TpNewState),
    ("sport", CfgIndex::TpSport),
    ("dport", CfgIndex::TpDport),
    ("family", CfgIndex::TpFamily),
    ("saddr", CfgIndex::TpSaddr),
    ("daddr", CfgIndex::TpDaddr),
];

/// 组装好的 `CFG` 数组（长度固定 [`CFG_LEN`]，未用槽为 0）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgValues {
    slots: Vec<u64>,
}

impl CfgValues {
    /// 槽数组（按 [`CfgIndex`] 下标取值）。
    #[must_use]
    pub fn slots(&self) -> &[u64] {
        &self.slots
    }

    /// 由槽数组直接构造（长度必须是 [`CFG_LEN`]，测试与特殊场景用）。
    pub fn from_slots(slots: Vec<u64>) -> Result<Self, String> {
        if slots.len() != CFG_LEN as usize {
            return Err(format!(
                "CFG 槽数不符：期望 {CFG_LEN}，实际 {}",
                slots.len()
            ));
        }
        Ok(Self { slots })
    }

    /// 取某个下标的值（用户态测试与排障用）。
    #[must_use]
    pub fn get(&self, index: CfgIndex) -> u64 {
        self.slots.get(index as usize).copied().unwrap_or_default()
    }

    /// 从 BTF 与 tracepoint `format` 组装。
    ///
    /// `format_text` 为 `None` 时用 [`tracepoint_format::defaults`] 兜底（文件不可读，
    /// 通常是 tracing 权限不足）；调用方应把这件事写进日志。
    pub fn build(
        btf: &Btf,
        format_text: Option<&str>,
        config: &EbpfConfig,
    ) -> Result<Self, String> {
        let mut slots = vec![0u64; CFG_LEN as usize];
        slots[CfgIndex::Version as usize] = CFG_VERSION;

        for (struct_name, member, index) in SOCK_FIELDS {
            let offset = btf.member_offset_bytes(struct_name, member)?;
            slots[index as usize] = u64::from(offset);
        }

        let fields = match format_text {
            Some(text) => tracepoint_format::require(
                &tracepoint_format::parse(text),
                &tracepoint_format::SOCK_STATE_FIELDS,
            )?,
            None => tracepoint_format::defaults(),
        };
        for (name, index) in TRACEPOINT_FIELDS {
            let FieldOffset { offset, .. } = *fields
                .get(name)
                .ok_or_else(|| format!("tracepoint format 缺少字段 {name}"))?;
            slots[index as usize] = u64::from(offset);
        }

        slots[CfgIndex::FamilyInet as usize] = AF_INET;
        slots[CfgIndex::FamilyInet6 as usize] = AF_INET6;
        for (index, value) in [
            (CfgIndex::TcpEstablished, TCP_ESTABLISHED),
            (CfgIndex::TcpSynSent, TCP_SYN_SENT),
            (CfgIndex::TcpSynRecv, TCP_SYN_RECV),
            (CfgIndex::TcpFinWait1, TCP_FIN_WAIT1),
            (CfgIndex::TcpFinWait2, TCP_FIN_WAIT2),
            (CfgIndex::TcpTimeWait, TCP_TIME_WAIT),
            (CfgIndex::TcpClose, TCP_CLOSE),
            (CfgIndex::TcpCloseWait, TCP_CLOSE_WAIT),
            (CfgIndex::TcpLastAck, TCP_LAST_ACK),
            (CfgIndex::TcpListen, TCP_LISTEN),
            (CfgIndex::TcpClosing, TCP_CLOSING),
            (CfgIndex::TcpNewSynRecv, TCP_NEW_SYN_RECV),
        ] {
            slots[index as usize] = value;
        }
        slots[CfgIndex::IncludeLoopback as usize] = u64::from(config.include_loopback);
        slots[CfgIndex::RawEventsEnabled as usize] = u64::from(config.raw_events_enabled);

        // 关键字段校验：偏移不能为 0（除 `skc_daddr` 天然是 0），否则说明解析错了结构体。
        for (struct_name, member, index) in SOCK_FIELDS {
            let value = slots[index as usize];
            if value == 0 && !(struct_name == "sock_common" && member == "skc_daddr") {
                return Err(format!(
                    "{struct_name}.{member} 解析出的偏移是 0，疑似解析错结构体，拒绝下发"
                ));
            }
            if value > 4096 {
                return Err(format!(
                    "{struct_name}.{member} 偏移 {value} 不合理（> 4096），拒绝下发"
                ));
            }
        }
        Ok(Self { slots })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VMLINUX: &str = "/sys/kernel/btf/vmlinux";

    fn fixture_format() -> String {
        // 取自 6.1 内核 `sock/inet_sock_set_state` 的字段布局（含 common 头与 5.19 起的 cookie）。
        [
            "field:unsigned short common_type;\toffset:0;\tsize:2;\tsigned:0;",
            "field:int common_pid;\toffset:4;\tsize:4;\tsigned:1;",
            "field:const void * skaddr;\toffset:8;\tsize:8;\tsigned:0;",
            "field:int oldstate;\toffset:16;\tsize:4;\tsigned:1;",
            "field:int newstate;\toffset:20;\tsize:4;\tsigned:1;",
            "field:__u16 sport;\toffset:24;\tsize:2;\tsigned:0;",
            "field:__u16 dport;\toffset:26;\tsize:2;\tsigned:0;",
            "field:__u16 family;\toffset:28;\tsize:2;\tsigned:0;",
            "field:__u8 saddr[4];\toffset:30;\tsize:4;\tsigned:0;",
            "field:__u8 daddr[4];\toffset:34;\tsize:4;\tsigned:0;",
            "field:__u8 saddr_v6[16];\toffset:38;\tsize:16;\tsigned:0;",
            "field:__u8 daddr_v6[16];\toffset:54;\tsize:16;\tsigned:0;",
            "field:__u64 cookie;\toffset:72;\tsize:8;\tsigned:0;",
        ]
        .join("\n")
    }

    fn real_btf() -> Option<Btf> {
        let path = std::path::Path::new(VMLINUX);
        if !path.exists() {
            eprintln!("跳过：{VMLINUX} 不存在");
            return None;
        }
        Some(crate::btf::load_vmlinux_btf(path).expect("解析 vmlinux BTF"))
    }

    #[test]
    fn builds_from_real_btf_and_format() {
        let Some(btf) = real_btf() else { return };
        let config = EbpfConfig {
            include_loopback: true,
            raw_events_enabled: true,
            ..EbpfConfig::default()
        };
        let cfg = CfgValues::build(&btf, Some(&fixture_format()), &config).unwrap();
        assert_eq!(cfg.get(CfgIndex::Version), CFG_VERSION);
        assert_eq!(cfg.get(CfgIndex::SockDaddr), 0, "skc_daddr 在 0");
        assert_eq!(cfg.get(CfgIndex::SockRcvSaddr), 4);
        assert!(cfg.get(CfgIndex::SockDport) >= 8);
        assert!(cfg.get(CfgIndex::SockFamily) > cfg.get(CfgIndex::SockNum));
        assert!(cfg.get(CfgIndex::SockProtocol) > 0);
        assert_eq!(cfg.get(CfgIndex::TpNewState), 20);
        assert_eq!(cfg.get(CfgIndex::TpDaddr), 34);
        assert_eq!(cfg.get(CfgIndex::FamilyInet), 2);
        assert_eq!(cfg.get(CfgIndex::TcpEstablished), 1);
        assert_eq!(cfg.get(CfgIndex::TcpClose), 7);
        assert_eq!(cfg.get(CfgIndex::IncludeLoopback), 1);
        assert_eq!(cfg.get(CfgIndex::RawEventsEnabled), 1);
        assert_eq!(cfg.slots().len(), CFG_LEN as usize);
    }

    #[test]
    fn falls_back_to_defaults_without_format_file() {
        let Some(btf) = real_btf() else { return };
        let cfg = CfgValues::build(&btf, None, &EbpfConfig::default()).unwrap();
        assert_eq!(cfg.get(CfgIndex::TpOldState), 16);
        assert_eq!(cfg.get(CfgIndex::TpNewState), 20);
        assert_eq!(cfg.get(CfgIndex::TpSaddr), 30);
        assert_eq!(cfg.get(CfgIndex::IncludeLoopback), 0, "默认不采回环");
        assert_eq!(cfg.get(CfgIndex::RawEventsEnabled), 0);
    }

    #[test]
    fn missing_tracepoint_field_is_rejected() {
        let Some(btf) = real_btf() else { return };
        // 少了 family：不能猜，必须整项拒绝。
        let text = fixture_format().replace("family", "familyX");
        let err = CfgValues::build(&btf, Some(&text), &EbpfConfig::default()).unwrap_err();
        assert!(err.contains("family"), "错误里要指出缺哪个字段：{err}");
    }

    #[test]
    fn empty_btf_is_rejected() {
        let empty = Btf::default();
        let err =
            CfgValues::build(&empty, Some(&fixture_format()), &EbpfConfig::default()).unwrap_err();
        assert!(err.contains("sock_common"), "{err}");
    }
}
