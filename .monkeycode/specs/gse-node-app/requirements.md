# Requirements Document

## Introduction

为 GSE Server 的 HTTP 管理口提供节点管理前端。应用包为 `@vectorman/node`，落在已冻结的前端多包工作区 `frontend/apps/node`。运维通过浏览器完成 hosts、access_points、agents、agent_configs 四类台账的列表、登记、查看与删除（agent_configs 后端无删除路由，前端提供列表、查看与保存）。本期先完成需求与设计，不实现代码。

本应用复用 `@vectorman/primitives` 与 `@vectorman/adapters`，不平行引入第二套 HTTP 或错误处理。

## Glossary

- **节点应用（Node app）**：应用包 `@vectorman/node`，GSE 台账的运维控制台。
- **台账（Ledger）**：GSE Server sqlite 中的 hosts、access_points、agents、agent_configs 四表。
- **主机（Host）**：被纳管机器资产，主键 `host_id`。
- **接入点（Access Point）**：GSE Server 通信地址登记，主键 `id`。
- **Agent 实例（Agent）**：预登记安装实例，主键 `agent_id`，含 token 与运行状态。
- **Agent 运行时配置（Agent Config）**：按 `agent_id` 保存的 CPU/内存上限与日志级别；后端提供列表、保存、按 id 查询，无独立删除路由。
- **GSE 管理 HTTP**：gse-server 管理端口，默认 `127.0.0.1:7101`。
- **运行状态**：agents 表字段 `status`，取值 `online`、`offline`、`unknown`。
- **错误契约**：与前端分层架构相同，含 `code` 与 `message`。
- **装配入口**：`@vectorman/node` 内注入原子能力与 `GseAdminAdapter` 的唯一组装点。
- **Toast 宿主**：节点应用内订阅 `Notifier` 并渲染提示条的 UI 组件。

## Requirements

### Requirement 1: 应用包位置

**User Story:** AS 前端开发者, I want 节点管理作为独立应用包, so that 台账控制台可以单独开发与编译。

#### Acceptance Criteria

1. THE 仓库 SHALL 在 `frontend/apps/node` 提供应用包 `@vectorman/node`。
2. THE `@vectorman/node` SHALL 依赖 `@vectorman/primitives` 与 `@vectorman/adapters`。
3. THE `@vectorman/node` SHALL 通过 `GseAdminAdapter` 访问台账，不直接调用 `fetch`。
4. THE `@vectorman/node` 的开发服务器 SHALL 将 `/api/gse` 转发到 GSE 管理 HTTP，并允许 `*.monkeycode-ai.online` 主机名访问。

### Requirement 2: 信息架构与导航

**User Story:** AS 运维人员, I want 四个台账入口在同一应用内切换, so that 预登记主机与 Agent 不必换工具。

#### Acceptance Criteria

1. THE 节点应用 SHALL 提供四个一级入口：主机、接入点、Agent、Agent 配置。
2. WHEN 运维打开节点应用，THE 应用 SHALL 默认进入主机列表。
3. THE 每个一级入口 SHALL 提供该资源的列表视图；登记、查看与编辑 SHALL 在列表页右侧抽屉中完成，列表保持可见。
4. THE 前端工作区 SHALL 保留 `@vectorman/console` 与 `@vectorman/job` 应用包，并新增 `@vectorman/node`。

> **架构修订（2026-09-10）**：`@vectorman/node` 现为 UI 包（导出四个页面与 Runtime Provider），由 `@vectorman/console` 组合；节点页面路由与四入口不变。
5. THE 节点应用 SHALL 使用简体中文界面文案。

### Requirement 3: 主机 CRUD

**User Story:** AS 运维人员, I want 登记与维护主机资产, so that 未装 Agent 的机器也能被追踪。

#### Acceptance Criteria

1. WHEN 运维打开主机列表，THE 应用 SHALL 展示 `host_id`、`inner_ip`、`hostname`、`os_type`、`os_version`。
2. THE 主机登记表单 SHALL 要求填写 `host_id` 与 `inner_ip`，并允许填写 `hostname`、`os_type`、`os_version`、`cpu_spec`、`mem_spec`。
3. WHEN 运维提交合法主机表单，THE 应用 SHALL 调用 `GseAdminAdapter.upsertHost` 并刷新列表。
4. WHEN 运维打开某台主机，THE 应用 SHALL 展示该主机的全部字段。
5. WHEN 运维确认删除某台主机，THE 应用 SHALL 调用 `GseAdminAdapter.deleteHost` 并刷新列表。
6. THE 主机列表 SHALL 提供「登记」与「编辑」两个入口；「登记」打开空表单抽屉，「编辑」打开带出已有字段的抽屉且 `host_id` 只读。

### Requirement 4: 接入点 CRUD

**User Story:** AS 运维人员, I want 登记每个 GSE Server 的地址与端口, so that 后期多 Server 有台账可查。

#### Acceptance Criteria

1. WHEN 运维打开接入点列表，THE 应用 SHALL 展示 `id`、`name`、`server_ip`、`rpc_port`。
2. THE 接入点登记表单 SHALL 要求填写 `id`、`name`、`server_ip` 与 `rpc_port`，并允许填写 `file_port` 与 `data_port`。
3. WHEN 运维提交合法接入点表单，THE 应用 SHALL 调用 `GseAdminAdapter.upsertAccessPoint` 并刷新列表。
4. WHEN 运维打开某个接入点，THE 应用 SHALL 展示该接入点的全部字段。
5. WHEN 运维确认删除某个接入点，THE 应用 SHALL 调用 `GseAdminAdapter.deleteAccessPoint` 并刷新列表。
6. THE 接入点列表 SHALL 提供「登记」与「编辑」两个入口；「登记」打开空表单抽屉，「编辑」打开带出已有字段的抽屉且 `id` 只读。

### Requirement 5: Agent 预登记与查询

**User Story:** AS 运维人员, I want 预登记 Agent 凭据并查看运行状态, so that 安装前完成纳管准备。

#### Acceptance Criteria

1. WHEN 运维打开 Agent 列表，THE 应用 SHALL 展示 `agent_id`、`host_id`、`status`、`last_heartbeat_at`、`version`，且不展示 `token`。
2. THE Agent 预登记表单 SHALL 要求以文本输入 `agent_id`、`host_id` 与 `token`，并允许以文本输入 `access_point_id`、`version`、`install_path`。
3. THE Agent 预登记表单 SHALL 将 `host_id` 与 `access_point_id` 作为手填字段，不从主机或接入点列表生成下拉选项。
4. WHEN 运维提交合法 Agent 表单，THE 应用 SHALL 调用 `GseAdminAdapter.upsertAgent` 并刷新列表。
5. WHEN 运维打开某个 Agent 的查看抽屉，THE 应用 SHALL 展示该 Agent 的全部字段，含 `status` 与 `last_heartbeat_at`；`token` 默认掩码，并提供显示与复制两个操作。
6. WHEN 运维打开某个 Agent 的编辑抽屉，THE 应用 SHALL 将 `token` 作为只读掩码展示，并提供显示与复制；提交 upsert 时 SHALL 使用服务端返回的原 `token`，不接受运维修改。
7. WHEN 运维确认删除某个 Agent，THE 应用 SHALL 调用 `GseAdminAdapter.deleteAgent` 并刷新列表。
8. THE Agent 预登记、查看与编辑 SHALL 将 `status` 与 `last_heartbeat_at` 作为只读展示，提交时不要求运维填写这两项。
9. WHILE 运维停留在 Agent 列表视图，THE 应用 SHALL 每 30 秒重新请求 Agent 列表以更新 `status` 与 `last_heartbeat_at`。
10. WHEN 运维离开 Agent 列表视图，THE 应用 SHALL 停止该 30 秒轮询。
11. THE Agent 列表 SHALL 提供「预登记」与「编辑」两个入口；「预登记」打开空表单抽屉，「编辑」打开带出已有字段的抽屉且 `agent_id` 只读。

### Requirement 6: Agent 运行时配置

**User Story:** AS 运维人员, I want 为每个 Agent 保存资源上限与日志级别, so that 配置可在台账中查询。

#### Acceptance Criteria

1. WHEN 运维打开 Agent 配置列表，THE 应用 SHALL 展示 `agent_id`、`host_id`、`cpu_limit_percent`、`mem_limit_percent`、`log_level`。
2. THE 配置保存表单 SHALL 要求填写 `agent_id` 与 `host_id`，并允许填写 `cpu_limit_percent`、`mem_limit_percent`、`log_level`。
3. WHEN 运维提交合法配置表单，THE 应用 SHALL 调用 `GseAdminAdapter.upsertAgentConfig` 并刷新列表。
4. WHEN 运维打开某条配置，THE 应用 SHALL 展示该配置的全部字段。
5. THE 节点应用 SHALL 不为 Agent 配置提供独立删除按钮；删除 Agent 后由后端级联清理对应配置。
6. THE Agent 配置列表 SHALL 提供「保存」与「编辑」两个入口；「保存」打开空表单抽屉，「编辑」打开带出已有字段的抽屉且 `agent_id` 只读。

### Requirement 7: 校验与错误提示

**User Story:** AS 运维人员, I want 缺字段和后端失败有明确提示, so that 我知道下一步改什么。

#### Acceptance Criteria

1. IF 必填字段为空，THE 应用 SHALL 阻止提交，并通过 `Notifier` 提示缺失字段名称。
2. IF 适配器返回错误契约，THE 应用 SHALL 通过 `Notifier.error` 展示该 `message`，并保持当前列表数据。
3. WHILE 列表或提交请求进行中，THE 对应视图 SHALL 使用 `QueryStore` 的 loading 状态，并禁止重复提交同一表单。
4. IF 查询单个资源得到 `not_found`，THE 应用 SHALL 在抽屉内展示资源不存在提示，并提供关闭抽屉的入口。
5. THE `@vectorman/node` SHALL 提供 Toast 宿主：订阅 `Notifier`，将成功、警告、错误三类提示渲染为可见提示条。

### Requirement 8: 删除确认

**User Story:** AS 运维人员, I want 删除前再次确认, so that 误点击不会立刻清掉台账与会话。

#### Acceptance Criteria

1. WHEN 运维点击删除主机、接入点或 Agent，THE 应用 SHALL 先展示确认步骤，显示将删除的主键。
2. WHEN 运维取消确认，THE 应用 SHALL 保持该资源不变。
3. WHEN 运维确认删除 Agent，THE 应用 SHALL 在提示文案中说明将同时清理该 Agent 的运行时配置与活跃会话。

### Requirement 9: v1 范围边界

**User Story:** AS 开发者, I want 本期范围与后端能力对齐, so that 前端不承诺尚未存在的接口。

#### Acceptance Criteria

1. THE 节点应用 SHALL 将作业编排、文件分发、进程托管、SQL 查询、Prom 查询列为后续范围。
2. THE 节点应用 v1 SHALL 不提供登录页；空会话下仍允许调用 GSE 管理 HTTP。
3. THE 节点应用 SHALL 将 ping/pong 信令下发列为后续范围。
4. THE 本期交付物 SHALL 为需求文档与设计文档；实现代码列为后续任务。

### Requirement 10: 界面组件

**User Story:** AS 前端开发者, I want 使用 Ant Design 实现表格与抽屉, so that 列表、表单、确认框有统一交互。

#### Acceptance Criteria

1. THE `@vectorman/node` SHALL 使用 Ant Design 实现表格、抽屉、表单、确认对话框与 Toast 宿主。
2. THE `@vectorman/primitives` 与 `@vectorman/adapters` SHALL 不依赖 Ant Design。
3. THE 删除确认 SHALL 使用 Ant Design 确认对话框，显示将删除的主键。
