# Requirements Document

## Introduction

把「Agent 配置」从**只存不生效的台账**做成**以单台 Agent 为单位的期望状态（spec）配置中心**：一份 spec 描述这台 Agent 该用什么运行参数、该采哪些项；保存只写期望，**手动下发**整份 spec；Agent 热应用不重启进程；生效结果回来做逐字段核验与可视化。

现状缺口（2026-09-29 核对代码）：

- `agent_configs` 表只有 `cpu_limit_percent` / `mem_limit_percent` / `log_level`，结构体注释写明「本期仅存储与查询」——HTTP 写入后**没有任何通道把它送到 Agent**；且这三个字段在 Agent 侧**零实现**（全仓 grep 无使用点）。
- `collect_items` 是**全局共享表**（一条 item 带 `agent_ids`，多台 Agent 共用一行）：在某台 Agent 的视角改一条采集项，会同时改掉别的 Agent。
- Agent 本地参数（`crates/gse-agent-core/src/config.rs`）启动时读一次，**没有热加载路径**；改参数必须改文件 + 重启进程。
- 已有可复用的下发原语：`crates/gse-server-core/src/server.rs` 的 `handle_collect_items` / `push_collect_items`（Server 注册 RPC + Agent 注册同名 handler + 认证后拉全表 + 写入后推送）；Agent 侧 `collect::supervise` 的 `reconcile` 已能按 `item_id` 对采集器做增删改对齐。
- 前端「Agent 配置」页只有一个通用表单抽屉；dataplane「采集链路」页是全局采集项列表。

本 feature 的模型（一句话）：**一台 Agent 一份 spec**，spec 里同时装「Agent 运行参数」与「这台的采集项数组」；
Server 只提供三个动作 —— **取 spec / 存 spec / 下发 spec**。采集项因此不再是一等资源，没有独立的增删改查。

## Glossary

- **spec**：某台 Agent 的完整期望状态，含 `params`（Agent 运行参数）与 `items`（采集项数组）。一台 Agent 一份。
- **params**：Agent 运行参数（心跳周期、作业执行、OTLP、`token`、`cpu/mem_limit_percent`、`log_level`）。
- **items**：该 Agent 的采集项数组，元素形状与既有采集项一致，但**不带 `agent_ids`**（归属由所在 spec 决定）。
- **期望值（Desired）**：Server 台账里存的 spec。
- **生效值（Applied）**：Agent 上报的真实在用 spec。
- **revision**：spec 的内容指纹（`sha256(规范化 JSON)` 前 16 位十六进制），覆盖 `params` 与 `items`。
- **下发（Apply）**：把某台 Agent 的期望 spec 整份推给它并取回执。**手动触发，单台 Agent，不批量**。
- **热加载（Hot Reload）**：不重启进程即让新 spec 生效；仅 `token` 变更触发一次主动重连（非重启）。
- **not_enforced**：Agent 收到并记录、但本期**没有真实实现**的字段清单（显式上报，不假装生效）。
- **脱敏哨兵（Mask Sentinel）**：API 读出的敏感字段占位值 `"***"`；写请求里出现该值表示「保持不变」。
- **不可变字段（Immutable）**：`server_addr` / `agent_id` —— 不下发，只能改本地文件；随生效上报只读展示。
- **本地重读**：改本地 `gse-agent.toml` 后发 `SIGHUP`（systemd `systemctl reload`）让 Agent 重读并热应用。
- **sync_status**：`synced` / `stale` / `rejected` / `unknown`，描述「期望值与生效值是否一致」。
- 其余术语沿用 gse-server-agent、gse-server-cmdb、gse-node-app、gse-dataplane-ingest。

## Requirements

### Requirement 1: 以单台 Agent 为单位的 spec 模型与优先级

- AS 运维人员, I want 一台 Agent 只有一个地方描述它的全部期望, so that 不用在多个页面之间对账。
- 验收：THE spec SHALL 为 `{params, items}` 两份内容的组合；THE 下发与生效回执 SHALL 以**整份 spec** 为单位，
  SHALL NOT 支持按字段或按采集项单独下发。
- 验收：THE 下发动作 SHALL 只作用于**单台 Agent**；THE API SHALL NOT 提供跨 Agent 的批量下发。
- 验收：Agent 侧取值优先级 SHALL 为 `Server 下发的 spec > 本地 SIGHUP 重读的文件值 > 环境变量 > gse-agent.toml > 内置默认`；
  即**下发赢**，本地手改会被下一次下发（或重连后的自动拉取）覆盖。
- 验收：`server_addr` 与 `agent_id` SHALL 为不可变字段：不出现在下发 payload，不出现在 diff；
  仅在生效上报里作为**只读字段**回传供展示；Agent 收到含这两者的变更 SHALL 以 `rejected` 拒绝且不改任何状态。
- 验收：THE `params` SHALL 覆盖：`heartbeat_interval_secs`、`allowed_interpreters`、
  `job_default_interpreter`、`max_concurrent_jobs`、`job_work_dir`、`otlp_enabled`、`otlp_listen`、
  `otlp_max_body_bytes`、`otlp_token`、`otlp_allowed_cidrs`、`token`、`cpu_limit_percent`、
  `mem_limit_percent`、`log_level`。
- 验收：THE `items` 元素 SHALL 覆盖：`item_id`、`name`、`kind`、`enabled`、`collector`、`storage`；
  SHALL NOT 含 `agent_ids`。

### Requirement 2: spec 的读写与校验

- AS 维护者, I want 存进去的 spec 是合法的, so that 不至于下发一份 Agent 用不了的配置。
- 验收：`GET /api/gse/agents/{agent_id}/spec` SHALL 返回期望 spec（脱敏）、生效 spec（脱敏）、
  逐字段 diff、`not_enforced`、`sync_status`、会话状态与两个时间戳。
- 验收：`PUT /api/gse/agents/{agent_id}/spec` SHALL 整体覆盖期望 spec，SHALL NOT 推送。
- 验收：保存时 THE Server SHALL 校验并拒绝（400 `invalid_argument`，附字段名）：
  ① `heartbeat_interval_secs` 缺失或 ≤ 0；② `heartbeat_interval_secs > heartbeat_timeout_secs / 3`；
  ③ `items` 内 `item_id` 重复或为空；④ `kind` 不在白名单（`metrics_host` / `log_file` /
  `log_k8s_stdout` / `apm_otlp` / `ebpf_network` / `ebpf_tcp` / `ebpf_process` / `ebpf_syscall`）；
  ⑤ 目标 `agent_id` 在 `agents` 台账不存在；⑥ `agent_id == "apply"`（路由保留字）。
- 验收：THE `revision` SHALL 为 `sha256(期望 spec 规范化 JSON)` 的前 16 位十六进制；同内容重复保存 SHALL 不变。
- 验收：`item_id` 由 Server 生成并在该 Agent 的 spec 内保持稳定；Agent 侧按 `item_id` 对齐采集器。

### Requirement 3: 存储与一次性迁移

- AS 维护者, I want 新模型上线不丢已有配置, so that 迁移是可审计的一次性动作。
- 验收：THE 存储 SHALL 使用新建表 `agent_specs`（期望）与 `agent_spec_states`（生效），
  SHALL NOT 依赖给既有表加列（sqlite 无迁移框架，`CREATE TABLE IF NOT EXISTS` 不给旧表加列）。
- 验收：`agents` 表 SHALL 新增可选列 `prev_token`（用于 token 轮换宽限，见 Requirement 7）。
- 验收：WHEN 启动且 `agent_specs` 为空，THE Server SHALL 一次性搬运：
  旧 `agent_configs` 每行 → 对应 Agent 的 spec 的 `params` 三个字段；
  旧 `collect_items` 每行 → 按 `agent_ids` **展开**成各 Agent spec 的 `items` 元素（去掉 `agent_ids` 字段）。
- 验收：WHEN 重复启动，THE Server SHALL NOT 重复搬运或覆盖 `agent_specs` 已有数据。
- 验收：搬运结果 SHALL 记为 `sync_status = stale`（有期望、未下发），供页面提示「迁移后未下发」。

### Requirement 4: 下发通道

- AS 运维人员, I want spec 能可靠送到 Agent, so that 不必登机器改文件。
- 验收：THE 协议 SHALL 新增 RPC 方法 `agent_spec`，**双侧同名注册**（形状照抄 `collect_items`）：
  Agent 认证成功后 SHALL 立即 `call("agent_spec")` 拉取本 Agent 的期望 spec；
  Server SHALL 注册同名 handler 应答该拉取。
- 验收：THE 既有 `collect_items` RPC 通道 SHALL 被 `agent_spec` **取代**（采集项随 spec 一起下发），
  不再存在独立的采集项下发通道。
- 验收：WHEN 无期望 spec，THE 拉取应答 SHALL 返回空 `revision`，Agent 保持本地基线（不回退成内置默认）。
- 验收：WHEN 触发下发，THE Server SHALL `call("agent_spec", <期望 spec>)` 并等待回执；
  WHEN 目标 Agent 无会话或会话非 `Online`，THE Server SHALL 返回 `agent_offline` 且 SHALL NOT 写生效状态。
- 验收：`POST /api/gse/agents/{agent_id}/spec/apply` SHALL 返回 `{ok, ack}`，`ack` 含 `revision`、`outcome`、
  `applied`、`not_enforced`、`detail`。

### Requirement 5: 一致性与自动收敛

- AS 运维人员, I want Agent 不会长期跑旧配置, so that 手动下发只是「立即生效」，不是「唯一生效」。
- 验收：Agent 认证成功后 SHALL 自动拉取并应用最新期望 spec（即「重连 = 收敛」）。
- 验收：THE 幂等性 SHALL 成立：`revision` 与已应用值相同时，Agent SHALL 回 `unchanged`，
  SHALL NOT 重启任何采集器、SHALL NOT 重建作业执行器、SHALL NOT 重连。
- 验收：WHEN 一台 Agent 的 spec 里删掉了某条采集项，THE Agent SHALL 在收到该 spec 后停止该采集项
  （按 `item_id` 对齐时整表里没有该项 → 停掉对应采集器），SHALL NOT 影响其余采集项的采集位置。
- 验收：WHEN Agent 侧改动只涉及 `otlp_*`，THE Agent SHALL 只重启 `apm_otlp` 类采集项。

### Requirement 6: Agent 热加载

- AS 运维人员, I want 改配置不重启进程、不断采集, so that 调整不产生观测断点。
- 验收：THE Agent SHALL 按下表生效；除 `token` 外 SHALL NOT 断开连接、SHALL NOT 重启进程：

  | 内容 | 生效方式 | 需重连 |
  | --- | --- | --- |
  | `heartbeat_interval_secs` | 心跳循环每轮读取最新值 | 否 |
  | `allowed_interpreters` / `job_default_interpreter` / `job_work_dir` | 作业执行器读取最新值，作用于**新**作业 | 否 |
  | `max_concurrent_jobs` | 重建并发信号量，在跑作业不受影响 | 否 |
  | `otlp_*` | 更新共享参数并触发采集项重对齐（重绑 listener） | 否 |
  | `items` | 按 `item_id` 对齐：新增/变更重启该采集器，移除/停用则停止 | 否 |
  | `token` | 更新并主动重连、用新 token 重新认证 | 是（重连，非重启） |
  | `cpu_limit_percent` / `mem_limit_percent` / `log_level` | 仅记录与上报（见 Requirement 11） | 否 |

- 验收：WHEN Agent 收到 `SIGHUP`，THE Agent SHALL 重读其配置文件（`GSE_AGENT_CONFIG` / 默认
  `./gse-agent.toml`）并**热应用**新值，SHALL NOT 退出进程、SHALL NOT 要求重连。
- 验收：WHEN 本地文件读取或解析失败，THE Agent SHALL 保留当前生效配置、打印错误并继续运行
  （SHALL NOT 因一次坏重读而退出或清空配置）。
- 验收：THE Agent SHALL NOT 做文件监听（不引入文件监听依赖）；`SIGHUP` 是本地热应用的唯一触发方式。

### Requirement 7: token 轮换的双凭据宽限

- AS 运维人员, I want 轮换 token 不会让 Agent 永久失联, so that 一次网络抖动不会变成无法恢复的事故。
- 验收：WHEN 期望 spec 里的 `token` 变更，THE Server SHALL 把旧值写入 `agents.prev_token`
  并在同一写请求内更新 `agents.token`；THE 认证 SHALL 同时接受 `token` 与 `prev_token`。
- 验收：WHEN Agent 用新 `token` 认证成功，THE Server SHALL 清除该 Agent 的 `prev_token`。
- 验收：THE 宽限 SHALL 无时间窗（不依赖时钟），仅由「Agent 已用新 token 认证成功」这一事实终结。

### Requirement 8: 生效核验与逐字段 diff

- AS 运维人员, I want 看到「下发了但没生效」的字段, so that 排查不用猜。
- 验收：Agent 对下发 SHALL 回执 `{revision, outcome, applied, not_enforced, detail}`，
  `outcome` ∈ `applied` / `unchanged` / `partial` / `rejected`。
- 验收：THE Server SHALL 落库 `applied`、`not_enforced`、`outcome`、上报时间，并计算**逐字段 diff**
  （`params` 逐字段 + `items` 按 `item_id` 的增/删/改集合）后一并落库。
- 验收：THE `sync_status` SHALL 按下列规则派生：无 state → `unknown`；
  `state.outcome = rejected` → `rejected`；`state.revision == desired.revision` → `synced`；
  其余 → `stale`。
- 验收：Agent 心跳 SHALL 携带当前生效快照作为**一次性补报**（连接后首拍、以及每次应用后的下一拍各一次），
  Server 确认后 Agent 清除；Server 收到与库中不一致的 `revision` SHALL 刷新 state 与 diff。
- 验收：WHEN 心跳未被服务端确认，THE Agent SHALL 在下一拍继续携带（上限 N 次后放弃并打日志）。

### Requirement 9: 脱敏

- AS 安全负责人, I want 凭据不出现在页面与 API 响应里, so that 台账与截图不会泄漏 token。
- 验收：`token` 与 `otlp_token` 在所有 HTTP 响应（列表、spec 详情、diff、applied）里 SHALL 输出脱敏哨兵
  `"***"`；原值为空 SHALL 输出空字符串（区分「未设置」与「已设置」）。
- 验收：WHEN 写请求里敏感字段为脱敏哨兵或空字符串，THE Server SHALL 保留原值；
  清空 SHALL 需要显式传 `null`；哨兵但无既有值 SHALL 返回 400 `invalid_argument`。
- 验收：THE Agent SHALL NOT 在 `applied` 里回声 `token` 与 `otlp_token` 的值。
- 验收：Server 日志与错误消息 SHALL NOT 打印敏感字段的实际值。

### Requirement 10: 可视化

- AS 运维人员, I want 一屏看清期望值、生效值、差异, so that 不必来回切页面对账。
- 验收：THE「Agent 配置」页 SHALL 提供列表：`agent_id`、`host_id`、会话状态、`sync_status`、
  `revision`、`updated_at`、`reported_at`、操作（查看 / 编辑 / 下发）。
- 验收：THE 详情 SHALL 是**独立整页**（`/agent-configs/{agent_id}`，不是抽屉），含四个视图：
  ① 分组表单（Agent 参数：作业执行 / OTLP / 资源与日志 / 身份只读）；
  ② 原始 JSON 高级模式（可编辑，非法 JSON 拒绝提交）；
  ③ 逐字段差异（`changed` 高亮、`not_enforced` 标注、采集项按 `item_id` 的增删改）；
  ④ 采集项列表（该 Agent 的 `items`，可增删改，随 spec 一起保存与下发）。
- 验收：WHEN 该 Agent 无期望 spec，THE 表单 SHALL 用 Agent 上报的本地生效值预填。
- 验收：下发 SHALL 有二次确认（复用 antd `Modal.confirm`）；`stale` / `rejected` / `unknown` 在列表可见。
- 验收：THE 前端 SHALL NOT 引入新的第三方依赖（JSON 编辑用 `Input.TextArea` + `JSON.parse` 校验）。
- 验收：dataplane 的「采集链路」页 SHALL 改为**跨 Agent 只读总览**（从各 Agent 的 spec 派生
  `agent_id` / `item_id` / `kind` / `enabled`，叠加既有 stream 上报状态），编辑入口 SHALL 指向 `Agent 配置` 页。
- 验收：THE dataserver SHALL 删除 `/v1/collect-items*` 的读写转发，改为**只读**转发
  `/v1/agent-specs` → `{gse_admin_url}/api/gse/agent-specs`；THE 采集链路总览 SHALL 经该只读转发取数；
  配置编辑 SHALL NOT 经过 dataserver（由 console 直连 gse-server）。

### Requirement 11: 未实现字段必须显式标注

- AS 维护者, I want 不做假功能, so that 配置中心的可信度不被未实现字段污染。
- 验收：`cpu_limit_percent` / `mem_limit_percent` / `log_level` SHALL 存期望值、随 spec 下发送达、
  出现在 `applied` 里，且（有值时）出现在 `not_enforced` 列表。
- 验收：HTTP 响应与前端 SHALL 以可见标注区分「已生效」与「未实现（仅记录）」。
- 验收：升级路径 SHALL 记录在案：CPU/内存限制落到作业子进程 `setrlimit`（`libc` 已是 `gse-agent-core` 依赖）；
  `log_level` 需先给 Agent 一个最小级别门控（现全用 `eprintln!`）。

### Requirement 12: 范围边界与前置依赖

- AS 维护者, I want 明确本 feature 不动什么, so that 评审能一眼看到风险面。
- 范围边界：SHALL NOT 提供跨 Agent 批量下发；SHALL NOT 提供采集项的一等资源 CRUD（含
  `POST/PUT/DELETE /api/gse/collect-items*`）；SHALL NOT 提供「从另一台 Agent 复制采集项」或全局模板库；
  SHALL NOT 做文件监听；SHALL NOT 提供 CLI（`vmctl` 本期不动）；SHALL NOT 建配置历史表与回滚；
  SHALL NOT 做配置审批/双人确认；SHALL NOT 用 cgroup 做资源隔离；SHALL NOT 改 dataserver 的**接入与查询**能力
  （但保留清理的**数据源**必须改，见下方破坏性变更 5），SHALL NOT 改作业协议。
- 破坏性变更（须写进 README 与发布说明）：
  1. `/api/gse/collect-items*` 与 `/api/gse/agent-configs*` 整组路由删除，由
     `/api/gse/agents/{agent_id}/spec*` 与 `/api/gse/agent-specs` 取代；
  2. `collect_items` RPC 通道删除，采集项随 `agent_spec` 下发；
  3. `gse-proto::CollectItem` 去掉 `agent_ids`（改为 spec 内的元素）;
  4. dataplane「采集链路」页由可编辑改为只读总览；
  5. dataserver 的 `/v1/collect-items*` 读写转发删除，改为只读 `/v1/agent-specs`；
     `bins/dataserver/src/cleanup.rs` 的保留清理改为从 `/api/gse/agent-specs` 取 live 采集项
     （口径按 `item_id` 去重，`enabled = false` 不算 live，同一 `item_id` 在多台 Agent 的 spec 里
     `retention_days` 不一致时**取最大值**，宁可不提早删数据）；
     原先由 `DELETE /v1/collect-items/{id}` 写 `retain/{item_id}` 的清理入口改为在 cleanup 内自行发现：
     记 `spec-live/{item_id}` 保存上一轮 live 集合，消失即排清理、回来即撤销删除计划。
     **GSE 不可达必须与「空列表」区分**，否则会把全部采集项误判为已删除。
- 前置依赖：`token` 轮换正确性依赖 Requirement 7 的双凭据宽限；`sync_status` 依赖 Agent 心跳补报；
  前端依赖 `frontend/apps/node` 分层与 `@vectorman/adapters`（均已具备）。
- 已知风险（须在本 feature 内收口或明确接受）：
  1. **gse-server 管理口鉴权**（observability-hardening 待办 1.19）：配置下发是能改所有 Agent 行为的
     敏感写操作。现已提供密码开关 `GSE_SERVER_ADMIN_PASSWORD`（空 = 不认证，只罩 `/api/gse/*`），
     但**默认仍是关闭的**，需要在部署里显式启用；
  2. 一律「下发赢 + 重连自动收敛」意味着**本地手改只在下次下发/重连前有效**，这是刻意的取舍；
  3. 一旦某台 Agent 有过期望 spec，就**没有回到「纯本地 TOML 基线」的路径**（本 feature 不提供删除）。

### Requirement 13: 审计边界

- AS 维护者, I want 知道期望值多久没同步, so that 出问题能定位。
- 验收：THE 台账 SHALL 记录 `updated_at`（期望 spec）与 `reported_at`（生效状态），SHALL NOT 建历史表。
- 验收：THE HTTP 响应 SHALL 暴露这两个时间戳供页面展示「多久没同步」。

## References

- 实现基线：`crates/gse-agent-core/src/config.rs`、`crates/gse-agent-core/src/lib.rs`、
  `crates/gse-agent-core/src/job.rs`、`crates/gse-agent-core/src/collect/mod.rs`、
  `crates/gse-server-core/src/{config,ledger,http,server}.rs`、`crates/gse-proto/src/lib.rs`。
- 可抄的下发形状：`crates/gse-server-core/src/server.rs` 的 `handle_collect_items` /
  `push_collect_items` / `push_collect_items_to`，与 `collect_items_reaches_online_agent` 测试；
  Agent 侧 `crates/gse-agent-core/src/collect/mod.rs` 的 `reconcile`（按 `item_id` 对齐）。
- 前端：`.monkeycode/specs/gse-node-app/`、`.monkeycode/specs/frontend-layered-architecture/`。
- 受影响的既有 spec：`.monkeycode/specs/vmctl-collect-chain/`（collect 部分需重新定范围）、
  `.monkeycode/specs/gse-dataplane-ingest/`（采集项下发通道改为随 spec）、
  `.monkeycode/specs/observability-hardening/`（无鉴权待办与 ConfigMap 口径）。
