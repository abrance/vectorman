# Requirements Document

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

**User Story:** AS 运维人员, I want 应用只改上报地址就能接入, so that 现有 OpenTelemetry 插桩无需改造链路。

#### Acceptance Criteria

1. THE `gse-agent` SHALL 提供内置 OTLP receiver，接收 OTLP/HTTP（protobuf）的 trace 导出请求。
2. THE receiver SHALL 默认监听 `0.0.0.0:4318`，监听地址取自 Agent 本地 TOML 配置 `otlp_listen`，并允许 `GSE_OTLP_LISTEN` 环境变量覆盖。

   （默认监听全网卡的原因是应用与 Agent 不同容器网络命名空间：同主机的 docker compose 容器、同节点的 k8s Pod 都无法访问 Agent 的 `127.0.0.1`。若部署上确认应用与 Agent 同 netns，运维可改回 `127.0.0.1:4318`。）
3. THE Agent SHALL 支持两种应用部署形态的接入：同主机 docker compose 容器（经宿主机 IP 或 `host.docker.internal`）与同节点 Kubernetes Pod（经节点 IP；Pod 侧不假设 hostNetwork）。设计文档 SHALL 给出两种形态的 CMD 配置示例。
4. THE Agent SHALL 支持可选静态凭据：配置 `otlp_token` 非空时，receiver SHALL 校验请求头 `Authorization: Bearer <token>`，不匹配时返回 HTTP 401；`otlp_token` 为空时不校验（与既有接入接口无鉴权的现状一致）。
5. THE Agent SHALL 支持 `otlp_allowed_cidrs`（字符串数组，空表示不限）限制来源地址，超出来源的请求返回 HTTP 403 并计数。
3. WHEN Agent 配置项 `otlp_enabled` 为 false 或该采集项未启用，THE receiver SHALL 不监听端口。
4. THE receiver SHALL 实现 OTLP `ExportTraceServiceRequest` 的解析，接受 protobuf 与 JSON 两种编码。
5. THE receiver SHALL 支持 gzip 压缩请求体。
6. WHEN 请求解析成功，THE receiver SHALL 返回 OTLP 标准响应 `ExportTraceServiceResponse`，且 `partial_success` 为空。
7. IF 请求非 OTLP trace 导出或无法解析，THE receiver SHALL 返回 HTTP 400 并带 OTLP 错误消息。
8. THE receiver SHALL 不依赖控制面的采集项下发即可接收数据；采集项只控制是否启用与过滤规则。
9. THE v1 OTLP receiver SHALL 只支持 trace 信号；metrics 与 logs 信号的 OTLP 接收列入后续范围。

### Requirement 2: Agent span 转封与上报

**User Story:** AS 平台开发者, I want Agent 把 OTLP span 无损转成统一信封, so that 上行链路与存储不需要理解 OTLP 编码。

#### Acceptance Criteria

1. WHEN Agent 收到 OTLP span，THE Agent SHALL 按 `observability-data-model` 的映射表转换为 `TraceSpan`。
2. THE `TraceSpan.record_id` SHALL 为 `{trace_id}:{span_id}`，两者均为小写 hex。
3. THE `TraceSpan.timestamp` SHALL 为 `start_unix_nano / 1000` 的整除结果（Unix 微秒）。
4. THE `TraceSpan.collector` SHALL 固定为 `otlp`。
5. THE Agent SHALL 把转换结果放入 `data_type=traces` 的数据批次，`data_id` 为该采集项的 `item_id`。
6. THE Agent SHALL 复用既有批次缓冲、退避重试与直连 dataserver 上报逻辑，不新增第二条上行通路。
7. WHEN OTLP 请求中的 resource 缺失 `service.name`，THE Agent SHALL 写 `unknown_service`。
8. THE Agent SHALL 保留 span 的 `events`、`links`、`dropped_*_count` 与全量 `attributes`，不做语义裁剪。
9. WHEN 单条 span 序列化后超过 256 KiB，THE Agent SHALL 不截断，由 dataserver 按非法记录计入 `failures`。
10. THE Agent SHALL 按采集项 `batch_max_records`（缺省 100）与 `flush_interval_secs`（缺省 5 秒）攒批上报。
11. IF Agent 缓冲达到上限，THE Agent SHALL 按既有约定丢弃最旧批次并向标准错误输出一行，含 `data_type=traces` 与丢弃条数。

### Requirement 3: APM 采集项与下发

**User Story:** AS 运维人员, I want 用采集项控制哪些服务上报, so that 接入范围可收敛且可热更新。

#### Acceptance Criteria

1. THE 采集项类型 SHALL 新增 `apm_otlp`，与既有 `metrics_host`、`log_file`、`log_k8s_stdout` 并列存在。
2. THE `apm_otlp` 采集项的采集端配置 SHALL 包含：`service_allowlist`（字符串数组，空表示全收）、`service_denylist`、`attribute_allowlist`（span 属性白名单，空表示全量）、`batch_max_records`、`flush_interval_secs`。
3. THE `apm_otlp` 采集项的入库配置 SHALL 包含 `retention_days`，缺省 3 天。
4. THE 采集项 SHALL 复用既有 `collect_items` 表的 `collector_json` 与 `storage_json` 列，不新增数据库列。
5. WHEN 采集项 `enabled` 为 false，THE Agent SHALL 停止该采集项的上报并释放 OTLP 接收缓冲；已入库数据仍可查询，保存周期继续生效。
6. WHEN Agent 收到新的采集项列表，THE Agent SHALL 在不重启进程的情况下应用；被移除的 `apm_otlp` 采集项对应的 OTLP receiver 按 Requirement 1 第 3 条停止监听。
7. WHEN 采集项 `service_allowlist` 非空且 span 的 `service` 不在名单内，THE Agent SHALL 丢弃该 span 并在本地计数，不上报。
8. THE 采集项的有效性校验 SHALL 在 GSE Server 写入时完成：`batch_max_records` 上限 5000、`flush_interval_secs` 上限 60、`retention_days` 上限 30；越界返回 `invalid_argument`。

### Requirement 4: dataserver 落库

**User Story:** AS 查询调用方, I want 明细与摘要分别存储, so that trace 详情能取全、列表能排序分页。

#### Acceptance Criteria

1. WHEN `dataserver` 收到 `data_type=traces` 且记录合法的批次，THE `dataserver` SHALL 把每条 span 作为明细写入 `LogStore`，并在索引字段 `trace_id`、`service`、`data_id` 上建立可检索值。
2. THE `dataserver` SHALL 对同一 `record_id` 幂等：重放不新增明细、不增加摘要 `span_count`。
3. WHEN 一条 span 写入成功，THE `dataserver` SHALL upsert `apm_trace_summary`：`span_count` 累加 1（仅首次见到该 `record_id` 时）、`duration_micros` 取该 trace 的 `max(end) - min(start)`、`status` 在该 trace 任一 span 为 `error` 时为 `error`、`error_count` 为该 trace `status_code=error` 的 span 数。
4. THE 根 span SHALL 为 `parent_span_id` 为空的 span；存在多个时取 `start_ts` 最小者，其 `service` 与 `name` 写入 `root_service` 与 `root_operation`。
5. THE `apm_trace_summary.services_json` SHALL 为该 trace 出现过的 `service` 去重后按字典序排序的 JSON 数组。
6. WHEN 一条 span 的 `collector` 为 `otlp`，THE `dataserver` SHALL 用其 resource 中 `service.name`、`service.instance.id`、`k8s.pod.name`、`k8s.node.name`、`host.ip` upsert 一行 `apm_service_endpoint`。
7. THE `dataserver` SHALL 用 span 的 `service` 与 `resource["host.ip"]` 填充摘要的 `agent_id`、`host_id` 之外的服务信息，`agent_id` 与 `host_id` 取自信封。
8. IF 整批 `data_type=traces` 的信封无法解析或 `agent_id` 为空，THE `dataserver` SHALL 拒绝该批并返回 `invalid_argument`。
9. WHEN 批次内部分 span 非法（缺 `trace_id`/`span_id`、长度不符、超 256 KiB），THE `dataserver` SHALL 写入合法记录并返回 `status=partial` 与失败明细。

### Requirement 5: trace 列表查询

**User Story:** AS 运维人员, I want 按服务、操作、耗时和状态筛 trace, so that 能快速定位慢或错的调用链。

#### Acceptance Criteria

1. THE `dataserver` SHALL 提供 `POST /v1/traces/search`。
2. THE 请求 SHALL 支持过滤字段：`from_ts`、`to_ts`、`service`、`operation`、`status`（`ok`/`error`）、`min_duration_micros`、`agent_id`、`host_id`、`data_id`。
3. THE 请求 SHALL 支持 `sort` 取 `start_ts` 或 `duration_micros`，以及 `order` 取 `asc` 或 `desc`；缺省 `sort=start_ts`、`order=desc`。
4. THE 请求 SHALL 支持 `limit`（默认 50、上限 500）与 `offset`。
5. WHEN 查询命中，THE `dataserver` SHALL 返回 JSON，字段包含 `traces` 数组与 `total`；每条含 `trace_id`、`start_ts`、`duration_micros`、`root_service`、`root_operation`、`span_count`、`error_count`、`status`、`services`。
6. IF `from_ts` 晚于 `to_ts`，THE `dataserver` SHALL 返回 HTTP 400 与 `code=invalid_argument`。
7. WHEN 查询未指定时间范围，THE `dataserver` SHALL 默认最近 1 小时。
8. THE 查询 SHALL 由 sqlite `apm_trace_summary` 的索引满足，不依赖 `LogStore` 扫描。

### Requirement 6: trace 详情查询

**User Story:** AS 运维人员, I want 打开单个 trace 看到完整 span 调用链, so that 能定位耗时与错误发生在哪一跳。

#### Acceptance Criteria

1. THE `dataserver` SHALL 提供 `GET /v1/traces/{trace_id}`。
2. THE 响应 SHALL 包含 `summary`（`apm_trace_summary` 该行）与 `spans` 数组。
3. THE `spans` SHALL 按 `start_unix_nano` 升序返回，覆盖全部字段（含 `attributes`、`events`、`links`）。
4. THE `dataserver` SHALL 通过 `LogStore` 的索引检索（`trace_id` 精确匹配、`order=asc`、`limit=10000`）取该 trace 的全部 span，不使用 post-filter 路径。
5. WHEN 返回条数少于 `summary.span_count`，THE 响应 SHALL 带 `partial=true` 与 `expected_span_count`。
6. IF `trace_id` 长度不为 32 或非 hex，THE `dataserver` SHALL 返回 HTTP 400 与 `code=invalid_argument`。
7. IF 该 `trace_id` 无摘要，THE `dataserver` SHALL 返回 HTTP 404 与 `code=not_found`。
8. THE `spans` 中每条 SHALL 带 `parent_span_id`，使前端能在不额外查询的情况下构建树。

### Requirement 7: RED 指标聚合

**User Story:** AS 运维人员, I want 按服务与操作看 QPS、错误率与 P95, so that 有全局性能视图而不必逐条看 trace。

#### Acceptance Criteria

1. THE `dataserver` SHALL 每 60 秒执行一次聚合任务，处理上一分钟的 `apm_trace_summary`。
2. THE 聚合 SHALL 产出 `apm_service_requests_total`、`apm_service_errors_total`、`apm_service_duration_micros`，measurement、`field_name` 与维度 label 严格按 `observability-data-model` 的命名规范表。
3. THE 聚合维度 SHALL 为 `service`、`operation`（用 span 的 `name`）、`span_kind`、`status`，并带 `source=otlp`。
4. THE 每个聚合点的 `timestamp` SHALL 为该分钟桶起点（Unix 微秒，整分钟对齐）。
5. `apm_service_duration_micros` SHALL 用 span 的 `(end_unix_nano - start_unix_nano) / 1000` 作为样本，产出 `avg`、`p50`、`p95`、`p99`、`max` 五个 `field_name` 点。
6. THE 聚合 SHALL 只用 `collector=otlp` 的 span 计算 `apm_service_*` 指标，不使用 eBPF 派生的伪 span。
7. WHEN 某个分钟桶没有数据，THE `dataserver` SHALL 不写入该桶的零值点。
8. WHEN 聚合任务某轮失败，THE `dataserver` SHALL 记录错误并向标准错误输出一行，下一轮按 60 秒周期继续，不回填历史桶。
9. THE 聚合 SHALL 只读 sqlite 与内存，不重新扫描 `LogStore` 明细。

### Requirement 8: 服务拓扑边

**User Story:** AS 运维人员, I want 服务之间的调用关系与边指标, so that 能看出依赖方向与瓶颈服务。

#### Acceptance Criteria

1. THE `dataserver` SHALL 从 `kind=client` 的 span 与其对端 `kind=server` span 推导拓扑边：`src_service` 为 client span 的 `service`，`dst_service` 为该 client span 触发的 server span 的 `service`。
2. THE 配对规则 SHALL 为：同一 `trace_id` 且 `child.parent_span_id == client.span_id` 且 `child.kind == server`；一条 client span 匹配到多条 server span 时全部计为该边的一次调用。
3. THE 聚合 SHALL 产出 `apm_edge_requests_total`、`apm_edge_errors_total`、`apm_edge_duration_micros`，维度为 `src_service`、`dst_service`、`span_kind`、`status`，并带 `source=otlp`。
4. THE 边延迟样本 SHALL 用 client span 的耗时（含网络与对端处理），`field_name` 为 `avg` 与 `p95`。
5. WHEN 一条 client span 找不到配对的 server span，THE `dataserver` SHALL 用其 `attributes` 中的 `server.address`（或 `net.peer.name` / `net.peer.ip`）作为 `dst_service` 兜底，值前缀 `unknown:`；都缺失时用 `unknown`。
6. THE `apm_service_endpoint` 表 SHALL 参与 `dst_service` 归一：若 `dst_service` 是 `unknown:*` 且能按 `host_ip` 或 Pod 名反查到服务，THE `dataserver` SHALL 用反查结果替换。
7. THE 拓扑页合并查询 SHALL 通过 `sum by (src_service, dst_service)` 同时统计 `source=otlp` 与 `source=ebpf` 的边，两路互不覆盖。
8. WHEN `ebpf-observability` 未部署或未启用，THE 拓扑数据 SHALL 仍由 `source=otlp` 单独构成，不报错。

### Requirement 9: 日志与 trace 关联

**User Story:** AS 运维人员, I want 从日志跳到对应 trace, so that 报错日志能直接看到调用链。

#### Acceptance Criteria

1. WHEN 日志记录的 labels 含 `trace_id`，THE 日志检索响应 SHALL 保留该 label 原样返回。
2. THE 前端日志检索页 SHALL 在命中记录的 `trace_id` 上提供跳转到 trace 详情的入口。
3. WHEN 运维点击跳转且该 `trace_id` 存在摘要，THE 前端 SHALL 打开 Requirement 6 的详情视图。
4. IF 该 `trace_id` 无摘要，THE 前端 SHALL 提示「该 trace 明细不在保留期内或未被采样」，不展示空白页。
5. THE trace 详情页 SHALL 反向提供「查看该服务的日志」入口，跳转日志页并带上 `service` 与 trace 的时间范围。
6. THE 关联 SHALL 只依赖 `trace_id` 精确匹配，不引入新的存储结构。

### Requirement 10: 采样与写入保护

**User Story:** AS 平台开发者, I want 采样策略由客户端决定且服务端有上限保护, so that 写入量可控而语义不歧义。

#### Acceptance Criteria

1. THE 采样决策 SHALL 由应用侧 OTel SDK 完成；`dataserver` 与 Agent SHALL 不做二次采样，不改变 `trace_flags` 的 sampled 位。
2. THE `dataserver` SHALL 在配置中提供 `apm_ingest_max_batches_per_sec`（默认 0 表示不限制），超过上限的批次 SHALL 返回 HTTP 429 与 `code=unavailable`。
3. THE `dataserver` SHALL 统计并暴露被限流的批次数与记录数，供自监控页读取。
4. WHEN 限流发生，THE Agent SHALL 按既有退避重试约定处理该批，不丢弃。
5. THE `dataserver` SHALL 提供 `apm_min_duration_micros_for_detail`（默认 0，表示全存）：低于阈值的 trace 只写摘要与聚合指标，不写 span 明细。
6. WHEN Requirement 10 第 5 条生效导致明细缺失，THE trace 详情响应 SHALL 带 `partial=true` 且 `reason=detail_filtered`。
7. THE `dataserver` SHALL 不因采样或过滤而破坏幂等：同一 `record_id` 的处理结果在配置不变时一致。

### Requirement 11: 保留期与清理

**User Story:** AS 运维人员, I want trace 明细按周期自动清理, so that 磁盘占用可预期。

#### Acceptance Criteria

1. THE trace 明细保留期 SHALL 取自采集项 `storage_json.retention_days`，缺省 3 天。
2. THE `dataserver` SHALL 每小时执行清理任务，删除 `timestamp` 早于 `now - retention_days` 的 `data_type=traces` 明细。
3. THE 清理 SHALL 通过 `LogStore` 按索引字段 `data_id` 与时间上界删除，并重复调用直到单次返回 0，以突破单次扫描上限。
4. THE 清理 SHALL 同时删除 `apm_trace_summary` 中 `start_ts` 早于保留期的行。
5. THE `apm_service_endpoint` 的保留期 SHALL 独立配置为 30 天，不随 trace 明细清理。
6. WHEN 采集项被删除，THE `dataserver` SHALL 复用既有 `retain/{item_id}` 机制，把该条目的明细按原保留期到期后清理，摘要与明细同时删除。
7. THE 聚合指标（`apm_service_*`、`apm_edge_*`）的清理 SHALL 依赖同期交付的 `/.monkeycode/specs/dataplane-ts-retention/`：THE 全局保留期 SHALL 取 `ts_retention_days`（默认 30 天）；聚合指标不随 trace 明细的 3 天保留期删除。
7a. THE APM 明细与摘要的默认保留期 SHALL 为 `apm_retention_days_default`（缺省 3 天），端点表为 `apm_endpoint_retention_days`（缺省 30 天）。
8. WHEN 某采集项配置了短于全局窗口的保留期，THE `dataserver` SHALL 由 `dataplane-ts-retention` 的时序清理任务按 `item_id` matcher 删除该采集项的超期指标点。
9. THE 保留执行（`ts_retention_enforced`）SHALL 缺省开启；写入超窗聚合点时 THE `dataserver` SHALL 记入接入应答 `failures` 并向自监控口暴露计数。
8. THE 清理任务 SHALL 向标准输出记录一行，含删除的明细条数、摘要行数与耗时。

### Requirement 11b: 全局容量上限与最久远优先淘汰

**User Story:** AS 运维人员, I want 给数据目录设一个容量上限并在超限时淘汰最久远的数据, so that 磁盘不会被观测数据写满。

#### Acceptance Criteria

1. THE `dataserver` SHALL 提供配置项 `apm_max_bytes`（字节，缺省 0 表示不限）；非 0 时对所有观测数据生效。
2. THE 计量范围 SHALL 为 `data_path` 下的全部数据（本进程唯一的落盘目录，磁盘告急时告急的是整个目录），由递归目录大小得出。
3. WHEN 计量值超过 `apm_max_bytes`，THE 保留策略 SHALL 按「最久远优先」淘汰 **APM 数据**：抬高删除时间界并逐轮删除 span 明细、trace 摘要、边摘要、聚合点（`apm_*` measurement 墓碑）。
4. THE 淘汰步长 SHALL 至少为「保存窗口 / 最大轮数」与 `evict_step_secs` 的较大值，保证在 `max_rounds` 轮内能覆盖整个窗口（否则近期数据永远不会落在删除界之内）。
5. THE 淘汰 SHALL 不涉及日志、eBPF 与自监控指标；若 APM 数据已无可删而仍超限，THE `dataserver` SHALL 以 `stopped_reason=no_apm_data_left` 记录并输出标准错误提示，SHALL NOT 越权删除其它类型的数据。
6. THE 每轮淘汰 SHALL 调用 `LogStore::reclaim_space`（合并/清理不再被引用的段文件）；时序库的墓碑空间由底层 compaction 回收，SHALL NOT 承诺立即释放。
7. THE `dataserver` SHALL 输出一行标准输出，含各类删除条数、`bytes_before`/`bytes_after`、淘汰轮数与停止原因。
8. THE 保留策略 SHALL 按 `apm_clean_interval_secs`（缺省 3600）周期执行，且任一步失败时记录错误、下一轮继续。

### Requirement 12: 查询 API 与错误码

**User Story:** AS 查询调用方, I want 稳定的接口与错误码, so that 前端与 CLI 可以复用同一套约定。

#### Acceptance Criteria

1. THE 新增接口 SHALL 在 `dataserver` 的 SQL HTTP 端口（默认 8081）同源提供：`POST /v1/traces/search`、`GET /v1/traces/{trace_id}`、`POST /v1/edges/search`、`GET /v1/apm/services`。
2. `POST /v1/edges/search` SHALL 支持过滤 `from_ts`、`to_ts`、`src_service`、`dst_service`、`source`、`min_requests`、`limit`、`offset`，返回 `edges` 数组与 `total`。
3. `GET /v1/apm/services` SHALL 返回 `apm_service_endpoint` 与服务维度汇总（该时间范围内最近一次出现的服务），用于前端下拉与拓扑图例。
4. THE 所有错误响应 SHALL 为 JSON 且含机器可读 `code`，取值限于既有稳定值：`invalid_argument`、`unavailable`、`query_failed`、`not_found`。
5. THE 接入错误响应 SHALL 沿用既有形状；限流为 `unavailable` 且 HTTP 429。
6. WHEN 请求字段未知，THE `dataserver` SHALL 忽略未知字段而不报错，保持向前兼容。
7. THE `dpc` 命令行 SHALL 增加只读子命令 `traces`（`--service`、`--operation`、`--min-duration-ms`、`--status`、`--limit`）与 `trace <trace_id>`，通过 HTTP 调用上述接口并打印 JSON。

### Requirement 13: 前端视图

**User Story:** AS 运维人员, I want 在数据面自带页面完成 APM 查看, so that 查询与接入上下文在同一进程内闭环。

#### Acceptance Criteria

1. THE 前端 SHALL 扩展 `@vectorman/dataplane`，不新增独立应用；页面与 API 同源，沿用既有 HttpClient。
2. THE 路由 SHALL 新增 `/traces`、`/traces/:trace_id`、`/topology`、`/apm`、`/settings/service-aliases`，并保留既有 `/`、`/metrics`、`/logs`。
3. THE `/traces` 页 SHALL 提供时间范围、服务、操作、状态、最小耗时过滤与排序切换，调用 `POST /v1/traces/search`，列表展示 `start_ts`、`root_service`、`root_operation`、`duration_micros`、`span_count`、`error_count`、`status`。
4. THE `/traces/:trace_id` 页 SHALL 调用 `GET /v1/traces/{trace_id}`，按 `parent_span_id` 渲染 span 瀑布图，时间轴按 `start_unix_nano` 与 `end_unix_nano` 对齐，错误 span 高亮。
5. THE span 详情 SHALL 展示 `attributes`、`resource`、`events`、`links`、`status_message` 与 `dropped_*` 计数。
6. THE `/topology` 页 SHALL 用 `sum by (src_service, dst_service)` 查询边指标渲染服务拓扑图，边宽度按 QPS、颜色按错误率，点击边跳转 `/traces` 并带上该边的服务与时间范围。
7. THE topology 页 SHALL 提供 `source` 切换：`全部`、`otlp`、`ebpf`，对应加或不加 `source` matcher。
8. THE `/apm` 页 SHALL 按 `service` 与 `operation` 展示 QPS、错误率、P50/P95/P99 曲线，数据来自 `apm_service_*` 指标。
9. THE `/apm` 页 SHALL 提供「查看 trace」入口，跳转 `/traces` 并带上服务、操作与时间范围。
10. THE 日志页 SHALL 按 Requirement 9 提供 trace 跳转入口。
11. WHEN 详情响应 `partial=true`，THE 详情页 SHALL 顶部提示原因（索引重建窗口 / 明细已过保留期 / 明细被过滤）。
12. THE 页面数据刷新 SHALL 由运维手动触发，不引入定时器。
13. WHEN 查询结果为空，THE 页面 SHALL 给出与上下文相关的空态提示（例如采集项未启用、时间范围无数据）。
14. THE 图表渲染 SHALL 复用 `@vectorman/dataplane` 既有的手绘 SVG 组件（`ui/line-chart.tsx` 用于时序曲线、`ui/waterfall.tsx` 用于 span 瀑布图、拓扑同样手绘 SVG），SHALL NOT 引入图表库依赖；THE 后端 SHALL 只返回数据点（Prom 形状的 `query_range` 结果与 span 原文），不承担任何图表逻辑。拓扑布局 SHALL 由前端纯函数确定性分层（`features/apm/layout.ts`），SHALL NOT 使用力导向布局。

### Requirement 14: 服务端配置与开关

**User Story:** AS 运维人员, I want APM 相关能力可独立开关, so that 上线可以与既有链路解耦。

#### Acceptance Criteria

1. THE `dataserver` 配置 SHALL 新增 `apm_enabled`（默认 true）、`apm_agg_interval_secs`（默认 60）、`apm_retention_days_default`（默认 3）、`apm_endpoint_retention_days`（默认 30）、`apm_ingest_max_batches_per_sec`（默认 0）、`apm_min_duration_micros_for_detail`（默认 0）。
2. THE 配置项 SHALL 支持 `DATASERVER_` 前缀环境变量覆盖，沿用既有配置加载方式。
3. WHEN `apm_enabled` 为 false，THE `dataserver` SHALL 关闭 trace 相关路由的写入与聚合任务，查询路由返回 `unavailable`，既有 SQL、Prom、日志检索不受影响。
4. THE Agent 配置 SHALL 新增 `otlp_enabled`（默认 false）、`otlp_listen`（默认 `0.0.0.0:4318`）、`otlp_max_body_bytes`（默认 8 MiB）、`otlp_token`（默认空）、`otlp_allowed_cidrs`（默认空数组）。
5. WHEN `otlp_enabled` 为 false，THE Agent SHALL 不监听 OTLP 端口且不注册 `apm_otlp` 采集器。

### Requirement 15: 前置依赖与范围边界

**User Story:** AS 开发者, I want 明确本期交付边界与外部依赖, so that 实现范围与存储层能力对齐。

#### Acceptance Criteria

1. THE 本 feature SHALL 依赖 `LogStore` 索引版本 v2（新增索引字段 `trace_id`、`service`、`data_id` 与 `search_indexed` 方法）；该变更 SHALL 先于 trace 查询实现落地。
2. THE 本 feature SHALL 依赖 sqlite 观测表初始化（`obs_schema_meta`、`apm_trace_summary`、`apm_service_endpoint`）。
3. THE 聚合指标的保留能力 SHALL 由同期交付的 `/.monkeycode/specs/dataplane-ts-retention/` 提供（`TimeSeriesStore::delete_series`、全局保留窗口与 `dataserver_ts_*` 自监控指标），本 feature 不再将其列为外部依赖。
4. THE 下列能力 SHALL 列入后续范围：OTLP metrics 与 logs 信号接收、应用侧 SDK、日志推导 trace、trace 告警与通知、exemplar 关联、`attribute_allowlist` 之外的数据裁剪策略、跨 dataserver 查询、trace 归档到对象存储。
5. THE 既有 `data_type=apm` 的写入与查询 SHALL 保持可用，且不参与新的 `apm_service_*` 与 `apm_edge_*` 聚合。
6. THE 本 feature SHALL 不修改 `DataEnvelope` 结构，只通过 `data_type` 扩展承载 `traces`。
7. THE 控制面现有认证、心跳、会话、作业下发、采集项下发与既有采集器行为 SHALL 保持不变。
8. THE 实现顺序 SHALL 为：`LogStore` 索引版本 v2 → `dataplane-ts-retention` → 本 feature；`ebpf-observability` 在本 feature 之后。

### Requirement 16: 可观测性与错误处理

**User Story:** AS 运维人员, I want APM 自身链路可观测, so that 能区分应用不报、Agent 丢批与存储失败。

#### Acceptance Criteria

1. THE Agent SHALL 统计并输出：OTLP 接收请求数、解析失败数、被 `service_allowlist` 丢弃的 span 数、入缓冲批次数、上报成功与失败次数。
2. THE `dataserver` SHALL 统计并输出：trace 批次数、span 接收条数、非法记录数、限流丢弃数、聚合任务成功与失败次数、清理任务删除条数。
3. THE 统计 SHALL 通过既有自监控指标口暴露为 Prom 形指标，供既有监控页展示。
4. WHEN OTLP 解析失败，THE Agent SHALL 向标准错误输出一行，含 `agent_id`、失败原因与丢弃的 span 数，不做无限重试。
5. WHEN `dataserver` 写入 `traces` 部分失败，THE `dataserver` SHALL 在接入应答中返回失败条数与 `code`，Agent 按既有约定把 `partial` 视为已确认。
6. WHEN sqlite 写入失败，THE `dataserver` SHALL 返回 `query_failed`，Agent 退避重试，且明细的幂等去重保证重试不产生重复数据。
7. WHEN 应用与 Agent 网络不互通（容器跨命名空间、防火墙、`otlp_allowed_cidrs` 拒绝），THE Agent SHALL 将对应拒绝计数暴露到自监控口，并在链路页提示 OTLP 接收地址与当前监听配置，使运维能直接看出是配置不通而非应用不报。

### Requirement 17: 服务名映射配置

**User Story:** AS 运维人员, I want 给无插桩应用与未识别 IP 配服务名, so that eBPF 兜底数据能在拓扑图里给出可读的服务名。

#### Acceptance Criteria

1. THE `dataserver` SHALL 持久化静态服务名映射表 `apm_service_alias`（定义见 `observability-data-model`），字段含 `alias_id`、`match_kind`、`match_value`、`service`、`enabled`、`note`、`updated_ts`。
2. THE `match_kind` SHALL 限于 `process_name`、`process_prefix`、`pod_prefix`、`cidr` 四种。
3. THE `dataserver` SHALL 提供 `GET`、`POST`、`PUT`、`DELETE /v1/apm/service-aliases`（同源 SQL 口），列表接口支持按 `match_kind` 与 `enabled` 过滤。
4. THE 校验 SHALL 拒绝：`match_value` 为空、`service` 为空、`cidr` 非法（非合法 CIDR）、`match_kind` 不在枚举内；返回 `invalid_argument`。
5. WHEN 保存或删除映射，THE `dataserver` SHALL 立即让反查缓存失效，使新配置无需重启生效。
6. THE 反查优先级 SHALL 为：静态映射（同类多条取 `updated_ts` 最新）> 动态端点表 `apm_service_endpoint` > `unknown-<ip>`。
7. THE 静态映射 SHALL 只用于 eBPF 边与 `unknown-*` 值的归一，SHALL 不覆盖 OTLP span 已有的 `service`（后者以 resource 为准）。
8. THE 前端 SHALL 提供 `/settings/service-aliases` 页：列表（含启用开关与编辑/删除）、新建/编辑表单（类型下拉、匹配值、服务名、备注）。
9. THE 前端 SHALL 提供批量导入（粘贴 `match_kind,match_value,service` 三列文本），导入前展示将要新增与覆盖的条数，确认后提交。
10. THE `/topology` 页 SHALL 在存在 `unknown-*` 节点时，提供「为该节点建立映射」快捷入口，跳到映射页并预填 `cidr` 或 `pod_prefix`。
11. WHEN 映射表为空或全部未启用，THE 系统行为 SHALL 与新增该表之前一致（全部落 `unknown-<ip>`），不影响其它功能。
