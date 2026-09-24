# 需求实施计划

本期只交付设计，本清单为待实施拆分，全部未开工。P1/P2/P3 是交付阶段，阶段间可独立上线；P3 未开工不影响 P1/P2。

实施顺序：本 feature 在 `apm-tracing` 之后（`LogStore` 索引 v2 → `dataplane-ts-retention` → `apm-tracing` → 本 feature）。服务名静态映射的 CRUD 由 `apm-tracing` 提供，本 feature 只消费。

- [ ] 1. P1 前置：eBPF 构建链与前置校验
  - [x] 1.1 新增 `crates/gse-ebpf-programs`（`#![no_std]`，aya-bpf 风格），产出 `*.o`；CI 单独一步用 `bpfel-unknown-none` 构建，产物入库 `packaging/ebpf/`
    - 对应需求 17.6 与设计 Pitfalls 第一条
    - 状态：已实现（PR #48）。三个 bin（`network`/`tcp`/`process`）用 aya-ebpf 0.2 编写，`crates/ebpf-abi`（`no_std` 无依赖）保存与用户态共享的 `#[repr(C)]` 布局；
      该 crate **排除在工作区之外**（`exclude`），CI 新增 `ebpf-programs` 作业跑 `scripts/build-ebpf.sh --check`（只做类型检查，不依赖 bpf-linker）。
      `scripts/build-ebpf.sh` 产出 `.o` 到 `packaging/ebpf/`（需 `cargo install bpf-linker`），**尚未入库**：本机 LLVM 14 装不上 bpf-linker，需在带新 LLVM 的机器或 CI 上生成
  - [x] 1.2 新增 `crates/gse-agent-ebpf`：preflight（内核 ≥5.8、`/sys/kernel/btf/vmlinux`、`CapEff` bit 39/38 或 euid 0）
    - 对应需求 1.1-1.3、1.4；失败只降级该项能力并在 Agent 日志输出 warn（含建议动作），不阻止 Agent 启动
    - 状态：已实现（PR #46）：新 crate `crates/gse-agent-ebpf` 的 `preflight`（内核 ≥5.8、`/sys/kernel/btf/vmlinux`、root 或 `CAP_BPF`+`CAP_PERFMON`/`CAP_SYS_ADMIN`），读取路径可注入
  - [x] 1.3 校验结果上报：`agent_ebpf_capability` 指标点，链路页可读
    - 对应需求 1.5
    - 状态：已实现（PR #50）：Agent 采集项启动时先做前置校验，通过/失败都会上报一次 `agent_ebpf_capability`（含 kernel/btf/capability 与 reason 标签），链路页据此区分「eBPF 不可用」与「没有数据」
  - [x] 1.4 单测：注入式 `uname` / `/proc/self/status` 夹具，断言各检查项判定与错误文本
- [x] 2. P1 内核态程序（网络、TCP、进程）
    - 状态：已实现（PR #48）：三个 bin 类型检查通过（`scripts/build-ebpf.sh --check`）；`.o` 待带 bpf-linker 的环境生成。
      **内核态不硬编码任何结构体偏移与 TCP 状态值**：`sock_common`/`sock` 字段偏移由用户态解析 `/sys/kernel/btf/vmlinux`、tracepoint 字段偏移与状态常量由用户态解析，统一经 `CFG` map 下发（见 `ebpf_abi::CfgIndex`）
  - [x] 2.1 `network.bpf.c`/`.rs`：`inet_sock_set_state`、`kretprobe/tcp_connect`、`kretprobe/inet_csk_accept`、`kretprobe/tcp_sendmsg`/`tcp_recvmsg`、`kprobe/tcp_close`
    - 对应需求 3.1-3.8 与设计挂载点表
    - 状态：已实现（PR #48）：`inet_sock_set_state`（建连/关闭时长/超时失败）、kprobe+kretprobe 配对的 `tcp_sendmsg`/`tcp_recvmsg`/`tcp_connect`。**与设计的两处偏差**（已同步 design.md）：① 存续时长改用 `inet_sock_set_state` 的两端时间差，不挂 `kprobe/tcp_close`（`tcp_close` 只有 `sk` 指针，要挖连接键就得多一处结构体偏移依赖）；② 不挂 `kretprobe/inet_csk_accept`，被动建立已由 ESTABLISHED 迁移覆盖。kprobe 的参数在 kretprobe 里取不到，所以用 `ENTRY` map 按 tid 暂存入口键，未配对时跳过而不是猜
  - [x] 2.2 `CONN_AGG` per-CPU map（`ConnKey`/`ConnAgg`）与 log2 直方图槽、`OVERFLOW_SLOT` 累加
    - 对应需求 9.1-9.4、9.7
    - 状态：已实现（PR #48）：`CONN_AGG: PerCpuHashMap<ConnKey, ConnAggWire>`（容量占位 16384，加载期经 `EbpfLoader::map_max_entries` 覆盖）；`ConnAggWire` 带 `[u64; 32]` log2 直方图；per-CPU 值由本 CPU 独占读改写。**map 满的 `OVERFLOW_SLOT` 累加未实现**（PerCPU_HASH 满时 `insert` 返回错误，当前静默丢弃，计数上报留给下一 PR）
  - [x] 2.3 `tcp.bpf.c`：`tcp_retransmit_skb`、`tcp_send_active_reset`
    - 对应需求 5.1-5.6
    - 状态：已实现（PR #48）：`tcp.rs` 的 `tcp_retransmit_skb`（返回 `>= 0` 才计成功重传）与 `tcp_send_active_reset`（返回 void，入口即计）；使用**独立 map 与独立采集项**，避免与 `network` 共享 buff 造成重复计数
  - [x] 2.4 `process.bpf.c`：`sched_process_exec`/`exit`/`fork`，`cmdline` 截断 512 字节
    - 对应需求 4.1-4.6
    - 状态：部分实现（PR #48）：`sched_process_exec`/`exit`/`fork` 三个 tracepoint 已实现并计数。**偏差**：进程名用助手 `bpf_get_current_comm()`（16 字节）而不是从 `bprm` 读 `cmdline` 截断 512 字节 —— 少一处版本相关偏移依赖；完整 `cmdline` 留到 P2
  - [x] 2.5 `EVENTS` RingBuf 与 `CFG` Array map（运行期参数下发）
    - 对应需求 9.5-9.7
    - 状态：已实现（PR #48）：`EVENTS: RingBuf`（容量加载期覆盖）承载原始事件，`RawEvent` 布局在 `ebpf-abi`；`CFG: Array<u64>` 共 64 槽，下标见 `ebpf_abi::CfgIndex`，含版本号 `CFG_VERSION`，内核态版本不匹配直接不采集
  - [x] 2.6 `max_cpu_percent` 内核态令牌桶限流
    - 对应需求 9.5、12.1-12.2
- [ ] 3. P1 用户态加载、差分与聚合（差分/聚合/过滤已完成见 PR #46；运行期参数解析/退避/挂载计划已完成见 PR #49；aya 加载与挂载管理待做）
    - 状态：未实现（PR #48 不含）：`max_cpu_percent` 的内核态令牌桶限流未做；当前只有采集项配置里的上限字段（`config.rs`）与用户态侧的过滤/丢弃统计，限流留到 P1 收尾阶段
  - [x] 3.3 差分线程：遍历全部 CPU 副本求和、与上周期相减、写零值复位、清理零增量键
    - 状态：已实现（PR #46）：`sum_per_cpu` / `diff`（首次出现按绝对值、快照回退饱和不为负、最大值单调）与 `run_loop` 中的快照清理；「写零复位」由 aya `MapSource` 实现负责（PR-B）
  - [x] 3.4 过滤：`cgroup`/`process`/`port` include-exclude、`include_loopback`
    - 状态：已实现（PR #46）：`config::keep` + 组合矩阵单测（回环、include/exclude 优先级、前缀匹配、进程名未知时不误杀）
  - [x] 3.5 `EbpfEdge` 组装与 10 秒桶对齐；跨桶时只输出整桶
    - 状态：已实现（PR #46）：`edge_record`（空增量不产生记录、record_id 规则、失败原因枚举）与 `bucket_start` 对齐
  - [x] 3.6 1 分钟指标汇总：保留最近 6 个 10 秒桶，输出 `ebpf_*` 与 `apm_edge_*`（`source=ebpf`）
    - 状态：部分实现（PR #46）：`MinuteAccumulator` 按分钟汇总 10 秒桶并只输出已关闭桶，产出 `ebpf_edge_connections_total`/`ebpf_edge_bytes_total{direction}`/`ebpf_edge_duration_micros{avg,max}`/`ebpf_tcp_retrans_total`/`ebpf_tcp_failures_total`；**`apm_edge_*{source=ebpf}` 改由服务端在服务名反查后产生**（Agent 不知道全局服务表），已同步到设计文档
  - [x] 3.8 单测：假 map 快照驱动差分、过滤矩阵、桶对齐与 P95 近似、`record_id` 规则、退避序列、上限汇总
    - 状态：部分实现（PR #46）：假快照驱动差分、过滤矩阵、桶对齐与分钟汇总、`record_id`、能力降级路径；退避序列与资源上限汇总随 aya loader（PR-B）
  - [x] 3.1 aya 加载与挂载管理：幂等启停、detach→drop links→删 map、启动时清理遗留
    - 对应需求 1.6-1.8、16.2
    - 状态：已实现（PR #50）：`loader.rs` 用 `EbpfLoader` 加载并按挂载计划 attach，容量按采集项覆盖；
      卸载走 `LoadedItem::unload`（`Ebpf` drop 先 detach link 再删 map），采集项停用/改配置时由框架 abort 任务 → drop → detach，重复启停幂等。
      **遗留**：启动时清理上一次崩溃留下的 pin（当前没有 pin，`Ebpf` drop 已覆盖；若后续引入 pin 需补）
  - [x] 3.2 加载失败退避重试（30 秒起、×2、上限 10 分钟，成功清零）
    - 对应需求 1.7
    - 状态：已实现（PR #49）：`backoff.rs` 纯状态机（不碰时钟，调用方拿等待时长去 sleep），序列单测 `30/60/120/240/480/600/600`，成功清零后从 30 秒重来
  - [ ] 3.3 差分线程：遍历全部 CPU 副本求和、与上周期相减、写零值复位、清理零增量键
    - 对应需求 9.3-9.4 与设计 Pitfalls「不能只读 CPU 0」「必须写回 0」
  - [ ] 3.4 过滤：`cgroup`/`process`/`port` include-exclude、`include_loopback`
    - 对应需求 3.7、4.7
  - [ ] 3.5 `EbpfEdge` 组装与 10 秒桶对齐；跨桶时只输出整桶
    - 对应需求 3.4-3.6、10.1、10.7
  - [ ] 3.6 1 分钟指标汇总：保留最近 6 个 10 秒桶，输出 `ebpf_*` 与 `apm_edge_*`（`source=ebpf`）
    - 对应需求 10.2 与设计指标产出映射表
  - [ ] 3.7 资源限制汇总与本地自监控计数（`agent_ebpf_*`）
    - 对应需求 12.1-12.6
    - 状态：部分实现（PR #46 起）：`EbpfStats`/`EbpfSnapshot` 已有 flushes/edges/metrics/filtered/idle_keys/read_errors/map_overflow_dropped 计数与能力状态指标 `agent_ebpf_capability`；`agent_ebpf_*` 指标点与 CPU 占用采集随加载器（下一 PR）
  - [ ] 3.8 单测：假 map 快照驱动差分、过滤矩阵、桶对齐与 P95 近似、`record_id` 规则、退避序列、上限汇总
- [x] 3.9 运行期参数解析与下发（实现期新增的子项，设计 Pitfalls 要求「不硬编码偏移与状态值」）
  - [x] 3.9.1 `/sys/kernel/btf/vmlinux` 解析：自实现的极简 BTF 解析器（aya 用户态无 CO-RE 字段重定位，`aya-obj` 的成员信息不对外），
        **递归穿过匿名 struct/union** 找成员偏移；本机用真实 6.1 vmlinux 验证（`skc_daddr`@0、`skc_rcv_saddr`@4、`sock.__sk_common`@0）
    - 状态：已实现（PR #49）：`btf.rs`
  - [x] 3.9.2 tracepoint `format` 解析：取 `inet_sock_set_state` 各字段偏移，读不到文件时用文档化兜底布局（并有单测保证兜底值与真实 format 一致）
    - 状态：已实现（PR #49）：`tracepoint_format.rs`
  - [x] 3.9.3 组装 `CFG: Array<u64>`：BTF 偏移 + tracepoint 偏移 + TCP 状态常量 + 开关，含版本号与「偏移为 0 / 超出合理范围」的拒绝校验
    - 状态：已实现（PR #49）：`cfg.rs`
    - 附：本机实测推翻了一条想当然的假设 —— 6.1 里 `skc_dport`(12) 与 `skc_num`(14) 是**顺序字段**而非同一个 union，所以内核态必须按 `CFG` 给的两个偏移分别读，不能只读一个再推算

- [ ] 4. P1 Agent 采集项集成
  - [x] 4.1 采集项类型 `ebpf_network`、`ebpf_process`、`ebpf_tcp` 与配置字段、GSE 侧校验
    - 对应需求 2.1-2.5
    - 状态：部分实现（PR #50）：Agent 侧三个类型已在 `spawn_collector` 注册（`collect/ebpf.rs`），配置由 `EbpfConfig::from_value` 解析并夹取；
      GSE 侧 `build_collect_item` 加入类型白名单并校验端口数组、`bucket_secs`/`flush_interval_secs` 范围、`raw_events_sample_ratio` 范围（e2e 用例覆盖非法值与合法值）。
      **遗留**：前端采集项表单尚未提供 eBPF 类型的字段（只能走接口创建），留下一个 PR
  - [x] 4.2 热更新：按 `item_id` 对齐启停；`enabled=false` 卸载程序
    - 对应需求 2.6-2.8
    - 状态：已实现（PR #50）：复用既有 `reconcile`（按 `item_id` + 指纹比对）——配置变更或 `enabled=false` 会 abort 采集任务，
      任务被 drop 时 `LoadedItem`/`AyaMapSource` 一并 drop，从而 detach 并删 map；`LoadedItem::unload` 幂等
  - [x] 4.3 原始事件抽样上行 `data_type=ebpf`（`raw_events_sample_ratio`）
    - 对应需求 4.5、10.3
    - 状态：部分实现（PR #50）：进程项的 `exec`/`exit`/`fork` 原始事件经 RingBuf 读取后按比例抽样上行（`sample()` 单测覆盖 0/1/0.1 与极端值）；
      未知事件类型不丢弃，按 `unknown_<n>` 上报便于新内核排障。**network/tcp 的原始事件内核态尚未发出**，故只有进程项有明细
  - [ ] 4.4 单测：采集项启停序列、抽样比例统计、既有采集器行为不受影响
    - 状态：部分实现（PR #50）：抽样比例统计（含 0/1/0.1/极端小值）、原始事件字段映射与未知类型、进程分钟汇总（只输出已关闭桶）、
      空对象文件的错误提示、GSE 侧校验（非法端口/范围/比例 + 合法创建）。**未覆盖**：真实启停序列与「既有采集器不受影响」（需要特权环境）
- [ ] 5. P1 dataserver 接入与查询
  - [x] 5.1 `crates/dataplane-ingest/src/edge.rs`：`EbpfEdge` DTO + JSON 往返测试
    - 对应共享模型 `EbpfEdge` 定义
    - 状态：已实现（PR #51）：`EbpfEdge` DTO 与 Agent 侧逐字段一致（可选字段同样带 `#[serde(default)]`，避免一次字段裁剪让整条变 `partial`）；
      `EdgeSink` trait + JSON 往返/缺省值/校验规则单测
  - [x] 5.2 `DataType::EbpfEdges` 分支：幂等、字段校验（`failures <= connections`、`hist_slots`）、sqlite 主键覆盖写、流索引
    - 对应需求 3.9、10.1、10.4-10.7
    - 状态：已实现（PR #51）：`apply_with_sinks` 新增 `ebpf_edges` 分支（`apply_with_trace_sink` 保持原签名，供既有调用点与测试复用）；
      校验不合法整条记 `partial` 不落库；`record_id` 已受理则跳过（少一次反查与 sqlite 写）；落库 `INSERT OR REPLACE` 主键覆盖写；受理后计入流索引（沿用通用路径）
  - [x] 5.3 服务名反查接入 `EndpointRegistry`（固定顺序：静态映射 alias → `(dst_ip,dst_port)` → `dst_pod` → `unknown-<ip>`；命中与负面结果均缓存 60 秒）
    - 对应需求 11.1-11.6 与共享模型反查顺序；alias 由 `apm-tracing` 的 `/v1/apm/service-aliases` 维护
    - 状态：已实现（PR #51）：`dataplane-apm/src/ebpf_edge.rs` 按固定顺序反查（Agent 已填写的服务名不覆盖），
      复用 `AliasCache`（快照 + 显式失效）与 `EndpointRegistry`（命中/负面都缓存 60 秒）；未命中落 `unknown-<ip>` 且**不**写端点表。
      单测覆盖：两面均未识别、静态映射命中（含缓存失效契约）、端点表 `(ip,port)` 命中、负面缓存 TTL 内仍返回未识别、覆盖写幂等
  - [ ] 5.4 `POST /v1/edges/search`：过滤、分页、`source` 为空时两路合并汇总
    - 对应需求 13.1-13.3、13.5-13.6
  - [ ] 5.5 `POST /v1/ebpf/events/search` 与 `GET /v1/ebpf/capability`
    - 对应需求 13.4 与 1.5、12.6
  - [ ] 5.6 保留期清理：`ebpf_edges` 分批删除、`data_type=ebpf` 循环删除、`retain/` 机制；聚合指标接入 `dataplane-ts-retention`（`ts_retention_days` 缺省 30 天）
    - 对应需求 14.1-14.4
  - [ ] 5.7 httptest：幂等重放、非法字段 `partial`、合并查询求和、清理三类数据、时间范围非法 400
    - 状态：部分实现（PR #51）：dataserver e2e 覆盖「1 条合法 + 2 条非法 → `partial` 且只落 1 行、重放幂等、未识别服务归一为 `unknown-<ip>`」。
      **未覆盖**：合并查询求和、清理三类数据、时间范围非法 400（随查询与保留期一起做）
- [ ] 5.8 边指标由 dataserver 派生（实现期新增子项）
  - [ ] 状态：未做（PR #51 先落库）。**口径决定**：Agent 侧当前也会产出 `ebpf_*` 边指标，但那些点缺少
    `src_service`/`dst_service`（Agent 无法解析全局服务表），与共享模型的指标维度不符，且会与 dataserver
    派生出的同名序列形成两套。下一 PR 改为：Agent 只发 `agent_ebpf_capability`、进程指标与原始事件；
    `ebpf_edge_*`/`ebpf_tcp_*`/`apm_edge_*{source=ebpf}` 全部由 dataserver 从 `ebpf_edges` 表按分钟派生
    （直方图在边上，`p95` 由槽上界近似，口径与 APM 侧一致地标注为近似）

- [ ] 6. 检查点 - P1 在特权 runner 上跑通受控流量用例后再进入 P2
  - 确保所有测试通过,如有疑问请询问用户
- [ ] 7. P1 前端与 CLI
  - [ ] 7.1 `/ebpf` 页「边」视图：过滤、表格列、手动刷新、降级标记、前置校验状态
    - 对应需求 15.2-15.4、15.7-15.9；图表用 `echarts`（边指标曲线 line series，不用力导向布局）
  - [ ] 7.2 `/ebpf` 页「事件」视图：`event_type`/`pid`/`process_name`/关键词过滤
    - 对应需求 15.3
  - [ ] 7.3 `/topology` 页 `source` 切换（全部 / otlp / ebpf）
    - 对应需求 15.5、11.4-11.5
  - [ ] 7.4 边行「查看该边 trace」跳转 `/traces`
    - 对应需求 15.6
  - [ ] 7.5 `dpc edges`、`dpc ebpf-events` 子命令
    - 对应需求 13.7
  - [ ] 7.6 vitest：两视图参数拼装、`source` 切换表达式、降级标记、跳转 URL
- [ ] 8. P2 文件与 syscall、DNS
  - [ ] 8.1 内核态：`sys_enter/sys_exit_openat|read|write|fsync` 延迟直方图与错误码计数
    - 对应需求 6.1-6.6
  - [ ] 8.2 内核态：`udp_sendmsg`/`udp_recvmsg` 端口 53 过滤与 `DNS_PENDING` 匹配
    - 对应需求 7.1-7.5
  - [ ] 8.3 用户态：`ebpf_syscall_duration_micros`、`ebpf_syscall_failures_total`、`ebpf_dns_duration_micros`、`ebpf_dns_timeouts_total`
    - 对应需求 6.2-6.3、7.2-7.3
  - [ ] 8.4 慢调用阈值与原始慢事件上行（`slow_threshold_micros`）
    - 对应需求 6.5
  - [ ] 8.5 采集项类型 `ebpf_syscall`、`ebpf_dns` 与 GSE 校验
    - 对应需求 2.1-2.5
  - [ ] 8.6 单测：直方图到 avg/p95、errno 分组、DNS 事务匹配与超时、路径截断
- [ ] 9. P3 CPU profile 与火焰图
  - [ ] 9.1 内核态：每 tid `perf_event_open`、`bpf_get_stackid(BPF_F_USER_STACK)`、`STACKS` map
    - 对应需求 8.1-8.2
  - [ ] 9.2 用户态符号化：`/proc/{pid}/maps` + build-id 查符号表，失败保留地址并标记
    - 对应需求 8.3、8.7、16.4
  - [ ] 9.3 折叠栈聚合与压缩上行 `EbpfProfile`（`data_type=ebpf_profiles`）
    - 对应需求 8.4-8.5、17.3
  - [ ] 9.4 `max_profiled_processes` 限制与计数
    - 对应需求 8.6、12.1
  - [ ] 9.5 dataserver：`EbpfProfile` DTO、`FileStore` 落盘、`ebpf_profile_index` 建表与写入、解压超限拒绝
    - 对应需求 8.4 与设计 Data Models
  - [ ] 9.6 `GET /v1/ebpf/profiles`、`GET /v1/ebpf/profiles/{record_id}` 与独立保留期清理
    - 对应需求 14.5
  - [ ] 9.7 前端 `/ebpf/profile` 火焰图页面替换占位空态
    - 对应需求 15.2
  - [ ] 9.8 单测：folded 往返、超 8 MiB/20 万行拒绝、幂等覆盖、产物与索引同时清理
- [ ] 10. 检查点 - 确保所有测试通过
  - 确保所有测试通过,如有疑问请询问用户
- [ ] 11. 端到端与文档收尾
  - [ ] 11.1 特权 runner 集成：受控流量断言边记录、回环开关、限流计数、卸载无残留
    - 对应需求 16.2-16.3
  - [ ] 11.2 混合场景：eBPF 边指标与 OTLP 边指标在同分钟合并求和一致
    - 对应需求 11.5 与共享模型合并口径
  - [ ] 11.3 降级与不可用场景：无 BTF / 无权限宿主机上链路页显示不可用且其余采集正常
    - 对应需求 16.1、16.5-16.7
  - [ ] 11.4 回改 `.monkeycode/specs/gse-dataplane-ingest/design.md` 与共享模型的交叉引用
  - [ ] 11.5 回改 `observability-data-model/design.md`：聚合指标保留从「外部缺口」改为「依赖 `dataplane-ts-retention`」
