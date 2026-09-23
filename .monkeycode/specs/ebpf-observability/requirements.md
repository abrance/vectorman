# Requirements Document

## Introduction

本 feature 交付 eBPF 可观测能力：`gse-agent` 以内核态 eBPF 采集网络连接、进程生命周期、TCP 异常、文件与 syscall 延迟、DNS 与 CPU 采样信号，在内核态与用户态两级聚合后按既有采集信道上行；`dataserver` 落库边聚合与事件明细、生成网络与拓扑指标、做服务名反查，前端在 `@vectorman/dataplane` 增加 eBPF 事件页并与 APM 拓扑页共享边指标。

范围前提（本期只交付设计，不写代码）：

- 数据模型与共享部分见 `observability-data-model`：新增 `data_type=ebpf_edges`（边聚合，落 sqlite 并映射为边指标）与 `ebpf`（原始事件，落 `LogStore`，默认关闭）；指标命名、服务名反查、拓扑合并口径在同一文件定义。
- 复用 `gse-dataplane-ingest` 的采集项下发、Agent 缓冲与重试、直连 `dataserver` 上报、保留期机制。
- 采用 aya（纯 Rust）实现，不引入 libbpf/clang 构建链；内核基线 5.8 + BTF，Agent 需 root 或 CAP_BPF。
- 与 `apm-tracing` 的关系：eBPF 是 OTLP 未覆盖场景的兜底数据源，两者在拓扑页通过指标 label `source=ebpf` / `source=otlp` 区分并合并；eBPF 不产生 span，不写 `apm_trace_summary`。
- 服务名归一首选静态映射表 `apm_service_alias`（由 `apm-tracing` 提供 CRUD，本 feature 只消费）。
- 聚合指标的保留与删除依赖同期交付的 `dataplane-ts-retention`（`TimeSeriesStore::delete_series` + 全局保留窗口），不再是外部依赖。

实施顺序：`LogStore` 索引 v2 → `dataplane-ts-retention` → `apm-tracing` → 本 feature。

信号按阶段交付（本期设计覆盖全部阶段，实现按阶段推进）：

| 阶段 | 信号 | 说明 |
| --- | --- | --- |
| P1 | 网络连接与流量、进程生命周期、TCP 异常 | 支撑「无插桩应用也能看到服务拓扑与连接健康」 |
| P2 | 文件与 syscall 延迟、DNS 延迟 | 支撑「慢在哪一层」的定位 |
| P3 | CPU profile | 支撑火焰图；符号化依赖重，单独阶段 |

不覆盖：内核模块形式（无模块、无 DKMS）、Windows 与 macOS、容器运行时插桩（OCI hook）、网络报文解码（L7 协议解析）、安全审计与阻断、告警与通知、跨主机调用链拼接（eBPF 只给边，不给完整 trace）。

## Glossary

- **eBPF**：Linux 内核虚拟机，通过 kprobe、tracepoint、fentry 等挂载点采集内核事件。
- **aya**：纯 Rust 的 eBPF 库，含内核态程序构建与用户态加载，不依赖 libbpf/clang。
- **BTF**：BPF Type Format，`/sys/kernel/btf/vmlinux` 存在表示支持 CO-RE，是内核基线的一部分。
- **map**：eBPF 内核态键值存储，用于事件传递与聚合。
- **per-CPU map**：每个 CPU 一份副本的 map，避免跨 CPU 竞争，用于高频计数。
- **内核态聚合**：在 eBPF 程序内完成计数累加，用户态只读差分，避免每事件唤醒。
- **用户态差分**：用户态按周期读取 per-CPU map 快照并与上一周期相减，得到区间增量。
- **聚合桶（bucket）**：`EbpfEdge` 的时间粒度，默认 10 秒，由采集项 `bucket_secs` 配置。
- **边（edge）**：`源 → 目标` 的连接关系及其流量、连接数、失败与延迟统计。
- **原始事件**：单次事件记录（一次 connect、一次 exec），默认不上行。
- **信号（signal）**：一类可独立启停的采集内容，对应一个采集项类型。
- **前置校验（preflight）**：启动采集前对内核版本、BTF、权限的检查。
- **CAP_BPF**：Linux 5.8 起可用的能力位，配合 CAP_PERFMON 可替代 root 运行 BPF 程序。
- **cgroup_id**：连接或进程所属 cgroup 标识，用于反查容器与 Pod。
- **栈采样（P3）**：按周期触发 perf 事件采样用户态调用栈，用于 CPU 火焰图。
- **符号化（P3）**：把采样得到的地址转换为函数名，需要 `/proc/{pid}/maps` 与符号表。

## Requirements

### Requirement 1: eBPF 采集器与前置校验

**User Story:** AS 运维人员, I want 采集前先确认环境可用, so that 不满足条件的机器不会静默失败。

#### Acceptance Criteria

1. THE `gse-agent` SHALL 提供 eBPF 采集器，使用 aya 加载内核态程序，不依赖 libbpf 或 clang。
2. THE 前置校验 SHALL 检查：内核版本不低于 5.8、`/sys/kernel/btf/vmlinux` 可读、当前进程具备 root 或 `CAP_BPF`（配合 `CAP_PERFMON`）。
3. WHEN 前置校验通过，THE Agent SHALL 允许启动对应 eBPF 采集项。
4. WHEN 前置校验失败，THE Agent SHALL 禁用 eBPF 采集项并在 Agent 日志中输出 warn 级别一行，含 `item_id`、失败的检查项、实测值与建议动作（以 root 或带 `CAP_BPF`+`CAP_PERFMON` 的账号运行）。

   （Agent 以 systemd 或普通进程运行，运维侧具备 sudo 能力。因此权限不足属配置问题而非致命错误：只降级该项能力并留下 warn，不让整个 Agent 启动失败。）
5. THE 校验结果 SHALL 上报到 `dataserver`，使链路页能显示该 Agent 的 eBPF 不可用状态与原因。
6. IF 内核态程序加载失败（`EPERM`、`EINVAL`、验证器拒绝），THE Agent SHALL 记录原因并向标准错误输出一行，进程与其他采集项继续运行。
7. THE Agent SHALL 在加载失败后按退避重试（初始 30 秒，上限 10 分钟），并在成功后恢复采集；重试次数与最近失败原因 SHALL 可查询。
8. THE 内核态程序的加载与卸载 SHALL 幂等：同一信号重复启停不产生重复挂载点或泄漏 map。

### Requirement 2: eBPF 采集项与配置下发

**User Story:** AS 运维人员, I want 每类信号独立配置与启停, so that 采集范围与开销可逐项控制。

#### Acceptance Criteria

1. THE 采集项类型 SHALL 新增：`ebpf_network`、`ebpf_process`、`ebpf_tcp`、`ebpf_syscall`、`ebpf_dns`、`ebpf_cpu_profile`，与既有类型并列。
2. THE 采集项的采集端配置 SHALL 包含：`bucket_secs`（缺省 10）、`flush_interval_secs`（缺省 10）、`cgroup_include`、`cgroup_exclude`、`process_include`、`process_exclude`、`port_include`、`port_exclude`、`include_loopback`（缺省 false）、`hist_slots`（缺省 24）。
3. THE 采集项的入库配置 SHALL 包含 `retention_days`（缺省 3）、`raw_events_enabled`（缺省 false）、`raw_events_sample_ratio`（缺省 0.01）。
4. THE 采集项 SHALL 复用既有 `collect_items` 表的 `collector_json` 与 `storage_json` 列。
5. THE 有效性校验 SHALL 在 GSE Server 写入时完成：`bucket_secs` 上限 60、`flush_interval_secs` 上限 60、`hist_slots` 上限 32、`retention_days` 上限 30、`raw_events_sample_ratio` 取值 0 到 1；越界返回 `invalid_argument`。
6. WHEN 采集项 `enabled` 为 false，THE Agent SHALL 卸载对应的内核态程序并停止上报；已入库数据仍可查询，保存周期继续生效。
7. WHEN Agent 收到新的采集项列表，THE Agent SHALL 在不重启进程的情况下按 `item_id` 对齐启停挂载点。
8. THE 采集项 SHALL 支持按 `agent_ids` 多机下发，语义与既有类型一致。

### Requirement 3: 网络连接与流量采集（P1）

**User Story:** AS 运维人员, I want 看到主机与容器之间的连接关系与流量, so that 无插桩应用也能进服务拓扑。

#### Acceptance Criteria

1. WHEN `ebpf_network` 采集项启用，THE Agent SHALL 采集 TCP 与 UDP 的连接建立、关闭、收发字节与连接存续时长。
2. THE 采集 SHALL 覆盖 `connect`、`accept`、`close` 与连接失败路径（`ECONNREFUSED`、`ETIMEDOUT`、`ENETUNREACH`）。
3. THE 记录维度 SHALL 为：`protocol`、`src_ip`、`src_port`、`dst_ip`、`dst_port`，并附加发起方 `pid`、`process`、`cgroup_id` 与由其反查的容器/Pod。
4. THE Agent SHALL 按 `bucket_secs` 聚合为 `EbpfEdge` 记录，字段与不变量见 `observability-data-model`。
5. THE `EbpfEdge.failures` 与 `failure_reason` SHALL 按同一桶内失败连接数与主因填写，主因取出现次数最多的原因，相同次数按原因字典序取小。
6. THE `EbpfEdge.latency_hist` SHALL 为连接存续时长的 log2 直方图，槽数由 `hist_slots` 控制。
7. THE 过滤 SHALL 生效：`cgroup_include/exclude`、`process_include/exclude`、`port_include/exclude`；`include_loopback=false` 时回环地址的连接不入库。
8. THE 采集 SHALL 只采集连接元数据与计数，不采集报文内容。
9. WHEN `record_id` 相同的边记录重复上报，THE `dataserver` SHALL 以幂等处理（同桶覆盖），不产生重复边。

### Requirement 4: 进程生命周期（P1）

**User Story:** AS 运维人员, I want 主机上进程的启动与退出可追踪, so that 能关联流量与进程变化。

#### Acceptance Criteria

1. WHEN `ebpf_process` 采集项启用，THE Agent SHALL 采集进程 `exec` 与 `exit`、以及 `fork` 的父子关系。
2. THE 记录字段 SHALL 包含：`event_type`（`exec`/`exit`/`fork`）、`pid`、`ppid`、`process_name`、`cmdline`、`cgroup_id`、`container_id`、发生时间。
3. THE `cmdline` SHALL 截断到 512 字节，超出部分丢弃并在 `labels` 标记 `cmdline_truncated=true`。
4. THE Agent SHALL 按桶聚合为指标（`ebpf_process_exec_total`、`ebpf_process_exit_total`）并上行 `metrics` 信封，维度含 `process_name`、`service`、`container_id`；不产生 `ebpf_edges` 记录。
5. WHEN `raw_events_enabled` 为 true，THE Agent SHALL 按 `raw_events_sample_ratio` 抽样上行 `data_type=ebpf` 的原始事件，字段与既有 `EbpfRecord` 兼容。
6. THE `exit` 事件的 `labels` SHALL 含 `exit_code` 与 `signal`（被信号终止时）。
7. THE 采集 SHALL 不受 `process_include/exclude` 之外的额外限制，默认全量；过滤命中时丢弃事件并计数。

### Requirement 5: TCP 异常（P1）

**User Story:** AS 运维人员, I want 重传与连接失败可量化, so that 能区分应用慢与网络差。

#### Acceptance Criteria

1. WHEN `ebpf_tcp` 采集项启用，THE Agent SHALL 采集 TCP 重传、RST、连接失败三类事件。
2. THE 指标 SHALL 为 `ebpf_tcp_retrans_total` 与 `ebpf_tcp_failures_total`，维度含 `src_service`、`dst_service`、`reason`（失败类）。
3. THE `reason` 取值 SHALL 限于：`refused`、`timeout`、`unreachable`、`reset`、`other`。
4. THE 采集 SHALL 只统计计数与时序，不采集序号、窗口等报文细节。
5. WHEN 同一桶内既有成功连接又有失败连接，THE Agent SHALL 分别计入 `connections` 与 `failures`，两者不互斥。
6. THE 重传计数 SHALL 按连接聚合后计入对应边的 `tcp_retrans` 字段与指标。

### Requirement 6: 文件与 syscall 延迟（P2）

**User Story:** AS 运维人员, I want 知道慢 IO 发生在哪个进程与路径, so that 能定位磁盘与文件系统瓶颈。

#### Acceptance Criteria

1. WHEN `ebpf_syscall` 采集项启用，THE Agent SHALL 采集 `openat`、`read`、`write`、`fsync` 的调用耗时与错误码。
2. THE 指标 SHALL 为 `ebpf_syscall_duration_micros`（`field_name` 为 `avg` 与 `p95`），维度含 `op`、`process_name`、`service`。
3. THE 错误计数 SHALL 聚合为 `ebpf_syscall_failures_total`，维度含 `op`、`errno`。
4. THE 路径类字段 SHALL 只在慢调用样本中保留，且路径长度截断到 256 字节；默认不上行全量路径。
5. THE 慢调用阈值 SHALL 由采集项 `slow_threshold_micros` 配置（缺省 100_000）；原始慢调用事件在 `raw_events_enabled` 为 true 时上行 `data_type=ebpf`。
6. THE 采集 SHALL 不改变被观测进程行为，不使用会阻塞目标进程的 hook（不使用 uprobe 替换返回值）。

### Requirement 7: DNS 延迟（P2）

**User Story:** AS 运维人员, I want 域名解析耗时可见, so that 能排除解析导致的偶发超时。

#### Acceptance Criteria

1. WHEN `ebpf_dns` 采集项启用，THE Agent SHALL 采集发往 53 端口的 UDP 与 TCP 请求及其响应的匹配耗时。
2. THE 指标 SHALL 为 `ebpf_dns_duration_micros`（`field_name` 为 `avg` 与 `p95`），维度含 `query_name`、`rcode`、`service`。
3. THE 请求与响应 SHALL 按 `(pid, transaction_id)` 匹配；超时未匹配的请求计入 `ebpf_dns_timeouts_total`。
4. THE `query_name` SHALL 从 DNS 报文的 question 段解析，不做缓存；解析失败时用 `unknown` 并计数。
5. THE 采集 SHALL 不修改或转发 DNS 报文。

### Requirement 8: CPU profile（P3）

**User Story:** AS 运维人员, I want 按服务看火焰图, so that 能定位 CPU 热点函数。

#### Acceptance Criteria

1. WHEN `ebpf_cpu_profile` 采集项启用，THE Agent SHALL 按 `sample_frequency_hz`（缺省 99）周期性采样用户态调用栈。
2. THE 采样 SHALL 记录：`pid`、`tid`、`process_name`、`container_id`、采样时间、栈帧地址列表（上限 `max_stack_depth`，缺省 64）。
3. THE 符号化 SHALL 在 Agent 侧完成：读取 `/proc/{pid}/maps` 定位模块与偏移，按 build-id 查找符号表；符号表缺失时保留地址并标记 `symbolized=false`。
4. THE Agent SHALL 按 `profile_interval_secs`（缺省 60）聚合，上行聚合后的折叠栈（folded stack）与样本计数，写入 `dataserver` 的 `FileStore` 或 sqlite（具体形态见设计），不上行逐样本记录。
5. THE 指标 SHALL 为 `ebpf_cpu_profile_samples_total`，维度含 `service`、`process_name`。
6. THE 采样开销 SHALL 受 `max_profiled_processes`（缺省 50）与目标进程过滤限制；超限进程不采样并计数。
7. IF 符号化所需文件不可读（无权限、容器内无符号表），THE Agent SHALL 保留地址栈并计数，不丢弃采样。
8. THE P3 SHALL 允许独立关闭，不影响 P1、P2 信号。

### Requirement 9: 内核态聚合与用户态差分

**User Story:** AS 运维人员, I want 采集开销可控, so that 打开 eBPF 不会拖慢业务。

#### Acceptance Criteria

1. THE 高频计数 SHALL 在内核态完成：使用 per-CPU map 累加，不按事件唤醒用户态。
2. THE 每 CPU 的 map 条目数 SHALL 有上限（缺省 16_384），达到上限时丢弃新键并计数，不覆盖已有键。
3. THE 用户态 SHALL 每 `flush_interval_secs` 读取一次 map 快照，与上一周期差分得到增量，差分后重置内核态计数。
4. THE 用户态差分 SHALL 处理 map 条目在周期内被淘汰的情况：无法确定增量时该项本周期不出数并计数。
5. THE eBPF 采集的 CPU 占用 SHALL 受 `max_cpu_percent`（缺省 5）限制：内核态程序使用 `bpf_ktime_get_ns` 做速率限制采样，用户态检测到超限时降采样并计数。
6. THE 采集 SHALL 不引入每秒超过 `max_events_per_sec`（缺省 50_000）的用户态唤醒次数；超限时丢弃并计数。
7. THE 环缓冲（ring buffer）的容量 SHALL 可配（缺省 256 KiB/信号），满时丢弃最旧事件并计数。

### Requirement 10: 上行模型

**User Story:** AS 平台开发者, I want 上行数据量固定且可预期, so that 采集规模可估算。

#### Acceptance Criteria

1. THE 边聚合 SHALL 以 `data_type=ebpf_edges` 上行，每条 `EbpfEdge` 为一个记录，`data_id` 为采集项 `item_id`。
2. THE 指标 SHALL 以 `data_type=metrics` 上行，measurement 与维度按 `observability-data-model` 命名表。
3. THE 原始事件 SHALL 以 `data_type=ebpf` 上行，仅在 `raw_events_enabled=true` 时启用，并受 `raw_events_sample_ratio` 抽样。
4. THE 上行 SHALL 复用既有批次缓冲、退避重试与直连 `dataserver` 逻辑。
5. THE 边聚合与指标上行 SHALL 不依赖 span 或 OTLP 链路，`apm-tracing` 未部署时各自独立可用。
6. WHEN 缓冲达到上限，THE Agent SHALL 按既有约定丢弃最旧批次并与 stderr 一行，含 `data_type` 与条数。
7. THE `record_id` 规则 SHALL 与共享模型一致：边为 `{agent_id}:{bucket_start_micros}:{src_ip}:{src_port}:{dst_ip}:{dst_port}:{protocol}`，指标走既有指标 `record_id` 规则。

### Requirement 11: 服务名反查与拓扑合并

**User Story:** AS 运维人员, I want eBPF 的 IP/端口数据能用服务名展示, so that 拓扑图与 APM 视图一致。

#### Acceptance Criteria

1. THE Agent SHALL 在边记录中填充 `src_pod`、`src_container_id`、`src_process`（由 `cgroup_id` 反查），服务名留空由 `dataserver` 反查。
2. THE `dataserver` SHALL 按 `observability-data-model` 的反查规则填充 `src_service`、`dst_service`，顺序为：静态映射 `apm_service_alias` → 端点表 `apm_service_endpoint` → `unknown-<ip>`。
3. THE Agent 侧无法得知静态映射；映射命中完全由 `dataserver` 完成。
4. WHEN 反查失败，THE `dataserver` SHALL 落 `unknown-<ip>`，并在前端拓扑图例中单列「未识别」，同时在 `/topology` 页提供「为该节点建立映射」快捷入口。
4. THE 边指标 SHALL 带 `source=ebpf`；`apm-tracing` 未启用时，拓扑页只展示 eBPF 边，不报错。
5. THE 同一逻辑边被两路同时观测时，SHALL 保留两条数据（不同 `source`），由查询侧合并，服务端不做去重。
6. THE 反查 SHALL 不影响写入延迟：反查结果缓存在内存中，未命中的查询也缓存负面结果 60 秒。

### Requirement 12: 资源限制与自我保护

**User Story:** AS 运维人员, I want eBPF 采集自身有硬上限, so that 不会因为观测把机器拖垮。

#### Acceptance Criteria

1. THE Agent SHALL 对每个 eBPF 采集项应用 `max_cpu_percent`、`max_events_per_sec`、map 条目上限与环缓冲容量四项限制。
2. THE 超限时 SHALL 优先降采样与丢弃新键，不阻塞被观测路径。
3. THE Agent SHALL 统计并输出每项限制的触发次数与被丢弃的数据量。
4. THE `dataserver` SHALL 统计并输出 eBPF 批次数、边记录数、非法记录数与限流丢弃数。
5. THE 统计 SHALL 通过既有自监控指标口暴露，供监控页展示。
6. WHEN 单个采集项持续超限超过 5 分钟，THE Agent SHALL 向标准错误输出一行并在链路页标记该采集项为「降级运行」。

### Requirement 13: 查询 API

**User Story:** AS 查询调用方, I want 边与事件可检索, so that 前端与 CLI 能定位问题主机。

#### Acceptance Criteria

1. THE `dataserver` SHALL 提供 `POST /v1/edges/search`，支持过滤 `from_ts`、`to_ts`、`src_service`、`dst_service`、`src_ip`、`dst_ip`、`dst_port`、`protocol`、`source`、`agent_id`、`min_requests`、`limit`（默认 100，上限 1000）、`offset`。
2. THE 响应 SHALL 含 `edges` 数组与 `total`；每条含 `bucket_ts`、`src_service`、`dst_service`、`src_ip`、`dst_ip`、`dst_port`、`protocol`、`connections`、`failures`、`bytes_sent`、`bytes_recv`、`duration_avg_micros`、`tcp_retrans`、`source`。
3. WHEN 查询未指定 `source`，THE `dataserver` SHALL 合并 `source=ebpf` 与 `source=otlp` 的边并按 `(bucket_ts, src_service, dst_service, protocol)` 汇总。
4. THE `dataserver` SHALL 提供 `POST /v1/ebpf/events/search`，复用日志检索接口形态，过滤 `data_type=ebpf`，支持 `event_type`、`pid`、`process_name`、关键词与时间范围。
5. IF 过滤时间范围非法（`from_ts` 晚于 `to_ts`），THE `dataserver` SHALL 返回 HTTP 400 与 `code=invalid_argument`。
6. THE 错误响应 SHALL 沿用既有稳定错误码：`invalid_argument`、`unavailable`、`query_failed`、`not_found`。
7. THE `dpc` SHALL 增加只读子命令 `edges` 与 `ebpf-events`，通过 HTTP 调用上述接口并打印 JSON。

### Requirement 14: 保留期与清理

**User Story:** AS 运维人员, I want eBPF 数据按周期清理, so that 磁盘占用可预期。

#### Acceptance Criteria

1. THE 边聚合与原始事件的保留期 SHALL 取自采集项 `storage_json.retention_days`，缺省 3 天。
2. THE `dataserver` SHALL 每小时清理 `ebpf_edges` 中 `bucket_ts` 早于保留期的行，以及 `data_type=ebpf` 的明细（按 `data_id` 与时间上界循环删除直到返回 0）。
3. WHEN 采集项被删除，THE `dataserver` SHALL 复用既有 `retain/{item_id}` 机制到期清理。
4. THE 聚合指标（`ebpf_*` 与 `apm_edge_*{source="ebpf"}`）的清理 SHALL 依赖同期交付的 `/.monkeycode/specs/dataplane-ts-retention/`：全局保留期取 `ts_retention_days`（默认 30 天），短于全局窗口的采集项由该 spec 的清理任务按 `item_id` matcher 删除。
5. THE CPU profile 聚合产物（P3）的保留期 SHALL 独立配置为 `profile_retention_days`（缺省 7），按产物创建时间清理。

### Requirement 15: 前端视图

**User Story:** AS 运维人员, I want 在数据面页面查看 eBPF 数据, so that 与 APM 视图在同一处闭环。

#### Acceptance Criteria

1. THE 前端 SHALL 扩展 `@vectorman/dataplane`，不新增独立应用。
2. THE 路由 SHALL 新增 `/ebpf`（事件与边）与 `/ebpf/profile`（P3 火焰图，阶段三再交付视图占位）。
3. THE `/ebpf` 页 SHALL 提供两个视图切换：「边」调用 `POST /v1/edges/search`，展示时间、源/目标服务、IP:端口、协议、连接数、失败数、双向字节、平均时长、重传数、来源；「事件」调用 `POST /v1/ebpf/events/search`，展示时间、事件类型、进程、容器、消息。
4. THE `/ebpf` 页 SHALL 支持按 `agent_id`、`source`、服务、端口与时间范围过滤，并支持手动刷新。
5. THE `/topology` 页 SHALL 通过 `source` 切换展示 eBPF 边与 OTLP 边的合并结果或单路结果。
6. THE `/ebpf` 页的边行 SHALL 提供「查看该边 trace」入口，跳转 `/traces` 并带 `src_service`、`dst_service` 与时间范围。
7. THE `/ebpf` 页 SHALL 展示该 Agent 的 eBPF 前置校验状态与最近失败原因（Requirement 1 第 5 条）。
8. WHEN 采集项处于降级运行状态，THE 页面 SHALL 在对应行展示降级标记与超限类型。
9. THE 页面数据刷新 SHALL 由运维手动触发，不引入定时器。

### Requirement 16: 错误处理与降级

**User Story:** AS 运维人员, I want 环境不满足时明确降级, so that 采集失败不影响其它能力。

#### Acceptance Criteria

1. WHEN 前置校验失败，THE eBPF 采集项 SHALL 标记为「不可用」而不影响指标、日志、APM 采集。
2. WHEN 内核态程序加载失败或运行中被内核卸载，THE Agent SHALL 卸载残留 map 与挂载点并按退避重试。
3. WHEN 环缓冲满或 map 满，THE Agent SHALL 丢新数据并计数，不阻塞目标进程。
4. WHEN 符号化失败（P3），THE Agent SHALL 保留地址栈并标记 `symbolized=false`。
5. WHEN 权限不足（前置校验失败），THE Agent SHALL 在日志输出 warn 一行并禁用该采集项，不阻止 Agent 启动，也不影响其它采集项。
5. WHEN 某个 eBPF 采集项不可用，THE 链路页 SHALL 显示不可用状态与原因，其余采集项状态独立。
6. IF `dataserver` 不支持 `data_type=ebpf_edges`（版本不匹配），THE Agent SHALL 收到 `invalid_argument` 并确认该批（不无限重试），同时向标准错误输出一行版本不匹配提示。
7. THE eBPF 采集的失败 SHALL 不改变既有采集器（`metrics_host`、`log_file`、`log_k8s_stdout`）、作业与文件传输的行为。

### Requirement 17: 范围边界与阶段

**User Story:** AS 开发者, I want 明确阶段划分与依赖, so that 实现顺序有据可依。

#### Acceptance Criteria

1. THE P1 交付范围 SHALL 覆盖 `ebpf_network`、`ebpf_process`、`ebpf_tcp` 三类采集项与对应指标的端到端链路。
2. THE P2 交付范围 SHALL 覆盖 `ebpf_syscall`、`ebpf_dns`。
3. THE P3 交付范围 SHALL 覆盖 `ebpf_cpu_profile` 与火焰图视图。
4. THE 本 feature SHALL 依赖 `LogStore` 索引版本 v2（原始事件按 `data_id` 检索与清理）与 sqlite 观测表（`ebpf_edges`、`obs_schema_meta`、`apm_service_endpoint`、`apm_service_alias`）。
5. THE 聚合指标保留 SHALL 依赖同期交付的 `/.monkeycode/specs/dataplane-ts-retention/`，不再列为外部依赖。
6. THE 实现顺序 SHALL 在 `apm-tracing` 之后（见两份 spec 的 Introduction）。
6. THE Agent 构建 SHALL 保持现有 musl 静态链接方式；eBPF 内核态程序的构建产物 SHALL 以字节形式随二进制打包，不在部署期编译。
7. THE 下列能力 SHALL 列入后续范围：L7 协议解析、安全审计与阻断、跨主机调用链拼接、容器运行时集成（OCI hook）、内核模块回退方案、P3 之外的持续性能分析。
