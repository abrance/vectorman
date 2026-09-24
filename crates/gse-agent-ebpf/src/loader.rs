//! aya 加载与挂载（需求 1.6–1.8、3.1、9.3–9.4）。
//!
//! 职责边界：
//!
//! - 加载对象文件、按 [`crate::attach::AttachPlan`] 挂载、卸载时先 detach 再 drop links
//!   最后删 map（aya 的 `Ebpf` drop 语义），保证重复启停幂等。
//! - 把 [`crate::cfg::CfgValues`] 写进 `CFG` map、按配置覆盖 map 容量（编译期容量只是占位）。
//! - 实现 [`MapSource`]：读**全部 CPU** 副本求和，并把内核侧计数**写零复位**（只读 CPU 0
//!   会漏计约 `1/ncpu` 的流量且难以察觉；不写零会让下一周期重复计入同一批数据）。
//!
//! 加载失败由调用方按 [`crate::backoff::Backoff`] 重试。

use aya::maps::{Array, MapData, PerCpuHashMap, PerCpuValues};
use aya::programs::{KProbe, TracePoint};
use aya::Ebpf;

use crate::aggregate::{ConnKey, ProcessContext};
use crate::attach::{AttachPlan, AttachPoint, EbpfItemKind};
use crate::cfg::CfgValues;
use crate::config::EbpfConfig;
use crate::{ConnSnapshot, MapSource, ProcSnapshot};
use ebpf_abi::{ConnAggWire, ProcAggWire, ProcKey};

/// `CFG` map 名（内核态程序里同名）。
pub const CFG_MAP: &str = "CFG";

mod objects {
    include!(concat!(env!("OUT_DIR"), "/ebpf_objects.rs"));
}

/// 采集项对应的内嵌目标文件（由 `build.rs` 从 `packaging/ebpf/` 生成）。
///
/// 返回空切片表示**尚未构建**：调用方按 [`LoadedItem::load`] 的错误提示处理，
/// 不要把它当成「内核不支持」——那是 preflight 的判断。
#[must_use]
pub fn object_bytes(kind: EbpfItemKind) -> &'static [u8] {
    match kind {
        EbpfItemKind::Network => objects::NETWORK,
        EbpfItemKind::Tcp => objects::TCP,
        EbpfItemKind::Process => objects::PROCESS,
    }
}

/// 三个目标文件是否都已内嵌（`.o` 入库后为 `true`）。
#[must_use]
pub fn objects_embedded() -> bool {
    !objects::NETWORK.is_empty() && !objects::TCP.is_empty() && !objects::PROCESS.is_empty()
}

/// `aya::Pod` 需要类型在本 crate 里实现（孤儿规则）：用 `#[repr(transparent)]` 包一层，
/// 布局与 `ebpf-abi` 里的定义完全一致，读写时 `.0` 取出即可。
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct PodConnKey(ConnKey);

// SAFETY: `repr(transparent)` + 全整数字段，无填充、无非法位模式。
unsafe impl aya::Pod for PodConnKey {}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
struct PodConnAgg(ConnAggWire);

// SAFETY: 同上（`ConnAggWire` 是 `#[repr(C)]` 全整数字段，显式 pad 字段已声明）。
unsafe impl aya::Pod for PodConnAgg {}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct PodProcKey(ProcKey);

// SAFETY: 同上。
unsafe impl aya::Pod for PodProcKey {}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
struct PodProcAgg(ProcAggWire);

// SAFETY: 同上。
unsafe impl aya::Pod for PodProcAgg {}

/// 加载并挂载好的一个采集项。
pub struct LoadedItem {
    /// 采集项类型。
    pub kind: EbpfItemKind,
    /// aya 句柄：drop 时会 detach 全部 link 并卸载程序。
    /// 用 `Option` 是为了让 [`LoadedItem::unload`] 显式释放且可重复调用。
    bpf: Option<Ebpf>,
    /// 已挂载的程序名（排障与断言用）。
    attached: Vec<String>,
}

impl LoadedItem {
    /// 加载对象文件并挂载计划里的全部程序。
    ///
    /// `object` 是 `packaging/ebpf/<kind>.o` 的字节。
    pub fn load(
        kind: EbpfItemKind,
        object: &[u8],
        config: &EbpfConfig,
        cfg_values: &CfgValues,
    ) -> Result<Self, String> {
        if object.is_empty() {
            return Err(format!(
                "eBPF 目标文件为空（{kind}）：请先运行 scripts/build-ebpf.sh 生成 packaging/ebpf/{}.o",
                kind.object_name()
            ));
        }
        let plan = AttachPlan::for_kind(kind);
        let mut loader = aya::EbpfLoader::new();
        // 编译期容量只是占位：这里按采集项配置覆盖。
        loader.map_max_entries(
            AttachPlan::aggregate_map(kind),
            config.map_max_entries as u32,
        );
        if kind == EbpfItemKind::Process {
            loader.map_max_entries("EVENTS", config.ring_buffer_bytes as u32);
        }
        let mut bpf = loader
            .load(object)
            .map_err(|e| format!("加载 eBPF 对象失败（{kind}）：{e}"))?;

        write_cfg(&mut bpf, cfg_values)?;

        let mut attached = Vec::new();
        for point in &plan.points {
            attach_one(&mut bpf, point)?;
            attached.push(point.program().to_string());
        }
        Ok(Self {
            kind,
            bpf: Some(bpf),
            attached,
        })
    }

    /// 已挂载的程序名。
    #[must_use]
    pub fn attached(&self) -> &[String] {
        &self.attached
    }

    /// 主动卸载：先 detach（aya 的 program drop 会处理 link），再 drop `Ebpf` 删 map。
    ///
    /// 幂等：重复调用只会走一次释放路径（`Ebpf` 被取走后为 `None`）。
    pub fn unload(&mut self) {
        // drop 顺序：`Ebpf` 释放时先 detach 全部 link，再卸载程序并删除 map。
        self.bpf = None;
        self.attached.clear();
    }

    /// 取出 per-CPU 聚合 map 作为差分源（顺带把 `Ebpf` 的所有权带过去，避免 map 失效）。
    pub fn into_map_source(mut self) -> Result<AyaMapSource, String> {
        let kind = self.kind;
        let map_name = AttachPlan::aggregate_map(kind);
        let mut bpf = self.bpf.take().ok_or_else(|| "采集项已卸载".to_string())?;
        let map = bpf
            .take_map(map_name)
            .ok_or_else(|| format!("对象文件里没有 map {map_name}"))?;
        let cpus = aya::util::nr_cpus().map_err(|(_, e)| format!("获取 CPU 数量失败：{e}"))?;
        match kind {
            EbpfItemKind::Network | EbpfItemKind::Tcp => {
                let map: PerCpuHashMap<MapData, PodConnKey, PodConnAgg> =
                    PerCpuHashMap::try_from(map)
                        .map_err(|e| format!("map {map_name} 类型不符：{e}"))?;
                Ok(AyaMapSource::Conn(Box::new(ConnSource { bpf, map, cpus })))
            }
            EbpfItemKind::Process => {
                let map: PerCpuHashMap<MapData, PodProcKey, PodProcAgg> =
                    PerCpuHashMap::try_from(map)
                        .map_err(|e| format!("map {map_name} 类型不符：{e}"))?;
                let events = bpf
                    .take_map("EVENTS")
                    .and_then(|m| aya::maps::RingBuf::try_from(m).ok());
                Ok(AyaMapSource::Process(Box::new(ProcessSource {
                    bpf,
                    map,
                    cpus,
                    events,
                })))
            }
        }
    }
}

/// 把 `CFG` 数组写进 map（内核态读它拿偏移与状态常量）。
fn write_cfg(bpf: &mut Ebpf, values: &CfgValues) -> Result<(), String> {
    let map = bpf
        .map_mut(CFG_MAP)
        .ok_or_else(|| format!("对象文件里没有 map {CFG_MAP}"))?;
    let mut cfg: Array<&mut MapData, u64> =
        Array::try_from(map).map_err(|e| format!("map {CFG_MAP} 类型不符：{e}"))?;
    for (index, value) in values.slots().iter().enumerate() {
        cfg.set(index as u32, value, 0)
            .map_err(|e| format!("写 CFG[{index}] 失败：{e}"))?;
    }
    Ok(())
}

fn attach_one(bpf: &mut Ebpf, point: &AttachPoint) -> Result<(), String> {
    match point {
        AttachPoint::TracePoint {
            program,
            category,
            name,
        } => {
            let target: &mut TracePoint = bpf
                .program_mut(program)
                .ok_or_else(|| format!("对象文件里没有程序 {program}"))?
                .try_into()
                .map_err(|e| format!("程序 {program} 不是 tracepoint：{e}"))?;
            target
                .load()
                .map_err(|e| format!("加载程序 {program} 失败：{e}"))?;
            target
                .attach(category, name)
                .map_err(|e| format!("挂载 {program} 到 {category}/{name} 失败：{e}"))?;
        }
        AttachPoint::KProbe { program, function }
        | AttachPoint::KRetProbe { program, function } => {
            let target: &mut KProbe = bpf
                .program_mut(program)
                .ok_or_else(|| format!("对象文件里没有程序 {program}"))?
                .try_into()
                .map_err(|e| format!("程序 {program} 不是 kprobe/kretprobe：{e}"))?;
            target
                .load()
                .map_err(|e| format!("加载程序 {program} 失败：{e}"))?;
            target
                .attach(function.as_str(), 0)
                .map_err(|e| format!("挂载 {program} 到 {function} 失败：{e}"))?;
        }
    }
    Ok(())
}

/// aya 的 map 读取实现。
///
/// 与 `MapSource`（差分）之间只差「读快照」这一件事，所以这里只实现读取。
pub enum AyaMapSource {
    /// 连接型采集项（network/tcp）。
    Conn(Box<ConnSource>),
    /// 进程型采集项。
    Process(Box<ProcessSource>),
}

/// 连接型采集项的 map 读取状态（字段私有：`aya::Pod` 包装类型不外露）。
pub struct ConnSource {
    /// 持有 aya 句柄，保证 map/program 不被提前释放。
    #[allow(dead_code)]
    bpf: Ebpf,
    map: PerCpuHashMap<MapData, PodConnKey, PodConnAgg>,
    cpus: usize,
}

/// 进程型采集项的 map 读取状态。
pub struct ProcessSource {
    #[allow(dead_code)]
    bpf: Ebpf,
    map: PerCpuHashMap<MapData, PodProcKey, PodProcAgg>,
    cpus: usize,
    /// 原始事件环缓冲（`raw_events_enabled=false` 时内核态不写，读到的也是空）。
    events: Option<aya::maps::RingBuf<MapData>>,
}

impl AyaMapSource {
    /// 采集项类型。
    #[must_use]
    pub fn kind(&self) -> EbpfItemKind {
        match self {
            Self::Conn(_) => EbpfItemKind::Network,
            Self::Process(_) => EbpfItemKind::Process,
        }
    }

    /// 当前 CPU 数量（写零复位时要构造同样长度的值）。
    #[must_use]
    pub fn cpus(&self) -> usize {
        match self {
            Self::Conn(source) => source.cpus,
            Self::Process(source) => source.cpus,
        }
    }

    /// 读原始事件（只对 process 采集项有意义；内核态开启抽样时才写）。
    ///
    /// 环缓冲是「尽力而为」：读空返回空，不阻塞。
    pub fn drain_raw_events(&mut self) -> Vec<ebpf_abi::RawEvent> {
        let Self::Process(source) = self else {
            return Vec::new();
        };
        let Some(ring) = source.events.as_mut() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Some(item) = ring.next() {
            let bytes = item.as_ref();
            if bytes.len() < std::mem::size_of::<ebpf_abi::RawEvent>() {
                continue;
            }
            // 内核态写的就是 `RawEvent` 的 `#[repr(C)]` 布局（见 ebpf-abi）。
            let event =
                unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<ebpf_abi::RawEvent>()) };
            out.push(event);
        }
        out
    }

    /// 进程型快照（只对 process 采集项有意义）。
    pub fn drain_process(&mut self) -> Result<Option<ProcSnapshot>, String> {
        let Self::Process(source) = self else {
            return Ok(None);
        };
        let map = &mut source.map;
        let cpus = source.cpus;
        let mut out = Vec::new();
        let mut keys = Vec::new();
        for entry in map.iter() {
            let (key, values) = entry.map_err(|e| format!("遍历 per-CPU map 失败：{e}"))?;
            out.push((
                key.0,
                values.iter().map(|v| v.0).collect::<Vec<ProcAggWire>>(),
            ));
            keys.push(key);
        }
        for key in keys {
            // 写零复位：不写会让下一周期重复计入同一批数据。
            let zero = PerCpuValues::try_from(vec![PodProcAgg::default(); cpus])
                .map_err(|e| format!("构造零值失败：{e}"))?;
            map.insert(key, zero, 0)
                .map_err(|e| format!("写零复位失败：{e}"))?;
        }
        Ok(Some(out))
    }
}

impl MapSource for AyaMapSource {
    fn drain(&mut self) -> Result<ConnSnapshot, String> {
        let Self::Conn(source) = self else {
            return Ok(Vec::new());
        };
        let map = &mut source.map;
        let cpus = source.cpus;
        let mut out = Vec::new();
        let mut keys = Vec::new();
        for entry in map.iter() {
            let (key, values) = entry.map_err(|e| format!("遍历 per-CPU map 失败：{e}"))?;
            // 遍历**全部 CPU 副本**：只读 CPU 0 会漏计。
            let per_cpu: Vec<ConnAggWire> = values.iter().map(|v| v.0).collect();
            out.push((key.0, per_cpu));
            keys.push(key);
        }
        for key in keys {
            let zero = PerCpuValues::try_from(vec![PodConnAgg::default(); cpus])
                .map_err(|e| format!("构造零值失败：{e}"))?;
            map.insert(key, zero, 0)
                .map_err(|e| format!("写零复位失败：{e}"))?;
        }
        Ok(out)
    }

    fn process_context(&self, _key: &ConnKey) -> Option<ProcessContext> {
        // 进程上下文由内核态一起采集（`comm` 在原始事件与进程项里），用户态不在采集热路径上
        // 读 `/proc`；边记录的服务名由 dataserver 反查。
        None
    }
}

/// 进程快照求和：把 per-CPU 计数器相加。
#[must_use]
pub fn sum_process(values: &[ProcAggWire]) -> crate::ProcCounts {
    let mut exec = 0u64;
    let mut exit = 0u64;
    let mut fork = 0u64;
    for value in values {
        exec = exec.saturating_add(value.exec);
        exit = exit.saturating_add(value.exit);
        fork = fork.saturating_add(value.fork);
    }
    (exec, exit, fork)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_object_reports_build_hint() {
        let config = EbpfConfig::default();
        let cfg = CfgValues::from_slots(vec![0u64; ebpf_abi::CFG_LEN as usize]).unwrap();
        let err = match LoadedItem::load(EbpfItemKind::Network, &[], &config, &cfg) {
            Ok(_) => panic!("空对象文件必须失败"),
            Err(err) => err,
        };
        assert!(err.contains("scripts/build-ebpf.sh"), "{err}");
        assert!(err.contains("network.o"), "{err}");
    }

    #[test]
    fn sum_process_adds_counters() {
        let a = ProcAggWire {
            exec: 1,
            exit: 2,
            fork: 3,
        };
        let b = ProcAggWire {
            exec: 10,
            exit: 20,
            fork: 30,
        };
        assert_eq!(sum_process(&[a, b]), (11, 22, 33));
        assert_eq!(sum_process(&[]), (0, 0, 0));
    }
}
