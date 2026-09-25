# 需求：可观测加固与真集群验证（observability-hardening）

## Introduction

v1.1.0 已交付 APM 全链路与 eBPF 四类采集项（`ebpf_network` / `ebpf_tcp` / `ebpf_process` /
`ebpf_syscall`），但存在三类未闭合的缺口：

1. **真机验证缺口**：eBPF 与 APM 的全部验证都在单机（本机 sudo + 检查点工具）完成，
   **没有在真实 Kubernetes 集群上验证过部署形态、容器/Pod 反查与多 Agent 场景**；
   `src_pod` 目前只有 Pod uid（k8s API 的 uid→name 反查逻辑已实现，但从未在真集群里跑过）。
2. **容量口径缺口**：v1.1.0 修掉了「上行缓冲丢数据**不可见**」（`agent_ebpf_buffer_dropped_total`），
   但**没有给出「什么算不丢数据」的验收口径**，也没有在真实负载下观察过丢弃计数。
3. **规格与实现的偏差**：需求 9.3 仍写着「与上一周期差分」的旧口径（实现已改为「每次 drain 即增量」）；
   设计里的图表选型写着 `echarts`（实现改为手写 SVG）；`/ebpf/profile` 被写成「本期占位路由」但实际不存在；
   DNS 的挂载点与覆盖面之间存在一处冲突（见 Requirement 6）。

本 feature **不加新能力**，目标是：把「已验证的范围」扩大到真集群、把容量口径写成可验收的判据、
把规格与实现对齐，并明确记录**本版不做**的事项，避免后续反复讨论。

## Glossary

| 术语 | 含义 |
| --- | --- |
| DaemonSet 形态 Agent | gse-agent 以特权容器形式在每个节点跑一份，通过 hostPath 看宿主机的 `/sys/kernel/btf`、tracing 与 `/proc` |
| 被动观测 | 不主动制造压力与异常，只观察真实业务负载下的既有指标与丢弃计数 |
| 丢弃计数 | `agent_ebpf_buffer_dropped_total`、`agent_ebpf_rate_limited_total`、`agent_ebpf_map_overflow_dropped_total`、`agent_ebpf_read_errors_total` 四项之和为 0 即「未丢数据」 |
| 对账 | 采集侧上报的记录数与 dataserver 侧 `dataserver_ingest_records_total{data_type=...}` 一致（差额不超过在途批次） |
| 最小清单 | 直接用 `kubectl apply -f` 的 YAML（不含模板引擎），与 v1.3 的 helm charts 区分 |

## Requirements

### Requirement 1: 集群内以 DaemonSet 形态部署 Agent

**User Story:** AS 运维人员, I want 用 k8s 原生方式在每个节点部署 Agent, so that 采集不依赖手工登机安装。

#### Acceptance Criteria

1. THE 仓库 SHALL 提供一份**最小 K8s 清单**（`packaging/deploy/k8s/gse-agent-daemonset.yaml`），
   用 `kubectl apply -f` 即可部署 gse-agent（不引入 helm）。
2. THE 清单 SHALL 包含：`hostPID: true`（共享宿主机 PID 命名空间后，容器内 `/proc` 即为宿主机视图，
   容器/Pod 反查与进程名读取依赖它 —— **无需**再 hostPath 挂载 `/proc`）、
   hostPath 只读挂载 `/sys/kernel/btf` 与 `/sys/kernel/tracing`（BTF 与 tracepoint `format` 读取依赖它们）、
   特权或 `CAP_BPF`+`CAP_PERFMON`、以及 agent 配置的 ConfigMap 挂载。
2a. THE 清单 SHALL 包含 RBAC（ServiceAccount + ClusterRole + ClusterRoleBinding），
   授予 `get/list` `pods`（跨命名空间，供 Pod 名反查），凭据走 in-cluster ServiceAccount。
3. THE `agent_id` SHALL 按节点稳定（用 Downward API 的 `spec.nodeName`），避免重启后变成新 Agent。
4. THE Agent 配置 SHALL 通过 ConfigMap 提供（`server_addr` 指向 gse-server），
   `agent_id`/`token` 通过 Secret 或预登记台账提供。
5. IF 节点不满足 eBPF 前置条件（内核 < 5.8 / 无 BTF / 无权限），THE Agent SHALL 仍正常启动并纳管，
   仅把 eBPF 该项标记为不可用。

### Requirement 2: 集群内 eBPF 采集可用性

**User Story:** AS 运维人员, I want 在集群节点上确认 eBPF 采集真的能用, so that 边界主机不再靠猜。

#### Acceptance Criteria

1. THE 验证 SHALL 覆盖四类采集项（`ebpf_network`/`ebpf_tcp`/`ebpf_process`/`ebpf_syscall`）在集群节点上的
   加载与挂载成功（`agent_ebpf_capability` 指标 `available=1`，且 `/ebpf` 页能力状态显示可用与内核版本）。
2. THE 验证 SHALL 确认内核态程序在 Pod 重启后被正确卸载，`bpftool prog list` 无残留。
3. THE 验证 SHALL 确认重启后数据不重复（同 `record_id` 覆盖写语义生效）。

### Requirement 3: 容器与 Pod 反查在真集群可用

**User Story:** AS 运维人员, I want 边记录与服务名归一带上真实 Pod, so that 拓扑能按业务单元收敛。

#### Acceptance Criteria

1. WHEN Agent 在 k8s 节点上采集，THE 边记录 SHALL 携带真实 `container_id` 与 **Pod 名**（`src_pod`），
   而不是 `pod<uid>`。
2. THE Agent SHALL 通过 k8s API 反查 `uid → Pod 名`（复用 `crates/gse-agent-core/src/collect/k8s.rs`
   与 `kubeconfig.rs`，命名空间为空时列全部命名空间），索引按 TTL 缓存。
3. THE 服务名归一 SHALL 能在真集群里命中「按 Pod 名登记的端点表」这一层（此前因只有 uid 而永远落空）。
4. IF k8s 凭据不可用或 API 不可达，THE Agent SHALL 退回 uid（不报错、不影响其它采集项）并留下日志。
5. THE k8s 客户端 SHALL 支持**集群自签 CA**：k3s/kubeadm 的 apiserver 用集群自签证书，
   而客户端的默认根是公有 CA，因此凭证 SHALL 带上 CA（in-cluster 的 `ca.crt`，
   或 kubeconfig 的 `certificate-authority` / `certificate-authority-data`）并**只对该客户端生效**。
   _（2026-09-25 实施时发现并修复：缺这一步 Pod 名反查会以 TLS 校验失败告终，
   `src_pod` 静默退化成 uid —— 见 `todo.md` 与 `kubeconfig.rs::tls_config`。）_

### Requirement 4: 容量与丢弃的可验收口径（被动观测）

**User Story:** AS 运维人员, I want 知道「打开 eBPF 会不会丢数据」有明确判据, so that 不需要靠感觉。

#### Acceptance Criteria

1. THE 设计 SHALL 给出「不丢数据」的可验收判据：四项丢弃计数为 0；若不为 0，SHALL 要求给出原因与规模。
2. THE 验证 SHALL 在真实负载下观察并记录：节点活跃连接数量级、Agent 数、
   `agent_ebpf_flushes_total`/`agent_ebpf_edges_total`/`agent_ebpf_metric_points_total` 与
   `dataserver_ingest_records_total{data_type="ebpf_edges"|"ebpf"|"metrics"}` 的**对账差额**。
3. THE 目标量级 SHALL 为「单节点 1 万活跃连接、5 个 Agent」；达不到达该量级时 SHALL 如实记录实测值，
   **不得**通过造压达成（本版影响面为轻量验证）。
4. THE 验证结论 SHALL 落盘到 `ebpf-observability/todo.md`（实测规模、丢弃计数、对账结果、遗留风险）。

### Requirement 5: 限流与丢弃的正确性在单机验证

**User Story:** AS 运维人员, I want 限流与缓冲边界行为可复现, so that 出问题时知道看哪个指标。

#### Acceptance Criteria

1. THE 限流（内核态令牌桶）与缓冲淘汰（上行队列）的正确性 SHALL 在**单机非业务环境**验证
   （用 `crates/gse-agent-ebpf/examples/checkpoint.rs` 与可控流量），不在集群内造压。
2. THE 验证 SHALL 覆盖：`agent_ebpf_rate_limited_total > 0` 时事件确实被丢弃且计数可见；
   上行阻塞时 `agent_ebpf_buffer_dropped_total > 0` 且日志有对应记录。
3. THE 上述用例 SHALL 以命令 + 期望输出的形式写进设计文档，作为可重复执行的回归清单。

### Requirement 6: 规格与实现对齐

**User Story:** AS 后续维护者, I want 规格不骗人, so that 不会照着一份过期的需求去实现。

#### Acceptance Criteria

1. THE `ebpf-observability/requirements.md` 的 9.3 SHALL 修正为「每次 drain 的结果即本周期增量，
   差分后内核侧已复位」（与实现一致），并注明该口径的修正原因。
2. THE `ebpf-observability/design.md` 的图表选型 SHALL 修正为**手绘 SVG**（与实现一致），
   且 SHALL 删除或改正「`/ebpf/profile` 本期占位路由」这一与实现不符的描述。
3. THE DNS 挂载点与覆盖面的**冲突** SHALL 在设计评审时收口。**已收口（2026-09-25）**：
   选择②，覆盖面降级为「仅 `sendto`/`recvfrom` 形态」，TCP 53 与已 `connect()` 的 `send`/`recv` 形态不在范围；
   需求 7.1 与 `todo.md` 已同步（含真实影响与「实现前用数据核对形态比例」的前置）。
   未完成该数据核对前 SHALL NOT 开始 DNS 实现。
4. THE DNS 与 CPU profile 的已定选型（内核态解析域名、per-tid 采样、只做 ELF 符号等）
   SHALL 记录在 `ebpf-observability/todo.md`，供其实施时直接引用。

### Requirement 7: 明确本版不做的事项

**User Story:** AS 决策者, I want 不做的事也写清楚, so that 不会被反复提起。

#### Acceptance Criteria

1. THE 前端真浏览器 e2e（Playwright）SHALL NOT 在本版引入；现有「jsdom 渲染 + 真实接口」的验证
   SHALL 视为本版目标态，其覆盖边界（CSS/布局/真实事件循环）SHALL 记录在已知限制中。
2. THE helm charts SHALL 排到 v1.3；本版 SHALL 删除 `.github/workflows/helm-ci.yml`
   （其 `charts/**` 路径过滤已无对应目录，属悬空配置）。
3. THE `ebpf_dns` 与 `ebpf_cpu_profile` SHALL 排在本 feature 之后（各自先出设计）。

### Requirement 8: 范围边界与前置依赖

**User Story:** AS 实施者, I want 知道前置条件, so that 验证不会卡在环境上。

#### Acceptance Criteria

1. THE 验证环境 SHALL 为**自建 Kubernetes 集群**（kubeadm 等），节点内核 **≥ 5.8** 且带 BTF。
2. THE 验证 SHALL 需要：节点可用特权容器、gse-server 与 dataserver 从集群内可达
   （本版用安装在集群外的安装包实例，不要求 Ingress/Service 暴露）。
3. THE 验证影响面 SHALL 限于轻量：只部署 Agent、下发采集项、观察指标与页面，不制造压力与异常。
4. THE 本 feature SHALL NOT 改变任一已交付采集项的数据模型与指标命名（只做验证、口径与文档对齐）。

### Requirement 9: 内核态程序的上线安全与回滚

**User Story:** AS 运维人员, I want 明确 eBPF 会不会把主机搞崩、出事怎么退, so that 敢在业务节点上开。

#### Acceptance Criteria

1. THE 设计 SHALL 说明加载条件：内核态程序**只在对应 `ebpf_*` 采集项被下发时**才加载与挂载；
   未下发任何 eBPF 采集项的部署**不加载任何 BPF 程序**（默认零侵入）。
2. THE 设计 SHALL 给出「内核态风险的现实边界」并区分严重度：
   ① verifier 拒绝 → **不可用**（只降级该项，本仓库已是此行为）；
   ② 性能开销（挂在热路径上）→ **真实且最常见的风险**，缓解手段与观测项须写明；
   ③ 内核自身缺陷（verifier/kprobe 引起）→ 概率极低但非零，须给出灰度与回滚手段。
3. THE 验证 SHALL 覆盖卸载：停用采集项或停止 Agent 后，`bpftool prog list` 与 `bpftool map list`
   **无残留**（此前已在单机验证，本 feature 要求在集群节点上再确认一次）。
4. THE 上线流程 SHALL 为灰度：先单节点 → 观察（`dmesg` 无 `verifier`/`kprobe`/`soft lockup` 报错、
   节点 CPU 未见异常抬升、四项丢弃计数为 0）→ 再铺开全部节点。
5. THE 回滚 SHALL 只需**停用采集项或停止 Agent**（`Ebpf` drop 即 detach link 并删除 map），
   不需要改内核、不需要重启节点。
