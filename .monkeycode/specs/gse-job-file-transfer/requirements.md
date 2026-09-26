# Requirements Document

> **已归档**（2026-09-26 文档整理）：本 feature 已实现并合入 main，本文件压缩为需求索引——完整 EARS 条款见 git 历史（本文件重写前的最后一个版本），design.md 全文保留为实施细节档案。

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

- AS 运维人员, I want 提交一次文件传输作业, so that 可以在 Agent 之间或 Agent 与 Server 临时目录之间搬运单个文件。
- 验收：WHEN 运维人员提交含源端与目标端的文件传输请求，SERVER SHALL 生成全局唯一 job_id，将作业以 `pending` 状态写入台账，并标记作业种类为文件传输；WHEN 请求缺少源端或目标端必填字段，SERVER SHALL 返回 400 并指明缺失字段。
### Requirement 2: Agent 到 Agent 传输

- AS 运维人员, I want 把一台 Agent 上的文件拷到另一台 Agent, so that 机器之间可以互传文件。
- 验收：WHEN 源端与目标端均为 Agent，SERVER SHALL 从源 Agent 读取指定文件，经中转写入目标 Agent 指定路径；WHEN 目标路径的父目录不存在，目标 AGENT SHALL 创建缺失的父目录后再写入。
### Requirement 3: Agent 到 Server 临时目录

- AS 运维人员, I want 把 Agent 上的文件存到 Server 临时目录, so that 可以在控制面暂存后再分发或下载。
- 验收：WHEN 目标端为 Server 临时目录，SERVER SHALL 从源 Agent 读取指定文件并写入本次作业对应的临时文件；WHEN 写入 Server 临时目录成功，SERVER SHALL 在作业结果中返回可再次作为源端使用的文件标识与字节大小。
### Requirement 4: Server 临时目录到 Agent

- AS 运维人员, I want 把 Server 临时目录中的文件下发到 Agent, so that 暂存文件可以落到目标机器。
- 验收：WHEN 源端为 Server 临时目录且目标端为 Agent，SERVER SHALL 读取该临时文件并写入目标 Agent 指定路径；IF 指定的临时文件标识不存在，SERVER SHALL 返回 404 并给出 `not_found` 错误码。
### Requirement 5: 控制台上传到 Server 临时目录

- AS 运维人员, I want 从作业平台上传一个本地文件到 Server 临时目录, so that 不经过源 Agent 也能作为后续下发的源端。
- 验收：WHEN 运维人员经作业平台上传单个文件，SERVER SHALL 将该文件写入 Server 临时目录并返回文件标识、原始文件名与字节大小；IF 上传字节数超过配置的单文件上限，SERVER SHALL 返回 400 并给出 `invalid_argument` 错误码。
### Requirement 6: 校验、超时与完整性

- AS 运维人员, I want 传输有明确边界和结果, so that 失败时能区分超时、源不存在与写入失败。
- 验收：THE 单文件大小上限 SHALL 为可配置项，缺省 64 MiB；IF 源文件大小超过单文件上限，SERVER SHALL 拒绝传输并将作业状态更新为 `failed`，原因码为 `file_too_large`。
### Requirement 7: 作业查询与审计

- AS 运维人员, I want 在既有作业列表和详情中看到文件传输作业, so that 脚本作业与文件作业可以一起跟踪。
- 验收：THE 文件传输作业 SHALL 使用与脚本作业相同的状态机：`pending`、`dispatched`、`running`、`succeeded`、`failed`、`timeout`、`rejected`、`lost`；THE 作业查询响应 SHALL 包含作业种类、源端、目标端、字节数、校验和与失败原因。
### Requirement 8: Server 临时文件清理

- AS 运维人员, I want 临时文件自动过期, so that Server 磁盘不会被暂存文件占满。
- 验收：THE Server 临时文件的保留时长 SHALL 为可配置项，缺省 24 小时；WHEN 临时文件超过保留时长，SERVER SHALL 删除该文件。
### Requirement 9: 作业平台提交与展示

- AS 运维人员, I want 在作业平台提交文件传输并查看结果, so that 不必手写 HTTP。
- 验收：THE 作业平台 SHALL 提供文件传输提交表单，字段包括源端类型、源 Agent 或临时文件标识、源路径、目标端类型、目标 Agent、目标路径与超时；THE 作业平台 SHALL 提供上传入口，将浏览器本地文件写入 Server 临时目录并回填为源端。
