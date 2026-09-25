# eBPF 可观测性：遗留项清单（TODO）

本文档记录 `ebpf-observability` 交付后**仍未完成或未验证**的事项。设计基线见同目录
`requirements.md` / `design.md` / `tasklist.md`；这里只写「还欠什么、为什么欠、怎么补、怎么验收」。

状态日期：2026-09-25。P1 主线已在 main 上可用，且做过一次本机端到端真跑（见下）。

## 一、闭环状态：逐跳核对

数据链路每一跳的**实现**与**验证**是两件事。下表区分「已实现且实测」与「已实现但未实测」，
避免把「代码在」误当成「能跑」。

| # | 跳 | 实现 | 验证方式 | 状态 |
| --- | --- | --- | --- | --- |
| 1 | 内核态采集（network/tcp/process） | `crates/gse-ebpf-programs` + `packaging/ebpf/*.o` | 本机 sudo 加载挂载并读快照 | ✅ 实测（network 610 连接 / tcp 9 重传 / process 1857 事件） |
| 2 | 运行期参数（BTF/tracepoint 偏移/状态常量） | `gse-agent-ebpf` 的 `btf`/`tracepoint_format`/`cfg` | 真实 `/sys/kernel/btf/vmlinux` + 打印 CFG | ✅ 实测（偏移与 6.1 一致） |
| 3 | 用户态差分、过滤、边记录组装 | `aggregate` + `run_loop` | 单测 + 真跑差分 | ✅ 实测（5877 条边记录） |
| 4 | 容器/Pod 反查（`/proc/<pid>/cgroup`） | `cgroup` + `process_context_by_pid` | 单测 + 真跑打印上下文 | ⚠️ 部分：宿主机进程显示 `host`；**容器形态只有单测覆盖**（本机无容器） |
| 5 | Agent 采集项装配（preflight/卸载/退避/限流） | `collect/ebpf.rs` + `preflight`/`loader`/`backoff` | 单测 + `ebpf-programs` 作业 | ⚠️ 采集项**经 GSE 下发**这一跳未端到端跑过（见 TODO-1） |
| 6 | 上行信封与接入校验 | `dataplane-ingest`（`ebpf_edges`/`ebpf`/`metrics`） | 本机 POST `/v1/ingest` | ✅ 实测（三类信封均 `ok`） |
| 7 | 服务名反查与落库 | `dataplane-apm/ebpf_edge.rs`（schema v3） | 本机查询 `ebpf_edges` | ✅ 实测（未识别归一为 `unknown-<ip>`） |
| 8 | 查询接口 | `/v1/edges/search`（含合并）、`/v1/ebpf/events/search`、`/v1/ebpf/capability` | 本机 curl | ✅ 实测（588 行 / 38 条合并边 / 事件带正确时间戳 / 能力 `available=true`） |
| 9 | 指标派生（边与 TCP） | `dataplane-apm/ebpf_metrics.rs`（60 秒滞后窗口 + 游标） | 本机 Prom 查询 | ✅ 实测（`ebpf_edge_connections_total` 164 序列、`apm_edge_requests_total{source=ebpf}` 32 序列） |
| 10 | 进程指标与能力状态入时序库 | Agent 侧产出 → 通用 metrics 路径 | 本机 Prom 查询 | ✅ 实测（`ebpf_process_{exec,exit,fork}_total` 192/233/104 序列、`agent_ebpf_capability=1`） |
| 11 | 保留期清理（`ebpf_edges` 分批删 + `retain/`） | `dataplane-apm/ebpf_retention.rs` + `dataserver/cleanup.rs` | 单测（分批/只删指定项/到期清空） | ⚠️ 单测通过，**未在真实数据上跑过一整轮保存期** |
| 12 | 前端 `/ebpf` 页（边/事件/能力状态） | `apps/dataplane` | 单测 + tsc + build | ⚠️ **无浏览器实跑**（见 TODO-2） |
| 13 | CLI（`dpc edges` / `ebpf-events` / `ebpf-capability`） | `bins/dpc` | 单测（请求体拼装） | ⚠️ 未对真实 dataserver 跑过命令 |
| 14 | 构建链（`.o` 产出、入库、CI 断言） | `scripts/build-ebpf.sh` + `packaging/ebpf/*.o` + CI 手动作业 | CI 实跑 | ✅ 实测（段与 map 数量断言通过） |

结论：**数据面主线（1→11）已闭环并实测**；未闭环的是「GSE 下发」这一跳、前端实跑、
容器形态反查、以及若干边界能力（下面按优先级列）。

## 二、遗留项

### TODO-1（高）GSE 下发采集项这一跳未端到端验证

- **现状**：`collect/ebpf.rs` 的入口是 `spawn_collector` 按 `item.kind` 分发；本机验证用的是
  `--example checkpoint`，它复用**同一套用户态代码**（preflight、加载挂载、差分、边记录、上报，
  连记录构造函数都相同），但由手工启动，没有经过 GSE 下发。
- **为什么欠**：跑通需要同时起 `gse-server`（台账 + 采集项下发）+ `gse-agent`（注册、心跳、拉采集项）
  + `dataserver`（注册为数据面并被 GSE 选路），属于环境装配成本，而不是缺代码。
- **怎么补**：在测试环境按 `packaging/` 的部署方式起三件套，用 `POST /api/gse/collect-items`
  下发 `ebpf_network` 采集项（参考 `dataserver/tests/e2e.rs` 的台账装配思路），
  然后在目标机制造流量并查 `/v1/edges/search`。
- **验收**：采集项下发后 Agent 日志出现 `已挂载程序：[...]`；`/v1/streams` 有该 `item_id`；
  `agent_ebpf_capability` 值为 1；停用采集项后内核态探针消失（`bpftool prog list` 无残留）。
- **依赖**：目标机需 root 或 `CAP_BPF`+`CAP_PERFMON`，且内核 ≥ 5.8 + BTF。

### TODO-2（高）前端 `/ebpf` 页只做了单测，没有浏览器实跑

- **现状**：`App.test.tsx` 覆盖了能力状态卡片、边表字段、未识别服务跳转、事件视图提示；
  `tsc --noEmit`、`build:dataplane` 通过。
- **为什么欠**：单测用 fake HTTP，验证不了真实接口形状与渲染细节（例如长字段换行、
  `field=avg|p95` 曲线的真实数据、能力状态的失败文案在真实数据下的展示）。
- **怎么补**：用 Playwright 对本地 dataserver（可用本文档附录的命令起）跑一遍：
  打开 `/ebpf` → 断言能力状态卡片、边表出现真实行 → 切到事件视图 → 断言有事件与时间格式。
- **验收**：截图 + 断言通过；发现的前后端字段不一致问题修掉（本轮已修两个同类问题，见 PR #60）。

### TODO-3（中）进程指标的 `service` 维度未补齐（5.9 剩余部分）—— ✅ 已完成

- **原状**：`ebpf_process_*` 带 `pid`/`cgroup_id`/`process_name`/`container_id`/`pod_uid`；
  共享模型要求 `service`/`container_id`，`service` 仍缺（dataserver 侧未填）。
- **已完成**：给 `data_type=metrics` 增加与 `TraceSink`/`EdgeSink` 同形的 `MetricSink` 钩子
  （新文件 `crates/dataplane-ingest/src/metric.rs`），`ApmSink` 实现它并用 `AliasCache`
  按 `process_name`/`process_prefix` 归一服务名。三个出口在模块内打包成私有 `Sinks` 结构传递
  （否则 `apply_one` 参数超 clippy 阈值）。**sink 不返回错误**：补维度是尽力而为，失败不能丢指标；
  未命中填 `unknown-<process_name>`（与边记录 `unknown-<ip>` 同约定）。
- **实测**（本机 dataserver + 真采集）：建映射 `true→coreutils`、`sleep→sleeper` 后上报，
  Prom 查询 `ebpf_process_exec_total{service="coreutils"}` 命中 58 条序列（`process_name="true"`）、
  `{service="sleeper"}` 59 条（`process_name="sleep"`）；未映射的进程为 `unknown-<进程名>`
  （如 `unknown-git-submodule`）。`cpu_usage` 等非进程指标原样落库（单测断言不受影响）。

### TODO-4（中）Pod 名反查（k8s）—— ✅ 已完成（未在真实集群验证）

- **原状**：`src_pod` 里放的是 **Pod uid**（`pod<uid>` 只能解出 uid），而 dataserver 的端点表按
  Pod **名**登记，因此「按 Pod 名命中端点表」这一层永远命中不了。
- **已完成**：`cgroup::ProcessResolver` 支持注入 `PodNameLoader`（一次全量 `uid → name`），
  Agent 侧复用采集项里既有的 `namespace`/`kubeconfig`（与 `log_k8s_stdout` 同一份配置，**不新增配置面**），
  用 `k8s::list_pod_name_index` 拉索引。要点：只遇到 Pod uid 且索引过期时才拉（宿主机进程不触发）、
  同一 uid 复用同一份索引、失败不带崩采集也不清空旧索引、凭据探不到就整体关闭反查、
  名字拿不到时 `src_pod` 退回 uid（**与未启用时行为一致，不是回归**）。`ContainerInfo::pod_label()`
  是这一退回语义的唯一出口（loader 与检查点工具共用）。
- **验证**：新增 5 个用例 —— 反查器注入索引（命中 / 复用索引只拉一次 / 失败退回 uid / 宿主机不触发）、
  `list_pod_name_index` 全链路（假 apiserver + 临时 kubeconfig）、`pod_name_loader` 有/无凭据两种状态。
  本机真跑回归通过（`src_pod` 与改动前一致，宿主机进程仍是 `host`）。
- **残留风险**：**没有真实 k8s 集群验证**。上述用例覆盖了「凭据 → HTTP → 索引 → 补名」的逻辑，
  但真实环境里的 RBAC（是否有列 Pod 权限）、网络策略、大集群下的索引大小都未验证。
  验收标准（k8s 环境里 `src_pod` 是真实 Pod 名、dataserver 按 Pod 名命中端点表）仍需真实集群确认。

### TODO-5（中）`max_cpu_percent` 没有实际作用点

- **现状**：配置里解析并夹取（0..=100），但**代码里没有任何地方消费它** —— 内核态限流用的是
  `max_events_per_sec`（设计里已记录该口径修正：BPF 程序里拿不到 CPU 百分比）。
- **为什么欠**：需要一个用户态测量点（Agent 自身采集线程的 CPU 占用）。
- **怎么补**：Agent 定期读 `/proc/self/stat` 算采集线程 CPU 占用；超过 `max_cpu_percent` 时
  发一条 `agent_ebpf_throttle_hint` 指标并（可选）临时下调 `max_events_per_sec`。
- **验收**：人为压低阈值能看到告警指标；不改变内核态丢弃逻辑。
- **备注**：若确认不做，应把该配置项从 DTO 中移除，避免「配了没用」的误导。

### TODO-6（中）`agent_ebpf_*` 自监控指标点未输出 —— ✅ 已完成

- **原状**：`EbpfStats`/`EbpfSnapshot` 有 8 个计数（`flushes`/`edges`/`metrics`/`filtered`/`idle_keys`/
  `read_errors`/`map_overflow_dropped`/`rate_limited`），但**只存在于内存**；需求 12.3 要求
  「统计并输出每项限制的触发次数与被丢弃的数据量」。内核态限流、map 满、map 读失败这些
  「出事了但没报错」的情况，只留在内存里等于运维在 Prom 上看不见，采集看起来一直「正常」。
- **已完成**：`stats_metrics(agent_id, item_id, &snapshot)` 把快照发成 `agent_ebpf_*_total` 八条点，
  两个采集循环（连接/进程）每轮各发一次（`record_id` 带时间戳，接入侧按 id 去重）。
  检查点工具也调用**同一个函数**，否则工具会「验证」出假的通过（生产漏发的点，工具照样漏发）。
- **实测**：本机真跑并在 Prom 查回 `agent_ebpf_flushes_total=3`、`agent_ebpf_metric_points_total=604`、
  `agent_ebpf_idle_keys_total=0`（干净数据目录；见下面「本地 scratch 环境的一个坑」）。

### TODO-7（中）dataserver 侧接入计数不完整 —— ✅ 已完成

- **原状**：已有 `vectorman_ingest_records_{accepted,failed}_total`（**没有 `data_type` 维度**，
  因此分不出 eBPF 与 traces），加上 eBPF 聚合侧的 4 项自监控。
- **已完成**：新增带标签的计数器 `dataserver_ingest_batches_total{data_type,status}` 与
  `dataserver_ingest_records_total{data_type,result=accepted|invalid}`。`data_type=~"ebpf.*"`
  即需求 12.4 的口径（批次数、边记录数、非法记录数），顺带也覆盖 traces/logs/metrics。
  为此给 `vectorman-metrics` 加了 `inc_counter_labeled`（标签名首次固定、个数不符就丢弃而不是 panic，
  自监控出问题不该带崩数据面；同名不可既做普通又做带标签计数器，注册表按名字唯一）。
- **实测**：`dataserver_ingest_records_total{data_type="ebpf",result="accepted"} 1241`、
  `{data_type="metrics",result="accepted"} 612`，与采集侧上报数一致。

### 本地 scratch 环境的一个坑（排查时别误判）

用同一个数据目录反复 `pkill` dataserver 后，tsink 会进入 **fail-fast**，之后所有写入都返回
`tsink: Storage is shutting down`（**底层原因被这句话掩盖**），连原本正常的进程指标也写不进，
很容易误判成「刚改的代码把存储搞坏了」。判断方法：换一个**全新的数据目录**再试；正常即可确认是
本地残留状态问题。要观察 fail-fast 之前的真因，需要看底层日志而不是接口返回。

### TODO-8（低）`ebpf_edge_duration_micros` 少了 `field=max`

- **现状**：边表里有 `duration_max`，查询接口返回 `duration_max`，但指标只派生 `avg`/`p95`。
- **怎么补**：在 `ebpf_metrics::aggregate` 增加一个 `field=max` 的点（`MAX(duration_max)` 已可按分钟聚合）。
- **验收**：Prom 查询 `apm_edge_duration_micros{source="ebpf",field="max"}` 有值。

### TODO-9（低）保留期未在真实数据上跑满一轮

- **现状**：`delete_edges_before_batched` 的分批、只删指定 `data_id`、`retain/` 到期清空都有单测；
  但没有观察过「真实数据 + 真实清理循环（每小时）」跑一整轮的效果与耗时。
- **怎么补**：在测试环境写入若干天的数据（或用 `--ingest-url` 快速灌入），把保留期调到最小，
  观察 `dataserver_ebpf_edges_deleted_total` 与表行数变化。
- **验收**：清理后表行数与预期一致；单批耗时不影响同连接的其它查询。

### TODO-10（低）CLI 未对真实服务跑过

- **现状**：`dpc edges`/`ebpf-events`/`ebpf-capability` 的请求体拼装有单测，但没对真实 dataserver 执行过。
- **怎么补**：`dpc --sql-url http://127.0.0.1:18081 edges --source ebpf --protocol tcp`（附录有起服务的命令）。
- **验收**：输出 JSON 与 curl 结果一致。

### TODO-11（低）P2 / P3 未实现

- **P2**：文件与 syscall 延迟（`sys_enter/sys_exit_openat|read|write|fsync`）、DNS 延迟
  （`udp_sendmsg/recvmsg` 过滤 53）。设计已写挂载点与指标名，内核态与用户态都未实现。
- **P3**：CPU profile（perf 采样 + 折叠栈 + 符号化），单独阶段。
- **验收**：按 `tasklist.md` 第 8/9 节的验收标准。

## 三、工程缺口与维护约定

### TODO-12（高）前端测试不在 CI 里 —— ✅ 已完成

- **原状**：`.github/workflows` 只有 `rust-ci`（含 `ebpf-programs`、手动 `ebpf-objects`）、
  `release`、`helm-ci`；前端 `npm test`、`tsc --noEmit`、`build:dataplane` **只在本地跑过**。
- **影响**：本轮 `/ebpf` 页（PR #55）是「本地跑绿 + 人工确认」合入的，之后任何前端改动都不会被流水线拦住。
- **已完成**：新增 `.github/workflows/frontend-ci.yml`（复用 yoc 的 `react-ci@v1.0.0`，与前两个工作流同约定），
  在 `frontend/**` 变更时跑**安装 → 全量类型检查 → 全部 workspace 测试 → `build:dataplane`**；
  根 `package.json` 增加 `typecheck`（7 个 workspace 逐个 `tsc --noEmit`）。
- **副产品（正是这条待办存在的价值）**：第一次全量类型检查就暴露 **4 个 workspace 早就存在的类型错误**
  （`npm test` 一直是绿的，因为没有类型检查）：`job-rerun-drawer` 的 `dest_path` 不在表单类型里
  （表单与 `buildRerunRequest` 都在用）、`job-submit-drawer` 传了可能为 `undefined` 的 `agent_id`、
  以及 `fetch-client.test.ts` 两处 mock 未声明参数（`mock.calls[0][1]` 因此类型不成立）。已一并修掉。
  「没进 CI 的测试约等于没有测试」，这条待办自己证明了这一点。

### 维护约定：改了内核态源码必须重建 `.o` 并一起提交

`packaging/ebpf/*.o` 已入库（按用户要求）。这带来一个**陈旧风险**：源码改了、产物忘了重编，
运行时是旧程序而且看不出来。现有防护：

- `scripts/build-ebpf.sh` 构建期校验「无未定义函数符号」（挡 `__multi3` 这类问题）；
- CI 手动作业 `ebpf-objects` 校验段名与 **map 数量**（network 7 / tcp 4 / process 4）。

两者都**挡不住**「逻辑改了但结构没变」的漂移。约定：**任何触及 `crates/gse-ebpf-programs/`
或 `crates/ebpf-abi/` 的改动，必须在同一个 PR 里跑一次 `scripts/build-ebpf.sh` 并提交新产物**，
PR 描述里写明重建过。

### 已知边界（不是缺陷，写下来避免误判）

- 指标派生有 **60 秒滞后窗口**（覆盖迟到边），接入后立刻查 Prom 可能为空；
- eBPF 边**没有 trace 可跳**（只给边不给 trace），因此 `/ebpf` 页只在未识别服务上给「建立映射」入口；
- 合并视图按 `(bucket_ts, src_service, dst_service)` 汇总（OTLP 侧没有协议），协议降为行字段，
  同分钟混合协议时记 `mixed`；
- `ns → µs` 用 `>> 10`（偏差约 2.4%），因为内核态不能出现常量除法；
- `p95` 是由直方图槽上界近似，**不能与 OTLP 侧的精确 p95 相加**。

## 附录：本机跑闭环的命令

```bash
# 1. 内核态产物（需要 bpf-linker；也可从 CI 的 ebpf-objects artifact 取）
scripts/build-ebpf.sh

# 2. 本地 dataserver（APM 开启，聚合间隔 5 秒便于观察）
mkdir -p /tmp/ebpf-loop && cat > /tmp/ebpf-loop/config.toml <<'EOF'
data_path = "/tmp/ebpf-loop/data"
apm_enabled = true
apm_agg_interval_secs = 5
ts_clean_interval_secs = 3600
[sql_http]
listen = "127.0.0.1:18081"
[prom_http]
listen = "127.0.0.1:19090"
[metrics_http]
listen = "127.0.0.1:19091"
[auth]
enabled = false
EOF
setsid nohup ./target/debug/dataserver --config /tmp/ebpf-loop/config.toml > /tmp/ebpf-loop/ds.log 2>&1 &

# 3. 真采集并上报（另一个终端制造流量：curl http://127.0.0.1:18080/）
sudo -E cargo run -p gse-agent-ebpf --example checkpoint -- \
    --kind ebpf_network --seconds 10 --ingest-url http://127.0.0.1:18081

# 4. 查回来
curl -s -X POST http://127.0.0.1:18081/v1/edges/search \
    -H 'content-type: application/json' -d '{"source":"ebpf","limit":3}'
curl -s http://127.0.0.1:18081/v1/ebpf/capability
# 过 60 秒滞后窗口后查指标
curl -s "http://127.0.0.1:19090/api/v1/query?query=ebpf_edge_connections_total"
```
