# Requirements Document

> **已归档**（2026-09-26 文档整理）：本 feature 已实现并合入 main，本文件压缩为需求索引——完整 EARS 条款见 git 历史（本文件重写前的最后一个版本），design.md 全文保留为实施细节档案。

## Introduction

GSE Server 增加 Server 侧资产台账能力（CMDB 化）：用 sqlite 持久化四张登记表——主机资产表（hosts）、接入点配置表（access_points）、Agent 实例表（agents）、Agent 运行时配置表（agent_configs）。运维在接入前预登记主机、Agent 与凭据；Server 认证数据源切换到 agents 表；Agent 接入后由 Server 回填运行状态（在线/离线/最后心跳）。本期只登记不选路：接入点与多 Server 仅记录台账，Agent 仍沿用配置文件直连当前 Server。全部访问通过 gse-server-core 同进程 Rust API 提供。

## Glossary

- **主机（Host）**：被纳管的物理/虚拟机器资产，无论是否安装 Agent 都应在 hosts 表中登记。
- **接入点（Access Point）**：GSE Server 实例的通信地址与端口登记（Server 自身信息，后期可扩展为多 Server 高可用）。
- **Agent 实例（Agent）**：预登记的安装实例，持有 agent-id 与认证 token，可能与某台主机 1:1 或 N:1 关联。
- **Agent 运行时配置（Agent Config）**：单个 Agent 的资源限制与日志级别等个性化配置。
- **台账（Ledger）**：上述四张表的统称。
- **预登记（Pre-registration）**：Agent 接入前由运维在台账中建档主机、Agent 与凭据。
- **查库认证（Table-based auth）**：Server 认证时仅从 agents 表校验 token，不再使用配置文件 `[agents]` 段。

## Requirements

### Requirement 1: 持久化台账存储

- AS 运维人员, I want 台账数据在 Server 重启后依然存在, so that 资产登记与运行状态不因进程重启丢失。
- 验收：WHEN gse-server 启动，THE Server SHALL 打开该 Server 的 sqlite 台账数据库；WHEN 数据库打开成功，THE Server SHALL 自动创建四张表（hosts、access_points、agents、agent_configs）。
### Requirement 2: 主机资产登记

- AS 运维人员, I want 登记每台被纳管机器的资产信息, so that 无论是否安装 Agent，机器都能被追踪。
- 验收：THE 主机表 SHALL 记录 host-id、内网 IP、主机名、操作系统类型、操作系统版本、CPU 规格、内存规格与登记时间；WHEN 调用方登记主机，THE Server SHALL 以 host-id 为主键幂等写入。
### Requirement 3: 接入点登记与 Server 自登记

- AS 运维人员, I want 记录每个 GSE Server 的通信地址与端口, so that 后期可扩展为多个 Server 高可用。
- 验收：THE 接入点表 SHALL 记录接入点标识、名称、Server IP、RPC/心跳端口、文件传输端口与数据上报端口；WHEN gse-server 启动，THE Server SHALL 将自身信息以接入点记录 upsert 入表。
### Requirement 4: Agent 预登记与查库认证

- AS 运维人员, I want 只有预登记且 token 匹配的 Agent 才能接入, so that 未授权机器无法连入。
- 验收：THE Agent 表 SHALL 记录 agent-id、关联 host-id、绑定接入点、token、Agent 版本、安装路径与登记时间，agent-id 为主键；WHEN Agent 发起认证，THE Server SHALL 仅从 agents 表校验 agent-id 与 token 的一致性。
### Requirement 5: Agent 运行状态回写

- AS 上层模块, I want agents 表实时反映 Agent 在线状态, so that 可在选路和告警前判断可用性。
- 验收：WHEN Agent 认证成功，THE Server SHALL 将 agents 表对应记录状态置为在线并更新最后心跳时间；WHILE 会话存活，THE Server SHALL 在每次收到心跳时更新该 Agent 的最后心跳时间。
### Requirement 6: Agent 运行时配置登记

- AS 运维人员, I want 为每个 Agent 维护资源限制与日志配置, so that Agent 不会抢占业务资源。
- 验收：THE 运行时配置表 SHALL 以 agent-id 关联记录 CPU 使用率上限、内存使用率上限与日志级别；THE 运行时配置 SHALL 支持按 agent-id 查询与全量列表。
### Requirement 7: 同进程管理 API

- AS 上层模块（Task/File/Proc/Data）, I want 以 Rust API 管理台账, so that 无需额外网络协议即可完成登记与查询。
- 验收：THE gse-server-core SHALL 提供 `Ledger` 接口，覆盖四张表的增删改查；THE Ledger SHALL 提供 Agent 认证校验与运行状态更新方法。
### Requirement 8: 台账与现有链路共存

- AS 开发者, I want 新增台账不破坏现有关键链路, so that 认证、心跳、会话、信令继续可用。
- 验收：WHEN 台账启用，THE 现有 auth/heartbeat/exec RPC 链路 SHALL 保持可用；IF `auth_enabled` 关闭，THE Server SHALL 跳过 token 校验并照常建立会话。
### Requirement 9: HTTP 管理接口

- AS 运维人员, I want 通过 HTTP 管理端口登记与查询台账, so that 无需编程即可在远程机器安装 Agent 前完成预登记与纳管。
- 验收：THE Server SHALL 提供独立 HTTP 管理端口，暴露 hosts、access_points、agents、agent_configs 四表的增删改查；THE HTTP 管理端口 SHALL 由配置项 `http_enabled` 控制开关、`http_listen` 指定监听地址，并支持 `GSE_SERVER_HTTP_LISTEN` 环境变量覆盖。
