# Requirements Document

> **已归档**（2026-09-26 文档整理）：本 feature 已实现并合入 main，本文件压缩为需求索引——完整 EARS 条款见 git 历史（本文件重写前的最后一个版本），design.md 全文保留为实施细节档案。

## Introduction

本 feature 打通采集数据全链路：Agent 在目标机器采集运维数据；GSE Server 纳管数据面服务并向 Agent 下发上报地址；Agent 直连数据面完成接入；数据面组件 `dataserver` 负责落盘与查询。

数据面复用已交付的分层存储（时序、日志检索、KV、文件、关系型），本 feature 在其上增加「接入」与「按采集类型查询」。GSE Server 继续作为控制面会话与信令底座，并提供数据面服务的运维登记与上报地址下发。`dataserver` 不向 GSE 自登记。查询与采集链路上下文由 `dataserver` 自带前端维护。

v1 可运行范围：以「采集项」为配置单元打通指标与日志（文件路径模糊匹配、K8s Pod 标准输出模糊匹配）。采集项在 dataserver 前端新建/编辑，持久化在 GSE Server sqlite，经 GSE 下发到 Agent。每个采集项含采集端配置（路径/Pod 匹配、开始标记、攒批上报、清洗）与入库配置（保存周期）。APM 与 eBPF 在 v1 完成统一信封、接入落盘与按类型查询；对应采集器列入后续范围。上行可靠性：内存批次确认、失败重试、缓冲满丢最旧；默认上限 1000 条，进程重启缓冲清空。

## Glossary

- **dataserver**：数据面二进制组件，负责采集数据接入与查询。
- **数据面服务**：由运维在 GSE Server 登记的 dataserver 实例，含服务标识、上报地址、查询地址与状态。
- **上报地址（ingest_url）**：Agent 直连写入采集批次的 HTTP 基址，由 GSE Server 下发。
- **查询地址（query_url）**：客户端查询已接入数据的 HTTP 基址。
- **接入（Ingest）**：将一批采集记录写入 dataserver 存储。
- **查询（Query）**：按类型、时间范围、标签与关键词检索已接入数据。
- **GSE Server**：全局调度引擎调度端，纳管数据面服务，向已认证 Agent 下发上报地址。
- **GSE Agent**：目标机器上的执行端，运行采集器，向 GSE Server 拉取上报地址后直连 dataserver 报送。
- **采集器（Collector）**：Agent 内按采集项类型工作的采集模块。
- **采集项（CollectItem）**：一条可独立下发的采集配置，含类型、采集端配置与入库配置。
- **采集链路**：dataserver 前端中的采集项，及其对应的最近接入状态。
- **指标（Metrics）**：周期性数值样本，例如 CPU、内存。
- **日志（Logs）**：带时间戳的文本记录，来自文件路径匹配或 K8s Pod 标准输出。
- **APM 数据**：应用性能记录，至少含 trace、span、服务名与耗时。
- **eBPF 数据**：内核可观测事件，至少含事件类型、进程标识与时间戳。
- **数据信封（Envelope）**：接入共用的一批记录外包装，含类型、来源与批次标识。
- **数据批次（Batch）**：一次接入请求中的多条记录。
- **data_type**：采集类型标识，取值 `metrics`、`logs`、`apm`、`ebpf`。
- **data_id**：采集项标识 `item_id`，用于区分同一类型下的不同采集项。
- **agent_id**：Agent 唯一标识，与现有会话、台账一致。
- **host_id**：主机资产标识；有台账关联时由 GSE Server 随上报地址一并下发。
- **记录时间**：每条记录的事件时间，Unix 微秒。
- **接入应答**：dataserver 对一批记录的处理结果，含成功条数与失败原因。

## Requirements

### Requirement 1: dataserver 接入与查询

- AS 平台开发者, I want 名为 dataserver 的数据面组件承担接入与查询, so that 存储与管道在同一二进制内演进。
- 验收：THE 数据面二进制 SHALL 命名为 `dataserver`；THE `dataserver` SHALL 提供采集数据接入接口，按 `data_type` 将记录写入对应存储。
### Requirement 2: 统一数据信封

- AS Agent, I want 四类采集共用同一信封, so that 直连接入只需识别类型和批次。
- 验收：THE 数据信封 SHALL 包含字段：`batch_id`、`data_type`、`data_id`、`agent_id`、`host_id`、`sent_at_micros`、`records`；THE `data_type` SHALL 取值为 `metrics`、`logs`、`apm`、`ebpf` 四者之一。
### Requirement 3: 四类记录字段

- AS 查询调用方, I want 每类记录有稳定字段, so that 查询条件可以按类型编写。
- 验收：THE `metrics` 记录 SHALL 包含：`record_id`、`timestamp`、`measurement`、`tags`（字符串到字符串）、`field_name`、`field_value`（数值）；THE `logs` 记录 SHALL 包含：`record_id`、`timestamp`、`level`、`message`、`source`（文件路径或采集源名）、`labels`。
### Requirement 4: Agent 指标采集

- AS 运维人员, I want Agent 周期性采集主机指标, so that 可以按时间查看机器资源使用。
- 验收：WHEN Agent 收到类型为 `metrics_host` 且启用的采集项，THE Agent SHALL 按该采集项的间隔采集主机指标；THE 指标采集间隔 SHALL 取自该采集项，缺省 15 秒。
### Requirement 5: Agent 日志文件采集

- AS 运维人员, I want Agent 按采集项中的路径模式采集日志文件, so that 可以集中检索主机日志。
- 验收：WHEN Agent 收到类型为 `log_file` 且启用的采集项，THE Agent SHALL 按该采集项的路径模式匹配文件，从开始标记处读取日志行，并在到达文件末尾后继续采集后续新增行；THE 路径模式 SHALL 支持模糊匹配（`*` 与 `?` glob）。
### Requirement 6: APM 与 eBPF 接入能力

- AS 平台开发者, I want dataserver 先收下 APM 与 eBPF 信封, so that 后续采集器接入时不用改查询模型。
- 验收：WHEN `dataserver` 收到 `data_type=apm` 且字段完整的批次，THE `dataserver` SHALL 将每条记录写入日志检索存储；WHEN `dataserver` 收到 `data_type=ebpf` 且字段完整的批次，THE `dataserver` SHALL 将每条记录写入日志检索存储。
### Requirement 7: GSE Server 纳管数据面服务

- AS 运维人员, I want GSE Server 登记并跟踪 dataserver 实例, so that Agent 能拿到可用的上报地址。
- 验收：THE GSE Server SHALL 持久化数据面服务登记，字段包含：`service_id`、`ingest_url`、`query_url`、`status`、`last_seen_at`、`registered_at`；WHEN 运维通过 HTTP 提交合法登记，THE GSE Server SHALL 以 `service_id` 为主键幂等写入 `ingest_url` 与 `query_url`，并将 `status` 置为 `unknown`。
### Requirement 8: Agent 获取上报地址

- AS Agent, I want 从 GSE Server 拉取 dataserver 上报地址, so that 采集数据直连数据面，控制面只做纳管与选路。
- 验收：WHEN Agent 认证成功，THE Agent SHALL 通过与 GSE Server 的已有连接请求上报地址；WHEN GSE Server 收到已认证 Agent 的上报地址请求，且至少有一个 `status=online` 的数据面服务，THE GSE Server SHALL 在 online 集合上按 `agent_id` 稳定哈希选出一条，返回其 `ingest_url`，以及台账中该 Agent 关联的 `host_id`（无关联时为空）。
### Requirement 9: Agent 直连 dataserver 上报

- AS Agent, I want 把采集批次直接发给 dataserver, so that 采集流量走数据面，GSE Server 连接保持控制面职责。
- 验收：WHEN Agent 持有有效 `ingest_url` 且本地存在未确认批次，THE Agent SHALL 向该地址的接入接口发送数据信封；WHEN `dataserver` 返回接入成功（`status=ok`），THE Agent SHALL 丢弃该批次的本地缓冲。
### Requirement 10: dataserver 接入接口

- AS Agent, I want 调用稳定的接入接口, so that 上报逻辑与存储引擎解耦。
- 验收：THE `dataserver` SHALL 提供批次接入接口，请求体为数据信封；WHEN 接入成功，THE `dataserver` SHALL 返回 JSON，字段包含 `batch_id`、`accepted`（条数）、`status`（值为 `ok`）。
### Requirement 11: 指标查询

- AS 查询客户端, I want 用 Prometheus 形状向 dataserver 查询已接入指标, so that 现有 Prom 查询入口可以读到 Agent 采集数据。
- 验收：WHEN 指标记录接入成功，THE `dataserver` SHALL 使该样本可被 `GET /api/v1/query` 与 `GET /api/v1/query_range` 查询到；THE 查询层 SHALL 将 `measurement` 作为指标名，将 `tags` 作为 labels，将 `field_value` 作为样本值。
### Requirement 12: 日志、APM 与 eBPF 查询

- AS 查询客户端, I want 向 dataserver 按类型、时间、标签和关键词检索记录, so that 三类事件数据可以用同一查询入口过滤。
- 验收：THE `dataserver` SHALL 在 SQL HTTP 同一端口提供 `POST /v1/logs/search`；THE 查询请求 SHALL 支持过滤字段：`data_type`、`agent_id`、`host_id`、`data_id`、时间范围、`level`、message 关键词、`trace_id`、`event_type`、以及额外 labels。
### Requirement 13: 采集项配置与下发

- AS 运维人员, I want 在 dataserver 以采集项为单位配置采集和入库并由 GSE 下发, so that 每条采集链路可独立维护。
- 验收：THE 采集项 SHALL 包含：`item_id`、目标 `agent_id` 列表（至少一个）、名称、类型、启用开关、采集端配置、入库配置；THE 采集项类型 SHALL 为 `metrics_host`、`log_file`、`log_k8s_stdout` 三者之一。
### Requirement 14: 错误与可观测性

- AS 运维人员, I want 接入失败和查询失败有明确输出, so that 可以区分采集、选路与存储问题。
- 验收：THE 接入与查询错误响应 SHALL 使用 JSON，并包含机器可读的 `code` 字段；THE `code` SHALL 使用已有稳定值：`invalid_argument`、`unavailable`、`query_failed`、`not_found`。
### Requirement 15: v1 范围边界

- AS 开发者, I want 明确本期交付边界, so that 实现范围与存储层、控制面已有规格对齐。
- 验收：THE v1 可运行采集器 SHALL 覆盖 `metrics_host`、`log_file`、`log_k8s_stdout` 三类采集项；THE v1 `dataserver` SHALL 接受并查询 `metrics`、`logs`、`apm`、`ebpf` 四类信封。
### Requirement 16: dataserver 前端

- AS 运维人员, I want 在 dataserver 自带页面查看采集链路并检索数据, so that 查询与接入上下文在数据面进程内闭环。
- 验收：THE `dataserver` SHALL 在 SQL HTTP 端口按配置 `http_web_dir` 托管自有前端静态资源；WHEN `http_web_dir` 已配置，THE `dataserver` SHALL 对未匹配 API 的 GET 请求回退 `index.html`。
### Requirement 17: K8s Pod 标准输出采集

- AS 运维人员, I want 按 Pod 名模糊匹配采集容器标准输出, so that 应用日志不必先落文件。
- 验收：WHEN Agent 收到类型为 `log_k8s_stdout` 且启用的采集项，THE Agent SHALL 调用 Kubernetes API 采集该命名空间内匹配 Pod 的容器标准输出（与 `kubectl logs` 同一路径：apiserver 的 pod log 接口，`follow=true`）；THE 命名空间 SHALL 为精确值，必填。
