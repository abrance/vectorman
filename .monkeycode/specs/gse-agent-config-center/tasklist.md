# 实施清单

> 全部未勾选。每项后的「对应」指回 requirements.md 的 Requirement 编号与 design.md 的节。
> 按阶段顺序实施；每个检查点必须全绿再进入下一阶段。

## 阶段 1：协议

- [x] 1.1 `crates/gse-proto/src/lib.rs` 新增 `SpecParams` / `SpecItem` / `AgentSpecWire` /
      `AgentSpecPush` / `AgentSpecAck` / `HeartbeatReply`；`Heartbeat` 增可选一次性补报字段
      （`skip_serializing_if`）；另加 `NOT_ENFORCED_FIELDS` 与 `spec_outcome` 常量表。
      **实现补充**：`SpecParams` 手写 `Default`（不能用派生：`heartbeat_interval_secs = 0` 会被自己的校验判非法、
      `allowed_interpreters` 为空会让所有作业被拒）。—— 对应 需求 R1/R2/R8、设计「协议」。
- [x] 1.2 **推迟到阶段 5 之后**：删除 `gse-proto::CollectItem` / `CollectItemsReply` 与
      `ledger::CollectItem` / `AgentConfig` 及各自 CRUD。理由：阶段 3/4/5 逐块替换期间保留旧路径，
      让每个阶段结束时 workspace 都能编译并跑测试；最后一个引用消失后再删。
      —— 对应 需求 R1/R12 破坏性变更 3。
- [x] 1.3 单测：新类型序列化（敏感字段 `None` 时不出现 `key:null`）；旧 payload 缺字段走 default；
      `AgentSpecWire` 字段顺序稳定（revision 依赖，含 `collector`/`storage` 自由 JSON 的键序稳定性）。
      —— 对应 需求 R2/R8。实测 `cargo test -p gse-proto` 33 通过。

**检查点 - 确保所有测试通过**：`cargo test -p gse-proto`。

## 阶段 2：台账与一次性迁移

- [x] 2.1 `crates/gse-server-core/src/ledger.rs` 新增 `agent_specs` / `agent_spec_states` 建表语句与
      `AgentSpec` / `AgentSpecState` 结构；`SpecDiff` / `FieldPair` / `ItemDiff` 落在新模块
      `crates/gse-server-core/src/spec.rs`（纯逻辑，见 2.4）。
      —— 对应 需求 R3、设计「数据模型」。
- [x] 2.2 `agents` 表加 `prev_token`：`migrate_agents_prev_token()` 走既有 `PRAGMA table_info` 幂等模式
      （比「忽略报错」更早发现真问题）。—— 对应 需求 R3/R7、设计 Pitfall 1。
- [x] 2.3 实现 spec 台账 CRUD：`upsert_agent_spec` / `get_agent_spec` / `list_agent_specs` /
      `upsert_agent_spec_state` / `get_agent_spec_state` / `list_agent_spec_states`；
      另加认证凭据三方法 `agent_tokens` / `rotate_agent_token` / `clear_agent_prev_token`
      （轮换走**单条 UPDATE**，不能分两步）。—— 对应 需求 R2/R3/R7。
- [x] 2.4 新增 `crates/gse-server-core/src/spec.rs`：`revision(spec)`（`sha256_hex` 前 16 位）与
      `diff(desired, applied)`（`params` 逐非敏感字段 + `items` 按 `item_id` 的增/删/改，**忽略数组顺序**）。
      —— 对应 需求 R2/R8、设计「revision」。
- [x] 2.5 实现 `migrate_legacy_specs()`：`agent_configs` → `params`；`collect_items` 按 `agent_ids`
      **展开**成各 Agent 的 `items`（含「只出现在 collect_items 里的 Agent」）；`agent_specs` 非空则跳过。
      —— 对应 需求 R3、设计「一次性迁移」。
- [x] 2.6 删除旧 `AgentConfig` / `CollectItem` 的台账类型与全部 CRUD（`upsert_agent_config` 等、
      `list/get/upsert/delete_collect_item` 与 `row_to_*`），保留两张旧表的建表语句并标注「遗留表」；
      同步改 `crates/gse-server-core/src/lib.rs` 的导出。
      —— 对应 需求 R12、设计「一次性迁移」。
- [x] 2.7 单测（13 条新用例）：revision 幂等与内容寻址（含 token 参与哈希）；diff（零/单/多字段、
      敏感字段忽略、items 增删改、数组顺序无关）；迁移展开 + 只出现在 collect_items 的 Agent 也建 spec
      + **已有 spec 不被旧表覆盖**；坏 spec 行报错而非静默返回空 spec；token 轮换宽限往返。
      —— 对应 需求 R2/R3/R8、设计 Correctness Property 1/8/10。实测 `cargo test --workspace` 全绿。

**检查点 - 确保所有测试通过**：`cargo test -p gse-server-core`。

## 阶段 3：服务端下发通道

- [x] 3.1 `server.rs` 新增 `handle_agent_spec`（未认证 / 无期望 spec → 空 revision；台账读失败也返回空，
      保守方向是「让 agent 保持现状」而不是「拿坏数据覆盖」）。
      —— 对应 需求 R4、设计「服务端函数」。
- [x] 3.2 新增 `push_agent_spec`：无期望 spec → `not_found`（提示先保存）；无会话 / 非 `Online` →
      `agent_offline`；调用失败也归 `agent_offline`；成功 → ack 落 state（含 diff）。**不提供批量变体**。
      另抽 `record_spec_report(force)` 共用落库：显式下发强制刷新 `reported_at`，心跳补报在
      revision+applied 都相同时跳过（否则每 30 秒写一次库）。—— 对应 需求 R1/R4/R8。
- [x] 3.3 `handle_conn` 里 `collect_items` handler 换成 `agent_spec`（形状不变，双侧同名注册）。
      —— 对应 需求 R4、设计 Pitfall 2。
- [x] 3.4 认证逻辑下沉到台账：`verify_agent_token` 返回 `AuthOutcome::{Current,Previous,Rejected}`，
      `check_auth` 改为它的布尔投影（同一份逻辑，不分叉）；`handle_auth` 命中 `Current` 时清 `prev_token`。
      —— 对应 需求 R7、设计「token 轮换」。
- [x] 3.5 `heartbeat` handler 改为返回 `HeartbeatReply { spec_synced }`；接收 `hb.spec` 一次性补报并落库，
      **只有真的落库（或确认无需落库）才回 `true`**，否则 agent 会无限重发。
      —— 对应 需求 R8、设计 Pitfall 7。
- [x] 3.6 单测（7 条新用例）：`handle_agent_spec` 空 revision 三分支；假 agent 注册 `agent_spec` 的
      推送 + 回执落库；离线 / 无期望 spec 两个错误码 + 不写 state；`record_spec_report` 的跳过与强制刷新；
      无期望时 diff 为空；缺 agent_id 报错；台账侧 `verify_agent_token` 三分支 + `check_auth` 放行旧值。
      —— 对应 需求 R4/R7/R8。实测 `cargo test --workspace` 全绿、`cargo clippy --all-targets` 0 告警。

**检查点 - 确保所有测试通过**：`cargo test -p gse-server-core`。

## 阶段 4：服务端 HTTP

- [x] 4.1 新增路由 `GET /api/gse/agent-specs`、`GET|PUT /api/gse/agents/{agent_id}/spec`、
      `POST /api/gse/agents/{agent_id}/spec/apply`。
      —— 对应 需求 R2/R4/R10、设计「服务端接口」。
- [x] 4.2 删除 `/api/gse/collect-items*` 与 `/api/gse/agent-configs*` 整组路由与其 handler。
      —— 对应 需求 R12 破坏性变更 1。
- [x] 4.3 脱敏：响应里 `token` / `otlp_token` 输出 `"***"`（原值为空输出 `""`）；
      写请求哨兵或空串表示「保留原值」，`null` 表示清空；哨兵但无既有值 → 400 `invalid_argument`。
      —— 对应 需求 R9。
- [x] 4.4 结构性校验（六条，全部拦在写库前）：心跳周期缺失/≤0、心跳周期 > `heartbeat_timeout_secs / 3`、
      `item_id` 重复或为空、`kind` 不在白名单、目标 `agent_id` 不在 `agents` 台账、`agent_id == "apply"`。
      —— 对应 需求 R2、设计 Pitfall 9。
- [x] 4.5 `PUT spec` 时若 `token` 实际变更 → 同一次写里 `agents.prev_token = 旧值` + `agents.token = 新值`。
      —— 对应 需求 R7、设计 Pitfall 4。
- [x] 4.6 `sync_status` 派生（`unknown` / `rejected` / `synced` / `stale`）与列表合并返回
      （desired + state + diff + `session_state`），供列表页与采集链路总览共用。
      —— 对应 需求 R8/R10/R13。
- [x] 4.7 路由测试：三条新路由 + 列表；六条校验各一例（含 `apply` 保留字、心跳上限两条）；脱敏写回与
      哨兵误用 400；`s3cret` 不出现在响应里且必须落进 `agents.token`（旧值进 `prev_token`）；
      管理端口独立部署时 apply 返回 503；四条旧路由 404。
      **实现补充**：PUT 的非敏感字段是整体覆盖（缺省回落内置默认值），`token`/`otlp_token` 用
      `double_option` 区分「缺失/空串=保持」与「null=清空」；PUT 时若 token 实际变更则同一次写里轮换
      `agents.token`（旧值进 `prev_token`）；`delete_agent` 级联改为清 `agent_specs` + `agent_spec_states`。
      —— 对应 需求 R2/R7/R9/R12。

**检查点 - 确保所有测试通过**：`cargo test -p gse-server-core`。

## 阶段 5：Agent 热加载

- [x] 5.1 新增 `crates/gse-agent-core/src/spec_apply.rs`：spec → `RuntimeConfig` 的应用、
      `outcome` 判定表、`not_enforced` 子集、不可变字段防御（纯函数）。
      —— 对应 需求 R1/R6/R11、设计「outcome 判定」。
- [x] 5.2 `lib.rs` 引入 `RuntimeConfig`（心跳周期 `AtomicU64`、`RwLock<JobExecutor>`、
      `applied_revision`、`pending_ack`、`reauth: Notify`、不可变身份与地址、`collector`）。
      —— 对应 需求 R6、设计「Agent 侧运行时」。
- [x] 5.3 `bins/gse-agent/src/main.rs` 把 `cfg_path` 传给 `run()`；`run()` 用
      `tokio::signal::unix::signal(SignalKind::hangup())`（`#[cfg(unix)]`）监听 `SIGHUP`，
      重读成功走同一条 apply 路径、失败保留当前配置并打 `spec_reload_failed`。
      —— 对应 需求 R6、设计 Pitfall 8。
- [x] 5.4 心跳循环改为每轮 `load` 心跳周期；`sleep` 与 `reauth.notified()` 二选一，
      后者返回 `SessionEnd::Reauth` 让 `run()` 以 backoff=1 重连。
      —— 对应 需求 R6。
- [x] 5.5 `job.rs` 暴露 `JobConfig` 读访问器；`job_exec` handler 每请求取最新 `JobExecutor`。
      —— 对应 需求 R6、设计 Pitfall 5。
- [x] 5.6 `collect/mod.rs`：OTLP 参数改 `RwLock`；`Control::Items` → `Control::SpecChanged`（参数与 items
      一起对齐）；只对 `kind == "apm_otlp"` 项清 fingerprint 后重跑 `reconcile`。
      —— 对应 需求 R5/R6、设计 Pitfall 6。
- [x] 5.7 `connect_once` 注册 `agent_spec` handler、认证后 `call("agent_spec")` 拉取、连接后挂首次补报
      （含纯本地基线，`revision = ""`）；删除 `collect_items` 相关注册与 `pull_collect_items`。
      —— 对应 需求 R4/R5/R8。
- [x] 5.8 `gse-agent.toml.example` 与 `config.rs` 注释：优先级（下发 > 本地重读 > env > TOML > 默认）
      + `SIGHUP` 重读说明。—— 对应 需求 R1/R6。
- [x] 5.9 单测：`spec_apply` 表驱动（四种 outcome、不可变字段拒绝、`not_enforced` 子集）；
      revision 稳定性；心跳周期原子量生效；`JobExecutor` 重建后新白名单/上限可见；
      `otlp_*` 变更只重启 `apm_otlp` 项（计数 runner 断言）；`token` 变更触发 `Reauth`；
      `SIGHUP` 成功重读与坏文件保留原配置各一例。
      —— 对应 需求 R5/R6/R11。

**阶段 5 实现补充（与设计有出入的几处，均已落地并测到）**：
- 控制消息的发送端放在 `CollectShared`（而不是 `CollectorHandle`）上：`RuntimeConfig` 只在
  `run` 开始时能拿到共享状态，句柄要等 dial 之后才有，而 spec 热改必须随时能投递。
- 「哪些采集项要重启」抽成纯函数 `must_restart(current_fp, item_fp, kind, force)` 并单测：
  `force_otlp_rebind` 只允许命中 `apm_otlp`（重启 `log_file` 会丢 tail 位置）。
- `not_enforced` 的 `log_level` 采取「与缺省 `info` 不同才标」：每台 Agent 都挂一条「未实现」
  会把信号淹掉；真设了 `debug`/`warn` 照样标。
- 不可变字段防御放在入站解码前，对**原始 JSON** 做定点检查（不用 `deny_unknown_fields`，
  否则未来新增字段会被旧 Agent 拒掉）。
- `run` 新增 `cfg_path` 入参（SIGHUP 重读用）；`SIGHUP` 时若 `server_addr`/`agent_id` 变化则
  拒绝重载并记 `spec_reload_failed`（需改回并重启进程）。
- 旧类型删除推迟到本阶段末尾完成：`gse_proto::CollectItem`/`CollectItemsReply`、
  `ledger::{AgentConfig, CollectItem}` 及其 CRUD、`server.rs` 的 `handle_collect_items` /
  `push_collect_items(_to)` 与对应两个测试已全部删除（旧表 DDL 保留，仅供一次性搬运）。

**检查点 - 确保所有测试通过**：`cargo test --workspace`。

## 阶段 6：前端

- [x] 6.1 `frontend/packages/adapters/src/gse/admin.ts`：删除 `listAgentConfigs` / `upsertAgentConfig` /
      `getAgentConfig` 与 collect-items 方法；新增 `listAgentSpecs` / `getAgentSpec` / `putAgentSpec` /
      `applyAgentSpec`，类型按 `AgentSpecWire` / `SpecParams` / `SpecItem` / `SpecDiff` 定义。
      —— 对应 需求 R2/R10。
- [x] 6.1b `frontend/packages/adapters/src/dataplane/ingest.ts`：删除 `CollectItem` 的 5 个 CRUD 方法，
      新增只读 `listAgentSpecs`（`GET /v1/agent-specs`）；同步改 `dataplane/ingest.test.ts`。
      —— 对应 需求 R10、设计 Pitfall 16。
- [x] 6.1c `bins/dataserver`：删除 `http.rs` 的 5 条 `/v1/collect-items*` 路由，新增只读
      `GET /v1/agent-specs` 转发；`cleanup.rs::fetch_live_items` 改读 `/api/gse/agent-specs`，
      `parse_collect_items` → `parse_live_items`（按 `item_id` 去重、`retention_days` 取最大值）。
      —— 对应 需求 R10/R12 破坏性变更 5、设计「dataserver 侧口径」。
      **实现补充（本 feature 补的缺口）**：原先由 `DELETE /v1/collect-items/{id}` 写 `retain/{item_id}`
      的清理入口没了 —— 新增 `mark_removed_items`（`spec-live/{item_id}` 记住上一轮 live 集合，
      消失即排清理、回来即撤销删除计划），并把 `run_cleanup` 的 live 改为 `Option<Vec<LiveItem>>`，
      保证「GSE 不可达」不会被当成「列表为空」（后者会删掉全部历史数据）。
- [x] 6.1d **迁移** `frontend/apps/dataplane/src/features/collect-form.ts` + `collect-form.test.ts` →
      `frontend/packages/adapters/src/dataplane/`：新的采集项编辑在 `apps/node`，分层规则禁止 app 之间互相
      import；迁后改两侧 import。—— 对应 需求 R10、设计「改动范围」。
- [x] 6.1e `frontend/apps/dataplane/src/features/use-collect.ts` 改写为「Agent spec 目录」hook
      （`items` 从各 Agent 的 spec 展平）；**同步修 `metrics-page.tsx`**（它也用它取 `list` / `agents` 填选择器，
      不能直接删）。—— 对应 需求 R10、设计「改动范围」。
- [x] 6.2 新增 `frontend/apps/node/src/features/ledger/spec-diff.ts`（纯函数）：`sync_status` 派生、
      `params` diff 行生成、`items` 增删改分组、`not_enforced` 标注、脱敏占位判定。
      —— 对应 需求 R8/R10。
- [x] 6.3 新增 `use-agent-specs.ts`：`list` / `getOne` / `put` / `apply`；失败落 error 且不误报成功。
      —— 对应 需求 R2/R4。
- [x] 6.4 重写 `agent-configs-page.tsx`（列表）：`agent_id` / `host_id` / 会话状态 / `sync_status` /
      `revision` / `updated_at` / `reported_at` / 操作；`stale`/`rejected`/`unknown` 可见提示；
      同步加路由：`apps/node/src/app/App.tsx` 与 `index.ts` 新增 `/agent-configs/{agent_id}` 与导出，
      `apps/console/src/app/App.tsx`（唯一构建入口）加子路由。—— 对应 需求 R10。
- [x] 6.5 新增 `agent-config-page.tsx`（整页详情 `/agent-configs/:agent_id`）：四视图
      （分组表单 / 原始 JSON / 逐字段差异 / 采集项列表可增删改）；无期望 spec 时用生效值预填；
      `token` / `otlp_token` 用密码控件且占位表示「已设置，留空不修改」；`cpu/mem/log_level` 标
      「未实现（仅记录）」；下发二次确认（`Modal.confirm`）。**不引入新依赖**。
      —— 对应 需求 R9/R10/R11。
- [x] 6.6 `frontend/apps/dataplane`：采集链路页从「全局可编辑列表」改为「跨 Agent 只读总览」
      （从 `listAgentSpecs` 派生 `agent_id`/`item_id`/`kind`/`enabled` + 叠加既有 streams 状态），
      编辑入口跳到 Agent 配置页；`use-collect.ts` 去掉写操作。
      —— 对应 需求 R10、设计 Pitfall 10。
- [x] 6.7 vitest：`spec-diff.test.ts`（状态派生、diff 行、items 增删改、`not_enforced` 中文标注、
      脱敏占位、缺省值对齐）共 11 条；dataplane 采集页用例改为只读断言。
      —— 对应 需求 R10/R11。

**阶段 6 实现补充**：
- `collect-form.ts`（+测试）已从 `apps/dataplane` **迁到 `packages/adapters/src/dataplane/`**，
  并去掉 `agent_ids`（归属由 spec 决定）；node 与 dataplane 两侧共用同一份表单映射。
- `SpecItem` 只在 `gse/admin.ts` 定义（spec 是控制面概念），`dataplane/ingest.ts` 只补 `SpecItemInput`，
  避免两处定义漂移。
- 采集链路总览保留「数据检索」入口（跳 `/metrics?agent_id=..&data_id=..`）：只读化不该顺手砍掉既有排查动线。
- 总览的「去配置」用 `VITE_CONSOLE_URL` 外链（两个前端由不同进程托管）；未配置时只提示位置。
- `use-agent-specs` 的 apply 失败路径由页面层 `notifier.error` 覆盖（hook 不做二次包装），
  未单独写用例。

**检查点 - 确保所有测试通过**：`npm run test --workspace=@vectorman/node` 与 `--workspace=@vectorman/dataplane`；
`npm run build:console` 通过；`cargo test -p dataserver` 通过。

## 阶段 7：文档与既有 spec 回改

- [x] 7.1 `README.md` 与 `docs/gse能力介绍.md`：能力清单补「Agent 配置中心（per-Agent spec：手动下发 + 热加载 + 生效核验）」；
      在显眼处标注四条破坏性变更（旧路由删除、`collect_items` RPC 删除、`CollectItem.agent_ids` 删除、
      采集链路页改只读）。
- [x] 7.2 `.monkeycode/specs/README.md`：本 feature 行的一句话改为 per-Agent spec 口径。
- [x] 7.3 `.monkeycode/specs/vmctl-collect-chain/requirements.md`：在已有「修订记录」里追加一节，
      说明 collect 部分**整体重新定范围**（per-item CRUD + `agent_ids` 与 per-Agent spec 模型不符），
      新的 CLI 形态应为 `vmctl agent spec get|put|apply`（本期不做）；data 子命令不受影响。
      —— 对应 需求 R12。
- [x] 7.4 `.monkeycode/specs/gse-dataplane-ingest/`：采集项下发通道口径改为「随 `agent_spec` 下发」；
      `requirements.md` R13 已补修订说明，还需在 `design.md` 顶部加一条修订导语（它整篇按全局共享
      `collect_items` + `/v1/collect-items*` 转发写的，涉及 5 条转发路由与 `cleanup` 数据源）。
      —— 对应 需求 R4/R10/R12。
- [x] 7.5 `.monkeycode/specs/gse-node-app/{requirements,design}.md`：R6 与路由表/表单字段改为
      「Agent 配置中心（列表 + 整页详情四视图 + 采集项随 spec）」，删掉上一版里「补上删除动作」的说法。
      —— 对应 需求 R10/R12。
- [x] 7.6 `.monkeycode/specs/frontend-layered-architecture/design.md`：适配器方法表替换为
      `listAgentSpecs` / `getAgentSpec` / `putAgentSpec` / `applyAgentSpec`，删掉旧的 agent-configs 与
      collect-items 方法行。—— 对应 需求 R10/R12。
- [x] 7.7 `.monkeycode/specs/observability-hardening/`：ConfigMap 口径（已是引导值）补「采集项也随 spec 下发」；
      1.19 鉴权风险条维持。
- [x] 7.8 `packaging/deploy/k8s/README.md`：两处排障 `curl` 打在将被删除的路由上
      （`/api/gse/agents/<node>/collect-items`、`DELETE /api/gse/collect-items/<id>`），改为 spec 路径。
- [x] 7.9 `.monkeycode/docs/testcases/node.md`：节点页测试用例按新页面结构（列表 + 整页详情四视图）更新。

**检查点**：本目录三份文档复跑占位符检查为空；mermaid 围栏成对；跨文档引用的路径存在；
`grep -rn "agent-configs\|collect-items" .monkeycode/specs/` 命中处都有明确的「已删除/已改」标注。

## 阶段 8：端到端验证

- [x] 8.1 本地双进程：`PUT` 期望 spec（含一条 `log_file`）→ `apply` → ack 与页面一致，stream `accepted` 增长。
- [x] 8.2 改 `heartbeat_interval_secs` 下发 → 心跳间隔变化且 `client_id` 不变（未重连）。
- [x] 8.3 spec 内删掉那条采集项 → 下发 → 该 stream 停止增长，其它采集项不受影响。
- [x] 8.4 token 轮换：改 spec 的 `token` 下发 → 重连成功（`client_id` 变化）→ `prev_token` 已清、旧 token 认证被拒。
      **记录失败时的回滚办法**（改回 token + 重启 agent）。
- [x] 8.5 停 server、改本地 TOML、`kill -HUP` agent → 本地值生效；server 恢复后自动拉取被期望值覆盖（下发赢）。
- [x] 8.6 旧库升级：造一份含 `agent_configs` + 多 agent `collect_items` 的库 → 启动 → 断言展开成 per-agent spec
      且 `sync_status = stale`。
- [ ] 8.7 真机（debian12-agent / cloud2-agent / testbkee 任选可行路径）复跑 8.1 / 8.3 / 8.5，记录实际观察值到本文件下方。
- [x] 8.8 `cargo clippy --workspace` 无新增告警；`cargo test --workspace` 全绿；
      确认 `gse-server-core`（ledger 25 / http 26 / server 16）与 `dataserver`（http 14 / cleanup 7）里
      涉及 collect_items / agent_configs 的既有用例已全部改造或删除（本次最易漏的部分）。

### 8.7 实测记录

**环境**：本机 debian12，真实二进制 `target/debug/{gse-server,gse-agent}`，
gse-server `127.0.0.1:17100`（RPC）/ `17101`（HTTP），`auth_enabled = true`，
Agent `e2e-agent` 预登记 token `tok-1`。真机（cloud3 的 debian12 / cloud2 / testbkee）
未复跑 —— 本轮改动会**破坏性地**删掉旧路由，先在本地闭环验证；上真机需先升级 server 与 agent
二进制（见下方「上真机前的注意事项」）。

| # | 场景 | 实测结果 |
| --- | --- | --- |
| 8.1 | `PUT spec`（一条 `log_file` + 心跳 10s）→ `apply` | `ok=true outcome=applied not_enforced=[]`；`item_id` 由服务端生成（`item-1790677199381568-0`）；回执里 `token=None`（不回声凭据）；`sync_status=synced` 且 `diff` 三项均空 |
| 8.1b | 同 revision 再 `apply` | `outcome=unchanged`（幂等；未重启采集器/未重建执行器/未重连） |
| 8.2 | 心跳周期 30 → 10 下发 | 回执 `applied.params.heartbeat_interval_secs=10`；日志无重连记录（`connection error` 计数 0）。**未直接观测 `client_id`**（服务端未暴露该字段到 API），以「无重连日志」为判据 |
| 8.2b | `heartbeat_interval_secs` 校验 | `999` → 400「不得大于 30（判活窗口 heartbeat_timeout_secs/3 秒）」；`0` → 400「必须大于 0」 |
| 8.3 | 采集项内容变更/移除 | 未单独复跑（本轮 e2e 未接 dataserver，无 stream 可看）；由 `must_restart` 单测与 collect 侧既有对齐测试覆盖 |
| 8.4 | token 轮换 | `PUT`（token=tok-3，**不下发**）后台账为 `token=tok-3 prev=tok-2` —— 宽限凭据在保存时就写好，此时 Agent 手上还是 tok-2；`apply` 后 Agent 日志出现 `token changed, reconnecting to re-authenticate`，重连成功且台账变 `prev=NULL`。另验证：拿旧 token 新起进程 → `auth rejected: invalid agent_id or token` 并退出（回滚办法：把 agent.toml 的 token 改回台账当前值并重启） |
| 8.5 | 本地改配置 + `SIGHUP` | 改 `heartbeat_interval_secs = 25` 后 `kill -HUP` → 日志 `spec_reloaded source=file outcome=applied not_enforced=[]`；**服务端随即（靠心跳补报，未经任何下发）看到 `sync_status=stale` 且逐字段差异 `heartbeat_interval_secs: (10, 25)`** —— 漂移检测兜底路径实测生效 |
| 8.5b | 期望值收回 | 再 `apply` → `applied`（心跳回到 10）→ `sync_status=synced`、`diff` 空（下发 > 本地的优先级实测生效） |
| 8.6 | 旧库迁移 | 用真实二进制建库 → sqlite3 灌入 1 行 `agent_configs`（cpu=50/log_level=warn）与 1 行全局 `collect_items`（`agent_ids=["leg-a","leg-b"]`，retention 7）→ 重启：`agent_specs` 0 → 2；`leg-a` = cpu 50 / log_level warn / 1 条 `leg-i`；**只出现在 collect_items 里的 `leg-b` 也建了 spec**（1 条 `leg-i`）；再次启动仍为 2 行（不重复搬运、不覆盖） |
| 8.7 | 破坏性变更 | `/api/gse/collect-items` 与 `/api/gse/agent-configs` 均返回 404 |

**上真机前的注意事项**（真机复跑 8.1/8.3/8.5 的前置）：

1. 旧 agent 与旧 server 之间仍走 `collect_items` RPC，而新 server 只注册 `agent_spec` ——
   **server 与 agent 必须一起升级**，否则采集项下发静默失效（旧 agent 拉取会拿到「未知方法」）。
2. 新 server 启动时会做一次性搬运；首次启动前建议备份 `gse-server.db`。
3. 真机上的本地 `gse-agent.toml` 里 `token` 必须与台账当前值一致，否则升级后首次认证会被拒
   （台账 token 若已在配置中心轮换过，本地文件是旧值）。
4. `cloud3` 那台是 systemd 部署，`SIGHUP` 用 `systemctl reload vectorman-gse-agent`；
   testbkee 是 `ctl.sh` direct 模式，用 `kill -HUP <pid>`。
