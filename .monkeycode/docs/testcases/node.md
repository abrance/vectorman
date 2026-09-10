# 节点管理测试用例

- 范围：GSE 节点管理，覆盖主机（hosts）、接入点（access-points）、Agent（agents）、Agent 配置（agent-configs）的台账管理，Agent 会话/鉴权/存活探测，以及节点管理前端（`frontend/apps/node`）。
- 分层：API（gse-server HTTP）、Unit（ledger/session/config）、E2E（server+agent 真实进程）、UI（前端组件与交互）。
- 自动化对应关系：`自动化` 列标注现有测试；`缺口` 表示当前无自动化覆盖，建议后续补充。
- 自动化执行命令见文末。

## 1. 测试环境与前置

| 项 | 说明 |
| --- | --- |
| 后端单测/集成 | `ServerConfig::default()`；ledger 使用 `fresh_ledger(name)` 临时库 |
| API 用例 | 构造 axum `Router` + `tower::ServiceExt::oneshot`，无需真实端口 |
| E2E 用例 | 每个用例独立临时 sqlite；`register`/`wait_online` 辅助；`auth_enabled` 可切换 |
| 前端 | Vitest + jsdom；`test-setup.ts` stub `getComputedStyle`；primitive 使用 `MemoryNotifier`/`MemoryQueryStore` |
| 浏览器 | https://<preview-host>；节点应用路由 `/hosts`、`/access-points`、`/agents`、`/agent-configs` |

通用前置：服务端已启动且 ledger 可写；API 用例默认初始空库；E2E 需要可用 agent 二进制。

## 2. 后端 API 用例

### 2.1 健康检查与静态托管

| 用例 ID | 前置 | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- | --- |
| NODE-API-001 | 服务启动 | `GET /health` | 200，返回健康标识 | `http::health_returns_ok` |
| NODE-API-002 | 配置 `http_web_dir` 指向含 SPA 的目录 | `GET /`、`GET /hosts`、`GET /assets/*` | 命中静态文件；未知路径回退 `index.html` | `http::web_dir_serves_static_and_spa_fallback` |

### 2.2 主机（hosts）

| 用例 ID | 前置 | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- | --- |
| NODE-API-101 | 空库 | `POST /api/gse/hosts` 缺 `host_id` | 400 校验失败 | `http::hosts_crud_and_validation` |
| NODE-API-102 | 空库 | `POST` 合法主机（`host_id`+`inner_ip`） | 200 创建成功 | `http::hosts_crud_and_validation` |
| NODE-API-103 | 已存在主机 | `GET /api/gse/hosts` | 列表包含该主机 | `http::hosts_crud_and_validation` |
| NODE-API-104 | 已存在主机 | `GET /api/gse/hosts/{id}` | 200 返回详情 | `http::hosts_crud_and_validation` |
| NODE-API-105 | 不存在 id | `GET /api/gse/hosts/ghost` | 404 | `http::hosts_crud_and_validation` |
| NODE-API-106 | 已存在主机 | `DELETE /api/gse/hosts/{id}` 后 `GET` | 删除成功，再查 404 | `http::hosts_crud_and_validation` |
| NODE-API-107 | 同一 `host_id` 再次 `POST` | 覆盖式 upsert | 字段被覆盖，非重复插入 | `ledger::host_upsert_overwrites_same_pk` |

### 2.3 Agent（agents）

| 用例 ID | 前置 | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- | --- |
| NODE-API-201 | 空库 | `POST /api/gse/agents` 缺 `agent_id`/`host_id` | 400 | `http::agents_crud_and_validation` |
| NODE-API-202 | 空库 | `POST` 合法 Agent | 200 创建成功 | `http::agents_crud_and_validation` |
| NODE-API-203 | 已存在 Agent | `GET /api/gse/agents`、`GET /api/gse/agents/{id}` | 列表/详情正确 | `http::agents_crud_and_validation` |
| NODE-API-204 | 不存在 id | `GET /api/gse/agents/ghost` | 404 | `http::agents_crud_and_validation` |
| NODE-API-205 | 已存在 Agent | `DELETE /api/gse/agents/{id}` 后 `GET` | 删除成功、再查 404 | `http::agents_crud_and_validation` |
| NODE-API-206 | Agent 带运行时配置 | 删除 Agent | 级联删除 agent-config，保留 host | `http::delete_agent_cascades_config_but_keeps_host` |

### 2.4 接入点与 Agent 配置

| 用例 ID | 前置 | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- | --- |
| NODE-API-301 | 空库 | 接入点 CRUD（缺必填/合法/查询/删除） | 校验与增删查结果正确 | `http::access_points_and_agent_configs_crud` |
| NODE-API-302 | 空库 | Agent 配置 CRUD（`agent_id`+`host_id` 必填） | 校验与读写正确 | `http::access_points_and_agent_configs_crud` |
| NODE-API-303 | 已存在记录 | `GET /api/gse/access-points/ghost` | 404 | `http::access_points_and_agent_configs_crud` |
| NODE-API-304 | 已存在配置 | `GET /api/gse/agent-configs/{id}` | 返回详情 | `http::access_points_and_agent_configs_crud` |

## 3. 后端单元/集成本用例

### 3.1 Ledger 台账

| 用例 ID | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- |
| NODE-LEDGER-001 | 连续两次 `init` | 幂等，无报错、结构一致 | `ledger::init_is_idempotent` |
| NODE-LEDGER-002 | host CRUD 往返 | 各字段读写一致 | `ledger::host_crud_roundtrip` |
| NODE-LEDGER-003 | access-point CRUD 往返 | 各字段（含可选端口）读写一致 | `ledger::access_point_crud_roundtrip` |
| NODE-LEDGER-004 | agent CRUD 往返 | 各字段读写一致 | `ledger::agent_crud_roundtrip` |
| NODE-LEDGER-005 | agent-config CRUD 往返 | 各字段读写一致 | `ledger::agent_config_crud_roundtrip` |
| NODE-LEDGER-006 | `check_auth` 三态（匹配/不匹配/未登记） | 分别返回允许/拒绝/未登记语义 | `ledger::check_auth_three_states` |
| NODE-LEDGER-007 | `runtime_state` 状态流转与落库 | 状态与 `last_heartbeat_at` 正确持久化 | `ledger::runtime_state_transitions` |
| NODE-LEDGER-008 | Agent 重连/再次认证 | 单会话替换，仅保留一个活跃会话 | `session::registry_insert_replace_keeps_single_active_session` |

### 3.2 会话与存活探测（session）

| 用例 ID | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- |
| NODE-SESSION-001 | 新建会话 | 状态 `online` | `session::session_created_online` |
| NODE-SESSION-002 | `touch` 刷新 | `last_seen` 前进 | `session::touch_refreshes_last_seen` |
| NODE-SESSION-003 | 窗口内 `advance` | 状态保持 `online` | `session::advance_within_window_keeps_state` |
| NODE-SESSION-004 | `online` 超过窗口 | 转 `checking` | `session::advance_online_to_checking` |
| NODE-SESSION-005 | `checking` 继续超时 | 转 `offline` | `session::advance_checking_to_offline` |
| NODE-SESSION-006 | `offline` 继续 `advance` | 保持 `offline` | `session::advance_offline_stays_offline` |
| NODE-SESSION-007 | `closed` 继续 `advance` | 保持 `closed` | `session::advance_closed_stays_closed` |
| NODE-SESSION-008 | `close` | 标记 `closed` | `session::close_marks_closed` |
| NODE-SESSION-009 | 不同 Agent 并存 | 互不影响 | `session::registry_distinct_agents_coexist` |
| NODE-SESSION-010 | 注册表批量 `touch`/`advance` | 所有会话同步处理 | `session::registry_touch_and_advance_all` |
| NODE-SESSION-011 | 注册表 `set_state` | 已存在会话状态被更新 | `session::registry_set_state_affects_existing_session` |
| NODE-SESSION-012 | `now_micros` | 单调递增 | `session::now_micros_is_positive_and_increasing` |

### 3.3 配置加载

| 用例 ID | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- |
| NODE-CONFIG-001 | 完整 server TOML | 全字段加载 | `config::load_full_toml` |
| NODE-CONFIG-002 | 缺省字段 TOML | 回落默认值 | `config::load_absent_fields_fall_back_to_defaults` |
| NODE-CONFIG-003 | 非法 TOML | 返回错误 | `config::malformed_toml_returns_error` |
| NODE-CONFIG-004 | 文件不存在 | 返回错误 | `config::missing_file_returns_error` |
| NODE-CONFIG-005 | 环境变量覆盖 | 覆盖 listen/auth/timeout | `config::env_overrides_listen_and_auth_and_timeout` |
| NODE-CONFIG-006 | 旧 `agents` 段 | 忽略不报错 | `config::legacy_agents_section_is_ignored` |
| NODE-CONFIG-007 | Agent 端完整 TOML | 全字段加载 | `gse-agent-core::config::load_full_toml` |
| NODE-CONFIG-008 | Agent 端缺省/覆盖/非法/缺失文件 | 默认值、env 覆盖、错误返回 | `gse-agent-core::config::{load_absent_fields_fall_back_to_defaults,env_overrides_all_fields,malformed_toml_returns_error,missing_file_returns_error}` |

## 4. 端到端用例（server + agent 真实进程）

| 用例 ID | 前置 | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- | --- |
| NODE-E2E-001 | auth=true，已登记 Agent | Agent 认证 → 心跳 → ping/pong | 会话 online，ledger 实时更新 | `e2e_auth_heartbeat_ping_pong_update_ledger` |
| NODE-E2E-002 | 已连接 | 发送未知命令 | 被拒绝、连接不崩溃 | `e2e_unknown_command_rejected` |
| NODE-E2E-003 | 未连接的 agent_id | 下发命令 | 返回失败 | `e2e_command_to_unknown_agent_fails` |
| NODE-E2E-004 | auth=true，token 错误 | Agent 启动 | 认证被拒，Agent 退出 | `e2e_auth_rejected_agent_exits` |
| NODE-E2E-005 | auth=true，未登记 Agent | Agent 启动 | 认证被拒，Agent 退出 | `e2e_auth_unregistered_agent_exits` |
| NODE-E2E-006 | auth=false | 未登记 Agent 启动 | 允许连接 | `e2e_auth_disabled_allows_unregistered_agent` |
| NODE-E2E-007 | 同一 Agent 重复认证 | 二次认证 | 仅保留一个会话 | `e2e_double_auth_keeps_single_session` |
| NODE-E2E-008 | 会话已离线 | 下发命令 | `unavailable` | `e2e_command_to_offline_session_unavailable` |
| NODE-E2E-009 | Agent 断线 | Agent 重启重连 | 恢复在线，会话重建 | `e2e_agent_reconnects_after_disconnect` |
| NODE-E2E-010 | Agent 在线 | 停止心跳至超过窗口 | ledger 标记 `offline` | `e2e_liveness_marks_agent_offline_in_ledger` |
| NODE-E2E-011 | Agent 在线且含配置 | `DELETE /api/gse/agents/{id}` | ledger 清除、会话关闭、host 保留 | `e2e_http_delete_agent_clears_ledger_and_session` |
| NODE-E2E-012 | 空库 | 接入点重复 bind | 幂等登记，仅一条 | `e2e_bind_registers_access_point_idempotently` |
| NODE-E2E-013 | 服务运行中 | 畸形连接/非法帧 | 服务不崩溃，后续连接正常 | `malformed_connection_does_not_kill_server` |

## 5. 前端用例（节点管理应用）

### 5.1 主机页 `/hosts`

| 用例 ID | 前置 | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- | --- |
| NODE-UI-101 | 进入页面 | 加载 | 自动刷新列表，展示 host_id/inner_ip/hostname/os_type/os_version | 缺口 |
| NODE-UI-102 | 点击「登记」 | 打开抽屉，留空提交 | 提示「缺少必填字段：host_id、inner_ip」，不发请求 | 缺口 |
| NODE-UI-103 | 填写合法字段 | 提交 | 调用 save，成功后关闭抽屉并刷新 | 缺口 |
| NODE-UI-104 | 某行「查看」 | 打开抽屉 | 只读（表单 disabled），字段回填 | 缺口 |
| NODE-UI-105 | 某行「编辑」 | 打开抽屉 | `host_id` 禁用不可改，其余可编辑并回填 | 缺口 |
| NODE-UI-106 | 某行「删除」 | 确认弹窗 | 文案含主机 id，确认后调用 remove，取消不删除 | 缺口 |

### 5.2 接入点页 `/access-points`

| 用例 ID | 前置 | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- | --- |
| NODE-UI-201 | 进入页面 | 加载 | 展示 id/name/server_ip/rpc_port | 缺口 |
| NODE-UI-202 | 点击「登记」 | 缺 id/name/server_ip/rpc_port | 提示缺少必填字段，不发请求 | 缺口 |
| NODE-UI-203 | 填写合法字段 | 提交 | `file_port`/`data_port` 为空时保存为 `null` | 缺口 |
| NODE-UI-204 | 查看/编辑 | 打开抽屉 | 查看只读；编辑时 `id` 禁用 | 缺口 |
| NODE-UI-205 | 删除 | 确认弹窗 | 确认后删除，取消不删 | 缺口 |

### 5.3 Agent 页 `/agents`

| 用例 ID | 前置 | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- | --- |
| NODE-UI-301 | 进入页面 | 加载 | 自动刷新且开启轮询；status 显示 online/offline/unknown 标签 | 缺口 |
| NODE-UI-302 | 预登记缺 agent_id/host_id/token | 提交 | 提示「缺少必填字段：agent_id、host_id、token」 | 缺口 |
| NODE-UI-303 | 预登记合法 | 提交 | 携带 token 保存成功 | 缺口 |
| NODE-UI-304 | 编辑已有 Agent | 回填后提交 | `token` 不可见，但提交保留原 token | `use-agents.test.ts: upserts with token from getAgent` |
| NODE-UI-305 | 编辑缺 agent_id/host_id | 提交 | 提示「缺少必填字段：agent_id、host_id」 | 缺口 |
| NODE-UI-306 | 某行删除 | 确认弹窗 | 文案提示同时清理运行时配置与活跃会话 | 缺口 |
| NODE-UI-307 | 轮询副作用 | 组件卸载 | `clearInterval` 被调用，无泄漏 | `use-agents.test.ts: clears interval`（间接） |

### 5.4 Agent 配置页 `/agent-configs`

| 用例 ID | 前置 | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- | --- |
| NODE-UI-401 | 进入页面 | 加载 | 展示 agent_id/host_id/cpu_limit_percent/mem_limit_percent/log_level | 缺口 |
| NODE-UI-402 | 缺 agent_id 或 host_id | 提交 | 提示「缺少必填字段：agent_id、host_id」 | 缺口 |
| NODE-UI-403 | `log_level` 留空 | 提交 | 默认写为 `info` | 缺口 |
| NODE-UI-404 | 查看/编辑 | 打开抽屉 | 查看只读；编辑时 `agent_id` 禁用 | 缺口 |

### 5.5 公共组件与路由

| 用例 ID | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- |
| NODE-UI-501 | `MaskedToken` 初始渲染 | 显示 `••••` | `masked-token.test.tsx: masks by default and reveals on click` |
| NODE-UI-502 | 点击「显示」 | 明文显示，再点「隐藏」还原 | `masked-token.test.tsx` |
| NODE-UI-503 | 点击「复制」成功/失败 | 分别提示「已复制 token」/「复制失败」 | 缺口（clipboard 需 stub） |
| NODE-UI-504 | 访问 `/hosts`、`/access-points`、`/agents`、`/agent-configs` | 渲染对应页面；未知路径按 SPA 回退 | 缺口 |
| NODE-UI-505 | 抽屉读取详情失败（如 404） | 抽屉展示错误信息 `missing` | 缺口 |
| NODE-UI-506 | 列表加载失败 | 表格空态展示错误信息 | 缺口 |

### 5.6 适配器（`packages/adapters` gse/admin）

| 用例 ID | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- |
| NODE-ADAPTER-001 | `listHosts` | `GET /api/gse/hosts` | `admin.test.ts: lists hosts with GET /api/gse/hosts` |
| NODE-ADAPTER-002 | `upsertAgent` | `POST` 且 body 为 Agent JSON | `admin.test.ts: upserts agent with POST body` |
| NODE-ADAPTER-003 | id 含特殊字符 | 路径正确 encode | `admin.test.ts: encodes path ids` |
| NODE-ADAPTER-004 | `deleteHost` | `DELETE` 正确路径 | `admin.test.ts: deletes host` |

## 6. 覆盖缺口汇总（建议补充）

1. 节点前端 4 个页面（hosts/access-points/agents/agent-configs）均无组件级测试，缺少必填校验、只读查看、编辑禁用主键、删除确认、错误提示等交互覆盖。
2. `use-hosts`、`use-access-points`、`use-agent-configs`、`errors.ts`、`ledger-drawer.tsx`、`App.tsx` 无测试。
3. `MaskedToken` 复制成功/失败路径未覆盖（需 stub clipboard）。
4. 适配器 `admin` 仅覆盖 hosts/agents 部分方法，access-points、agent-configs、getHost/getAgent 未覆盖。
5. API 层 `GET /api/gse/agents` 中携带运行时状态（online/offline）与轮询刷新未单测。

## 7. 自动化执行

```bash
# 后端全部单测 + 集成 + e2e
/root/.cargo/bin/cargo test -p gse-proto -p gse-agent-core -p gse-server-core

# 仅 HTTP/ledger/session 等模块
/root/.cargo/bin/cargo test -p gse-server-core

# 前端全量
cd /workspace/frontend && npm test

# 仅节点 UI 包与适配器
cd /workspace/frontend && npm run test -w @vectorman/node
cd /workspace/frontend && npm run test -w @vectorman/adapters
```
