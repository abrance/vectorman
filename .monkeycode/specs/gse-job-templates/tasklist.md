# 需求实施计划

- [x] 1. 台账 job_templates 表与 jobs.template_id 迁移
  - [x] 1.1 定义 `JobTemplate` 与 `NewJobTemplate`，在 `init` 建 `job_templates` 表、`name` 唯一约束与 `idx_templates_name` 索引（设计「job_templates 表」，需求 R1）
  - [x] 1.2 在 `init` 检测 `jobs` 缺失列并 `ALTER TABLE jobs ADD COLUMN template_id TEXT`，重复 `init` 幂等（设计「jobs 表变更」，需求 R4/R7）
  - [x] 1.3 实现 `insert_template`/`get_template`/`get_template_by_name`/`list_templates`/`update_template`/`delete_template`，唯一冲突映射 `already_exists`，`args`/`env` 以 JSON 文本存取（设计「ledger.rs」，需求 R1/R2/R7）
  - [x] 1.4 `JobRecord` 与 `NewJob` 增加 `template_id: Option<String>`，insert/scan 覆盖，更新既有 `NewJob` 字面量（设计「jobs 表变更」，需求 R4）
  - [x]* 1.5 编写 ledger 单元测试：`job_templates` 建表幂等、`jobs.template_id` 迁移幂等、CRUD、name 唯一冲突、按 name 过滤

- [x] 2. 检查点 - ledger 模板表测试通过
  - 确保所有测试通过,如有疑问请询问用户

- [x] 3. gse-server-core 模板校验与展开
  - [x] 3.1 新增 `template.rs`：定义 `TemplateInput` 与 `ExpandedJob`（设计「template.rs（新增）」，需求 R3/R4）
  - [x] 3.2 实现 `extract_variables`（扫描脚本/参数/env 值/工作目录）与 `validate_placeholders`（名称匹配 `[A-Za-z_][A-Za-z0-9_]*`，违规 `invalid_argument`）（设计「template.rs（新增）」，需求 R3）
  - [x] 3.3 实现 `expand`：替换 `${name}`，缺失取值拒绝、额外变量忽略，并复用作业约束（脚本字节数、超时上限、解释器默认 bash 与白名单）（设计「template.rs（新增）」，需求 R3/R4/R7）
  - [x] 3.4 在 `lib.rs` 导出 `JobTemplate`/`TemplateInput` 与 `template::{extract_variables, validate_placeholders, expand}`（设计「lib.rs」）
  - [x]* 3.5 编写单元测试：多来源提取、非法名称拒绝、缺变量拒绝、额外变量忽略、展开无 `${...}` 残留、展开后超限拒绝

- [x] 4. HTTP 模板接口
  - [x] 4.1 在 `/api/gse` 新增 `POST/GET /job-templates`、`GET/PUT/DELETE /job-templates/{template_id}` 路由、请求体与状态码映射（201/400/404/409/500）（设计「http.rs（模板接口）」，需求 R2）
  - [x] 4.2 新增 `POST /job-templates/{template_id}/submit`：读取模板 → `expand` → 经 `submit_job` 下发并记录 `template_id`；无进程内 Server 或作业禁用时 503（设计「http.rs（模板接口）」，需求 R4）
  - [x] 4.3 新增 `POST /jobs/{job_id}/save-as-template`：按作业字段创建模板且不复制来源 `template_id`，源作业缺失 404（设计「http.rs（模板接口）」，需求 R5）
  - [x]* 4.4 编写 http 测试：创建 201、缺字段 400、重名 409、列表过滤、单模板 404、更新 200、删除 204、模板提交 201 带 template_id、缺变量 400、另存 201、无 Server 提交 503

- [x] 5. 端到端验证
  - [x]* 5.1 扩展 `tests/e2e.rs`：创建含 `${svc}` 模板 → 用模板提交 `web-01` → 轮询 `succeeded` 且 stdout 为变量替换结果；再另存为模板并二次提交成功（设计「Test Strategy」，需求 R3/R4/R5）

- [x] 6. 检查点 - 后端模板链路测试通过
  - 确保所有测试通过,如有疑问请询问用户

- [x] 7. 前端 GseJobTemplateAdapter
  - [x] 7.1 在 `@vectorman/adapters` 新增 `gse/templates.ts`：`JobTemplate`/`TemplateInput`/`TemplateSubmitRequest` 与 `listTemplates`/`getTemplate`/`createTemplate`/`updateTemplate`/`deleteTemplate`/`submitTemplate`/`saveJobAsTemplate`，并在 `index.ts` 导出（设计「适配器新增」，需求 R6）
  - [x]* 7.2 编写适配器单元测试：各方法 method/url/查询参数/body

- [x] 8. 前端 @vectorman/job 模板功能
  - [x] 8.1 Runtime 增加 `templates: GseJobTemplateAdapter` 并在 `main.tsx` 装配（设计「作业平台前端」，需求 R6）
  - [x] 8.2 实现 `use-job-templates.ts`：列表加载、创建/更新/删除后刷新与成功提示，及本地 `extractVariables`（设计「作业平台前端」，需求 R6）
  - [x] 8.3 实现 `TemplatesPage` 与 `TemplateFormDrawer`（名称/描述/解释器/脚本/参数/工作目录/超时；必填与范围校验；导航新增「模板」路由）（设计「作业平台前端」，需求 R6）
  - [x] 8.4 `JobSubmitDrawer` 增加模板选择：选中后预填字段并渲染 `${name}` 变量输入项，提交改调 `submitTemplate` 携带 `vars`，未选模板走 `submitJob`（设计「作业平台前端」，需求 R3/R4/R6）
  - [x] 8.5 `JobDetailDrawer` 增加「另存为模板」入口并调用 `saveJobAsTemplate`（设计「作业平台前端」，需求 R5/R6）
  - [x]* 8.6 编写前端测试：模板适配器、`useJobTemplates` 刷新、变量提取、提交抽屉模板预填与 vars、表单校验

- [x] 9. 检查点 - 前端测试与构建通过
  - 确保所有测试通过,如有疑问请询问用户
