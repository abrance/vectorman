# 任务清单：可观测加固与真集群验证

> 对应需求 `.monkeycode/specs/observability-hardening/requirements.md`，
> 设计 `.monkeycode/specs/observability-hardening/design.md`。全部未开工，用户确认后按序执行。
>
> 说明：本 feature 不新增采集能力，因此没有「内核态程序」阶段；主体是**清单 + 验证执行 + 文档对齐**。

## 1. K8s 部署清单（对应需求 1、9）

- [ ] 1.1 新增 `packaging/deploy/k8s/gse-agent-daemonset.yaml`：Namespace / ServiceAccount /
      ClusterRole(pods get,list) / ClusterRoleBinding / ConfigMap / DaemonSet 六个对象，
      按设计的字段取值（`hostPID: true`、BTF 与 tracing 只读 hostPath、特权、Downward API 取 `spec.nodeName`
      作 `agent_id`、`token` 走 Secret、`emptyDir` 作上行队列目录）
- [ ] 1.2 新增 `packaging/deploy/k8s/README.md`：镜像怎么来（用安装包二进制 + `debian:12-slim` 自建）、
      台账里怎么预登记各节点 Agent、如何 `kubectl apply`、如何用 `nodeSelector` 灰度、
      以及「停用采集项/删 DaemonSet」两种回滚方式
- [ ] 1.3 `kubectl apply --dry-run=client -f` 本地校验通过（清单语法与 API 版本）
- [ ] 1.4 打包脚本 `packaging/build-package.sh` 把 `deploy/k8s/` 一并打入安装包（保持「随包分发」的一致性）

- **检查点 A**：清单能 `--dry-run` 通过；打包产物里含 `deploy/k8s/`；既有 Rust/前端测试保持全绿。

## 2. 单机回归清单（对应需求 5、9）

- [ ] 2.1 把设计里的 V12 三条命令固化成可复制的清单（写进 `ebpf-observability/todo.md` 附录或
      `packaging/deploy/k8s/README.md` 的「单机先验」一节）
- [ ] 2.2 执行限流回归：`checkpoint --kind ebpf_syscall` 出现限流丢弃日志且
      `agent_ebpf_rate_limited_total > 0`
- [ ] 2.3 执行缓冲淘汰回归：指向不可达 ingest 地址持续产数据，出现 `drop oldest batch` 日志且
      `agent_ebpf_buffer_dropped_total > 0`
- [ ] 2.4 执行「drain 即增量」回归：专用进程每轮稳定调用数，四轮计数与真实值一致
      （防止少计 bug 回归）

- **检查点 B**：三条回归结论（命令 + 输出要点）落盘到 `ebpf-observability/todo.md`。

## 3. 集群验证执行（对应需求 1–4、9）

- [ ] 3.1 环境准备：确认节点内核 ≥5.8 与 `/sys/kernel/btf/vmlinux` 存在；确认集群外
      gse-server 与 dataserver 可从节点访问；构建并推送验证用镜像
- [ ] 3.2 预登记台账：为每个节点登记 Agent（`agent_id = nodeName`，token 写入 Secret）
- [ ] 3.3 灰度单节点：`kubectl apply` 后只调度到一台，按设计观察 10–30 分钟
      （日志挂载数、`bpftool prog list`、`dmesg` 无 verifier/kprobe/soft lockup、节点 CPU 无异常）
- [ ] 3.4 下发四类 eBPF 采集项（前端「采集链路」页或 API），确认 `/v1/ebpf/capability` 全部 `available=true`
- [ ] 3.5 验证 V5/V6：`src_container_id` 为真实容器 ID、`src_pod` 为**真实 Pod 名**、
      服务名归一能命中按 Pod 名登记的端点
- [ ] 3.6 验证 V7：`/v1/streams` 出现三类流；`/v1/edges/search` 有量；Prom 有 `ebpf_edge_*` 与
      `ebpf_process_*`；`/ebpf` 与 `/topology` 页可见
- [ ] 3.7 容量与丢弃被动观测（V8）：记录实测规模（活跃连接数、Agent 数、事件率）与四项丢弃计数、
      采集侧与接入侧对账差额
- [ ] 3.8 铺开全部节点，重复 3.4–3.7 的抽查项
- [ ] 3.9 验证 V9/V10：Pod 重建后重新挂载且不重复计数；停用采集项/删 DaemonSet 后
      `bpftool prog list` 与 `bpftool map list` 无残留
- [ ] 3.10 验证 V11：在没有 BTF 或无特权的环境启动，确认只降级该项且原因可见

- **检查点 C**：V1–V11 逐条结论（命令 + 期望 vs 实测 + 证据）写入
  `ebpf-observability/todo.md` 的「验证结论」一节；未通过项单列并给出影响面。

## 4. 规格与实现对齐（对应需求 6）

- [x] 4.1 （随设计 PR 一并完成）`ebpf-observability/requirements.md` 的 9.3 改为「每次 drain 的结果即本周期增量；
      内核侧已写零复位」，并注明修正原因（避免再次被照抄成差分实现）
- [x] 4.2 （随设计 PR 一并完成）`ebpf-observability/design.md` 的图表选型由 `echarts` 改为**手绘 SVG**；
      删除或改正「`/ebpf/profile` 本期占位路由」这类与实现不符的描述
- [ ] 4.3 DNS 冲突收口：在 `ebpf-observability/todo.md` 明确二选一（扩到 `send`/`recv` ↔ 或降级覆盖形态），
      并把结论文档化（未收口前不得开始 DNS 实现）
- [ ] 4.4 复查 `ebpf-observability/todo.md` 的「已定选型」表与 `tasklist.md` 的勾选状态一致
      （P2 前半已完成、P2 后半与 P3 未开工）

- **检查点 D**：`grep -rn "echarts" .monkeycode/specs/ebpf-observability/` 无残留；
  需求 9.3 与实现口径一致；DNS 冲突有明确结论；全部 Rust/前端测试与 CI 保持绿。

## 5. 明确不做的收口（对应需求 7）

- [ ] 5.1 删除 `.github/workflows/helm-ci.yml`（`charts/**` 已不存在，属悬空配置），
      在 `todo.md` 记录「charts 排 v1.3、届时一并恢复该工作流」
- [ ] 5.2 在 `ebpf-observability/todo.md` 记明「前端浏览器 e2e 本版不做」及其覆盖边界
- [ ] 5.3 在 `todo.md` 记明 `ebpf_dns` 与 `ebpf_cpu_profile` 排在本 feature 之后（各自先出设计）

- **检查点 E**：`.github/workflows/` 下无悬空工作流；三项「不做」在文档中有结论与理由；
  CI 全绿且无新增作业耗时异常。

## 6. 文档与运维入口（对应需求 1、4、9）

- [x] 6.1 （随设计 PR 一并完成）README 的 eBPF 章节补三条：**默认不下发采集项就不加载任何 BPF 程序**；
      上线建议（先单节点灰度 → 观察丢弃计数与 `dmesg` → 铺开）；回滚方式（停用采集项/停 Agent）；
      指向 `observability-hardening/design.md` 的风险与回滚清单
- [ ] 6.2 `ebpf-observability/todo.md` 增「验证结论」一节（记录实测规模、四项丢弃计数、对账结果、遗留风险）
- [ ] 6.3 若验证中发现实现缺陷：单开修复 PR（不在本 feature 里夹带功能改动），
      并在 `todo.md` 记录缺陷与修法

- **检查点 F**：文档与实测一致；无未决占位；交付说明（PR 描述）含验证结论摘要与遗留风险。
