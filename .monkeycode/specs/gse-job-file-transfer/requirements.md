# Requirements Document

## Introduction

本 feature 在既有 GSE 作业（脚本下发与执行）之上，新增「文件传输作业」：运维人员在作业平台提交一次传输任务，GSE Server 经已认证会话通道在 Agent 之间搬运文件，或把文件落到 Server 临时目录。传输沿用作业的异步模型：受理后生成 `job_id`、落库、轮询状态与结果。

v1 已确认范围：

- 单文件传输（路径指向普通文件）。
- 源端为在线 Agent 本机绝对路径，或控制台上传后写入 Server 临时目录的文件。
- 目标端为一个在线 Agent 本机绝对路径，或 Server 临时目录。
- 字节经 GSE Server 中转（源 Agent 与目标 Agent 无需直连）。
- 作业平台提供「文件传输」提交入口，列表与详情复用既有作业页。

非目标（后续版本候选）：目录递归、断点续传、P2P 直传、多目标广播分发、传输过程流式进度推送、对象存储对接。

## Glossary

- **GSE Server / GSE Agent / 台账 / 作业平台**：沿用 gse-job-execution 定义。
- **文件传输作业（File Transfer Job）**：一种作业，载荷为源端、目标端与超时，终态语义与脚本作业共用状态机。
- **源端（Source）**：文件读取位置，取值为源 Agent 绝对路径，或 Server 临时目录中的文件标识。
- **目标端（Destination）**：文件写入位置，取值为目标 Agent 绝对路径，或 Server 临时目录。
- **Server 临时目录（Server Temp Store）**：GSE Server 本机用于暂存传输文件的目录，以传输作业标识隔离。
- **中转（Relay）**：文件字节先到 GSE Server，再写到目标端；源 Agent 与目标 Agent 不建立直连。

## Requirements

### Requirement 1: 提交文件传输作业

**User Story:** AS 运维人员, I want 提交一次文件传输作业, so that 可以在 Agent 之间或 Agent 与 Server 临时目录之间搬运单个文件。

#### Acceptance Criteria

1. WHEN 运维人员提交含源端与目标端的文件传输请求，SERVER SHALL 生成全局唯一 job_id，将作业以 `pending` 状态写入台账，并标记作业种类为文件传输。
2. WHEN 请求缺少源端或目标端必填字段，SERVER SHALL 返回 400 并指明缺失字段。
3. IF 源端与目标端指向同一 Agent 且路径相同，SERVER SHALL 返回 400 并给出 `invalid_argument` 错误码。
4. IF 源端或目标端指定的 Agent 无活跃在线会话，SERVER SHALL 返回 409 并给出 `unavailable` 错误码。
5. IF 请求超时值超过配置的最大超时，SERVER SHALL 返回 400 并给出 `invalid_argument` 错误码。

### Requirement 2: Agent 到 Agent 传输

**User Story:** AS 运维人员, I want 把一台 Agent 上的文件拷到另一台 Agent, so that 机器之间可以互传文件。

#### Acceptance Criteria

1. WHEN 源端与目标端均为 Agent，SERVER SHALL 从源 Agent 读取指定文件，经中转写入目标 Agent 指定路径。
2. WHEN 目标路径的父目录不存在，目标 AGENT SHALL 创建缺失的父目录后再写入。
3. WHEN 传输完成且目标文件字节与源文件一致，SERVER SHALL 将作业状态更新为 `succeeded`。
4. IF 源路径不存在、不是普通文件、或源 AGENT 无读权限，SERVER SHALL 将作业状态更新为 `failed` 并记录失败原因。
5. IF 目标 AGENT 写入失败，SERVER SHALL 将作业状态更新为 `failed` 并记录失败原因。
6. IF 目标路径已存在，SERVER SHALL 将作业状态更新为 `failed`，原因码为 `already_exists`，并保持原目标文件不变。

### Requirement 3: Agent 到 Server 临时目录

**User Story:** AS 运维人员, I want 把 Agent 上的文件存到 Server 临时目录, so that 可以在控制面暂存后再分发或下载。

#### Acceptance Criteria

1. WHEN 目标端为 Server 临时目录，SERVER SHALL 从源 Agent 读取指定文件并写入本次作业对应的临时文件。
2. WHEN 写入 Server 临时目录成功，SERVER SHALL 在作业结果中返回可再次作为源端使用的文件标识与字节大小。
3. THE Server 临时目录中的文件 SHALL 以 job_id 隔离，不同作业的文件路径互不覆盖。

### Requirement 4: Server 临时目录到 Agent

**User Story:** AS 运维人员, I want 把 Server 临时目录中的文件下发到 Agent, so that 暂存文件可以落到目标机器。

#### Acceptance Criteria

1. WHEN 源端为 Server 临时目录且目标端为 Agent，SERVER SHALL 读取该临时文件并写入目标 Agent 指定路径。
2. IF 指定的临时文件标识不存在，SERVER SHALL 返回 404 并给出 `not_found` 错误码。
3. IF 目标路径已存在，SERVER SHALL 将作业状态更新为 `failed`，原因码为 `already_exists`。

### Requirement 5: 控制台上传到 Server 临时目录

**User Story:** AS 运维人员, I want 从作业平台上传一个本地文件到 Server 临时目录, so that 不经过源 Agent 也能作为后续下发的源端。

#### Acceptance Criteria

1. WHEN 运维人员经作业平台上传单个文件，SERVER SHALL 将该文件写入 Server 临时目录并返回文件标识、原始文件名与字节大小。
2. IF 上传字节数超过配置的单文件上限，SERVER SHALL 返回 400 并给出 `invalid_argument` 错误码。

### Requirement 6: 校验、超时与完整性

**User Story:** AS 运维人员, I want 传输有明确边界和结果, so that 失败时能区分超时、源不存在与写入失败。

#### Acceptance Criteria

1. THE 单文件大小上限 SHALL 为可配置项，缺省 64 MiB。
2. IF 源文件大小超过单文件上限，SERVER SHALL 拒绝传输并将作业状态更新为 `failed`，原因码为 `file_too_large`。
3. WHEN 传输时长达到作业超时，SERVER SHALL 停止传输并将作业状态更新为 `timeout`。
4. WHEN 传输成功，SERVER SHALL 在作业结果中记录字节数与内容校验和。
5. IF 目标端落盘后的校验和与源端不一致，SERVER SHALL 将作业状态更新为 `failed`，原因码为 `checksum_mismatch`。

### Requirement 7: 作业查询与审计

**User Story:** AS 运维人员, I want 在既有作业列表和详情中看到文件传输作业, so that 脚本作业与文件作业可以一起跟踪。

#### Acceptance Criteria

1. THE 文件传输作业 SHALL 使用与脚本作业相同的状态机：`pending`、`dispatched`、`running`、`succeeded`、`failed`、`timeout`、`rejected`、`lost`。
2. THE 作业查询响应 SHALL 包含作业种类、源端、目标端、字节数、校验和与失败原因。
3. WHEN 作业到达终态，SERVER SHALL 保持该作业的结果字段不再变更。
4. IF 传输过程中源 Agent 或目标 Agent 会话离线，SERVER SHALL 将作业状态更新为 `lost`。

### Requirement 8: Server 临时文件清理

**User Story:** AS 运维人员, I want 临时文件自动过期, so that Server 磁盘不会被暂存文件占满。

#### Acceptance Criteria

1. THE Server 临时文件的保留时长 SHALL 为可配置项，缺省 24 小时。
2. WHEN 临时文件超过保留时长，SERVER SHALL 删除该文件。
3. WHEN 运维人员请求删除某个临时文件标识，SERVER SHALL 删除对应文件并返回成功。

### Requirement 9: 作业平台提交与展示

**User Story:** AS 运维人员, I want 在作业平台提交文件传输并查看结果, so that 不必手写 HTTP。

#### Acceptance Criteria

1. THE 作业平台 SHALL 提供文件传输提交表单，字段包括源端类型、源 Agent 或临时文件标识、源路径、目标端类型、目标 Agent、目标路径与超时。
2. THE 作业平台 SHALL 提供上传入口，将浏览器本地文件写入 Server 临时目录并回填为源端。
3. WHEN 运维人员打开文件传输作业详情，作业平台 SHALL 展示源端、目标端、字节数、校验和与失败原因。
4. THE 作业列表 SHALL 用种类字段区分脚本作业与文件传输作业。

## Open Questions

已于 2026-09-18 确认：

1. 源文件来源包含 Agent 本机路径与控制台直传到 Server 临时目录（Requirement 5 纳入 v1）。
2. 一次作业仅一个目标端（不做一源多目标广播）。
3. v1 仅单文件，不做目录递归。
