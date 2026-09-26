# Requirements Document

> **已归档**（2026-09-26 文档整理）：本 feature 已实现并合入 main，本文件压缩为需求索引——完整 EARS 条款见 git 历史（本文件重写前的最后一个版本），design.md 全文保留为实施细节档案。

## Introduction

本 feature 交付 APM 链路：被观测应用按 OpenTelemetry 标准产生 trace，Agent 在本机接收 OTLP 并把 span 转成可上报记录，经既有采集上行通道直连 `dataserver`，`dataserver` 负责 span 明细、trace 摘要、RED 指标与服务拓扑的落库与查询，前端在 `@vectorman/dataplane` 内提供 trace 列表、trace 详情、服务拓扑、APM 指标与日志关联五个视图。

范围前提（本期只交付设计，不写代码）：

- 复用 `gse-dataplane-ingest` 的采集项下发、Agent 缓冲与重试、Agent 直连 `dataserver` 上报、保留期机制。
- 数据模型与信封扩展见 `observability-data-model`：新增 `data_type=traces`，字段对齐 OTel 语义约定；现有 `data_type=apm` 保留兼容但新链路不再使用。
- 服务拓扑由两路数据合成：本 feature 的 OTLP span 配对，以及 `ebpf-observability` 的内核连接边；两路通过指标 label `source` 区分，拓扑页默认合并展示。
- 明细走 `LogStore`（需索引版本 v2，属前置依赖），聚合走 `TimeSeriesStore`（1 分钟粒度，标量点）。
- 聚合指标的保留与删除依赖同期交付的 `dataplane-ts-retention`（`TimeSeriesStore` 新增 `delete_series` 与全局保留窗口），不再是外部依赖。

实施顺序（本期两份观测 spec 的约定）：`LogStore` 索引 v2 → `dataplane-ts-retention` → 本 feature → `ebpf-observability`。本 feature 优先于 eBPF。

不覆盖：应用侧 SDK 提供、字节码注入、日志推导 trace、trace 告警与通知、跨 dataserver 集群查询、指标与 trace 的 exemplar 关联。

## Glossary

- **OTLP**：OpenTelemetry Protocol，trace 数据的标准上报协议，gRPC 用 4317、HTTP/protobuf 用 4318。
- **TraceSpan**：本 feature 的 span 记录模型，字段对齐 OTel Trace 数据模型，见 `observability-data-model`。
- **trace 摘要**：一个 trace 的汇总行，含根服务、根操作、总耗时、span 数、错误数，存 sqlite `apm_trace_summary`。
- **span 明细**：单个 span 的完整字段，存 `LogStore`，按 `trace_id` 建索引。
- **RED 指标**：按服务与操作统计的请求数（Rate）、错误数（Errors）、耗时分布（Duration）。
- **拓扑边**：`源服务 → 目标服务` 的调用关系及其 QPS、错误率、P95 延迟。
- **source 维度**：拓扑边指标的来源标签，取值 `otlp`（span 配对）或 `ebpf`（内核连接）。
- **端点半表**：`apm_service_endpoint`，把 `(host_ip, listen_port)`、Pod 名映射到服务名，供两路数据对齐。
- **采集项（CollectItem）**：继承 `gse-dataplane-ingest` 的可下发采集配置单元；本 feature 新增类型 `apm_otlp`。
- **保留期（retention_days）**：明细数据的保存天数，本 feature 缺省 3 天。
- **聚合桶**：RED 与边指标的时间粒度，固定 1 分钟。
- **OTLP receiver**：Agent 内置的 OTLP 服务端，接收应用上报。
- **partial 响应**：trace 详情返回条数少于摘要 `span_count` 时的标记，表示索引重建窗口或明细已过保留期。

## Requirements

### Requirement 1: Agent 内置 OTLP receiver

- AS 运维人员, I want 应用只改上报地址就能接入, so that 现有 OpenTelemetry 插桩无需改造链路。
- 验收：THE `gse-agent` SHALL 提供内置 OTLP receiver，接收 OTLP/HTTP（protobuf）的 trace 导出请求；THE receiver SHALL 默认监听 `0.0.0.0:4318`，监听地址取自 Agent 本地 TOML 配置 `otlp_listen`，并允许 `GSE_OTLP_LISTEN` 环境变量覆盖。
### Requirement 2: Agent span 转封与上报

- AS 平台开发者, I want Agent 把 OTLP span 无损转成统一信封, so that 上行链路与存储不需要理解 OTLP 编码。
- 验收：WHEN Agent 收到 OTLP span，THE Agent SHALL 按 `observability-data-model` 的映射表转换为 `TraceSpan`；THE `TraceSpan.record_id` SHALL 为 `{trace_id}:{span_id}`，两者均为小写 hex。
### Requirement 3: APM 采集项与下发

- AS 运维人员, I want 用采集项控制哪些服务上报, so that 接入范围可收敛且可热更新。
- 验收：THE 采集项类型 SHALL 新增 `apm_otlp`，与既有 `metrics_host`、`log_file`、`log_k8s_stdout` 并列存在；THE `apm_otlp` 采集项的采集端配置 SHALL 包含：`service_allowlist`（字符串数组，空表示全收）、`service_denylist`、`attribute_allowlist`（span 属性白名单，空表示全量）、`batch_max_records`、`flush_interval_secs`。
### Requirement 4: dataserver 落库

- AS 查询调用方, I want 明细与摘要分别存储, so that trace 详情能取全、列表能排序分页。
- 验收：WHEN `dataserver` 收到 `data_type=traces` 且记录合法的批次，THE `dataserver` SHALL 把每条 span 作为明细写入 `LogStore`，并在索引字段 `trace_id`、`service`、`data_id` 上建立可检索值；THE `dataserver` SHALL 对同一 `record_id` 幂等：重放不新增明细、不增加摘要 `span_count`。
### Requirement 5: trace 列表查询

- AS 运维人员, I want 按服务、操作、耗时和状态筛 trace, so that 能快速定位慢或错的调用链。
- 验收：THE `dataserver` SHALL 提供 `POST /v1/traces/search`；THE 请求 SHALL 支持过滤字段：`from_ts`、`to_ts`、`service`、`operation`、`status`（`ok`/`error`）、`min_duration_micros`、`agent_id`、`host_id`、`data_id`。
### Requirement 6: trace 详情查询

- AS 运维人员, I want 打开单个 trace 看到完整 span 调用链, so that 能定位耗时与错误发生在哪一跳。
- 验收：THE `dataserver` SHALL 提供 `GET /v1/traces/{trace_id}`；THE 响应 SHALL 包含 `summary`（`apm_trace_summary` 该行）与 `spans` 数组。
### Requirement 7: RED 指标聚合

- AS 运维人员, I want 按服务与操作看 QPS、错误率与 P95, so that 有全局性能视图而不必逐条看 trace。
- 验收：THE `dataserver` SHALL 每 60 秒执行一次聚合任务，处理上一分钟的 `apm_trace_summary`；THE 聚合 SHALL 产出 `apm_service_requests_total`、`apm_service_errors_total`、`apm_service_duration_micros`，measurement、`field_name` 与维度 label 严格按 `observability-data-model` 的命名规范表。
### Requirement 8: 服务拓扑边

- AS 运维人员, I want 服务之间的调用关系与边指标, so that 能看出依赖方向与瓶颈服务。
- 验收：THE `dataserver` SHALL 从 `kind=client` 的 span 与其对端 `kind=server` span 推导拓扑边：`src_service` 为 client span 的 `service`，`dst_service` 为该 client span 触发的 server span 的 `service`；THE 配对规则 SHALL 为：同一 `trace_id` 且 `child.parent_span_id == client.span_id` 且 `child.kind == server`；一条 client span 匹配到多条 server span 时全部计为该边的一次调用。
### Requirement 9: 日志与 trace 关联

- AS 运维人员, I want 从日志跳到对应 trace, so that 报错日志能直接看到调用链。
- 验收：WHEN 日志记录的 labels 含 `trace_id`，THE 日志检索响应 SHALL 保留该 label 原样返回；THE 前端日志检索页 SHALL 在命中记录的 `trace_id` 上提供跳转到 trace 详情的入口。
### Requirement 10: 采样与写入保护

- AS 平台开发者, I want 采样策略由客户端决定且服务端有上限保护, so that 写入量可控而语义不歧义。
- 验收：THE 采样决策 SHALL 由应用侧 OTel SDK 完成；`dataserver` 与 Agent SHALL 不做二次采样，不改变 `trace_flags` 的 sampled 位；THE `dataserver` SHALL 在配置中提供 `apm_ingest_max_batches_per_sec`（默认 0 表示不限制），超过上限的批次 SHALL 返回 HTTP 429 与 `code=unavailable`。
### Requirement 11: 保留期与清理

- AS 运维人员, I want trace 明细按周期自动清理, so that 磁盘占用可预期。
- 验收：THE trace 明细保留期 SHALL 取自采集项 `storage_json.retention_days`，缺省 3 天；THE `dataserver` SHALL 每小时执行清理任务，删除 `timestamp` 早于 `now - retention_days` 的 `data_type=traces` 明细。
### Requirement 11b: 全局容量上限与最久远优先淘汰

- AS 运维人员, I want 给数据目录设一个容量上限并在超限时淘汰最久远的数据, so that 磁盘不会被观测数据写满。
- 验收：THE `dataserver` SHALL 提供配置项 `apm_max_bytes`（字节，缺省 0 表示不限）；非 0 时对所有观测数据生效；THE 计量范围 SHALL 为 `data_path` 下的全部数据（本进程唯一的落盘目录，磁盘告急时告急的是整个目录），由递归目录大小得出。
### Requirement 12: 查询 API 与错误码

- AS 查询调用方, I want 稳定的接口与错误码, so that 前端与 CLI 可以复用同一套约定。
- 验收：THE 新增接口 SHALL 在 `dataserver` 的 SQL HTTP 端口（默认 8081）同源提供：`POST /v1/traces/search`、`GET /v1/traces/{trace_id}`、`POST /v1/edges/search`、`GET /v1/apm/services`；`POST /v1/edges/search` SHALL 支持过滤 `from_ts`、`to_ts`、`src_service`、`dst_service`、`source`、`min_requests`、`limit`、`offset`，返回 `edges` 数组与 `total`。
### Requirement 13: 前端视图

- AS 运维人员, I want 在数据面自带页面完成 APM 查看, so that 查询与接入上下文在同一进程内闭环。
- 验收：THE 前端 SHALL 扩展 `@vectorman/dataplane`，不新增独立应用；页面与 API 同源，沿用既有 HttpClient；THE 路由 SHALL 新增 `/traces`、`/traces/:trace_id`、`/topology`、`/apm`、`/settings/service-aliases`，并保留既有 `/`、`/metrics`、`/logs`。
### Requirement 14: 服务端配置与开关

- AS 运维人员, I want APM 相关能力可独立开关, so that 上线可以与既有链路解耦。
- 验收：THE `dataserver` 配置 SHALL 新增 `apm_enabled`（默认 true）、`apm_agg_interval_secs`（默认 60）、`apm_retention_days_default`（默认 3）、`apm_endpoint_retention_days`（默认 30）、`apm_ingest_max_batches_per_sec`（默认 0）、`apm_min_duration_micros_for_detail`（默认 0）；THE 配置项 SHALL 支持 `DATASERVER_` 前缀环境变量覆盖，沿用既有配置加载方式。
### Requirement 15: 前置依赖与范围边界

- AS 开发者, I want 明确本期交付边界与外部依赖, so that 实现范围与存储层能力对齐。
- 验收：THE 本 feature SHALL 依赖 `LogStore` 索引版本 v2（新增索引字段 `trace_id`、`service`、`data_id` 与 `search_indexed` 方法）；该变更 SHALL 先于 trace 查询实现落地；THE 本 feature SHALL 依赖 sqlite 观测表初始化（`obs_schema_meta`、`apm_trace_summary`、`apm_service_endpoint`）。
### Requirement 16: 可观测性与错误处理

- AS 运维人员, I want APM 自身链路可观测, so that 能区分应用不报、Agent 丢批与存储失败。
- 验收：THE Agent SHALL 统计并输出：OTLP 接收请求数、解析失败数、被 `service_allowlist` 丢弃的 span 数、入缓冲批次数、上报成功与失败次数；THE `dataserver` SHALL 统计并输出：trace 批次数、span 接收条数、非法记录数、限流丢弃数、聚合任务成功与失败次数、清理任务删除条数。
### Requirement 17: 服务名映射配置

- AS 运维人员, I want 给无插桩应用与未识别 IP 配服务名, so that eBPF 兜底数据能在拓扑图里给出可读的服务名。
- 验收：THE `dataserver` SHALL 持久化静态服务名映射表 `apm_service_alias`（定义见 `observability-data-model`），字段含 `alias_id`、`match_kind`、`match_value`、`service`、`enabled`、`note`、`updated_ts`；THE `match_kind` SHALL 限于 `process_name`、`process_prefix`、`pod_prefix`、`cidr` 四种。
