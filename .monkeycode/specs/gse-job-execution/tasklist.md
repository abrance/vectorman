# 需求实施计划

- [x] 1. 扩展 gse-proto 作业 DTO
  - [x] 1.1 定义 `JobStatus`、`JobExec`、`JobAck`、`JobResult` 与稳定错误码辅助（设计「crates/gse-proto（新增 DTO）」「RPC 方法表」，需求 R1-R4）
  - [x]* 1.2 为 DTO 序列化往返与 `JobStatus` 字符串稳定性编写单元测试（设计 Test Strategy）

- [x] 2. 扩展 ledger jobs 表与作业 CRUD
  - [x] 2.1 定义 `JobRecord` 与 `NewJob`，在 `init` 建 `jobs` 表与索引（设计「jobs 表」, 需求 R7）
  - [x] 2.2 实现 `insert_job`/`get_job`/`list_jobs`/`mark_running`/`finish_job`/`mark_rejected`/`mark_lost_by_agent`/`mark_lost_inflight_on_startup`，终态写用 `WHERE status NOT IN (终态)` 保证不可变（设计「ledger.rs」，需求 R4/R6/R7）
  - [x]* 2.3 编写 ledger jobs 单元测试：建表幂等、状态流转、终态不可覆盖、按 agent_id/status 过滤、启动恢复将 in-flight 置 `lost`

- [x] 3. 检查点 - gse-proto 与 ledger 测试通过
  - 确保所有测试通过,如有疑问请询问用户

- [x] 4. gse-agent-core 作业执行器
  - [x] 4.1 扩展 `AgentConfig`：`allowed_interpreters`、`job_default_interpreter`、`max_concurrent_jobs`、`job_work_dir` 及环境变量覆盖（设计「配置新增」，需求 R8）
  - [x] 4.2 实现 `JobExecutor`：临时脚本落盘、spawn 子进程、并发采集 stdout/stderr、按上限截断、超时 SIGTERM→SIGKILL、退出码/信号、完成后清理（设计「gse-agent-core（执行器）」，需求 R3）
  - [x] 4.3 在 `connect_once` 注册 `job_exec` handler，实现白名单/并发受理校验，并在执行完成后调用 `job_result` 回传（设计「RPC 方法表」，需求 R2/R4）
  - [x]* 4.4 编写执行器单元测试：成功、非零退出、stderr 捕获、截断、超时终止、非白名单、并发 busy、临时文件清理

- [x] 5. gse-server-core 作业调度
  - [x] 5.1 扩展 `ServerConfig` 作业参数与环境变量覆盖（设计「配置新增」，需求 R8）
  - [x] 5.2 在 `Server` 实现 `submit_job`/`dispatch_job`/`handle_job_result`，按状态机落库，按会话认证 agent_id 做归属校验（设计「server.rs（作业调度）」，需求 R1/R2/R4）
  - [x] 5.3 `job_result` RPC handler 与连接级 agent_id 绑定；liveness 离线兜底与启动恢复将 in-flight 置 `lost`（设计「会话绑定」，需求 R4/R6）
  - [x]* 5.4 编写调度属性测试：终态不可变、结果幂等、归属不符丢弃、并发与离线归宿

- [x] 6. 检查点 - 后端作业链路单元测试通过
  - 确保所有测试通过,如有疑问请询问用户

- [x] 7. HTTP 作业接口
  - [x] 7.1 在 `/api/gse` 新增 `POST /jobs`、`GET /jobs`、`GET /jobs/{job_id}` 与处理函数、校验与状态码映射（设计「http.rs（作业接口）」，需求 R1/R5）
  - [x]* 7.2 编写 http 作业接口测试：提交校验、201、列表过滤、404、Agent 离线 409

- [x] 8. 端到端验证
  - [x]* 8.1 扩展 `tests/e2e.rs`：提交→`succeeded`/`failed`/`timeout`/`lost`/`rejected`，校验 stdout/exit_code，重启 Server 后在途作业置 `lost`

- [x] 9. 前端 GseJobAdapter
  - [x] 9.1 在 `@vectorman/adapters` 新增 `gse/jobs.ts`：类型与 `submitJob`/`listJobs`/`getJob`，并在 `index.ts` 导出（设计「适配器新增」，需求 R9）
  - [x]* 9.2 编写适配器单元测试：method/url/查询参数/body

- [x] 10. 前端 @vectorman/job 应用
  - [x] 10.1 装配入口 `main.tsx`、`App.tsx`（Layout/导航/路由/ToastHost）（设计「定位与分层」「路由」，需求 R9）
  - [x] 10.2 实现 `useJobs`（非终态轮询与清除）、`useJobDetail`（终态停止）、`useOnlineAgents`（设计「轮询策略」）
  - [x] 10.3 实现 `JobsPage` 列表、Agent/状态过滤与工具栏（设计「页面交互」，需求 R1/R8）
  - [x] 10.4 实现提交抽屉、详情抽屉、`JobStatusTag`、`JobOutput` 截断提示（设计「页面交互」「状态 Tag 映射」）
  - [x]* 10.5 编写前端测试：适配器、hooks 轮询启停、表单校验、状态 Tag、截断提示

- [x] 11. 检查点 - 前端测试与构建通过
  - 确保所有测试通过,如有疑问请询问用户
