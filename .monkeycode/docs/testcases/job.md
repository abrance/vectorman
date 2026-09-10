# 作业平台测试用例

- 范围：GSE 作业平台，覆盖作业执行（脚本+解释器）、作业提交与查询、作业模板、历史作业重做，以及作业平台前端（`frontend/apps/job`）。
- 分层：Agent Unit（执行器）、Server Unit（提交/结果/ledger）、API（gse-server HTTP）、E2E（server+agent 真实进程）、UI（前端组件与交互）。
- 自动化对应关系：`自动化` 列标注现有测试；`缺口` 表示无自动化覆盖，建议后续补充。
- 自动化执行命令见文末。

## 1. 测试环境与前置

| 项 | 说明 |
| --- | --- |
| Agent 执行单测 | 直接调用 `gse-agent-core::job` 执行器；断言退出码、stdout 截断、超时、脚本文件清理 |
| Server 单测 | `ServerConfig::default()`；作业开关 `jobs_enabled`、默认/最大 timeout、脚本与输出上限 |
| API 用例 | axum `Router` + `oneshot`；带 in-process `Server` 时验证提交/重做，不带时验证 503 |
| E2E 用例 | 真实 server+agent 进程；`send_json`/`json_field`/`wait_terminal` 辅助；临时 sqlite |
| 前端 | Vitest + jsdom；primitive `MemoryNotifier`/`MemoryQueryStore`；AntD 按钮文本用正则匹配 |
| 变量占位符 | `${name}`，名满足 `[A-Za-z_][A-Za-z0-9_]*` |

通用前置：`jobs_enabled=true`；作业参数 `interpreter` 在允许列表（bash/sh/python3）；脚本字节数 ≤ 262144；timeout ∈ [1,3600] 且默认 300。

## 2. Agent 作业执行用例（gse-agent-core）

| 用例 ID | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- |
| JOB-EXEC-001 | 执行成功脚本（`echo`） | 退出码 0，`status=succeeded`，stdout 正确采集 | `job::successful_job_captures_stdout` |
| JOB-EXEC-002 | 执行非零退出脚本 | `status=failed`，保留退出码 | `job::nonzero_exit_is_failed` |
| JOB-EXEC-003 | 脚本 sleep 超过 timeout | `status=timeout`，进程组被杀死 | `job::timeout_marks_timeout_and_kills` |
| JOB-EXEC-004 | 输出超过 limit | stdout 按上限截断 | `job::stdout_is_truncated_at_limit` |
| JOB-EXEC-005 | 正常执行 | 临时脚本文件执行后被清理 | `job::script_file_is_removed_after_run` |
| JOB-EXEC-006 | interpreter 不在白名单 | 拒绝执行 | `job::reject_unknown_interpreter` |
| JOB-EXEC-007 | 每 Agent 并发 1 | 串行执行，无并发交错 | 缺口 |

## 3. Server 提交与结果用例（gse-server-core）

| 用例 ID | 前置 | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- | --- |
| JOB-SRV-001 | Agent 离线 | 提交作业 | 拒绝（`invalid_argument`/不可用语义） | `server::submit_job_rejects_offline_agent` |
| JOB-SRV-002 | 在线 | 提交空脚本/超限脚本/非法 timeout | 校验失败 | `server::submit_job_validates_script_and_timeout` |
| JOB-SRV-003 | `jobs_enabled=false` | 提交作业 | `unavailable` | `server::submit_job_disabled_returns_unavailable` |
| JOB-SRV-004 | 作业归属 Agent A | Agent B 上报结果 | 结果被丢弃（防串扰） | `server::job_result_from_wrong_agent_is_dropped` |
| JOB-SRV-005 | 作业已终态 | 重复写入结果 | 幂等，终态不可变 | `server::result_writes_are_idempotent_in_terminal_state` |
| JOB-SRV-006 | 未知 job_id | 上报结果 | 无副作用 | `server::job_result_for_unknown_job_is_noop` |

## 4. Ledger 作业/模板用例

| 用例 ID | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- |
| JOB-LEDGER-001 | 作业 CRUD + 按 agent/status 过滤 | 读写与过滤正确 | `ledger::jobs_crud_roundtrip_and_filters` |
| JOB-LEDGER-002 | `mark_running` 后 `finish` | 结果持久化、状态正确 | `ledger::mark_running_then_finish_persists_result` |
| JOB-LEDGER-003 | 终态作业再次更新 | 被拒绝/忽略，终态不可变 | `ledger::terminal_job_is_immutable` |
| JOB-LEDGER-004 | `mark_rejected` | 记录原因 | `ledger::mark_rejected_records_reason` |
| JOB-LEDGER-005 | `mark_lost_by_agent` | 仅影响该 Agent 的进行中作业 | `ledger::mark_lost_by_agent_only_affects_that_agent` |
| JOB-LEDGER-006 | 启动恢复 | 仅 `inflight` 作业标记 `lost` | `ledger::startup_recovery_marks_inflight_lost_only` |
| JOB-LEDGER-007 | 模板 CRUD 往返 | 各字段（args/env/timeout/working_dir）读写一致 | `ledger::template_crud_roundtrip` |
| JOB-LEDGER-008 | 创建同名模板 | 名称唯一约束，冲突报错 | `ledger::template_name_is_unique` |
| JOB-LEDGER-009 | 模板按 name/limit 查询 | 过滤与分页正确 | `ledger::template_list_filters_by_name_and_limit` |
| JOB-LEDGER-010 | 模板提交生成的作业 | `template_id` 记录来源模板 | `ledger::job_records_template_source` |
| JOB-LEDGER-011 | 重做生成的作业 | `rerun_of` 记录来源 job_id | `ledger::job_records_rerun_source` |

## 5. 模板校验与展开用例（template）

| 用例 ID | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- |
| JOB-TPL-001 | 脚本/args/env/working_dir 中含占位符 | 全部来源变量均被提取 | `template::extract_covers_all_sources` |
| JOB-TPL-002 | 无占位符 | 返回空集合 | `template::extract_returns_empty_without_vars` |
| JOB-TPL-003 | 非法占位符名（如 `${1a}`） | 拒绝 | `invalid_placeholder_name_rejected` |
| JOB-TPL-004 | 未闭合 `${` | 拒绝 | `unterminated_placeholder_rejected` |
| JOB-TPL-005 | 展开时提供多余变量 | 忽略多余，正常替换 | `expand_replaces_all_and_ignores_extras` |
| JOB-TPL-006 | 展开缺少变量 | 拒绝（`invalid_argument`） | `expand_rejects_missing_variable` |
| JOB-TPL-007 | 展开后脚本超限 | 拒绝 | `expand_rejects_script_over_limit` |
| JOB-TPL-008 | 展开后 timeout 超限 | 拒绝 | `expand_rejects_timeout_over_limit` |
| JOB-TPL-009 | 创建模板缺 name/script | 校验失败 | `validate_requires_name_and_script` |

## 6. 历史作业重做用例（rerun）

| 用例 ID | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- |
| JOB-RERUN-001 | 空请求 `{}` | 继承来源全部参数 | `rerun::empty_request_inherits_all` |
| JOB-RERUN-002 | 部分覆盖字段 | 覆盖项生效，其余继承 | `rerun::overrides_replace_fields` |
| JOB-RERUN-003 | `agent_id` 为空 | 回退来源 agent_id | `rerun::empty_agent_falls_back_to_source` |
| JOB-RERUN-004 | 显式空串/空集合 | `working_dir`/`args`/`env` 被清空 | `rerun::explicit_empty_clears_optional_fields` |
| JOB-RERUN-005 | 来源作业任意状态 | 均可重做（不校验来源终态） | `e2e::e2e_rerun_history_job` |
| JOB-RERUN-006 | 重做成功 | 新 job_id、`rerun_of`=来源、`template_id` 为空、来源作业不变 | `e2e::e2e_rerun_history_job` |
| JOB-RERUN-007 | 覆盖脚本后重做 | 新作业执行覆盖后的脚本 | `e2e::e2e_rerun_history_job` |

## 7. 后端 API 用例

### 7.1 作业

| 用例 ID | 前置 | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- | --- |
| JOB-API-101 | 空库 | `GET /api/gse/jobs` | 200 空列表 | `http::jobs_list_empty_and_unknown_get_404` |
| JOB-API-102 | 不存在 id | `GET /api/gse/jobs/nope` | 404 | `http::jobs_list_empty_and_unknown_get_404` |
| JOB-API-103 | 无 in-process Server | `POST /api/gse/jobs` | 503 | `http::jobs_without_in_process_server_return_503` |
| JOB-API-104 | 在线/离线 Agent | `POST /api/gse/jobs` | 校验失败/离线拒绝语义正确 | `http::jobs_create_validation_and_offline_agent` |
| JOB-API-105 | 多作业 | `GET /api/gse/jobs?agent_id=&status=` | 过滤正确 | `http::jobs_list_filters_by_agent_and_status` |

### 7.2 重做

| 用例 ID | 前置 | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- | --- |
| JOB-API-201 | 来源不存在 | `POST /api/gse/jobs/ghost/rerun` | 404 | `http::job_rerun_source_missing_is_404` |
| JOB-API-202 | 无 in-process Server | `POST /api/gse/jobs/{id}/rerun` | 503 | `http::job_rerun_requires_in_process_server` |
| JOB-API-203 | 来源存在 | 空体/带覆盖体 | 合并继承与覆盖，校验后落库 | `http::job_rerun_merges_and_validates` |
| JOB-API-204 | 来源存在 | 非法 JSON 体（`{`） | 400 | `http::job_rerun_merges_and_validates` |

### 7.3 模板

| 用例 ID | 前置 | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- | --- |
| JOB-API-301 | 空库 | 模板 CRUD 全流程 | 增删改查与列表正确 | `http::job_templates_crud` |
| JOB-API-302 | 缺 name/空 script/同名 | 创建模板 | 400 校验失败 / 409 冲突 | `http::job_templates_validation_and_conflict` |
| JOB-API-303 | 无 Server / 缺变量 | `POST /job-templates/{id}/submit` | 503 / 400 | `http::job_template_submit_requires_server_and_vars` |
| JOB-API-304 | 已存在作业 | `POST /api/gse/jobs/{id}/save-as-template` | 生成模板成功 | `http::save_job_as_template` |

## 8. 端到端用例

| 用例 ID | 前置 | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- | --- |
| JOB-E2E-001 | 在线 Agent | 提交作业（成功/失败/超时）并轮询 | 状态经 pending→running→终态，结果正确 | `e2e_job_lifecycle_success_failure_timeout` |
| JOB-E2E-002 | Agent 白名单不含该解释器 | 提交 | 作业 `rejected` | `e2e_job_rejected_when_interpreter_not_allowed` |
| JOB-E2E-003 | 作业进行中 | 重启 server | 进行中作业标记 `lost` | `e2e_inflight_jobs_become_lost_after_restart` |
| JOB-E2E-004 | 已存模板 | 模板提交 + 另存为模板 | 展开变量执行成功，另存模板落库 | `e2e_template_submit_and_save_as_template` |
| JOB-E2E-005 | 存在历史作业 | 空体重做 / 覆盖脚本重做 | `rerun_of` 正确、来源不变、id 互异 | `e2e_rerun_history_job` |

## 9. 前端用例（作业平台应用）

### 9.1 作业列表页 `/jobs`

| 用例 ID | 前置 | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- | --- |
| JOB-UI-101 | 进入页面 | 加载 | 列表轮询刷新（含非终态作业） | 缺口 |
| JOB-UI-102 | 选择 Agent/状态过滤 | 触发查询 | 以 `agent_id`/`status` 过滤列表 | 缺口 |
| JOB-UI-103 | 点击「刷新」 | 手动刷新 | 重新拉取列表 | 缺口 |
| JOB-UI-104 | 点击「提交作业」 | 填写并提交 | 成功后关闭抽屉、提示、打开新作业详情 | 缺口 |
| JOB-UI-105 | 选择模板提交 | 填写变量并提交 | 调用模板提交，成功后打开详情 | 缺口 |
| JOB-UI-106 | 详情中点「重做」 | 确认重做 | 关闭重做抽屉、提示、打开新作业详情 | 缺口 |
| JOB-UI-107 | 某行「查看」 | 打开详情抽屉 | 展示作业字段与结果 | 缺口 |

### 9.2 提交作业抽屉（job-submit-drawer）

| 用例 ID | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- |
| JOB-UI-201 | `argsText` = `"a  b c "` | 解析为 `["a","b","c"]`，丢弃空串 | `job-submit-drawer.test.tsx: splits on whitespace and drops empties` |
| JOB-UI-202 | 表单映射 | 生成正确 `JobSubmit`（agent/interpreter/script/args/timeout） | `maps form values into a job submit payload` |
| JOB-UI-203 | `working_dir` 为空 | 从 payload 省略；`args` 默认空数组 | `omits empty working_dir and defaults args to empty` |
| JOB-UI-204 | 缺必填（agent/script） | 校验拦截，不提交 | `does not submit when required fields are missing` |
| JOB-UI-205 | 选中模板并填变量 | 走模板提交 `{}`（含 vars） | `submits via template with vars when a template is selected` |

### 9.3 作业详情抽屉（job-detail-drawer）

| 用例 ID | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- |
| JOB-UI-301 | 作业带 `rerun_of` | 展示「来源作业」并触发 `onRerun` | `job-detail-drawer.test.tsx: shows the rerun source and invokes onRerun` |
| JOB-UI-302 | 终态作业 | 停止详情轮询 | 缺口 |
| JOB-UI-303 | 非终态作业 | 详情轮询直到终态 | 缺口 |
| JOB-UI-304 | 作业无结果/失败 | 展示空态与错误信息 | 缺口 |

### 9.4 重做抽屉（job-rerun-drawer）

| 用例 ID | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- |
| JOB-UI-401 | `parseEnv` 输入含空行/注释 | 解析 `KEY=VALUE`，忽略空行与 `#` | `job-rerun-drawer.test.tsx: parses KEY=VALUE lines and ignores blanks and comments` |
| JOB-UI-402 | `formatEnv`→`parseEnv` 往返 | 往返一致 | `round-trips through formatEnv` |
| JOB-UI-403 | 表单映射 | 生成正确重做 payload | `maps form values into a rerun payload` |
| JOB-UI-404 | `working_dir` 置空 | 发送空串以清空 | `sends an empty working_dir to clear it` |
| JOB-UI-405 | 打开抽屉 | 全部字段预填来源作业，确认提交 | `prefills every field from the source job and confirms` |

### 9.5 模板页 `/templates` 与模板表单

| 用例 ID | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- |
| JOB-UI-501 | 新建/编辑模板 | 映射并 trim 表单值 | `template-form-drawer.test.tsx: maps and trims form values` |
| JOB-UI-502 | `description`/`working_dir` 为空 | 从 payload 省略 | `omits empty description and working_dir` |
| JOB-UI-503 | 缺 name/script | 阻止提交 | `blocks submit when name and script are missing` |
| JOB-UI-504 | 模板列表删除 | `Popconfirm` 确认后删除 | 缺口 |
| JOB-UI-505 | 模板变量提取（脚本/args/env/working_dir） | 提取全部占位符，无占位符为空 | `use-job-templates.test.tsx: extracts from ...` |
| JOB-UI-506 | 模板加载/创建后刷新/删除后刷新/加载失败提示 | 行为与提示正确 | `use-job-templates.test.tsx: loads templates / refreshes after ... / notifies on load failure` |

### 9.6 状态展示与轮询

| 用例 ID | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- |
| JOB-UI-601 | 各 `JobStatus` 渲染 | `JobStatusTag` 颜色/文案正确 | 缺口 |
| JOB-UI-602 | 非终态作业列表 | 每 5s 轮询 | 缺口 |
| JOB-UI-603 | 详情终态 | 每 3s 轮询并在终态停止 | 缺口 |
| JOB-UI-604 | 在线 Agent | 每 15s 轮询 | 缺口 |
| JOB-UI-605 | 访问 `/jobs`、`/templates` | 渲染对应页面，未知路径 SPA 回退 | 缺口 |

### 9.7 适配器

| 用例 ID | 步骤/输入 | 预期 | 自动化 |
| --- | --- | --- | --- |
| JOB-ADAPTER-001 | `submitJob` | POST 且 body 正确 | `jobs.test.ts: submits a job with POST body` |
| JOB-ADAPTER-002 | `listJobs` 无过滤 | 不带 query | `lists jobs without query when no filters` |
| JOB-ADAPTER-003 | `listJobs` 带 agent/status/limit | query 正确拼接 | `lists jobs with agent, status and limit filters` |
| JOB-ADAPTER-004 | job id 含特殊字符 | 路径 encode | `encodes job id in path` |
| JOB-ADAPTER-005 | `rerunJob` 带覆盖 | POST `/jobs/{id}/rerun` 带 body | `reruns a job with overrides` |
| JOB-ADAPTER-006 | `rerunJob` 默认 | 空体重做 | `reruns a job with an empty body by default` |
| JOB-ADAPTER-007 | 模板 list/get/create/update/delete | 方法、路径、body 正确 | `templates.test.ts: lists/gets/creates/updates/deletes ...` |
| JOB-ADAPTER-008 | 模板提交 / 作业另存模板 | `{id}/submit` 与 save-as-template 正确 | `submits a template with vars` / `saves a job as a template` |

## 10. 覆盖缺口汇总（建议补充）

1. `jobs-page.tsx`、`templates-page.tsx`、`use-jobs`、`use-online-agents`、`use-job-templates` 的页面编排无组件级测试（提交/重做/删除后的刷新与详情联动）。
2. 轮询策略（列表 5s、详情 3s、在线 Agent 15s）无时间可控测试。
3. `status.ts`、`job-status-tag.tsx`、`template-vars-form.tsx`、`errors.ts`、`App.tsx` 无测试。
4. 适配器 `templates.test.ts` 已覆盖主体，`jobs.test.ts` 未覆盖 `getJob`（如存在）与错误映射路径。
5. Agent 端「每 Agent 并发 1」缺少显式并发用例。

## 11. 自动化执行

```bash
# 后端作业/模板/重做相关单测 + e2e
/root/.cargo/bin/cargo test -p gse-agent-core -p gse-server-core

# 仅作业执行器
/root/.cargo/bin/cargo test -p gse-agent-core job::

# 前端全量
cd /workspace/frontend && npm test

# 仅作业 UI 包与适配器
cd /workspace/frontend && npm run test -w @vectorman/job
cd /workspace/frontend && npm run test -w @vectorman/adapters

# 类型检查与统一产物构建
cd /workspace/frontend && npx tsc -p apps/job/tsconfig.json --noEmit
cd /workspace/frontend && npm run build:console
```
