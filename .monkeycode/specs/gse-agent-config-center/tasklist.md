# 实施清单

> 全部未勾选。每项后的「对应」指回 requirements.md 的 Requirement 编号与 design.md 的节。
> 按阶段顺序实施；每个检查点必须全绿再进入下一阶段。

## 阶段 1：协议

- [ ] 1.1 `crates/gse-proto/src/lib.rs` 新增 `SpecParams` / `SpecItem` / `AgentSpecWire` /
      `AgentSpecPush` / `AgentSpecAck` / `HeartbeatReply`；`Heartbeat` 增可选一次性补报字段
      （`skip_serializing_if`）。—— 对应 需求 R1/R2/R8、设计「协议」。
- [ ] 1.2 删除 `CollectItem` 的 `agent_ids` 与 `CollectItemsReply`，删除 `CollectItem`，
      改 `pub struct SpecItem`；同步修所有编译错误（`gse-server-core`、`gse-agent-core`）。
      —— 对应 需求 R1/R12 破坏性变更 3。
- [ ] 1.3 单测：新类型序列化（敏感字段 `None` 时不出现 `key:null`）；旧 payload 缺字段走 default；
      `AgentSpecWire` 字段顺序稳定（revision 依赖）。—— 对应 需求 R2/R8。

**检查点 - 确保所有测试通过**：`cargo test -p gse-proto`。

## 阶段 2：台账与一次性迁移

- [ ] 2.1 `crates/gse-server-core/src/ledger.rs` 新增 `agent_specs` / `agent_spec_states` 建表语句与
      `AgentSpec` / `AgentSpecState` / `SpecDiff` / `FieldPair` / `ItemDiff` 结构。
      —— 对应 需求 R3、设计「数据模型」。
- [ ] 2.2 `agents` 表加 `prev_token`：`ALTER TABLE agents ADD COLUMN prev_token TEXT`，
      忽略「列已存在」错误（唯一允许的加列场景）。
      —— 对应 需求 R3/R7、设计 Pitfall 1。
- [ ] 2.3 实现 spec 台账 CRUD：`upsert_agent_spec` / `get_agent_spec` / `list_agent_specs` /
      `upsert_agent_spec_state` / `get_agent_spec_state` / `list_agent_spec_states`。
      —— 对应 需求 R2/R3。
- [ ] 2.4 实现 `revision(spec)`（`hashutil::sha256_hex` 前 16 位）与 `SpecDiff` 计算
      （`params` 逐非敏感字段 + `items` 按 `item_id` 的增/删/改）。
      —— 对应 需求 R2/R8、设计「revision」。
- [ ] 2.5 实现 `migrate_legacy_tables()`：`agent_configs` → `params`；`collect_items` 按 `agent_ids`
      **展开**成各 Agent 的 `items`（含"只出现在 collect_items 里的 Agent"）；`agent_specs` 非空则跳过。
      —— 对应 需求 R3、设计「一次性迁移」。
- [ ] 2.6 删除旧 `AgentConfig` / `CollectItem` 的台账类型与全部 CRUD（`upsert_agent_config` 等、
      `list/get/upsert/delete_collect_item` 与 `row_to_*`），保留两张旧表的建表语句并标注「遗留表」；
      同步改 `crates/gse-server-core/src/lib.rs` 的导出。
      —— 对应 需求 R12、设计「一次性迁移」。
- [ ] 2.7 单测：revision 幂等；diff（params 多/单/零字段、items 增删改）；迁移展开正确 +
      重复启动不重复写 + 只出现在 collect_items 的 Agent 也建 spec。
      —— 对应 需求 R2/R3/R8、设计 Correctness Property 1/8。

**检查点 - 确保所有测试通过**：`cargo test -p gse-server-core`。

## 阶段 3：服务端下发通道

- [ ] 3.1 `server.rs`：`handle_collect_items` → `handle_agent_spec`（未认证 / 无期望 spec → 空 revision）。
      —— 对应 需求 R4、设计「服务端函数」。
- [ ] 3.2 `push_collect_items` / `push_collect_items_to` → `push_agent_spec`：会话非 `Online` →
      `agent_offline` 且不写 state；成功 → ack 落 state（含 diff）。**不再保留批量 `_to` 变体**
      （单台即可，列表页逐台调用）。—— 对应 需求 R1/R4/R8。
- [ ] 3.3 `handle_conn` 里把 `collect_items` handler 换成 `agent_spec` 同名 handler（形状不变）。
      —— 对应 需求 R4、设计 Pitfall 2。
- [ ] 3.4 `auth` 支持双凭据：`token` 或 `prev_token` 匹配即通过；命中 `prev_token` 的认证 SHALL 不清理，
      命中**新** `token` 时清理 `prev_token`。
      —— 对应 需求 R7、设计「token 轮换」。
- [ ] 3.5 `heartbeat` handler 接收一次性补报：与已存 state 的 revision 比对，不同则刷新 state + diff；
      返回 `HeartbeatReply { spec_synced }`。—— 对应 需求 R8、设计 Pitfall 7。
- [ ] 3.6 单测：`push_agent_spec_reaches_online_agent`（镜像 `collect_items_reaches_online_agent` 的
      假 agent 注册法）；离线不写 state；`handle_agent_spec` 空 revision；auth 双凭据三分支;
      心跳补报落 state。—— 对应 需求 R4/R7/R8。

**检查点 - 确保所有测试通过**：`cargo test -p gse-server-core`。

## 阶段 4：服务端 HTTP

- [ ] 4.1 新增路由 `GET /api/gse/agent-specs`、`GET|PUT /api/gse/agents/{agent_id}/spec`、
      `POST /api/gse/agents/{agent_id}/spec/apply`。
      —— 对应 需求 R2/R4/R10、设计「服务端接口」。
- [ ] 4.2 删除 `/api/gse/collect-items*` 与 `/api/gse/agent-configs*` 整组路由与其 handler。
      —— 对应 需求 R12 破坏性变更 1。
- [ ] 4.3 脱敏：响应里 `token` / `otlp_token` 输出 `"***"`（原值为空输出 `""`）；
      写请求哨兵或空串表示「保留原值」，`null` 表示清空；哨兵但无既有值 → 400 `invalid_argument`。
      —— 对应 需求 R9。
- [ ] 4.4 结构性校验（六条，全部拦在写库前）：心跳周期缺失/≤0、心跳周期 > `heartbeat_timeout_secs / 3`、
      `item_id` 重复或为空、`kind` 不在白名单、目标 `agent_id` 不在 `agents` 台账、`agent_id == "apply"`。
      —— 对应 需求 R2、设计 Pitfall 9。
- [ ] 4.5 `PUT spec` 时若 `token` 实际变更 → 同一次写里 `agents.prev_token = 旧值` + `agents.token = 新值`。
      —— 对应 需求 R7、设计 Pitfall 4。
- [ ] 4.6 `sync_status` 派生（`unknown` / `rejected` / `synced` / `stale`）与列表合并返回
      （desired + state + diff + `session_state`），供列表页与采集链路总览共用。
      —— 对应 需求 R8/R10/R13。
- [ ] 4.7 路由测试：三条新路由 + 列表；六条校验各一例（含 `apply` 保留字）；**响应体不含 token 明文**；
      旧路由 404。—— 对应 需求 R2/R9/R12。

**检查点 - 确保所有测试通过**：`cargo test -p gse-server-core`。

## 阶段 5：Agent 热加载

- [ ] 5.1 新增 `crates/gse-agent-core/src/spec_apply.rs`：spec → `RuntimeConfig` 的应用、
      `outcome` 判定表、`not_enforced` 子集、不可变字段防御（纯函数）。
      —— 对应 需求 R1/R6/R11、设计「outcome 判定」。
- [ ] 5.2 `lib.rs` 引入 `RuntimeConfig`（心跳周期 `AtomicU64`、`RwLock<JobExecutor>`、
      `applied_revision`、`pending_ack`、`reauth: Notify`、不可变身份与地址、`collector`）。
      —— 对应 需求 R6、设计「Agent 侧运行时」。
- [ ] 5.3 `bins/gse-agent/src/main.rs` 把 `cfg_path` 传给 `run()`；`run()` 用
      `tokio::signal::unix::signal(SignalKind::hangup())`（`#[cfg(unix)]`）监听 `SIGHUP`，
      重读成功走同一条 apply 路径、失败保留当前配置并打 `spec_reload_failed`。
      —— 对应 需求 R6、设计 Pitfall 8。
- [ ] 5.4 心跳循环改为每轮 `load` 心跳周期；`sleep` 与 `reauth.notified()` 二选一，
      后者返回 `SessionEnd::Reauth` 让 `run()` 以 backoff=1 重连。
      —— 对应 需求 R6。
- [ ] 5.5 `job.rs` 暴露 `JobConfig` 读访问器；`job_exec` handler 每请求取最新 `JobExecutor`。
      —— 对应 需求 R6、设计 Pitfall 5。
- [ ] 5.6 `collect/mod.rs`：OTLP 参数改 `RwLock`；`Control::Items` → `Control::SpecChanged`（参数与 items
      一起对齐）；只对 `kind == "apm_otlp"` 项清 fingerprint 后重跑 `reconcile`。
      —— 对应 需求 R5/R6、设计 Pitfall 6。
- [ ] 5.7 `connect_once` 注册 `agent_spec` handler、认证后 `call("agent_spec")` 拉取、连接后挂首次补报
      （含纯本地基线，`revision = ""`）；删除 `collect_items` 相关注册与 `pull_collect_items`。
      —— 对应 需求 R4/R5/R8。
- [ ] 5.8 `gse-agent.toml.example` 与 `config.rs` 注释：优先级（下发 > 本地重读 > env > TOML > 默认）
      + `SIGHUP` 重读说明。—— 对应 需求 R1/R6。
- [ ] 5.9 单测：`spec_apply` 表驱动（四种 outcome、不可变字段拒绝、`not_enforced` 子集）；
      revision 稳定性；心跳周期原子量生效；`JobExecutor` 重建后新白名单/上限可见；
      `otlp_*` 变更只重启 `apm_otlp` 项（计数 runner 断言）；`token` 变更触发 `Reauth`；
      `SIGHUP` 成功重读与坏文件保留原配置各一例。
      —— 对应 需求 R5/R6/R11。

**检查点 - 确保所有测试通过**：`cargo test --workspace`。

## 阶段 6：前端

- [ ] 6.1 `frontend/packages/adapters/src/gse/admin.ts`：删除 `listAgentConfigs` / `upsertAgentConfig` /
      `getAgentConfig` 与 collect-items 方法；新增 `listAgentSpecs` / `getAgentSpec` / `putAgentSpec` /
      `applyAgentSpec`，类型按 `AgentSpecWire` / `SpecParams` / `SpecItem` / `SpecDiff` 定义。
      —— 对应 需求 R2/R10。
- [ ] 6.1b `frontend/packages/adapters/src/dataplane/ingest.ts`：删除 `CollectItem` 的 5 个 CRUD 方法，
      新增只读 `listAgentSpecs`（`GET /v1/agent-specs`）；同步改 `dataplane/ingest.test.ts`。
      —— 对应 需求 R10、设计 Pitfall 16。
- [ ] 6.1c `bins/dataserver`：删除 `http.rs` 的 5 条 `/v1/collect-items*` 路由，新增只读
      `GET /v1/agent-specs` 转发；`cleanup.rs::fetch_live_items` 改读 `/api/gse/agent-specs`，
      `parse_collect_items` → `parse_live_items`（按 `item_id` 去重、`retention_days` 取最大值）。
      —— 对应 需求 R10/R12 破坏性变更 5、设计「dataserver 侧口径」。
- [ ] 6.1d **迁移** `frontend/apps/dataplane/src/features/collect-form.ts` + `collect-form.test.ts` →
      `frontend/packages/adapters/src/dataplane/`：新的采集项编辑在 `apps/node`，分层规则禁止 app 之间互相
      import；迁后改两侧 import。—— 对应 需求 R10、设计「改动范围」。
- [ ] 6.1e `frontend/apps/dataplane/src/features/use-collect.ts` 改写为「Agent spec 目录」hook
      （`items` 从各 Agent 的 spec 展平）；**同步修 `metrics-page.tsx`**（它也用它取 `list` / `agents` 填选择器，
      不能直接删）。—— 对应 需求 R10、设计「改动范围」。
- [ ] 6.2 新增 `frontend/apps/node/src/features/ledger/spec-diff.ts`（纯函数）：`sync_status` 派生、
      `params` diff 行生成、`items` 增删改分组、`not_enforced` 标注、脱敏占位判定。
      —— 对应 需求 R8/R10。
- [ ] 6.3 新增 `use-agent-specs.ts`：`list` / `getOne` / `put` / `apply`；失败落 error 且不误报成功。
      —— 对应 需求 R2/R4。
- [ ] 6.4 重写 `agent-configs-page.tsx`（列表）：`agent_id` / `host_id` / 会话状态 / `sync_status` /
      `revision` / `updated_at` / `reported_at` / 操作；`stale`/`rejected`/`unknown` 可见提示；
      同步加路由：`apps/node/src/app/App.tsx` 与 `index.ts` 新增 `/agent-configs/{agent_id}` 与导出，
      `apps/console/src/app/App.tsx`（唯一构建入口）加子路由。—— 对应 需求 R10。
- [ ] 6.5 新增 `agent-config-page.tsx`（整页详情 `/agent-configs/:agent_id`）：四视图
      （分组表单 / 原始 JSON / 逐字段差异 / 采集项列表可增删改）；无期望 spec 时用生效值预填；
      `token` / `otlp_token` 用密码控件且占位表示「已设置，留空不修改」；`cpu/mem/log_level` 标
      「未实现（仅记录）」；下发二次确认（`Modal.confirm`）。**不引入新依赖**。
      —— 对应 需求 R9/R10/R11。
- [ ] 6.6 `frontend/apps/dataplane`：采集链路页从「全局可编辑列表」改为「跨 Agent 只读总览」
      （从 `listAgentSpecs` 派生 `agent_id`/`item_id`/`kind`/`enabled` + 叠加既有 streams 状态），
      编辑入口跳到 Agent 配置页；`use-collect.ts` 去掉写操作。
      —— 对应 需求 R10、设计 Pitfall 10。
- [ ] 6.7 vitest：`spec-diff.test.ts`（`sync_status` 四种、diff 行、items 增删改、脱敏占位）；
      `use-agent-specs` 的 apply 失败路径。
      —— 对应 需求 R10/R11。

**检查点 - 确保所有测试通过**：`npm run test --workspace=@vectorman/node` 与 `--workspace=@vectorman/dataplane`；
`npm run build:console` 通过；`cargo test -p dataserver` 通过。

## 阶段 7：文档与既有 spec 回改

- [ ] 7.1 `README.md` 与 `docs/gse能力介绍.md`：能力清单补「Agent 配置中心（per-Agent spec：手动下发 + 热加载 + 生效核验）」；
      在显眼处标注四条破坏性变更（旧路由删除、`collect_items` RPC 删除、`CollectItem.agent_ids` 删除、
      采集链路页改只读）。
- [ ] 7.2 `.monkeycode/specs/README.md`：本 feature 行的一句话改为 per-Agent spec 口径。
- [ ] 7.3 `.monkeycode/specs/vmctl-collect-chain/requirements.md`：在已有「修订记录」里追加一节，
      说明 collect 部分**整体重新定范围**（per-item CRUD + `agent_ids` 与 per-Agent spec 模型不符），
      新的 CLI 形态应为 `vmctl agent spec get|put|apply`（本期不做）；data 子命令不受影响。
      —— 对应 需求 R12。
- [ ] 7.4 `.monkeycode/specs/gse-dataplane-ingest/`：采集项下发通道口径改为「随 `agent_spec` 下发」；
      `requirements.md` R13 已补修订说明，还需在 `design.md` 顶部加一条修订导语（它整篇按全局共享
      `collect_items` + `/v1/collect-items*` 转发写的，涉及 5 条转发路由与 `cleanup` 数据源）。
      —— 对应 需求 R4/R10/R12。
- [ ] 7.5 `.monkeycode/specs/gse-node-app/{requirements,design}.md`：R6 与路由表/表单字段改为
      「Agent 配置中心（列表 + 整页详情四视图 + 采集项随 spec）」，删掉上一版里「补上删除动作」的说法。
      —— 对应 需求 R10/R12。
- [ ] 7.6 `.monkeycode/specs/frontend-layered-architecture/design.md`：适配器方法表替换为
      `listAgentSpecs` / `getAgentSpec` / `putAgentSpec` / `applyAgentSpec`，删掉旧的 agent-configs 与
      collect-items 方法行。—— 对应 需求 R10/R12。
- [ ] 7.7 `.monkeycode/specs/observability-hardening/`：ConfigMap 口径（已是引导值）补「采集项也随 spec 下发」；
      1.19 鉴权风险条维持。
- [ ] 7.8 `packaging/deploy/k8s/README.md`：两处排障 `curl` 打在将被删除的路由上
      （`/api/gse/agents/<node>/collect-items`、`DELETE /api/gse/collect-items/<id>`），改为 spec 路径。
- [ ] 7.9 `.monkeycode/docs/testcases/node.md`：节点页测试用例按新页面结构（列表 + 整页详情四视图）更新。

**检查点**：本目录三份文档复跑占位符检查为空；mermaid 围栏成对；跨文档引用的路径存在；
`grep -rn "agent-configs\|collect-items" .monkeycode/specs/` 命中处都有明确的「已删除/已改」标注。

## 阶段 8：端到端验证

- [ ] 8.1 本地双进程：`PUT` 期望 spec（含一条 `log_file`）→ `apply` → ack 与页面一致，stream `accepted` 增长。
- [ ] 8.2 改 `heartbeat_interval_secs` 下发 → 心跳间隔变化且 `client_id` 不变（未重连）。
- [ ] 8.3 spec 内删掉那条采集项 → 下发 → 该 stream 停止增长，其它采集项不受影响。
- [ ] 8.4 token 轮换：改 spec 的 `token` 下发 → 重连成功（`client_id` 变化）→ `prev_token` 已清、旧 token 认证被拒。
      **记录失败时的回滚办法**（改回 token + 重启 agent）。
- [ ] 8.5 停 server、改本地 TOML、`kill -HUP` agent → 本地值生效；server 恢复后自动拉取被期望值覆盖（下发赢）。
- [ ] 8.6 旧库升级：造一份含 `agent_configs` + 多 agent `collect_items` 的库 → 启动 → 断言展开成 per-agent spec
      且 `sync_status = stale`。
- [ ] 8.7 真机（debian12-agent / cloud2-agent / testbkee 任选可行路径）复跑 8.1 / 8.3 / 8.5，记录实际观察值到本文件下方。
- [ ] 8.8 `cargo clippy --workspace` 无新增告警；`cargo test --workspace` 全绿；
      确认 `gse-server-core`（ledger 25 / http 26 / server 16）与 `dataserver`（http 14 / cleanup 7）里
      涉及 collect_items / agent_configs 的既有用例已全部改造或删除（本次最易漏的部分）。

### 8.7 实测记录

（待填）
