# Requirements Document

## Introduction

本 feature 修复 GSE Server 的**会话生命周期管理缺失**，以及由此派生的三个可观测性问题。

发现的起点是一次真实的 Agent 版本升级：把 `gse-agent` 1.1.0 分发到 testbkee 时，作业下发反复返回 `lost`（`error = agent offline`），而同一时刻 `GET /api/gse/agents` 显示该 Agent `status = online`、心跳持续推进。**只有重启 `gse-server` 才能短暂恢复作业通道**，几分钟后又失效。

排查后确认根因是一条**从未被清理的死会话**：Agent 的长连接断开后，Server 既没有摘除会话、也没有把它标成终止态；而心跳通道独立于连接存活，会持续刷新会话的 `last_seen`，使状态机永远停在 `Online`。作业下发拿到这条死会话、调用失败、把作业标成 `lost`，并以「agent offline」这一误导性措辞记录——**实际原因是连接已死，不是 Agent 离线**。

本 feature 的验收标准是：**会话状态与连接存活一致**；连接断开后作业下发不再拿到死会话；「心跳在线」与「作业通道可用」的差异对运维可见。

已确认范围（2026-09-26 问卷定稿）：

- **核心修复**：连接断开时清理会话（`remove` + `close` + 台账 `mark_offline`）。
- **心跳语义**：心跳到达即证明连接存活，允许它**重建/复活**会话（心跳与作业走同一条 geminio 连接）。
- **可观测性**：把会话真实状态暴露到 API 与前端，让「心跳 online 但会话已死」可被发现。
- **连接活性**：给 Agent 的 TCP 连接设置 keepalive（60s 级），让 OS 尽早发现半死连接。
- **一并排查**：Agent 侧指数退避重连逻辑（日志显示退避到 60s 后重置、`dial` 反复失败）。

非目标：更换 RPC 框架、改造 geminio 本身、引入服务端主动探测作业通道的心跳、多副本 gse-server 的会话共享（当前单副本）。

## Requirements

> **背景事实（排查结论，2026-09-26）**：三个 Agent（cloud2 / debian12 / testbkee）中，testbkee 从内网 `10.10.28.10` 经公网连 `186.244.201.55:30710`。实测其 TCP 连接呈**半死**形态：`ss` 显示 `ESTAB` 但 `Send-Q` 积压 122 字节（有数据发不出去），日志停在 `heartbeat rpc: multiplexer closed`。
> 该机 `tcp_keepalive_time = 600`（10 分钟）—— 对中间设备常见的 5~10 分钟空闲超时而言太慢。

### Requirement 1: 会话随连接断开而清理

**User Story:** AS 运维人员, I want 连接断开后会话立即从注册表移除, so that 作业下发不会拿到一条死会话。

#### Acceptance Criteria

1. THE `gse-server` SHALL 在连接结束后**主动清理该连接对应的会话**：从 `SessionRegistry` 移除、状态置 `Closed`、台账置 `offline`。
2. THE 清理 SHALL 覆盖**所有**连接结束路径：正常关闭、对端 RST、读循环出错、认证后立即断开。
3. THE 清理 SHALL 只作用于**该连接自己建立的那条会话**：若同一 `agent_id` 已有更新的连接接管（重连场景），旧连接结束**不得**摘掉新会话。
4. THE `SessionRegistry` SHALL 提供按**连接身份**（而非仅 `agent_id`）移除的能力，使上述第 3 条可达。
5. WHEN 连接结束触发清理, THE `gse-server` SHALL 在日志中记录（含 `agent_id` 与结束原因），不得静默。

### Requirement 2: 心跳可重建会话，且不复活已终止的会话

**User Story:** AS 运维人员, I want 心跳到达时能确认连接存活, so that 会话状态反映真实的可达性。

#### Acceptance Criteria

1. WHEN 收到某 `agent_id` 的心跳但注册表中**没有**该会话, THE `gse-server` SHALL 依据**该心跳所在连接**重建一条 `Online` 会话。
2. THE 重建 SHALL 使用心跳所在连接的 `End`，使后续作业下发走同一连接。
3. THE 心跳 SHALL NOT 让一条**已 `Closed`** 的会话复活——`Closed` 是终止态，只有新连接才能产生新会话。
4. IF 心跳重建了会话, THEN THE `gse-server` SHALL 在日志中记录该事件（区别于常规 `touch`）。
5. THE 心跳处理 SHALL 在连接**未认证**时不建立任何会话。

### Requirement 3: 会话真实状态对运维可见

**User Story:** AS 运维人员, I want 区分「心跳在线」与「作业通道可用」, so that 我能判断 Agent 是否能接作业。

#### Acceptance Criteria

1. THE 台账 API（`GET /api/gse/agents`）SHALL 为每个 Agent 暴露**会话状态**（`session_state`）与其可获得性，与 `status`（心跳口径）并列且语义区分清楚。
2. WHEN 某 Agent 的心跳在窗口内但会话不存在或非 `Online`, THE API SHALL 使这一差异**可被查询出来**（不得两种情况返回完全相同的结果）。
3. THE 前端 Agent 列表 SHALL 展示该差异（例如会话状态字段或异常标记），使用户不必查日志才能发现。
4. THE 作业下发失败的错误措辞 SHALL 区分**「Agent 确实离线」**与**「会话不可用」**两种原因。

### Requirement 4: Agent 侧连接活性与重连

**User Story:** AS 运维人员, I want Agent 尽早发现半死连接并快速重连, so that 作业通道的中断窗口尽量短。

> **2026-09-26 修订**：原计划「给 Agent 加 TCP keepalive」，但读 geminio 源码后确认
> `DialOptions` 不暴露底层 socket（做不到）。**不需要了** —— geminio **自带连接层心跳**：
> 默认 `Heartbeat::Seconds5`，closewait = 5s × 6 = **30 秒**无包即拆连接，比系统默认的
> TCP keepalive（600s）快 20 倍。问题不在检测慢，而在服务端**没监听检测结果**（Requirement 1）。

#### Acceptance Criteria

1. THE `gse-agent` SHALL 依赖 **geminio 的连接层心跳**（默认 30 秒 closewait）检测半死连接，
   不引入 TCP keepalive（`DialOptions` 不暴露 socket，做不到）。
   `DialOptions.heartbeat` 作为**可调旋钮**保留：现场若发现 30 秒过于敏感，可调至 `Seconds20`（120 秒窗口）。
2. THE Agent 的重连退避 SHALL 在**连接成功建立后重置**，使一次成功连接后若再断，不必等待 60s。
3. WHEN 心跳 RPC 失败, THE Agent SHALL 视为连接已死并进入重连，不得在已死连接上继续发心跳。
4. THE Agent SHALL 在日志中记录重连的原因与当前退避值（现状已记录措辞，需保持）。
   WHEN 半死连接被连接层心跳判定超时, THE Agent SHALL 让该错误**向上传播触发重连**
   （现状 `heartbeat_loop` 用 `?` 传播 `Err`，符合；加测试钉住）。

### Requirement 5: 回归可验证

**User Story:** AS 开发人员, I want 上述行为有可运行的测试, so that 修复不会在后续改动中静默退化。

#### Acceptance Criteria

1. THE 会话清理 SHALL 有测试覆盖：连接断开后注册表中不再存在该会话、台账被置 `offline`。
2. THE 重连接管 SHALL 有测试覆盖：旧连接结束不影响新连接建立的同名会话。
3. THE 心跳重建 SHALL 有测试覆盖：无会话时心跳能建、`Closed` 会话不被心跳复活、未认证连接的心跳不建会话。
4. THE 上述测试 SHALL 在移除对应修复后**失败**（即测试确实约束该行为，不通过则说明测试无效）。

### Requirement 6: 规格与文档同步

**User Story:** AS 后续维护者, I want 这次事故的根因与结论记录在案, so that 同类问题不必重新排查一遍。

#### Acceptance Criteria

1. THE `observability-hardening/tasklist.md` SHALL 指向本 feature 并说明两者的关系（前者记录现象与待办，本 feature 是修复）。
2. THE 本 feature 的 `design.md` SHALL 记录**排查路径**：心跳正常但作业失败的矛盾现象、如何定位到会话清理缺失、如何用 `ss` 的 `Send-Q` 判定连接半死。
3. THE `README.md` 的「已知限制」SHALL 反映本修复后的口径（若仍有残留限制）。

## Out of Scope

- **服务端主动探测作业通道**：本 feature 只做「连接断开即清理」与「心跳重建」，不新增独立于连接的健康探针。
- **多副本 gse-server**：当前单副本（`strategy: Recreate`），会话注册表在内存中；多副本的会话共享不在范围。
- **更换 RPC 框架**：geminio 的 multiplexer 语义问题（`end closed` / `multiplexer closed` 的判定）本轮不改，只在应用层做防御。
- **中间网络设备配置**：用户明确决定不动环境侧（问卷 2026-09-26），只在软件层解决。
