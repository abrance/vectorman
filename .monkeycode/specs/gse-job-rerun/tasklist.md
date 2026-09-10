# 需求实施计划

- [x] 1. 台账 `jobs.rerun_of` 列与迁移
  - [x] 1.1 `JobRecord` 与 `NewJob` 增加 `rerun_of: Option<String>`（设计「Ledger 扩展」，需求 R1/R5）
  - [x] 1.2 在 `init` 检测 `jobs` 缺失 `rerun_of` 列并 `ALTER TABLE jobs ADD COLUMN rerun_of TEXT`，重复 `init` 幂等（设计「Ledger 扩展」，需求 R5/R7）
  - [x] 1.3 `insert_job` 写入 `rerun_of`、`row_to_job` 读取 `rerun_of`，历史记录与手工提交记录为 NULL（设计「Ledger 扩展」，需求 R1/R5/R7）
  - [x] 1.4 更新既有 `NewJob` 字面量与构造点以补全 `rerun_of` 字段（设计「Ledger 扩展」，需求 R5）
  - [x] 1.5 编写 ledger 单元测试：`rerun_of` 写入读回、迁移幂等、手工/模板作业 `rerun_of` 为 NULL

- [x] 2. 检查点 - ledger 测试通过
  - 确保所有测试通过,如有疑问请询问用户

- [x] 3. 重做参数合并模块 `rerun.rs`
  - [x] 3.1 新增 `rerun.rs`：定义 `RerunRequest`（agent_id/interpreter/script/args/env/working_dir/timeout_secs 全可选）（设计「合并模块」，需求 R1/R2/R4）
  - [x] 3.2 实现 `build_rerun_submit`：未提供字段继承来源作业，`working_dir` 空串与 `args`/`env` 显式空值表示清空，`agent_id` 去空白（设计「合并规则」，需求 R1/R2/R4）
  - [x] 3.3 在 `lib.rs` 导出 `RerunRequest` 与 `build_rerun_submit`（设计「合并模块」）
  - [x] 3.4 编写单元测试：空请求全继承、逐字段覆盖、显式清空 working_dir/args/env、agent_id 去空白、timeout 覆盖

- [x] 4. Server 提交入口统一并接入重做
  - [x] 4.1 抽出 `submit_job_with_source(..., template_id, rerun_of)` 承载校验/落库/派发，`submit_job_with_template` 委托之（设计「Server 提交入口」，需求 R1/R7）
  - [x] 4.2 新增 `submit_rerun`：`build_rerun_submit` 合并后以 `template_id=None`、`rerun_of=来源 job_id` 提交，复用作业校验与下发语义（设计「Server 提交入口」，需求 R1/R4/R5）

- [x] 5. HTTP 重做接口
  - [x] 5.1 新增 `POST /api/gse/jobs/{job_id}/rerun`：加载来源作业（缺失 404 `not_found`）、反序列化可省略请求体、无进程内 Server 时 503，成功返回 201 JobRecord 并按 `job_status` 映射错误（设计「HTTP 接口」，需求 R2/R3/R6/R7）
  - [x] 5.2 编写 http 测试：空体复制来源参数、部分覆盖生效、来源 404、离线 Agent 409、脚本超限 400、模板来源重做 `template_id` 为 NULL、两次重做 job_id 不同、无 Server 503

- [x] 6. 端到端验证
  - [x] 6.1 扩展 `tests/e2e.rs`：提交作业至 `succeeded` → 重做 → 新作业 `succeeded`、`rerun_of` 指向来源、来源记录未变（设计「Test Strategy」，需求 R1/R3/R5）

- [x] 7. 检查点 - 后端重做链路测试通过
  - 确保所有测试通过,如有疑问请询问用户

- [x] 8. 前端 GseJobAdapter 重做方法
  - [x] 8.1 `Job` 增加 `rerun_of`，新增 `JobRerunRequest` 与 `rerunJob(jobId, req)`（`POST /api/gse/jobs/{job_id}/rerun`，空体默认 `{}`）（设计「前端适配器」，需求 R6）
  - [x] 8.2 编写适配器单元测试：`rerunJob` 的 method/url/body 与默认空体

- [x] 9. @vectorman/job 重做功能
  - [x] 9.1 `use-jobs.ts` 新增 `rerun(jobId, req)`：调用适配器后刷新列表并返回新作业（设计「作业平台」，需求 R6）
  - [x] 9.2 新增 `JobRerunDrawer`：以来源作业预填 agent_id/interpreter/script/args/env/timeout/working_dir 全部字段，新增 `parseEnv`/`formatEnv` 处理 `KEY=VALUE` 多行，确认后发送完整 `JobRerunRequest`（设计「作业平台」，需求 R1/R2/R4/R6）
  - [x] 9.3 `JobDetailDrawer` 增加「重做」按钮与 `onRerun` 回调，详情 `Descriptions` 增加「来源作业」展示 `rerun_of`（设计「作业平台」，需求 R5/R6）
  - [x] 9.4 `jobs-page.tsx` 编排：`onRerun` 打开重做抽屉，成功后刷新列表、关闭抽屉并打开新作业详情（设计「作业平台」，需求 R6）
  - [x] 9.5 编写前端测试：`rerunJob` 请求、重做抽屉预填与编辑提交、`parseEnv`/`formatEnv` 往返、详情「来源作业」与「重做」回调

- [x] 10. 检查点 - 前端测试与构建通过
  - 确保所有测试通过,如有疑问请询问用户
