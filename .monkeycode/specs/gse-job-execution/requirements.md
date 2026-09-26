# Requirements Document

> **已归档**（2026-09-26 文档整理）：本 feature 已实现并合入 main，本文件压缩为需求索引——完整 EARS 条款见 git 历史（本文件重写前的最后一个版本），design.md 全文保留为实施细节档案。

## Introduction

本 feature 在已有 GSE 会话通道（认证、心跳、`exec` 双向 RPC）之上，定义「Server 下发作业脚本、Agent 执行并回传结果」的协议与语义，并定义配套的作业平台前端。目标交付为协议 DTO、Agent 执行器语义、Server 作业管理、HTTP 接口与前端页面的设计文档，不包含代码实现。

作业采用异步模型：Server 受理提交后生成唯一 `job_id` 并落库，经会话通道将内联脚本与解释器下发至 Agent；Agent 以本机用户权限启动子进程，采集标准输出、标准错误与退出码，完成后经回传通道写回 Server；发起方通过轮询查询作业状态与结果。v1 以执行超时作为运行中作业的唯一终止手段。

作业平台前端为 `@vectorman/job` 应用，遵循前端分层架构，提供作业提交、列表跟踪与结果查看。

## Glossary

- **GSE Server**：底座服务，管理 Agent 会话、受理并跟踪作业。
- **GSE Agent**：部署在目标机器上的执行端，接收并执行作业脚本。
- **作业（Job）**：一次由脚本、解释器与执行参数构成的执行请求单元。
- **作业脚本（Job Script）**：随作业内联下发的脚本文本。
- **解释器（Interpreter）**：执行作业脚本的程序，取值属于白名单集合。
- **作业状态（Job Status）**：作业生命周期的稳定取值，见设计文档状态机。
- **受理应答（Job Ack）**：Agent 对下发作业是否受理的即时应答。
- **作业结果（Job Result）**：Agent 对已执行作业回传的终态载荷。
- **台账（Ledger）**：Server 端 sqlite 持久化存储，保存主机、Agent、配置与作业记录。
- **作业通道（Job Channel）**：承载作业下发的 geminio RPC 方法集合。
- **作业平台（Job App）**：`@vectorman/job` 前端应用，提供作业提交、跟踪与结果查看。
- **作业适配器（GseJobAdapter）**：`@vectorman/adapters` 中访问作业 HTTP 接口的前端适配器。

## Requirements

### Requirement 1: 作业提交与校验

- AS 运维人员, I want 向在线 Agent 提交作业脚本, so that 远程执行作业并获得受理确认。
- 验收：WHEN 运维人员向在线 Agent 提交含脚本正文与解释器的作业请求，SERVER SHALL 生成全局唯一 job_id 并将作业以 `pending` 状态写入台账；WHEN 作业请求缺少 agent_id 或脚本正文，SERVER SHALL 返回 400 并在响应中指明缺失字段。
### Requirement 2: 作业下发与受理

- AS 运维人员, I want 作业被 Agent 接收或明确拒绝, so that 可以区分排队、执行与拒绝三种情况。
- 验收：WHEN 作业状态为 `pending`，SERVER SHALL 经会话通道向目标 Agent 发送 job_id、脚本正文、解释器、参数、环境变量、工作目录与超时值，并将状态更新为 `dispatched`；WHEN Agent 收到作业下发，AGENT SHALL 在受理上限内返回受理应答。
### Requirement 3: 脚本执行

- AS 运维人员, I want Agent 按指定解释器执行脚本并采集完整结果, so that 可以审计远程执行。
- 验收：WHILE 作业处于运行态，AGENT SHALL 以指定解释器执行脚本并采集标准输出、标准错误与退出码；AGENT SHALL 以 Agent 进程自身的用户身份执行子进程。
### Requirement 4: 结果回传与持久化

- AS 运维人员, I want 作业结果可靠写回 Server, so that 断线或重启后仍可查询历史。
- 验收：WHEN Agent 完成作业，AGENT SHALL 经回传通道将 job_id、终态、退出码、终止信号、标准输出、标准错误、截断标记与起止时间发送至 Server；WHEN Server 收到作业结果，SERVER SHALL 将结果写入台账并更新作业为对应终态。
### Requirement 5: 作业查询

- AS 运维人员, I want 按条件查询作业状态与结果, so that 可以跟踪执行进度。
- 验收：SERVER SHALL 提供按 job_id 查询单个作业的接口；SERVER SHALL 提供按 agent-id 与状态筛选的作业列表接口。
### Requirement 6: 离线与重启处理

- AS 运维人员, I want 在途作业在异常时有一致归宿, so that 作业状态不存在永久悬挂。
- 验收：IF 会话在作业运行期间转为离线，SERVER SHALL 将该 Agent 处于 `pending`、`dispatched` 与 `running` 状态的作业标记为 `lost`；WHEN Server 进程启动，SERVER SHALL 从台账恢复历史作业记录。
### Requirement 7: 审计与终态不可变

- AS 运维人员, I want 作业记录可审计且结果稳定, so that 事后核对有可信依据。
- 验收：SERVER SHALL 持久化每个作业的 agent_id、解释器、脚本正文、超时值与最终状态；WHEN 作业到达终态，SERVER SHALL 保持该作业的结果字段不再变更。
### Requirement 8: 解释器与资源约束

- AS 运维人员, I want 明确的执行边界, so that 远程执行风险可控。
- 验收：THE 解释器白名单 SHALL 包含 bash、sh 与 python3；THE 未指定解释器的作业 SHALL 使用 bash 作为解释器。
### Requirement 9: 作业平台前端

- AS 运维人员, I want 在作业平台上提交、跟踪并查看作业结果, so that 无需命令行即可使用远程执行能力。
- 验收：THE 作业平台 SHALL 提供作业列表，展示 job_id、agent_id、解释器、状态、创建时间与退出码；WHEN 运维人员提交含 agent_id、脚本与解释器的表单，作业平台 SHALL 调用作业提交接口并在成功后刷新列表。
