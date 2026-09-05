# Requirements Document

## Introduction

GSE 全局调度引擎的最小闭环：Server 与 Agent 两个二进制组件。Server 作为底座服务，为上层模块（Task / File / Proc / Data）提供统一的 Agent 会话管理与信令上下行通道；Agent 部署在目标机器上，主动外连 Server，建立长连接，接收指令并执行。

本 feature 先实现「连接建立 → 身份认证 → 心跳保活 → 会话管理 → 信令双向上下行」的最小链路，并以一条示例指令（ping）验证全链路可用。

## Glossary

- **GSE Server**：底座服务二进制，集中管理 Agent 会话与信令通道。
- **GSE Agent**：部署在目标机器上的执行端二进制，主动外连 Server。
- **信令**：控制面消息单元，区分方向（下行指令 / 上行回执）。
- **会话**：Server 与单个 Agent 之间一条连接的完整生命周期（认证成功至断连）。
- **Agent ID**：Agent 的唯一标识。
- **上行通道**：Agent → Server 的消息方向。
- **下行通道**：Server → Agent 的消息方向。

## Requirements

### Requirement 1: Agent 主动外连与自动重连

**User Story:** AS 运维人员, I want Agent 主动外连 Server并具备自动重连能力, so that Agent 无需公网入口即可纳入管理。

#### Acceptance Criteria

1. WHEN Agent 启动且配置了 Server 地址，AGENT SHALL 主动发起连接。
2. WHEN 连接断开，AGENT SHALL 以指数退避策略自动重连，退避区间在 1 秒至 60 秒之间。
3. WHEN 连接恢复，AGENT SHALL 重新完成认证并恢复心跳。

### Requirement 2: 身份认证

**User Story:** AS 运维人员, I want 只有合法的 Agent 才能接入, so that 未授权机器无法下发指令。

#### Acceptance Criteria

1. WHEN Agent 建立连接，AGENT SHALL 向 Server 提交 agent-id 与认证凭据。
2. WHEN 凭据有效，SERVER SHALL 建立会话并返回成功。
3. IF 凭据无效或不存在的 agent-id，SERVER SHALL 拒绝连接并记录拒绝原因。
4. WHEN Server 拒绝连接，AGENT SHALL 停止重连并进入待人工处理状态。

### Requirement 3: 心跳与存活检测

**User Story:** AS 上层模块, I want 获知 Agent 是否在线, so that 可以避免向下线 Agent 下发任务。

#### Acceptance Criteria

1. WHILE 会话存活，AGENT SHALL 每间隔 30 秒发送一次心跳。
2. IF Server 在 90 秒内未收到该 Agent 的任何消息，SERVER SHALL 将会话标记为离线。
3. WHEN 会话离线上报，SERVER SHALL 通知订阅方（上层模块）。

### Requirement 4: 会话管理

**User Story:** AS 上层模块, I want 查询与追踪 Agent 会话, so that 可以按需选择在线 Agent。

#### Acceptance Criteria

1. WHEN Agent 认证成功，SERVER SHALL 创建会话并记录 agent-id、连接信息与上线时间。
2. WHEN Agent 断连，SERVER SHALL 移除该会话并记录下线时间。
3. SERVER SHALL 提供会话查询能力（列表、按 agent-id 查询、在线状态）。
4. THE 会话 SHALL 保持「一个 agent-id 至多一个活跃会话」的约束。

### Requirement 5: 信令双向上下行

**User Story:** AS 上层模块, I want 向指定 Agent 下发信令并接收回执, so that 后续任务/文件/进程能力可以复用该通道。

#### Acceptance Criteria

1. WHEN 上层模块向指定 agent-id 下发信令，SERVER SHALL 通过该 Agent 的会话通道送达下行消息。
2. IF 目标 Agent 离线，SERVER SHALL 返回「目标离线」错误。
3. WHEN Agent 产生回执，AGENT SHALL 通过上行通道回传至 Server。
4. SERVER SHALL 将上行回执按信令关联返回给发起方。

### Requirement 6: 链路验证指令

**User Story:** AS 开发者, I want 一条最小示例指令验证端到端链路, so that 后续模块接入时有一致的调用基线。

#### Acceptance Criteria

1. WHEN 上层模块下发 `ping` 信令，AGENT SHALL 回传 `pong` 回执。
2. THE 回执 SHALL 携带原信令的标识，使发起方可关联。