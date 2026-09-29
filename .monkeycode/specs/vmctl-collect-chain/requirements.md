# Requirements Document

## Introduction

本 feature 把 `vmctl` 从「gse-server 只读台账 + 作业客户端」扩为 **vectorman 全链路采集命令行**：一套命令完成
「下发采集配置 → 数据真的采上来 → 查得到数据 → 出问题能定位」的闭环，全部经 HTTP，不新建连接、不新起进程。

现状缺口（2026-09-29 核对代码）：`vmctl` 只覆盖 `gse-server` 21 条路由中的 9 条（health / hosts 只读 /
agents 只读 / jobs / job-files），**采集项的配置入口（`/api/gse/collect-items`）与 Agent 配置
（`/api/gse/agent-configs`）在 CLI 侧完全缺失**；数据侧查询能力散落在另一个二进制 `dpc`（dataserver 客户端）。
结果是「配一条链路」必须切到前端页面，「配完到底采上没有」没有任何命令行答案。

v1 交付两个新子命名空间（既有 `hosts` / `agents` / `jobs` 保留原样）：

- `vmctl collect ...`：采集项 CRUD、声明式 `apply`、生效核验 `status`、链路聚合诊断 `doctor`、`kinds` 字段自描述。
- `vmctl data ...`：dataserver 数据面查询与运维，覆盖 `dpc` 现有全部子命令并补齐 streams / APM 服务与别名 / query_range。

范围边界与前置依赖见 Requirement 15。

## Glossary

- **vmctl**：随安装包分发的单文件 HTTP 客户端（沿用 gse-cli 的血统），本 feature 后同时是控制面与数据面客户端。
- **GSE URL**：`--url` 指定的 gse-server HTTP 根地址，缺省 `http://127.0.0.1:7101`，与现状一致。
- **Data URL**：`--data-url` 指定的 dataserver SQL 口根地址，缺省 `http://127.0.0.1:8081`（沿用 `dpc` 的 `--sql-url` 缺省）。
- **采集项（Collect Item）**：`gse-proto` 的 `CollectItem { item_id, agent_ids, name, kind, enabled, collector, storage }`，
  由 gse-server 台账持久化、Agent 侧拉取后按 `kind` 起采集协程。
- **采集种类（Kind）**：`metrics_host`、`log_file`、`log_k8s_stdout`、`apm_otlp`、`ebpf_network`、`ebpf_tcp`、
  `ebpf_process`、`ebpf_syscall` 八类。
- **collector / storage**：采集项里两段自由 JSON。`collector` 的字段集见 `crates/gse-agent-core/src/collect/config.rs`
  的 `CollectorConfig`（interval_secs / path_patterns / namespace / pod_name_pattern / container / kubeconfig /
  start_mode / start_n / batch_max_records / flush_interval_secs / clean）；`storage` 至少含保留天数口径
  （dataserver 删除采集项时按 `retention_days` 计算清理窗口）。
- **声明式 spec（Spec File）**：一份 JSON 文件，描述「期望存在的采集项集合」，是 `collect apply` 的输入。
- **Drift**：spec 描述的期望集合与台账实际集合之间的差异，逐 `item_id` 判定为 `create` / `update` / `noop`
  （`prune` 开启时另有 `delete`）。
- **Stream**：dataserver KV 里 `stream/{agent_id}/{data_type}/{data_id}` 的流索引，含 `last_seen_micros` 与
  `accepted`（累计接受条数），是「采上来了没有」的唯一现成证据。
- **data_id**：落库时与采集项 `item_id` 同值，因此在 streams 里可用 `item_id` 反查该采集项的上报情况。
- **透传输出（Passthrough）**：把服务端响应正文原样写到标准输出，不做字段裁剪或重排，沿用 `dpc` 的现状语义。
- **Legacy 子命令**：本 feature 之前已存在的 `vmctl health` / `hosts` / `agents` / `jobs`。
- 其余术语沿用 gse-cli、gse-dataplane-ingest、observability-data-model。

## Requirements

### Requirement 1: 两个新命名空间与既有命令共存

- AS 运维人员, I want 用 `collect` 与 `data` 两个命名空间区分「配」与「查」, so that 不必记忆散落的子命令。
- 验收：WHEN 用户执行 `vmctl --help`，THE vmctl SHALL 在顶层列出 `health`、`hosts`、`agents`、`jobs`、
  `collect`、`data` 六个子命令；WHEN 用户执行 `vmctl collect --help`，THE vmctl SHALL 列出 `kinds`、`list`、
  `get`、`create`、`update`、`delete`、`apply`、`status`、`doctor`；WHEN 用户执行 `vmctl data --help`，
  THE vmctl SHALL 列出 `health`、`sql`、`query`、`query-range`、`logs`、`ebpf-events`、`ebpf-capability`、
  `streams`、`traces`、`trace`、`edges`、`apm`、`aliases`、`ts`。
- 验收：WHEN 用户执行任一 Legacy 子命令，THE vmctl SHALL 保持本 feature 之前的请求路径、请求体与输出格式不变。

### Requirement 2: 双地址与凭证参数

- AS 运维人员, I want 一条命令里分别指定控制面与数据面地址, so that 单机、k8s、远程三种形态都能用。
- 验收：WHEN 用户未提供 `--url`，THE vmctl SHALL 使用 `http://127.0.0.1:7101` 作为 GSE URL；
  WHEN 用户未提供 `--data-url`，THE vmctl SHALL 使用 `http://127.0.0.1:8081` 作为 Data URL；
  WHEN 用户提供 `--url` 或 `--data-url`，THE vmctl SHALL 以该值为请求根地址。
- 验收：WHEN 环境变量 `VECTORMAN_TOKEN` 已设置，或用户提供 `--token <值>`，THE vmctl SHALL 在发往 GSE URL 与
  Data URL 的每个请求上带 `Authorization: Bearer <值>`；两个来源同时存在时，命令行参数 SHALL 优先。
- 验收：WHEN 目标服务返回 401，THE vmctl SHALL 把响应体内容写到标准错误，并以退出码 1 结束。

### Requirement 3: 采集种类自描述

- AS 运维人员, I want 不查文档就知道每类采集项要填什么, so that 手写 spec 一次成功。
- 验收：WHEN 用户执行 `vmctl collect kinds`，THE vmctl SHALL 列出全部八类 kind，每类给出：是否属于 eBPF 家族、
  `collector` 的必填与可选字段、`storage` 的必填字段、一个最小可用示例 JSON、以及前置条件说明
  （eBPF 需 Linux + 内核 ≥ 5.8 + BTF 可读 + root 或 CAP_BPF/CAP_PERFMON；`apm_otlp` 需 Agent 侧
  `otlp_enabled=true`；`log_k8s_stdout` 需 kubeconfig 可达）。
- 验收：WHEN 用户执行 `vmctl collect kinds --kind <kind>`，THE vmctl SHALL 只输出该 kind 的上述信息。

### Requirement 4: 采集项读取

- AS 运维人员, I want 列出与查看采集项, so that 能确认现状再改。
- 验收：WHEN 用户执行 `vmctl collect list`，THE vmctl SHALL 请求 `GET /api/gse/collect-items` 并把响应正文
  透传到标准输出；WHEN 用户执行 `vmctl collect get <item_id>`，THE vmctl SHALL 请求
  `GET /api/gse/collect-items/<item_id>` 并透传响应正文。
- 验收：WHEN 用户在 `list` 或 `get` 上传入 `--table`，THE vmctl SHALL 以表格输出 `item_id`、`name`、`kind`、
  `enabled`、`agent_ids` 五列（`get` 的表额外给出 `collector` 与 `storage` 的紧凑 JSON）；WHEN 响应不是
  预期结构，THE vmctl SHALL 退回透传原正文而不是报解析失败。

### Requirement 5: 采集项写入（raw JSON）

- AS 运维人员, I want 直接下发我写好的 JSON, so that CLI 不会比台账模型先过期。
- 验收：WHEN 用户执行 `vmctl collect create -f <path>` 或 `vmctl collect create --json '<json>'`，THE vmctl SHALL
  把该 JSON 对象作为请求体发往 `POST /api/gse/collect-items`；WHEN 用户执行
  `vmctl collect update <item_id> -f <path>` 或 `--json '<json>'`，THE vmctl SHALL 把该 JSON 对象发往
  `PUT /api/gse/collect-items/<item_id>`；两种写入成功后 SHALL 透传响应正文。
- 验收：WHEN 用户执行 `vmctl collect delete <item_id>`，THE vmctl SHALL 请求
  `DELETE /api/gse/collect-items/<item_id>` 并透传响应正文。
- 验收：WHEN 用户在 `create` 或 `update` 上传入一个或多个 `--agent-id`，THE vmctl SHALL 用它整体覆盖请求体的
  `agent_ids` 字段（未传入时保持 `-f`/`--json` 里的值不变）；WHEN 两者同时缺失 `agent_ids`，THE vmctl SHALL
  在发请求前以退出码 1 报错，且不发出请求。
- 验收：WHEN `-f` 指向的文件不存在或内容不是 JSON 对象，THE vmctl SHALL 在发请求前以退出码 1 报错并说明原因。
- 验收：WHEN 服务端返回 4xx/5xx，THE vmctl SHALL 把响应体写到标准错误并以退出码 1 结束，且不改写正文。

### Requirement 6: 声明式 apply

- AS 运维人员, I want 一份 spec 描述整套链路并幂等下发, so that 装机/换环境/回归不必逐条敲命令。
- 验收：spec 文件 SHALL 为 JSON 对象，形如
  `{"items":[{"item_id":"...","agent_ids":["..."],"name":"...","kind":"...","enabled":true,"collector":{...},"storage":{...}}]}`。
- 验收：WHEN 用户执行 `vmctl collect apply -f <spec>`，THE vmctl SHALL 先 `GET /api/gse/collect-items` 取现台账，
  逐 `item_id` 判定 Drift：台账中不存在 → `create`（`POST`）；存在且 `name`/`kind`/`enabled`/`agent_ids`/
  `collector`/`storage` 任一与期望不等 → `update`（`PUT`）；完全相等 → `noop`（不发请求）。
- 验收：WHEN 用户传入 `--dry-run`，THE vmctl SHALL 只打印 `create` / `update` / `delete` / `noop` 计划与每条的
  差异字段名，不发出任何写请求，退出码为 0。
- 验收：WHEN 用户传入 `--prune`，THE vmctl SHALL 把台账中存在而 spec 未覆盖的采集项判为 `delete` 并执行
  `DELETE`；未传 `--prune` 时 THE vmctl SHALL 不动这些采集项，但 SHALL 在输出里以 `orphan` 列出它们。
- 验收：WHEN 任一条写入失败，THE vmctl SHALL 继续处理其余条目、最后以退出码 1 结束，并在输出中逐条标注
  成功或失败的 `item_id` 与服务端错误正文。

### Requirement 7: 采集生效核验

- AS 运维人员, I want 一条命令看到「配了但没采上来」, so that 排查不必先猜是配置还是链路。
- 验收：WHEN 用户执行 `vmctl collect status <item_id>`，THE vmctl SHALL 请求
  `GET /api/v1/streams`（Data URL）与 `GET /api/gse/collect-items/<item_id>`（GSE URL）；WHEN 后者返回
  404，THE vmctl SHALL 以退出码 1 报错并说明该采集项不存在。
- 验收：THE vmctl SHALL 对采集项的每个 `agent_id` 输出一行：`agent_id`、该 Agent 的 `online` 状态（取自
  `GET /api/gse/agents` 的 `status` 字段）、stream 的 `last_seen_micros`（无 stream 时为空）、
  stream 的 `accepted`（无 stream 时为 0）、以及判定 `reporting` / `stale` / `not_reporting`：
  有 stream 且 `last_seen_micros` 距当前时间不超过 `max(3 × 采集间隔, 60 秒)` 判为 `reporting`；有 stream 但更旧
  判为 `stale`；无 stream 判为 `not_reporting`。
- 验收：WHEN 所有 `agent_id` 均判为 `reporting`，THE vmctl SHALL 以退出码 0 结束；WHEN 存在非 `reporting`
  的 Agent，THE vmctl SHALL 以退出码 1 结束（供脚本判链路通断）。
- 验收：WHEN 采集项的 `collector` 未给出 `interval_secs`，THE vmctl SHALL 按 15 秒计算该阈值。

### Requirement 8: 链路聚合诊断

- AS 运维人员, I want 对一台 Agent 一次性看清链路每一段, so that 不用翻五个页面。
- 验收：WHEN 用户执行 `vmctl collect doctor --agent <agent_id>`，THE vmctl SHALL 聚合输出以下段落，每段缺失或
  不可达时以 `unknown` 标注而不是中断整条命令：Agent 台账（`GET /api/gse/agents/<id>`：`status`、`version`、
  `host_id`）、Agent 配置（`GET /api/gse/agent-configs/<id>`）、命中该 Agent 的采集项清单（由
  `GET /api/gse/collect-items` 过滤 `agent_ids` 得到，并复用 Requirement 7 的 stream 判定）、
  eBPF 能力（`GET /v1/ebpf/capability`，含未采集时的原因）、数据面连通性（Data URL 的 `GET /health`）。
- 验收：THE vmctl SHALL 以退出码 0 结束当且仅当：Agent 为 `online`、数据面 health 通过、且其全部 `enabled`
  采集项均判为 `reporting`；其余情况以退出码 1 结束，并在末行给出未通过的原因摘要。

### Requirement 9: 数据面健康、SQL 与指标查询

- AS 运维人员, I want 不装第二个二进制就能查数据, so that 一个 vmctl 走完全流程。
- 验收：WHEN 用户执行 `vmctl data health`，THE vmctl SHALL 请求 Data URL 的 `GET /health` 并透传正文。
- 验收：WHEN 用户执行 `vmctl data sql --stmt <sql>`，THE vmctl SHALL 请求 `POST /v1/sql`（正文 `{"stmt": ...}`）
  并透传正文；WHEN 用户执行 `vmctl data query --expr <promql>`，THE vmctl SHALL 请求
  `GET /api/v1/query`；WHEN 用户执行 `vmctl data query-range --expr <promql> --start <s> --end <s> --step <s>`，
  THE vmctl SHALL 请求 `GET /api/v1/query_range`；`query` 的 `--time` 传入时 SHALL 作为 `time` 查询参数。
- 验收：三个查询命令 SHALL 都走 Data URL（dataserver 的 SQL 口已同时暴露 `/api/v1/query*`），
  本 feature 不引入第二个 Prometheus 地址参数。

### Requirement 10: 数据面检索类命令

- AS 运维人员, I want 日志、trace、拓扑、eBPF 事件都能查, so that 排查一个入口到底。
- 验收：WHEN 用户执行 `vmctl data logs`，THE vmctl SHALL 请求 `POST /v1/logs/search`，支持
  `--data-type`、`--agent-id`、`--data-id`、`--level`、`--query`、`--from-ts`、`--to-ts`、`--limit`，
  并把 `from_ts`/`to_ts` 解释为 Unix 微秒。
- 验收：WHEN 用户执行 `vmctl data traces`，THE vmctl SHALL 请求 `POST /v1/traces/search`，支持
  `--service`、`--operation`、`--status`、`--min-duration-ms`、`--agent-id`、`--data-id`、`--sort`、`--order`、
  `--from-ts`、`--to-ts`、`--limit`；WHEN 用户执行 `vmctl data trace <trace_id>`，THE vmctl SHALL 请求
  `GET /v1/traces/<trace_id>` 并透传正文。
- 验收：WHEN 用户执行 `vmctl data edges`，THE vmctl SHALL 请求 `POST /v1/edges/search`，支持
  `--src`、`--dst`、`--source`（`otlp` / `ebpf`）、`--protocol`、`--dst-port`、`--min-requests`、
  `--from-ts`、`--to-ts`、`--limit`。
- 验收：WHEN 用户执行 `vmctl data ebpf-events`，THE vmctl SHALL 请求 `POST /v1/ebpf/events/search`，支持
  `--event-type`、`--process-name`、`--query`、`--from-ts`、`--to-ts`、`--limit`；WHEN 用户执行
  `vmctl data ebpf-capability`，THE vmctl SHALL 请求 `GET /v1/ebpf/capability`。
- 验收：WHEN 用户执行 `vmctl data streams`，THE vmctl SHALL 请求 `GET /v1/streams` 并透传正文；WHEN 用户传入
  `--data-id <item_id>` 或 `--agent-id <id>`，THE vmctl SHALL 在本地过滤 streams 结果后再输出。
- 验收：WHEN 用户执行 `vmctl data apm services`，THE vmctl SHALL 请求 `GET /v1/apm/services`。
- 验收：上述命令的 `--table` 行为沿用 Requirement 4：可选的表格视图，默认仍是透传。
- 验收：WHEN 数据面返回 503 且正文说明功能被关闭（如 `apm_enabled=false`、`apm is disabled`），THE vmctl SHALL
  原样透出该说明并以退出码 1 结束。

### Requirement 11: 服务名别名（链路可见性配置）

- AS 运维人员, I want 在命令行维护服务名归一规则, so that 拓扑与 APM 的服务名不落到 `unknown-<ip>`。
- 验收：WHEN 用户执行 `vmctl data aliases list`，THE vmctl SHALL 请求 `GET /v1/apm/service-aliases`；
  WHEN 用户执行 `vmctl data aliases create --json '<json>'`（或 `-f <path>`），THE vmctl SHALL 请求
  `POST /v1/apm/service-aliases`；WHEN 用户执行 `vmctl data aliases update <alias_id> --json '<json>'`，
  THE vmctl SHALL 请求 `PUT /v1/apm/service-aliases/<alias_id>`；WHEN 用户执行
  `vmctl data aliases delete <alias_id>`，THE vmctl SHALL 请求 `DELETE /v1/apm/service-aliases/<alias_id>`。
- 验收：四个命令 SHALL 透传响应正文。

### Requirement 12: 时序运维

- AS 运维人员, I want 看时序库状态并清掉脏序列, so that 误配置的数据能被回收。
- 验收：WHEN 用户执行 `vmctl data ts stats`，THE vmctl SHALL 请求 `GET /v1/ts/stats` 并透传正文。
- 验收：WHEN 用户执行 `vmctl data ts delete --from-ts <micros> --to-ts <micros>`，THE vmctl SHALL 请求
  `POST /v1/ts/delete`，支持可选的 `--metric` 与可重复的 `--matcher K=V`（`!=` 表示不等）；
  `--from-ts` 与 `--to-ts` 缺失时 SHALL 在发请求前以退出码 1 报错。

### Requirement 13: Agent 配置与接入点、数据面登记

- AS 运维人员, I want 命令行也能登记接入点与数据面、下发 Agent 运行参数, so that 新机器接入不必开页面。
- 验收：WHEN 用户执行 `vmctl collect agent-configs`，THE vmctl SHALL 请求 `GET /api/gse/agent-configs`；
  WHEN 用户执行 `vmctl collect agent-config <agent_id>`，THE vmctl SHALL 请求
  `GET /api/gse/agent-configs/<agent_id>`；WHEN 用户执行
  `vmctl collect set-agent-config --json '<json>'`（或 `-f <path>`），THE vmctl SHALL 请求
  `POST /api/gse/agent-configs`。THE vmctl SHALL NOT 提供 agent-config 的更新或删除子命令——服务端只有
  `GET` 与 `POST` 两条路由。
- 验收：WHEN 用户执行 `vmctl collect access-points list|get <id>|create --json '<json>'|delete <id>`，
  THE vmctl SHALL 分别请求 `GET /api/gse/access-points`、`GET /api/gse/access-points/<id>`、
  `POST /api/gse/access-points`、`DELETE /api/gse/access-points/<id>`。
- 验收：WHEN 用户执行 `vmctl collect dataplanes list|get <id>|create --json '<json>'|delete <id>`，
  THE vmctl SHALL 分别请求 `GET /api/gse/dataplanes`、`GET /api/gse/dataplanes/<id>`、
  `POST /api/gse/dataplanes`、`DELETE /api/gse/dataplanes/<id>`。
- 验收：以上命令 SHALL 透传响应正文；服务端返回 4xx/5xx 时按 Requirement 5 的错误口径处理。

### Requirement 14: 退出码与错误口径一致

- AS 运维人员, I want 所有命令的成败判断一致, so that 脚本能直接接。
- 验收：THE vmctl SHALL 以退出码 0 表示命令成功、1 表示失败（含 HTTP 非 2xx、本地参数错误、文件读取失败），
  2 保留给参数解析失败（沿用 clap 默认行为）。
- 验收：WHEN 请求失败，THE vmctl SHALL 在标准错误至少给出目标 URL 与原因，格式与 `dpc` 现状
  `url=<url> reason=<reason> code=<code>` 保持可读一致；正常输出 SHALL 只出现在标准输出。
- 验收：THE vmctl SHALL 不在任何子命令里打印凭证值（`--token` / `VECTORMAN_TOKEN`）。

### Requirement 15: 范围边界与前置依赖

- AS 维护者, I want 明确本 feature 不动什么, so that 评审能一眼看到风险面。
- 范围边界：THE vmctl SHALL NOT 调用 `/v1/ingest`（数据面内部接口）；SHALL NOT 实现采集项的强类型字段参数
  （`collector` / `storage` 一律由 spec JSON 承载，只有 `--agent-id` 一个覆盖项）；SHALL NOT 实现主机/标签
  选择器展开（目标只能是显式 `agent_id`）；SHALL NOT 引入配置文件与 profile 机制（只认命令行参数与环境变量）；
  SHALL NOT 实现认证与授权逻辑（只做 Bearer 注入，见 Requirement 2）；SHALL NOT 提供采集项预置模板；
  SHALL NOT 改动 `dpc`（保留但冻结，新能力只进 `vmctl data`）；SHALL NOT 改动前端与台账数据模型。
- 前置依赖：`collect status` / `doctor` 依赖 dataserver 的 `GET /v1/streams` 与 `GET /v1/ebpf/capability`
  （已存在）；`collect` 的采集项与 Agent 侧写入依赖 gse-server 的 `/api/gse/collect-items*` 与
  `/api/gse/agent-configs`（已存在）。本 feature 不需要服务端新增任何路由。
- 已知限制（需在使用文档中写明）：gse-server 管理面当前无鉴权（`--token` 只对已启用鉴权的部署生效）；
  dataserver 默认 `NoopAuth`；`agent-configs` 无法更新或删除，改配置只能重新 `POST`；采集项的删除清理窗口取决于
  `storage.retention_days`，删除后历史数据的实际回收由 dataserver 的保留机制决定。
