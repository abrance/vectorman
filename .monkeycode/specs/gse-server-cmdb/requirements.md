# Requirements Document

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

**User Story:** AS 运维人员, I want 台账数据在 Server 重启后依然存在, so that 资产登记与运行状态不因进程重启丢失。

#### Acceptance Criteria

1. WHEN gse-server 启动，THE Server SHALL 打开该 Server 的 sqlite 台账数据库。
2. WHEN 数据库打开成功，THE Server SHALL 自动创建四张表（hosts、access_points、agents、agent_configs）。
3. IF 数据库文件缺失，THE Server SHALL 自动创建新数据库后继续启动。
4. THE 台账数据库路径 SHALL 由配置项 `db` 指定并支持 `GSE_SERVER_DB` 环境变量覆盖。

### Requirement 2: 主机资产登记

**User Story:** AS 运维人员, I want 登记每台被纳管机器的资产信息, so that 无论是否安装 Agent，机器都能被追踪。

#### Acceptance Criteria

1. THE 主机表 SHALL 记录 host-id、内网 IP、主机名、操作系统类型、操作系统版本、CPU 规格、内存规格与登记时间。
2. WHEN 调用方登记主机，THE Server SHALL 以 host-id 为主键幂等写入。
3. WHEN 调用方查询主机，THE Server SHALL 支持按 host-id 查询与全量列表。
4. IF 主机尚未安装任何 Agent，THE 主机表 SHALL 仍保留该机器记录。

### Requirement 3: 接入点登记与 Server 自登记

**User Story:** AS 运维人员, I want 记录每个 GSE Server 的通信地址与端口, so that 后期可扩展为多个 Server 高可用。

#### Acceptance Criteria

1. THE 接入点表 SHALL 记录接入点标识、名称、Server IP、RPC/心跳端口、文件传输端口与数据上报端口。
2. WHEN gse-server 启动，THE Server SHALL 将自身信息以接入点记录 upsert 入表。
3. THE 接入点登记 SHALL 支持增删改查，且以接入点标识为主键幂等写入。
4. WHILE 本期仅登记不选路，THE Agent 连接目标 SHALL 仍来自 Agent 本地配置的 server_addr。

### Requirement 4: Agent 预登记与查库认证

**User Story:** AS 运维人员, I want 只有预登记且 token 匹配的 Agent 才能接入, so that 未授权机器无法连入。

#### Acceptance Criteria

1. THE Agent 表 SHALL 记录 agent-id、关联 host-id、绑定接入点、token、Agent 版本、安装路径与登记时间，agent-id 为主键。
2. WHEN Agent 发起认证，THE Server SHALL 仅从 agents 表校验 agent-id 与 token 的一致性。
3. IF agent-id 未登记或 token 不匹配，THE Server SHALL 拒绝认证并记录拒绝原因。
4. THE 配置文件 `[agents]` 静态表 SHALL 不再作为认证数据源。

### Requirement 5: Agent 运行状态回写

**User Story:** AS 上层模块, I want agents 表实时反映 Agent 在线状态, so that 可在选路和告警前判断可用性。

#### Acceptance Criteria

1. WHEN Agent 认证成功，THE Server SHALL 将 agents 表对应记录状态置为在线并更新最后心跳时间。
2. WHILE 会话存活，THE Server SHALL 在每次收到心跳时更新该 Agent 的最后心跳时间。
3. IF 会话因超时进入离线状态，THE Server SHALL 将 agents 表对应记录状态置为离线。
4. THE agents 表运行状态 SHALL 与 Server 会话状态保持一致。

### Requirement 6: Agent 运行时配置登记

**User Story:** AS 运维人员, I want 为每个 Agent 维护资源限制与日志配置, so that Agent 不会抢占业务资源。

#### Acceptance Criteria

1. THE 运行时配置表 SHALL 以 agent-id 关联记录 CPU 使用率上限、内存使用率上限与日志级别。
2. THE 运行时配置 SHALL 支持按 agent-id 查询与全量列表。
3. THE 运行时配置 SHALL 支持新增与更新，重复写入以 agent-id 幂等覆盖。
4. WHILE 本期仅存储与查询，THE Agent SHALL NOT 读取或应用这些配置项（此为后续需求，不在本期实现）。

### Requirement 7: 同进程管理 API

**User Story:** AS 上层模块（Task/File/Proc/Data）, I want 以 Rust API 管理台账, so that 无需额外网络协议即可完成登记与查询。

#### Acceptance Criteria

1. THE gse-server-core SHALL 提供 `Ledger` 接口，覆盖四张表的增删改查。
2. THE Ledger SHALL 提供 Agent 认证校验与运行状态更新方法。
3. WHEN 调用方使用台账接口，THE Server SHALL 在收到响应前完成落库。
4. THE 身份证查询（host-id 存在性）在 Agent 认证时 SHALL 无需强制校验，允许 agent 关联任意已登记 host-id。

### Requirement 8: 台账与现有链路共存

**User Story:** AS 开发者, I want 新增台账不破坏现有关键链路, so that 认证、心跳、会话、信令继续可用。

#### Acceptance Criteria

1. WHEN 台账启用，THE 现有 auth/heartbeat/exec RPC 链路 SHALL 保持可用。
2. IF `auth_enabled` 关闭，THE Server SHALL 跳过 token 校验并照常建立会话。
3. WHEN Ledger 查询失败，THE Server SHALL 将错误编码为 GseError 返回调用方，不影响其他会话。
4. THE 台账数据库连接错误 SHALL 导致 Server 以非零退出码结束并记录原因。

### Requirement 9: HTTP 管理接口

**User Story:** AS 运维人员, I want 通过 HTTP 管理端口登记与查询台账, so that 无需编程即可在远程机器安装 Agent 前完成预登记与纳管。

#### Acceptance Criteria

1. THE Server SHALL 提供独立 HTTP 管理端口，暴露 hosts、access_points、agents、agent_configs 四表的增删改查。
2. THE HTTP 管理端口 SHALL 由配置项 `http_enabled` 控制开关、`http_listen` 指定监听地址，并支持 `GSE_SERVER_HTTP_LISTEN` 环境变量覆盖。
3. WHEN 运维登记 Agent，THE HTTP 接口 SHALL 校验必填字段并以 JSON 返回操作结果；失败时返回明确错误信息。
4. WHEN 运维查询 Agent，THE HTTP 接口 SHALL 返回 agent-id、关联 host、token、版本、安装路径、运行状态与最后心跳时间。
5. THE HTTP 管理接口默认 SHALL 仅监听回环地址，供内网/受控环境使用，不对外暴露。