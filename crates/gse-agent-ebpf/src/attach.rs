//! 挂载计划：采集项类型 → 需要 attach 的程序集合。
//!
//! 计划是纯数据，`loader` 只按计划执行。这样「哪个采集项挂哪些点」可以在无特权环境断言
//! （本机无法真正 attach），也避免挂载逻辑散落在加载代码里。

use std::fmt;

/// 采集项类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EbpfItemKind {
    /// 网络连接与流量。
    Network,
    /// TCP 异常。
    Tcp,
    /// 进程生命周期。
    Process,
}

impl EbpfItemKind {
    /// 采集项类型字符串（与 GSE 采集项类型一致）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Network => "ebpf_network",
            Self::Tcp => "ebpf_tcp",
            Self::Process => "ebpf_process",
        }
    }

    /// 从采集项类型解析；不是 eBPF 类型返回 `None`。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ebpf_network" => Some(Self::Network),
            "ebpf_tcp" => Some(Self::Tcp),
            "ebpf_process" => Some(Self::Process),
            _ => None,
        }
    }

    /// 对应的内核态目标文件名（`packaging/ebpf/<name>.o`）。
    #[must_use]
    pub fn object_name(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Tcp => "tcp",
            Self::Process => "process",
        }
    }

    /// 是否产出 `ebpf_edges` 边记录。
    ///
    /// `ebpf_tcp` 只产出指标：它与 `ebpf_network` 用不同的 map，若两边都发边记录，
    /// 同一个 `record_id` 会被后写的覆盖（sqlite 主键覆盖写），两侧数据互相丢。
    #[must_use]
    pub fn emits_edges(self) -> bool {
        matches!(self, Self::Network)
    }

    /// 本采集项的全部类型（P1）。
    #[must_use]
    pub fn all() -> [Self; 3] {
        [Self::Network, Self::Tcp, Self::Process]
    }
}

impl fmt::Display for EbpfItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一个程序的挂载方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachPoint {
    /// `tracepoint/<category>/<name>`。
    TracePoint {
        /// eBPF 程序名（对象文件里的函数名，用于 `program_mut`）。
        program: String,
        category: String,
        name: String,
    },
    /// `kprobe/<function>`。
    KProbe { program: String, function: String },
    /// `kretprobe/<function>`。
    KRetProbe { program: String, function: String },
}

impl AttachPoint {
    /// eBPF 程序名。
    #[must_use]
    pub fn program(&self) -> &str {
        match self {
            Self::TracePoint { program, .. } | Self::KProbe { program, .. } => program,
            Self::KRetProbe { program, .. } => program,
        }
    }
}

/// 一个采集项的挂载计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachPlan {
    pub kind: EbpfItemKind,
    pub points: Vec<AttachPoint>,
}

impl AttachPlan {
    /// 按采集项类型给出计划。
    ///
    /// 程序名与内核态程序里的函数名一一对应（`crates/gse-ebpf-programs/src/*.rs`）。
    #[must_use]
    pub fn for_kind(kind: EbpfItemKind) -> Self {
        let points = match kind {
            EbpfItemKind::Network => vec![
                AttachPoint::TracePoint {
                    program: "inet_sock_set_state".into(),
                    category: "sock".into(),
                    name: "inet_sock_set_state".into(),
                },
                AttachPoint::KProbe {
                    program: "tcp_sendmsg_entry".into(),
                    function: "tcp_sendmsg".into(),
                },
                AttachPoint::KRetProbe {
                    program: "tcp_sendmsg_ret".into(),
                    function: "tcp_sendmsg".into(),
                },
                AttachPoint::KProbe {
                    program: "tcp_recvmsg_entry".into(),
                    function: "tcp_recvmsg".into(),
                },
                AttachPoint::KRetProbe {
                    program: "tcp_recvmsg_ret".into(),
                    function: "tcp_recvmsg".into(),
                },
                AttachPoint::KProbe {
                    program: "tcp_connect_entry".into(),
                    function: "tcp_connect".into(),
                },
                AttachPoint::KRetProbe {
                    program: "tcp_connect_ret".into(),
                    function: "tcp_connect".into(),
                },
            ],
            EbpfItemKind::Tcp => vec![
                AttachPoint::KProbe {
                    program: "tcp_retransmit_skb_entry".into(),
                    function: "tcp_retransmit_skb".into(),
                },
                AttachPoint::KRetProbe {
                    program: "tcp_retransmit_skb_ret".into(),
                    function: "tcp_retransmit_skb".into(),
                },
                AttachPoint::KProbe {
                    program: "tcp_send_active_reset_entry".into(),
                    function: "tcp_send_active_reset".into(),
                },
            ],
            EbpfItemKind::Process => vec![
                AttachPoint::TracePoint {
                    program: "sched_process_exec".into(),
                    category: "sched".into(),
                    name: "sched_process_exec".into(),
                },
                AttachPoint::TracePoint {
                    program: "sched_process_exit".into(),
                    category: "sched".into(),
                    name: "sched_process_exit".into(),
                },
                AttachPoint::TracePoint {
                    program: "sched_process_fork".into(),
                    category: "sched".into(),
                    name: "sched_process_fork".into(),
                },
            ],
        };
        Self { kind, points }
    }

    /// 聚合 map 名（差分读取对象）。
    #[must_use]
    pub fn aggregate_map(kind: EbpfItemKind) -> &'static str {
        match kind {
            EbpfItemKind::Network => "CONN_AGG",
            EbpfItemKind::Tcp => "TCP_AGG",
            EbpfItemKind::Process => "PROC_AGG",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn item_kind_round_trip() {
        for kind in EbpfItemKind::all() {
            assert_eq!(EbpfItemKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(EbpfItemKind::parse("cpu"), None);
        assert_eq!(EbpfItemKind::parse("ebpf_dns"), None, "P2 类型尚未实现");
        assert_eq!(EbpfItemKind::Network.to_string(), "ebpf_network");
    }

    #[test]
    fn plans_cover_declared_kernel_programs() {
        for kind in EbpfItemKind::all() {
            let plan = AttachPlan::for_kind(kind);
            assert_eq!(plan.kind, kind);
            assert!(!plan.points.is_empty());
            // 程序名不重复（重复挂同一个程序会 attach 两次）。
            let programs: HashSet<&str> = plan.points.iter().map(AttachPoint::program).collect();
            assert_eq!(
                programs.len(),
                plan.points.len(),
                "{kind} 计划里有重复程序名"
            );
        }
        // 每个 kretprobe 都要有配套的 kprobe 入口（否则拿不到连接键）。
        for kind in [EbpfItemKind::Network, EbpfItemKind::Tcp] {
            let plan = AttachPlan::for_kind(kind);
            for point in &plan.points {
                if let AttachPoint::KRetProbe { program, function } = point {
                    let entry = format!("{}_entry", function);
                    assert!(
                        plan.points
                            .iter()
                            .any(|p| matches!(p, AttachPoint::KProbe { program: name, .. } if name == &entry)),
                        "{kind} 的 kretprobe {program} 缺少 {entry} 入口"
                    );
                }
            }
        }
    }

    #[test]
    fn only_network_emits_edges_and_maps_differ() {
        assert!(EbpfItemKind::Network.emits_edges());
        assert!(
            !EbpfItemKind::Tcp.emits_edges(),
            "tcp 只出指标，避免覆盖边记录"
        );
        assert!(!EbpfItemKind::Process.emits_edges());
        let maps: HashSet<&str> = EbpfItemKind::all()
            .iter()
            .map(|k| AttachPlan::aggregate_map(*k))
            .collect();
        assert_eq!(maps.len(), 3, "三个采集项必须用各自的 map");
        assert_eq!(EbpfItemKind::Network.object_name(), "network");
    }
}
