# Requirements Document

## Introduction

本 feature 在 gse-job-execution 与 gse-job-templates 之上新增「历史作业重做」能力：运维人员可对一条已存在的作业发起重做，SERVER 以该作业已存储的执行参数（目标 agent_id、解释器、脚本正文、参数、环境变量、工作目录、超时值）作为默认值，经运维人员确认或编辑后创建一条新的独立作业并下发。

重做不修改来源作业，新作业通过 `rerun_of` 字段引用来源作业的 job_id，从而保持作业历史的不可变与可追溯。重做产生的作业沿用既有作业提交的校验、下发、受理与结果回传语义。本 feature 的交付物为需求与技术设计文档，不包含定时下发与批量重做能力。

首版决策：运维人员可在重做时编辑全部执行参数，未提供的字段沿用来源作业取值；重做读取来源作业已展开的脚本正文，不重新展开模板变量；重做作业的 template_id 留空；来源作业可处于任意状态。

## Glossary

- **历史作业（Historical Job）**：已存在于台账中、由运维人员选定用于重做的作业。
- **来源作业（Source Job）**：被重做的历史作业。
- **重做（Rerun）**：以来源作业的执行参数为默认值、经运维人员确认或编辑后提交一次新作业的行为。
- **重做作业（Rerun Job）**：重做产生的新作业，通过 rerun_of 引用来源作业的 job_id。
- **目标 Agent（Target Agent）**：重做作业的下发对象，默认为来源作业的 agent_id 且允许在重做时改派。
- **重做适配器（GseJobAdapter 重做方法）**：`@vectorman/adapters` 中访问作业重做 HTTP 接口的方法。
- 其余术语（GSE Server、GSE Agent、作业、作业脚本、解释器、台账、作业平台、作业状态、终态）沿用 gse-job-execution 定义；模板相关术语沿用 gse-job-templates 定义。

## Requirements

### Requirement 1: 重做发起与来源引用

**User Story:** AS 运维人员, I want 对一条历史作业发起重做, so that 无需重新填写即可再次执行相同的脚本与配置。

#### Acceptance Criteria

1. WHEN 运维人员对指定 job_id 提交重做请求，SERVER SHALL 以来源作业的 agent_id、解释器、脚本正文、参数、环境变量、工作目录与超时值作为默认值创建一条新作业并返回 201。
2. WHEN 重做请求包含对 agent_id、解释器、脚本正文、参数、环境变量、工作目录或超时值的显式取值，SERVER SHALL 以该取值覆盖对应默认值。
3. SERVER SHALL 为重做作业分配全局唯一 job_id。
4. WHEN 新作业由重做产生，SERVER SHALL 在作业记录中保存来源作业的 job_id 作为 rerun_of。
5. SERVER SHALL 保持来源作业记录不变。
6. THE 重做作业 SHALL 复用作业提交的校验、下发、受理与结果回传语义。

### Requirement 2: 目标 Agent 选择

**User Story:** AS 运维人员, I want 指定重做作业下发的 Agent, so that 可在原主机或另一台在线主机重跑。

#### Acceptance Criteria

1. THE 重做请求 SHALL 支持通过 agent_id 字段指定目标 Agent。
2. IF 重做请求未提供 agent_id，SERVER SHALL 使用来源作业的 agent_id 作为目标 Agent。
3. IF 合并后的 agent_id 为空字符串，SERVER SHALL 返回 400 并给出 `invalid_argument` 错误码。
4. IF 目标 Agent 无活跃在线会话，SERVER SHALL 返回 409 并给出 `unavailable` 错误码。

### Requirement 3: 来源作业存在性

**User Story:** AS 运维人员, I want 可对任意历史作业发起重做, so that 成功、失败与在途作业的执行参数都可复用。

#### Acceptance Criteria

1. IF 来源 job_id 不存在，SERVER SHALL 返回 404 并给出 `not_found` 错误码。
2. THE 可重做的来源作业 SHALL 覆盖 `pending`、`dispatched`、`running`、`succeeded`、`failed`、`timeout`、`rejected` 与 `lost` 全部状态。

### Requirement 4: 执行参数默认值与模板处理

**User Story:** AS 运维人员, I want 重做默认使用来源作业当时的参数, so that 重做可复现且不受模板后续变更影响。

#### Acceptance Criteria

1. THE 重做作业的默认执行参数 SHALL 取自来源作业记录中已存储的解释器、脚本正文、参数、环境变量、工作目录与超时值。
2. WHEN 来源作业来源于模板，SERVER SHALL 以来源作业记录中的已展开脚本正文作为默认脚本，且 SHALL 将重做作业的 template_id 置为空。
3. THE 重做作业 SHALL 以合并后的参数参与校验与下发。

### Requirement 5: 重做审计与独立终态

**User Story:** AS 运维人员, I want 每次重做都有独立可追溯的作业记录, so that 执行历史可审计。

#### Acceptance Criteria

1. SERVER SHALL 持久化重做作业的 rerun_of、template_id（空值）与创建时间。
2. WHEN 同一来源作业被多次重做，SERVER SHALL 为每次重做生成不同的 job_id。
3. THE 重做作业的状态与结果字段 SHALL 独立于来源作业。

### Requirement 6: 作业平台前端

**User Story:** AS 运维人员, I want 在作业平台直接重做历史作业, so that 无需命令行即可复用执行参数。

#### Acceptance Criteria

1. THE 作业平台 SHALL 在作业详情提供「重做」入口。
2. WHEN 运维人员触发「重做」，作业平台 SHALL 打开提交表单，并以来源作业的 agent_id、解释器、脚本正文、参数、环境变量、工作目录与超时值预填全部字段。
3. THE 作业平台 SHALL 允许运维人员编辑预填表单中的全部执行参数。
4. WHEN 运维人员确认重做，作业平台 SHALL 调用重做接口并在成功后展示成功提示。
5. WHEN 重做成功，作业平台 SHALL 刷新作业列表并打开重做作业的详情。
6. WHEN 运维人员打开重做作业详情，作业平台 SHALL 展示来源作业的 rerun_of 引用。
7. THE 作业平台 SHALL 通过 `/api/gse` 相对路径访问重做接口。
8. THE 作业平台 SHALL 复用 `@vectorman/primitives` 与 `@vectorman/adapters`，且不直接调用浏览器 fetch。

### Requirement 7: 重做约束与上限

**User Story:** AS 运维人员, I want 重做受既有执行边界约束, so that 复用不突破安全边界。

#### Acceptance Criteria

1. THE 重做作业的脚本正文字节数上限 SHALL 与作业脚本上限一致。
2. THE 重做作业的超时值 SHALL 不超过作业允许的最大超时值。
3. THE 重做作业的解释器 SHALL 通过作业提交的解释器白名单校验。
4. WHEN 台账中的作业由手工提交或模板提交产生且未经重做，SERVER SHALL 将其 rerun_of 记录为空。
