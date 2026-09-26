# Requirements Document

> **已归档**（2026-09-26 文档整理）：本 feature 已实现并合入 main，本文件压缩为需求索引——完整 EARS 条款见 git 历史（本文件重写前的最后一个版本），design.md 全文保留为实施细节档案。

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

- AS 前端开发者, I want 节点管理作为独立应用包, so that 台账控制台可以单独开发与编译。
- 验收：THE 仓库 SHALL 在 `frontend/apps/node` 提供应用包 `@vectorman/node`；THE `@vectorman/node` SHALL 依赖 `@vectorman/primitives` 与 `@vectorman/adapters`。
### Requirement 2: 信息架构与导航

- AS 运维人员, I want 四个台账入口在同一应用内切换, so that 预登记主机与 Agent 不必换工具。
- 验收：THE 节点应用 SHALL 提供四个一级入口：主机、接入点、Agent、Agent 配置；WHEN 运维打开节点应用，THE 应用 SHALL 默认进入主机列表。
### Requirement 3: 主机 CRUD

- AS 运维人员, I want 登记与维护主机资产, so that 未装 Agent 的机器也能被追踪。
- 验收：WHEN 运维打开主机列表，THE 应用 SHALL 展示 `host_id`、`inner_ip`、`hostname`、`os_type`、`os_version`；THE 主机登记表单 SHALL 要求填写 `host_id` 与 `inner_ip`，并允许填写 `hostname`、`os_type`、`os_version`、`cpu_spec`、`mem_spec`。
### Requirement 4: 接入点 CRUD

- AS 运维人员, I want 登记每个 GSE Server 的地址与端口, so that 后期多 Server 有台账可查。
- 验收：WHEN 运维打开接入点列表，THE 应用 SHALL 展示 `id`、`name`、`server_ip`、`rpc_port`；THE 接入点登记表单 SHALL 要求填写 `id`、`name`、`server_ip` 与 `rpc_port`，并允许填写 `file_port` 与 `data_port`。
### Requirement 5: Agent 预登记与查询

- AS 运维人员, I want 预登记 Agent 凭据并查看运行状态, so that 安装前完成纳管准备。
- 验收：WHEN 运维打开 Agent 列表，THE 应用 SHALL 展示 `agent_id`、`host_id`、`status`、`last_heartbeat_at`、`version`，且不展示 `token`；THE Agent 预登记表单 SHALL 要求以文本输入 `agent_id`、`host_id` 与 `token`，并允许以文本输入 `access_point_id`、`version`、`install_path`。
### Requirement 6: Agent 运行时配置

- AS 运维人员, I want 为每个 Agent 保存资源上限与日志级别, so that 配置可在台账中查询。
- 验收：WHEN 运维打开 Agent 配置列表，THE 应用 SHALL 展示 `agent_id`、`host_id`、`cpu_limit_percent`、`mem_limit_percent`、`log_level`；THE 配置保存表单 SHALL 要求填写 `agent_id` 与 `host_id`，并允许填写 `cpu_limit_percent`、`mem_limit_percent`、`log_level`。
### Requirement 7: 校验与错误提示

- AS 运维人员, I want 缺字段和后端失败有明确提示, so that 我知道下一步改什么。
- 验收：IF 必填字段为空，THE 应用 SHALL 阻止提交，并通过 `Notifier` 提示缺失字段名称；IF 适配器返回错误契约，THE 应用 SHALL 通过 `Notifier.error` 展示该 `message`，并保持当前列表数据。
### Requirement 8: 删除确认

- AS 运维人员, I want 删除前再次确认, so that 误点击不会立刻清掉台账与会话。
- 验收：WHEN 运维点击删除主机、接入点或 Agent，THE 应用 SHALL 先展示确认步骤，显示将删除的主键；WHEN 运维取消确认，THE 应用 SHALL 保持该资源不变。
### Requirement 9: v1 范围边界

- AS 开发者, I want 本期范围与后端能力对齐, so that 前端不承诺尚未存在的接口。
- 验收：THE 节点应用 SHALL 将作业编排、文件分发、进程托管、SQL 查询、Prom 查询列为后续范围；THE 节点应用 v1 SHALL 不提供登录页；空会话下仍允许调用 GSE 管理 HTTP。
### Requirement 10: 界面组件

- AS 前端开发者, I want 使用 Ant Design 实现表格与抽屉, so that 列表、表单、确认框有统一交互。
- 验收：THE `@vectorman/node` SHALL 使用 Ant Design 实现表格、抽屉、表单、确认对话框与 Toast 宿主；THE `@vectorman/primitives` 与 `@vectorman/adapters` SHALL 不依赖 Ant Design。
