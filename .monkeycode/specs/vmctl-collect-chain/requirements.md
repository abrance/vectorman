# Requirements Document

## Introduction

本 feature 把 `vmctl` 从「gse-server 只读台账 + 作业客户端」扩为 **vectorman 采集链路命令行**：一套命令完成
「写 spec → 下发 → 数据真的采上来 → 出问题能定位」的闭环，全部经 HTTP，不新建连接、不新起进程。

现状缺口（2026-09-30 核对代码）：`vmctl` 只覆盖 `gse-server` 的部分路由（health / hosts 只读 / agents 只读 /
jobs / job-files），**per-Agent spec 的读、写、下发在 CLI 侧完全缺失**
（`GET|PUT /api/gse/agents/{id}/spec`、`POST /api/gse/agents/{id}/spec/apply`、`GET /api/gse/agent-specs`
三条路由服务端已实现但只有前端在用）。结果是「配一条链路」必须切到前端页面，
「配完到底采上没有」没有任何命令行答案。

本版交付**一个顶层命名空间的扩展**（既有 `health` / `hosts` / `agents list|get` / `jobs` 保持原样）：

- `vmctl agents specs`：全部 Agent 的期望 spec 与生效状态总览（含 `sync_status` 与逐字段 diff）。
- `vmctl agents spec get|put|apply <agent_id>`：读期望 spec / 保存期望 spec / 下发整份 spec。
- `vmctl agents status <agent_id>`：逐采集项 × 逐 data_type 核验「采上来了没有」（结合数据面 stream）。
- `vmctl agents doctor <agent_id>`：把链路每一段聚合到一条命令里（台账 / spec / 采集项 / eBPF 能力 / 数据面连通性）。

范围边界见 Requirement 8：**本版不做 `data` 子命名空间**（`dpc` 已覆盖 dataserver 查询），
**不提供采集项的一等资源 CRUD**（采集项是 spec 内的元素，`agent_ids` 已不存在）。

## Glossary

- **vmctl**：随安装包分发的单文件 HTTP 客户端（沿用 gse-cli 的血统）。
- **GSE URL**：`--url` 指定的 gse-server HTTP 根地址，缺省 `http://127.0.0.1:7101`，与现状一致。
- **Data URL**：`--data-url` 指定的 dataserver SQL 口根地址，缺省 `http://127.0.0.1:8081`
  （沿用 `dpc` 的 `--sql-url` 缺省）。仅 `status` / `doctor` 使用。
- **Agent**：`gse-server-core` 台账里的 `Agent { agent_id, host_id, access_point_id, token, version,
  install_path, status, last_heartbeat_at, registered_at }`；`status` 是**心跳口径**（`online` / `offline` /
  `unknown`），与会话口径的 `session_state` 可能不一致。
- **spec**：一台 Agent 的期望状态，`gse-proto::AgentSpecWire { params, items }`。`params` 是 Agent 运行参数
  （心跳周期、解释器白名单、OTLP 开关与凭据等），`items` 是这台 Agent 的采集项数组。
- **采集项（Spec Item）**：`gse-proto::SpecItem { item_id, name, kind, enabled, collector, storage }`。
  注意**没有 `agent_ids`** —— 采集项归属由「它出现在哪台 Agent 的 spec 里」表达。
- **采集种类（Kind）**：`metrics_host`、`log_file`、`log_k8s_stdout`、`apm_otlp`、`ebpf_network`、`ebpf_tcp`、
  `ebpf_process`、`ebpf_syscall` 八类。前四类属通用采集，后四类属 eBPF 家族。
- **collector / storage**：采集项里两段自由 JSON。`collector` 的字段集见
  `crates/gse-agent-core/src/collect/config.rs` 的 `CollectorConfig`
  （`interval_secs` / `path_patterns` / `namespace` / `pod_name_pattern` / `container` / `kubeconfig` /
  `start_mode` / `start_n` / `batch_max_records` / `flush_interval_secs` / `clean`）；
  `storage` 至少含保留天数口径（`retention_days`，dataserver 保留清理的输入）。
- **revision**：spec 的内容指纹（`crates/gse-server-core/src/spec.rs`，sha256 前 16 位十六进制）。
  **同一份 spec 必得同一 revision**；`token` / `otlp_token` 也参与指纹，故 revision 是「是否已下发」的权威判据。
- **sync_status**：服务端派生的同步状态，取值 `synced` | `stale` | `rejected` | `unspecified` | `unknown`。
  - `unknown`：Agent 从未上报过生效快照；
  - `rejected`：最近一次应用被拒（`outcome = rejected`）；
  - `unspecified`：Agent 上报过但服务端没有期望 spec（Agent 跑的是本地文件基线）；
  - `synced`：期望 revision == 生效 revision；
  - `stale`：期望与生效 revision 不同（**保存过但没下发**，或下发失败）。
- **Stream**：dataserver KV 里 `stream/{agent_id}/{data_type}/{data_id}` 的流索引，含 `last_seen_micros` 与
  `accepted`（累计接受条数），是「采上来了没有」的唯一现成证据。`GET /v1/streams` 返回
  `{"streams":[{"agent_id","data_type","data_id","last_seen_micros","accepted"}]}`。
- **data_id**：落库时与采集项 `item_id` 同值，因此在 streams 里可用 `item_id` 反查该采集项的上报情况。
- **data_type**：`metrics` | `logs` | `traces` | `apm` | `ebpf` | `ebpf_edges`
  （`crates/dataplane-ingest/src/lib.rs` 的 `DataType`）。**一个采集项可能对应多个 data_type**，
  见 Requirement 5 的种类映射表。
- **透传输出（Passthrough）**：把服务端响应正文原样写到标准输出，不做字段裁剪或重排，沿用 `dpc` 的现状语义。
- **Legacy 子命令**：本 feature 之前已存在的 `vmctl health` / `hosts` / `agents` / `jobs`。
- 其余术语沿用 gse-server-agent、gse-agent-config-center、gse-dataplane-ingest、observability-data-model。

## Requirements

### Requirement 1: 命名空间与既有命令共存

- AS 运维人员, I want 在既有 `agents` 命令下扩展 spec 能力, so that 不必记忆第二套顶层命令名。
- 验收：WHEN 用户执行 `vmctl agents --help`，THE vmctl SHALL 列出 `list`、`get`、`specs`、`spec`、`status`、
  `doctor` 六个子命令。
- 验收：WHEN 用户执行 `vmctl agents spec --help`，THE vmctl SHALL 列出 `get`、`put`、`apply` 三个子命令。
- 验收：WHEN 用户执行 `vmctl --help`，THE vmctl SHALL **仍然只列出** `health`、`hosts`、`agents`、`jobs`
  这四个顶层子命令 —— 本 feature SHALL NOT 新增顶层子命令（不引入 `agent` 单数、不引入 `collect`、不引入 `data`）。
- 验收：WHEN 用户执行 `vmctl hosts ...`、`vmctl jobs ...`、`vmctl health`，THE vmctl SHALL 保持本 feature 之前的
  请求路径、请求体与输出格式不变。
- 验收：WHEN 用户执行 `vmctl agents list` 或 `vmctl agents get <agent_id>`，THE vmctl SHALL 保持透传行为不变。

### Requirement 2: 数据面地址与凭证参数

- AS 运维人员, I want 一条命令里分别指定控制面与数据面地址, so that 单机、k8s、远程三种形态都能用。
- 验收：WHEN 用户未提供 `--url`，THE vmctl SHALL 使用 `http://127.0.0.1:7101` 作为 GSE URL。
- 验收：WHEN 用户未提供 `--data-url`，THE vmctl SHALL 使用 `http://127.0.0.1:8081` 作为 Data URL；
  WHEN 用户提供 `--data-url`，THE vmctl SHALL 以该值为数据面请求根地址。
- 验收：THE vmctl SHALL 在发往 GSE URL 的每个请求上带 `Authorization: Bearer <值>`，凭据来源为
  `--password`（沿用现状；环境变量兜底 `VECTORMAN_PASSWORD`），两者同时存在时命令行参数优先，两者都未提供时
  SHALL 不发该头（保持「对端未开认证」可用）。
- 验收：THE vmctl SHALL NOT 在任何子命令里打印凭据值。
- 验收：WHEN 目标服务返回 401，THE vmctl SHALL 把响应体内容写到标准错误，并以退出码 1 结束。
- 验收：WHEN 用户执行只涉及 GSE URL 的子命令（`specs` / `spec get|put|apply`），THE vmctl SHALL NOT 向
  Data URL 发出任何请求。

### Requirement 3: 全部 Agent 的 spec 总览

- AS 运维人员, I want 一眼看到哪些 Agent 的配置还没生效, so that 不必逐台点开。
- 验收：WHEN 用户执行 `vmctl agents specs`，THE vmctl SHALL 请求 `GET /api/gse/agent-specs` 并把响应正文
  透传到标准输出。
- 验收：WHEN 用户执行 `vmctl agents specs --table`，THE vmctl SHALL 以表格输出一行一台 Agent，列为：
  `agent_id`、`session_state`、`sync_status`、`items`（`desired.spec.items` 的条数，无期望时为空）、
  `reported_at`（无上报时为空）。这些列 SHALL 全部来自响应字段，不做本地推断。
- 验收：WHEN 响应不是预期结构（缺 `agent_id` 或不是 JSON 数组），THE vmctl SHALL 退回透传原正文而不是报解析失败。
- 验收：WHEN 用户传入 `--agent-id <id>`，THE vmctl SHALL 在本地过滤后再输出（透传与 `--table` 两种形态都过滤），
  且响应中没有该 Agent 时 SHALL 输出空结果并以退出码 0 结束（与「Agent 不存在」区分：后者需查 `GET /api/gse/agents/<id>`）。

### Requirement 4: 读写单台 Agent 的 spec

- AS 运维人员, I want 直接读写 Agent 的 spec JSON, so that CLI 不会比台账模型先过期。
- 验收：WHEN 用户执行 `vmctl agents spec get <agent_id>`，THE vmctl SHALL 请求
  `GET /api/gse/agents/<agent_id>/spec` 并透传响应正文；WHEN 服务端返回 404，THE vmctl SHALL 把响应体写到
  标准错误并以退出码 1 结束。
- 验收：WHEN 用户执行 `vmctl agents spec put <agent_id> -f <path>` 或 `--json '<json>'`，THE vmctl SHALL 把该
  JSON 对象**原样**作为请求体发往 `PUT /api/gse/agents/<agent_id>/spec`，成功后透传响应正文。
- 验收：THE vmctl SHALL NOT 对 `params` / `items` 做字段裁剪、补默认值或类型校验 —— 请求体形态由服务端的
  `SpecPutBody` 决定（`params` 缺字段取默认值、敏感字段缺字段表示「不修改」）；服务端返回 400 时
  THE vmctl SHALL 原样透出错误正文并以退出码 1 结束。
- 验收：WHEN `-f` 与 `--json` 同时提供，或两者都未提供，THE vmctl SHALL 在发请求前以退出码 1 报错，且不发请求。
- 验收：WHEN `-f` 指向的文件不存在或内容不是 JSON 对象（数组、标量、空文件均属此列），
  THE vmctl SHALL 在发请求前以退出码 1 报错并说明原因。
- 验收：WHEN 用户执行 `vmctl agents spec apply <agent_id>`，THE vmctl SHALL 请求
  `POST /api/gse/agents/<agent_id>/spec/apply`（无请求体）并透传响应正文（含 `{ok, ack}`）。
- 验收：WHEN 服务端返回 409（Agent 离线），THE vmctl SHALL 把响应体写到标准错误并以退出码 1 结束。
- 验收：THE vmctl SHALL NOT 提供 spec 的删除子命令（服务端不提供，见 `gse-agent-config-center` R12
  「没有回到本地 TOML 基线的路径」）。

### Requirement 5: 采集生效核验（逐采集项 × 逐 data_type）

- AS 运维人员, I want 一条命令看到「配了但没采上来」, so that 排查不必先猜是配置还是链路。
- 验收：WHEN 用户执行 `vmctl agents status <agent_id>`，THE vmctl SHALL 请求
  `GET /api/gse/agents/<agent_id>/spec`（GSE URL）与 `GET /v1/streams`（Data URL）；
  WHEN 前者返回 404，THE vmctl SHALL 以退出码 1 报错并说明该 Agent 没有 spec。
- 验收：THE vmctl SHALL 以 **`desired.spec.items`** 为采集项来源（期望值），而不是 `applied.spec.items`；
  `enabled = false` 的采集项 SHALL 逐条列出但不参与退出码判定，并标注为 `disabled`。
- 验收：THE vmctl SHALL 按 kind 展开需要核验的 data_type（**一个采集项可对应多个**）：

  | kind | 期望 data_type |
  | --- | --- |
  | `metrics_host` | `metrics` |
  | `log_file`、`log_k8s_stdout` | `logs` |
  | `apm_otlp` | `traces` |
  | `ebpf_network`、`ebpf_tcp` | `ebpf_edges` |
  | `ebpf_process`、`ebpf_syscall` | `metrics` |

  映射依据：`crates/gse-agent-core/src/collect/ebpf.rs` 的 `EbpfSink::{edges,metrics,raw_events}`
  与 `metrics.rs` / `logfile.rs` / `k8s.rs` / `otlp.rs` 的 `shared.push(...)` 调用点。
  验收：WHEN 某采集项的 kind 不在上表（新 kind），THE vmctl SHALL 以 `unknown_kind` 标注该行而不中断命令。
- 验收：THE vmctl SHALL 逐 `(item_id, data_type)` 输出一行，含 `item_id`、`data_type`、stream 的
  `last_seen_micros`（无 stream 时为空）、stream 的 `accepted`（无 stream 时为 0）、
  `last_seen` 的**相对年龄**（人类可读，例如 `12s`；无 stream 时为空），以及判定：
  - `reporting`：有 stream 且 `last_seen_micros` 距当前时间不超过 `max(3 × interval_secs, 60 秒)`；
  - `stale`：有 stream 但更旧；
  - `not_reporting`：无 stream。
- 验收：`interval_secs` SHALL 取该采集项 `collector.interval_secs`；缺失、非数字或 ≤ 0 时 SHALL 按 15 秒计算该阈值。
- 验收：THE vmctl SHALL **额外**输出一行 spec 同步状态（取自 `GET /api/gse/agents/<id>/spec` 的 `sync_status`），
  并在 `sync_status != "synced"` 时把该行标注为 `dirty` —— 未下发时 stream 不可能更新，
  输出 SHALL 明确区分「未下发」与「已下发未上报」，避免把 `stale` 误判成采集故障。
- 验收：WHEN 全部 `enabled` 采集项的全部 data_type 均判为 `reporting` **且** `sync_status == "synced"`，
  THE vmctl SHALL 以退出码 0 结束；否则 SHALL 以退出码 1 结束（供脚本判链路通断）。
- 验收：WHEN Data URL 的 `GET /v1/streams` 不可达或返回非 2xx，THE vmctl SHALL 把 data_type 行的判定标为
  `unknown`、以退出码 1 结束，并在末行给出原因（SHALL NOT 把它当成 `not_reporting`）。

### Requirement 6: 链路聚合诊断

- AS 运维人员, I want 对一台 Agent 一次性看清链路每一段, so that 不用翻五个页面。
- 验收：WHEN 用户执行 `vmctl agents doctor <agent_id>`，THE vmctl SHALL 聚合输出以下段落，每段缺失或不可达时
  以 `unknown` 标注而不是中断整条命令：
  1. **Agent 台账**：`GET /api/gse/agents/<agent_id>` 的 `status`（心跳口径）、`session_state`（会话口径）、
     `version`、`host_id`、`last_heartbeat_at`；
  2. **spec 同步**：`GET /api/gse/agents/<agent_id>/spec` 的 `sync_status`、`desired.revision`、
     `applied.revision`、`applied.outcome`、`applied.not_enforced`；
  3. **采集项核验**：复用 Requirement 5 的判定，逐 `(item_id, data_type)` 输出；
  4. **eBPF 能力**：`GET /v1/ebpf/capability`（Data URL），含未采集时的原因字段；
  5. **数据面连通性**：Data URL 的 `GET /health` 的状态码。
- 验收：段 1 在 Agent 不存在（404）时 SHALL 以退出码 1 结束，且不再请求其余段落（无主体可诊断）。
- 验收：段 1 的会话口径（`session_state` / `job_channel_available`）SHALL 取自
  `GET /api/gse/agents`（列表）—— `GET /api/gse/agents/<agent_id>` 返回裸 `Agent`，**不含会话字段**；
  THE vmctl SHALL NOT 把「列表里找不到该 Agent」当成 `offline`，而应输出空值并计入 unknown。
- 验收：THE vmctl SHALL NOT 透传 `GET /api/gse/agents*` 的原始响应 —— 该响应含**明文 `token`**；
  `doctor` / `status` 的输出 SHALL 只含白名单字段（`agent_id` / `host_id` / `version` / `status` /
  `session_state` / `job_channel_available` / `last_heartbeat_at`）。
- 验收：THE vmctl SHALL 以退出码 0 结束当且仅当：段 1 的 `session_state == "online"`、段 5 通过、
  段 2 的 `sync_status == "synced"`、且段 3 的全部 `enabled` 采集项均为 `reporting`；
  其余情况以退出码 1 结束，并在末行 `summary:` 给出未通过的原因摘要（逐条列出失败的段与原因）。
- 验收：THE vmctl SHALL NOT 向任何 `POST` / `PUT` 路由发请求（doctor 是只读诊断）。

### Requirement 7: 退出码与错误口径一致

- AS 运维人员, I want 所有命令的成败判断一致, so that 脚本能直接接。
- 验收：THE vmctl SHALL 以退出码 0 表示命令成功、1 表示失败（含 HTTP 非 2xx、本地参数错误、文件读取失败），
  2 保留给参数解析失败（沿用 clap 默认行为）。
- 验收：WHEN 请求失败，THE vmctl SHALL 在标准错误至少给出目标 URL 与原因；正常输出 SHALL 只出现在标准输出。
- 验收：THE vmctl SHALL NOT 在错误信息里回显整个请求体（spec 的 `params` 可能含 `token` / `otlp_token`）。

### Requirement 8: 范围边界与前置依赖

- AS 维护者, I want 明确本 feature 不动什么, so that 评审能一眼看到风险面。
- 范围边界：THE vmctl SHALL NOT 实现 `data` 子命名空间（dataserver 查询仍由 `dpc` 提供，`dpc` 本版不动）；
  SHALL NOT 提供采集项的一等资源 CRUD（`/api/gse/collect-items*` 已删除，采集项是 spec 内的元素）；
  SHALL NOT 提供跨 Agent 批量下发与批量 spec 写入（服务端只有单台 Agent 的三个动作）；
  SHALL NOT 实现 spec 的强类型字段参数（`params` / `items` 一律由 JSON 承载）；
  SHALL NOT 实现主机/标签选择器展开（目标只能是显式 `agent_id`）；
  SHALL NOT 引入配置文件与 profile 机制（只认命令行参数与环境变量）；
  SHALL NOT 实现认证与授权逻辑（只做 Bearer 注入）；
  SHALL NOT 提供 spec 模板或预置示例；
  SHALL NOT 改动 `dpc`、前端与台账数据模型；
  SHALL NOT 新增服务端路由（本 feature 纯客户端）。
- 前置依赖：三条 GSE 路由（`GET /api/gse/agent-specs`、`GET|PUT /api/gse/agents/{id}/spec`、
  `POST /api/gse/agents/{id}/spec/apply`）与两条数据面路由（`GET /v1/streams`、`GET /v1/ebpf/capability`）
  均已实现于 `main`（本 feature 只消费）。凭据注入沿用既有 `--password` / `VECTORMAN_PASSWORD`
  （**不是**规格初稿写的 `--token` / `VECTORMAN_TOKEN`）。
- 已知限制（需在使用文档中写明）：
  1. gse-server 管理口鉴权默认**未开启**（`GSE_SERVER_ADMIN_PASSWORD` 空 = 不认证），
     未开启时 `--password` 无意义、写操作靠网络边界保护；
  2. dataserver 默认 `NoopAuth`，`status` / `doctor` 的 stream 读取同样无认证；
  3. eBPF 家族采集项依赖目标机内核 ≥ 5.8 + BTF 可读 + root 或 CAP_BPF/CAP_PERFMON，
     `doctor` 的段 4 只会给「不可用及原因」，不会替用户修环境；
  4. 一个采集项对应多个 data_type 时，**任一** data_type 未上报即判该采集项未通 —— 这是刻意的严格口径，
     因为「部分上报」在链路上就是断的；
  5. spec 保存 ≠ 下发：`spec put` 之后必须 `spec apply`，`status` / `doctor` 会以 `dirty` 显式提示。

## 修订记录

### 2026-09-30：整体重新定范围（本版）

本 feature 初稿（2026-09-29，见 git 历史）把范围定为 `vmctl collect ...` + `vmctl data ...` 两套新子命令。
经两轮规格修订与实施前的代码核对，范围整体收敛为**在既有 `agents` 下扩展**：

| 初稿条款 | 原口径 | 本版 |
| --- | --- | --- |
| R1 命名空间 | 新增顶层 `collect` / `data` | **只扩 `agents`**；不新增顶层子命令 |
| R2 凭证参数 | `--token` / `VECTORMAN_TOKEN` | `--password` / `VECTORMAN_PASSWORD`（与实现一致）；去掉该条「每请求带 token」 |
| R3 `collect kinds` | 八类 kind 字段自描述 | **删除** —— `collector` 字段集以 `CollectorConfig` 为准，硬编码到 CLI 必先过期 |
| R4 采集项读取 | `GET /api/gse/collect-items*` | **删除该路由**（`gse-agent-config-center` 破坏性变更 1）；改为 spec 读取 |
| R5 采集项写入 | `POST|PUT|DELETE /api/gse/collect-items*` | 改为 `PUT /api/gse/agents/{id}/spec`（整份、raw JSON）；无 DELETE |
| R6 声明式 apply | spec 文件 + Drift 判定 + `--prune` | **删除** —— 新模型下「期望集合」就是这份 spec 本身，Drift 退化为 `sync_status`，无需 CLI 重算 |
| R7 status | 逐 `agent_ids` 判 stream | 改为逐 `(item_id, data_type)`；新增 `dirty`（未下发）判定 |
| R8 doctor | 含 `GET /api/gse/agent-configs/<id>` 与 `GET /api/gse/collect-items` | 前者路由已删除、后者已删除；改为 spec + Agent 台账 |
| R9–R12 `data ...` | dataserver 查询与运维 | **整组移出本 feature**（用户决定；`dpc` 已覆盖，不重复实现） |
| R13 access-points / dataplanes / agent-configs | 五组 CRUD | access-points 与 dataplanes 路由**仍在**但本版不做（用户决定）；agent-configs 路由已删除 |
| R15 前置依赖 | 「不需要服务端新增路由」 | 修订为「服务端路由已实现，本 feature 纯客户端」 |

初稿中「本 feature 尚未实施、需按 `gse-agent-config-center` 调整」的对照表已由本表取代。
`data` 子命名空间如后续要做，应重新立项（`dpc` 的 14 条子命令 `--sql-url` 口径与 `vmctl` 的 `--data-url` 需先统一）。
