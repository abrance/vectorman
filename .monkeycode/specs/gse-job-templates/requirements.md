# Requirements Document

## Introduction

本 feature 在 gse-job-execution 之上新增「作业模板」能力：把可复用的作业参数（解释器、脚本正文、参数、环境变量、工作目录、超时值）保存为命名模板，供运维人员在作业平台中选择模板快速提交作业，并支持对已有作业「另存为模板」。

本 feature 的交付物为需求与技术设计文档。模板为执行参数的可复用描述，不改变作业执行通道与结果回传语义；模板本身不参与执行，仅在提交时展开为作业请求。定时或批量下发能力不在本 feature 范围内。

首版决策：模板由 GSE Server 持久化到台账并共享；脚本、参数、环境变量与工作目录支持 `${name}` 命名占位符，提交时填入取值；模板不绑定 Agent，目标 Agent 由提交时指定。

## Glossary

- **作业模板（Job Template）**：可复用的命名执行参数集合，由模板 ID、名称、解释器、脚本正文、参数、环境变量、工作目录与超时值构成。
- **模板变量（Template Variable）**：出现在模板脚本、参数、环境变量或工作目录中的 `${name}` 形式占位符，提交时由运维人员提供取值。
- **模板展开（Template Expansion）**：以变量取值替换占位符后生成作业请求的过程。
- **模板适配器（GseJobTemplateAdapter）**：`@vectorman/adapters` 中访问作业模板 HTTP 接口的前端适配器。
- 其余术语（GSE Server、GSE Agent、作业、作业脚本、解释器、台账、作业平台）沿用 gse-job-execution 定义。

## Requirements

### Requirement 1: 作业模板定义与持久化

**User Story:** AS 运维人员, I want 保存可复用的作业参数, so that 无需每次重复填写相同脚本与配置。

#### Acceptance Criteria

1. THE 作业模板 SHALL 保存名称、解释器、脚本正文、参数列表、环境变量、工作目录与超时值。
2. SERVER SHALL 为每个作业模板分配全局唯一 template_id 并将模板持久化到台账。
3. SERVER SHALL 记录作业模板的创建时间与更新时间。
4. THE 作业模板名称 SHALL 在台账内唯一。

### Requirement 2: 作业模板管理

**User Story:** AS 运维人员, I want 创建、查看、修改与删除模板, so that 模板内容可持续维护。

#### Acceptance Criteria

1. WHEN 运维人员提交含名称、解释器与脚本正文的模板创建请求，SERVER SHALL 创建模板并返回 201。
2. SERVER SHALL 提供模板列表、单个模板查询、模板更新与模板删除接口。
3. WHEN 查询或更新的 template_id 不存在，SERVER SHALL 返回 404 并给出 `not_found` 错误码。
4. WHEN 创建或更新的模板缺少名称、解释器或脚本正文，SERVER SHALL 返回 400 并给出 `invalid_argument` 错误码。
5. IF 模板名称与已有模板重复，SERVER SHALL 返回 409 并给出 `already_exists` 错误码。
6. WHEN 运维人员更新模板，SERVER SHALL 覆盖模板的执行参数字段并刷新更新时间。
7. WHEN 运维人员删除模板，SERVER SHALL 从台账移除该模板。

### Requirement 3: 模板变量与展开

**User Story:** AS 运维人员, I want 模板带可变占位符, so that 同一模板适配不同目标与参数。

#### Acceptance Criteria

1. THE 作业模板 SHALL 支持在脚本正文、参数、环境变量值与工作目录中使用 `${name}` 形式的命名占位符。
2. WHEN 运维人员创建或更新模板，SERVER SHALL 校验占位符名称匹配 `[A-Za-z_][A-Za-z0-9_]*`。
3. IF 模板包含不合法占位符名称，SERVER SHALL 返回 400 并给出 `invalid_argument` 错误码。
4. WHEN 运维人员从模板提交作业并传入变量取值，SERVER SHALL 以变量取值替换占位符生成作业请求。
5. IF 提交请求缺少模板声明的变量取值，SERVER SHALL 返回 400 并给出 `invalid_argument` 错误码。
6. IF 提交请求包含模板未声明的变量，SERVER SHALL 忽略该变量。

### Requirement 4: 从模板提交作业

**User Story:** AS 运维人员, I want 选择模板、指定 Agent 并填入变量后提交, so that 快速发起一次作业。

#### Acceptance Criteria

1. WHEN 运维人员指定模板、agent_id 与变量取值并提交，SERVER SHALL 展开模板生成作业请求并复用作业提交的校验规则。
2. THE 从模板提交的请求 SHALL 指定 agent_id。
3. IF 提交请求未指定 agent_id，SERVER SHALL 返回 400 并给出 `invalid_argument` 错误码。
4. WHEN 作业由模板生成，SERVER SHALL 在作业记录中保存来源 template_id。
5. WHEN 模板展开后的脚本正文或超时值超出作业约束，SERVER SHALL 返回 400 并给出 `invalid_argument` 错误码。

### Requirement 5: 从作业另存为模板

**User Story:** AS 运维人员, I want 将已有作业保存为模板, so that 成功执行过的脚本可复用。

#### Acceptance Criteria

1. WHEN 运维人员对指定作业执行「另存为模板」并提供模板名称，SERVER SHALL 以该作业的解释器、脚本正文、参数、环境变量、工作目录与超时值创建模板。
2. WHEN 另存为模板的作业记录包含来源 template_id，SERVER SHALL 不将来源 template_id 复制到新模板。
3. IF 源 job_id 不存在，SERVER SHALL 返回 404 并给出 `not_found` 错误码。

### Requirement 6: 模板查询与前端

**User Story:** AS 运维人员, I want 在作业平台管理并使用模板, so that 无需命令行即可复用执行参数。

#### Acceptance Criteria

1. THE 作业平台 SHALL 提供模板列表，展示模板名称、解释器与更新时间。
2. THE 作业平台 SHALL 提供模板创建、编辑与删除入口。
3. WHEN 运维人员在提交作业表单中选择模板，作业平台 SHALL 预填解释器、脚本正文、参数、超时值与工作目录，并展示模板变量的输入项。
4. WHEN 模板保存成功或删除成功，作业平台 SHALL 刷新模板列表并展示成功提示。
5. WHEN 提交表单引用了模板变量，作业平台 SHALL 将变量取值随提交请求发送至 SERVER。
6. THE 作业平台 SHALL 通过 `/api/gse` 相对路径访问模板接口。
7. THE 作业平台 SHALL 复用 `@vectorman/primitives` 与 `@vectorman/adapters`，且不直接调用浏览器 fetch。

### Requirement 7: 模板约束与上限

**User Story:** AS 运维人员, I want 模板受既有执行边界约束, so that 复用不突破安全边界。

#### Acceptance Criteria

1. THE 模板脚本正文的字节数上限 SHALL 与作业脚本上限一致。
2. THE 模板超时值 SHALL 不超过作业允许的最大超时值。
3. THE 模板未指定解释器时 SHALL 使用 bash 作为解释器。
4. WHEN 运维人员删除模板，SERVER SHALL 保留已提交作业记录的来源 template_id 作为历史引用。
