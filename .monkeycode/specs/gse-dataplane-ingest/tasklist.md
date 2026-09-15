# 需求实施计划

 - [x] 1. 重命名数据面二进制为 dataserver
   - [x] 1.1 将 `bins/apiserver` 重命名为 `bins/dataserver`（Cargo package `dataserver`），更新 workspace members、打包脚本与示例配置中的名称
     - 对应 Requirement 1
   - [x]* 1.2 确认 `dataserver --help` / 现有 SQL 与 Prom 测试在新包名下通过

- [x] 2. dataplane-ingest：信封、接入、检索、流索引
  - [x] 2.1 新建 `crates/dataplane-ingest`：`DataEnvelope`、四类记录 DTO、`IngestReply`、`apply`（校验、partial、KvStore `ingest/{record_id}` 去重、metrics→TimeSeriesStore、logs/apm/ebpf→LogStore）
    - 对应 Requirement 2、3、10
  - [x] 2.2 扩展 `LogFilter.limit`（默认 100，上限 1000）与 `LogStore::delete_matching`
    - 对应 Requirement 12、13.15
  - [x] 2.3 实现 `search` 映射与流索引 upsert `stream/{agent_id}/{data_type}/{data_id}`
    - 对应 Requirement 12、16
  - [x]* 2.4 四类信封往返、缺字段 partial、重复 record_id、limit、流索引单元测试（tempdir 真引擎）

- [x] 3. 检查点 - ingest crate 测试通过
  - 确保所有测试通过,如有疑问请询问用户

 - [x] 4. dataserver HTTP：接入、查询、流、静态页、采集项反代、清理
   - [x] 4.1 SQL 口增加 `POST /v1/ingest`、`POST /v1/logs/search`、`GET /v1/streams`、`GET /health`；同一口挂 `GET /api/v1/query` 与 `query_range`
    - 对应 Requirement 1、10、11、12、14
   - [x] 4.2 配置 `http_web_dir`、`gse_admin_url`；SPA 回退；采集项与 agents 反代；`DELETE` 写 `retain/{item_id}`
    - 对应 Requirement 13、16
   - [x] 4.3 每小时按 live 采集项与 `retain/` 前缀执行保存周期删除
    - 对应 Requirement 13.15、13.16
   - [x]* 4.4 httptest：ingest 后 Prom 命中、logs/search 过滤、streams、反代、retain 清理

- [x] 5. GSE 数据面登记、探活、选路
  - [x] 5.1 ledger 表 `dataplane_services` 与 upsert/list/get/delete/set_status/`pick_ingest_url(agent_id)`（online 集合按 service_id 排序后 `hash(agent_id) % len`）
    - 对应 Requirement 8、15.6
  - [x] 5.2 HTTP `/api/gse/dataplanes`；探活任务每 30s `GET {ingest_url}/health`，一次失败立刻 offline
    - 对应 Requirement 8
  - [x] 5.3 proto + RPC `dataplane_addr`（已认证才返回 ingest_url 与 host_id）
    - 对应 Requirement 8
  - [x]* 5.4 ledger/探活/选路/RPC 单元测试

- [x] 6. GSE 采集项持久化与下发
  - [x] 6.1 表 `collect_items`（`agent_ids` JSON、`collector_json`、`storage_json`），CRUD HTTP `/api/gse/collect-items`
    - 对应 Requirement 13
  - [x] 6.2 proto `CollectItem` / `CollectItemsReply`；RPC `collect_items` 拉取与向目标列表在线 Agent 推送
    - 对应 Requirement 13
  - [x]* 6.3 多 Agent 绑定、enabled=false、删除后推送剩余列表的测试

- [x] 7. 检查点 - 控制面与 dataserver HTTP 测试通过
  - 确保所有测试通过,如有疑问请询问用户

- [x] 8. Agent 采集、缓冲、上报
  - [x] 8.1 认证后 `dataplane_addr` + `collect_items`；无采集项不采集；热更新按 item_id 对齐采集器
    - 对应 Requirement 8、13
  - [x] 8.2 `metrics_host`：cpu_usage / mem_usage，第一轮 cpu 只打快照；`data_id=item_id`
    - 对应 Requirement 4
  - [x] 8.3 `log_file`：单层 glob、head/tail 开始标记、缺文件重试、清洗（include/exclude + regex/json 提取）、攒批
    - 对应 Requirement 5
  - [x] 8.4 `log_k8s_stdout`：kubectl logs 同款 API（in-cluster 或 kubeconfig）、30s 重新 list、follow、tailLines
    - 对应 Requirement 17
  - [x] 8.5 内存批次缓冲默认 1000：确认出队、5xx 重试、满则丢最旧
    - 对应 Requirement 9、14
  - [x]* 8.6 缓冲、glob、日志起点、清洗提取、k8s mock apiserver 单元测试

- [x] 9. 检查点 - Agent 采集测试通过
  - 确保所有测试通过,如有疑问请询问用户

- [x] 10. dataserver 前端 `@vectorman/dataplane`
  - [x] 10.1 新建 Vite 应用，proxy `/v1` `/api` `/health` → 8081，`allowedHosts` 含 `.monkeycode-ai.online`；`npm run build:dataplane`
    - 对应 Requirement 16
  - [x] 10.2 采集链路页：列表、新建、行内开关、编辑、详情、数据检索、删除确认、手动刷新
    - 对应 Requirement 16
  - [x] 10.3 指标页：默认 cpu/mem 两图 + 可展开 PromQL；日志页 data_type 下拉 logs/apm/ebpf
    - 对应 Requirement 11、12、16
  - [x]* 10.4 前端测试：三路由、开关/删除、检索跳转带 item_id、表单多选 Agent

- [x] 11. 检查点 - 前端测试与构建通过
  - 确保所有测试通过,如有疑问请询问用户

- [x] 12. 集成与 dpc
  - [x] 12.1 `dpc logs` 子命令打 `POST /v1/logs/search`
    - 对应 Requirement 12
  - [x]* 12.2 e2e：登记 dataserver → 探活 online → 新建 metrics 采集项 → Agent 上报 → Prom 查到 `cpu_usage{agent_id,item_id}`

- [x] 13. 检查点 - 全链路通过
  - 确保所有测试通过,如有疑问请询问用户
