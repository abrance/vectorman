# Requirements Document

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

**User Story:** AS 平台开发者, I want 名为 dataserver 的数据面组件承担接入与查询, so that 存储与管道在同一二进制内演进。

#### Acceptance Criteria

1. THE 数据面二进制 SHALL 命名为 `dataserver`。
2. THE `dataserver` SHALL 提供采集数据接入接口，按 `data_type` 将记录写入对应存储。
3. THE `dataserver` SHALL 提供按 `data_type` 查询已接入数据的接口。
4. THE `dataserver` SHALL 将 `metrics` 记录写入时序存储。
5. THE `dataserver` SHALL 将 `logs`、`apm`、`ebpf` 记录写入日志检索存储，并用标签保留 `data_type`。
6. THE `dataserver` SHALL 提供现有 SQL HTTP 与 Prometheus 查询 HTTP，并在同一进程内提供本 feature 的接入与日志检索 HTTP。
7. THE `dataserver` SHALL 托管自有前端，用于展示采集链路上下文并查询已接入数据。

### Requirement 2: 统一数据信封

**User Story:** AS Agent, I want 四类采集共用同一信封, so that 直连接入只需识别类型和批次。

#### Acceptance Criteria

1. THE 数据信封 SHALL 包含字段：`batch_id`、`data_type`、`data_id`、`agent_id`、`host_id`、`sent_at_micros`、`records`。
2. THE `data_type` SHALL 取值为 `metrics`、`logs`、`apm`、`ebpf` 四者之一。
3. THE `records` SHALL 为该批次内同一 `data_type` 的记录数组，条数大于 0。
4. THE 每条记录 SHALL 包含 `record_id` 与 `timestamp`（Unix 微秒）。
5. IF 信封缺少 `data_type` 或 `agent_id`，或 `records` 为空，THE `dataserver` SHALL 拒绝该批次并返回 `invalid_argument`。

### Requirement 3: 四类记录字段

**User Story:** AS 查询调用方, I want 每类记录有稳定字段, so that 查询条件可以按类型编写。

#### Acceptance Criteria

1. THE `metrics` 记录 SHALL 包含：`record_id`、`timestamp`、`measurement`、`tags`（字符串到字符串）、`field_name`、`field_value`（数值）。
2. THE `logs` 记录 SHALL 包含：`record_id`、`timestamp`、`level`、`message`、`source`（文件路径或采集源名）、`labels`。
3. THE `apm` 记录 SHALL 包含：`record_id`、`timestamp`、`trace_id`、`span_id`、`parent_span_id`、`service`、`operation`、`duration_micros`、`status`、`labels`。
4. THE `ebpf` 记录 SHALL 包含：`record_id`、`timestamp`、`event_type`、`pid`、`process_name`、`message`、`labels`。
5. WHEN `dataserver` 写入 `logs`、`apm` 或 `ebpf`，THE `dataserver` SHALL 在日志标签中写入 `data_type`、`agent_id`、`data_id`；`host_id` 非空时同时写入。

### Requirement 4: Agent 指标采集

**User Story:** AS 运维人员, I want Agent 周期性采集主机指标, so that 可以按时间查看机器资源使用。

#### Acceptance Criteria

1. WHEN Agent 收到类型为 `metrics_host` 且启用的采集项，THE Agent SHALL 按该采集项的间隔采集主机指标。
2. THE 指标采集间隔 SHALL 取自该采集项，缺省 15 秒。
3. THE v1 指标采集 SHALL 产出以下 measurement：`cpu_usage`、`mem_usage`。
4. THE 每条指标记录的 `tags` SHALL 包含 `agent_id`；存在 `host_id` 时同时包含 `host_id`。
5. WHEN 一次采集完成，THE Agent SHALL 将本轮指标放入 `data_type=metrics` 的数据批次。

### Requirement 5: Agent 日志文件采集

**User Story:** AS 运维人员, I want Agent 按采集项中的路径模式采集日志文件, so that 可以集中检索主机日志。

#### Acceptance Criteria

1. WHEN Agent 收到类型为 `log_file` 且启用的采集项，THE Agent SHALL 按该采集项的路径模式匹配文件，从开始标记处读取日志行，并在到达文件末尾后继续采集后续新增行。
2. THE 路径模式 SHALL 支持模糊匹配（`*` 与 `?` glob）。
3. THE 开始标记 SHALL 使用 `start_mode` 为 `head` 或 `tail`，以及非负整数 `start_n`。
4. WHEN `start_mode=head`，THE Agent SHALL 从文件第 `start_n` 行开始读；`start_n` 最小为 1，表示文件第一行。
5. WHEN `start_mode=tail`，THE Agent SHALL 从倒数第 `start_n` 行开始读；`start_n=0` 表示从当前文件末尾开始，不读取已有行。
6. THE 开始标记缺省 SHALL 为 `start_mode=tail` 且 `start_n=0`。
7. THE 每条日志记录的 `source` SHALL 为实际文件路径，`data_id` SHALL 为该采集项的 `item_id`。
8. THE 日志记录的 `level` SHALL 从该行文本识别；无法识别时使用 `info`。
9. WHEN 清洗后的日志行达到该采集项的批次上限或达到上报间隔，THE Agent SHALL 将日志放入 `data_type=logs` 的数据批次。
10. THE 日志批次上限与上报间隔 SHALL 取自该采集项；缺省上限 100 条、间隔 5 秒。
11. IF 某匹配路径当前不存在，THE Agent SHALL 跳过该路径并向标准错误输出一行，进程继续；后续每个采集周期重新匹配并打开。
12. THE Agent SHALL 在上报前按该采集项的清洗配置处理日志行：先按包含/排除正则过滤，再按提取规则写入 labels。
13. THE 清洗提取规则 SHALL 支持 `regex`（第一捕获组写入指定 label）与 `json`（按点分路径取值写入指定 label）。

### Requirement 6: APM 与 eBPF 接入能力

**User Story:** AS 平台开发者, I want dataserver 先收下 APM 与 eBPF 信封, so that 后续采集器接入时不用改查询模型。

#### Acceptance Criteria

1. WHEN `dataserver` 收到 `data_type=apm` 且字段完整的批次，THE `dataserver` SHALL 将每条记录写入日志检索存储。
2. WHEN `dataserver` 收到 `data_type=ebpf` 且字段完整的批次，THE `dataserver` SHALL 将每条记录写入日志检索存储。
3. THE v1 Agent SHALL 提供指标采集器与日志采集器作为可运行采集器。
4. THE APM 采集器与 eBPF 采集器 SHALL 列入后续范围；v1 文档列出这两类的信封字段与查询入口。

### Requirement 7: GSE Server 纳管数据面服务

**User Story:** AS 运维人员, I want GSE Server 登记并跟踪 dataserver 实例, so that Agent 能拿到可用的上报地址。

#### Acceptance Criteria

1. THE GSE Server SHALL 持久化数据面服务登记，字段包含：`service_id`、`ingest_url`、`query_url`、`status`、`last_seen_at`、`registered_at`。
2. WHEN 运维通过 HTTP 提交合法登记，THE GSE Server SHALL 以 `service_id` 为主键幂等写入 `ingest_url` 与 `query_url`，并将 `status` 置为 `unknown`。
3. THE `dataserver` 进程 SHALL 在 v1 不向 GSE Server 发起登记或心跳。
4. THE GSE Server HTTP 管理口 SHALL 提供数据面服务的列表、按 `service_id` 查询与删除。
5. THE GSE Server SHALL 按间隔对已登记实例发起 `GET {ingest_url}/health`；默认间隔 30 秒。
6. WHEN 探活返回 HTTP 200 且 JSON `status` 为 `ok`，THE GSE Server SHALL 将该实例 `status` 置为 `online` 并更新 `last_seen_at`。
7. IF 一次探活失败（连接失败、超时、非 200、或 JSON 不是 `ok`），THE GSE Server SHALL 将该实例 `status` 置为 `offline`。

### Requirement 8: Agent 获取上报地址

**User Story:** AS Agent, I want 从 GSE Server 拉取 dataserver 上报地址, so that 采集数据直连数据面，控制面只做纳管与选路。

#### Acceptance Criteria

1. WHEN Agent 认证成功，THE Agent SHALL 通过与 GSE Server 的已有连接请求上报地址。
2. WHEN GSE Server 收到已认证 Agent 的上报地址请求，且至少有一个 `status=online` 的数据面服务，THE GSE Server SHALL 在 online 集合上按 `agent_id` 稳定哈希选出一条，返回其 `ingest_url`，以及台账中该 Agent 关联的 `host_id`（无关联时为空）。
3. IF 当前没有 `status=online` 的数据面服务，THE GSE Server SHALL 返回 `unavailable`。
4. WHILE Agent 会话在线，THE Agent SHALL 在接入返回 `unavailable` 时重新向 GSE Server 请求上报地址。
5. THE Agent SHALL 将下发的 `host_id` 写入后续数据信封。
6. THE 上报地址请求 SHALL 使用独立于作业下发与 `ping` 信令的控制面方法，使选路与采集写入分离。

### Requirement 9: Agent 直连 dataserver 上报

**User Story:** AS Agent, I want 把采集批次直接发给 dataserver, so that 采集流量走数据面，GSE Server 连接保持控制面职责。

#### Acceptance Criteria

1. WHEN Agent 持有有效 `ingest_url` 且本地存在未确认批次，THE Agent SHALL 向该地址的接入接口发送数据信封。
2. WHEN `dataserver` 返回接入成功（`status=ok`），THE Agent SHALL 丢弃该批次的本地缓冲。
3. IF `dataserver` 返回可重试失败或接入超时，THE Agent SHALL 保留该批次并在退避后重试；退避区间在 1 秒至 60 秒之间。
4. IF 接入连续失败并触发重新拉地址，THE Agent SHALL 使用新的 `ingest_url` 重试未确认批次。
5. IF Agent 尚未拿到 `ingest_url`，THE Agent SHALL 将未确认批次保留在本地缓冲，并继续向 GSE Server 请求上报地址。
6. THE 本地缓冲条数上限 SHALL 从 Agent 配置读取，默认 1000；达到上限时，THE Agent SHALL 丢弃最旧批次并向标准错误输出一条包含 `data_type` 与丢弃条数的信息。

### Requirement 10: dataserver 接入接口

**User Story:** AS Agent, I want 调用稳定的接入接口, so that 上报逻辑与存储引擎解耦。

#### Acceptance Criteria

1. THE `dataserver` SHALL 提供批次接入接口，请求体为数据信封。
2. WHEN 接入成功，THE `dataserver` SHALL 返回 JSON，字段包含 `batch_id`、`accepted`（条数）、`status`（值为 `ok`）。
3. WHEN 批次内部分记录字段非法，THE `dataserver` SHALL 写入合法记录，并返回 `status` 为 `partial`，以及每条失败记录的 `record_id` 与 `code`。
4. IF 整批 `data_type` 未知或信封无法解析，THE `dataserver` SHALL 拒绝写入并返回 HTTP 400，JSON 含 `error` 与 `code`。
5. THE 接入接口 SHALL 在 SQL HTTP 同一端口提供 `POST /v1/ingest`。
6. THE `dataserver` SHALL 接受同一 `record_id` 的重复写入，查询结果对同一 `record_id` 只呈现一条。
7. WHEN 接入返回 `status=partial`，THE Agent SHALL 将该批次视为已确认（合法记录已写入，非法记录需改采集侧）。

### Requirement 11: 指标查询

**User Story:** AS 查询客户端, I want 用 Prometheus 形状向 dataserver 查询已接入指标, so that 现有 Prom 查询入口可以读到 Agent 采集数据。

#### Acceptance Criteria

1. WHEN 指标记录接入成功，THE `dataserver` SHALL 使该样本可被 `GET /api/v1/query` 与 `GET /api/v1/query_range` 查询到。
2. THE 查询层 SHALL 将 `measurement` 作为指标名，将 `tags` 作为 labels，将 `field_value` 作为样本值。
3. WHEN 客户端使用 `agent_id` label matcher 查询，THE `dataserver` SHALL 只返回该 Agent 的样本。

### Requirement 12: 日志、APM 与 eBPF 查询

**User Story:** AS 查询客户端, I want 向 dataserver 按类型、时间、标签和关键词检索记录, so that 三类事件数据可以用同一查询入口过滤。

#### Acceptance Criteria

1. THE `dataserver` SHALL 在 SQL HTTP 同一端口提供 `POST /v1/logs/search`。
2. THE 查询请求 SHALL 支持过滤字段：`data_type`、`agent_id`、`host_id`、`data_id`、时间范围、`level`、message 关键词、`trace_id`、`event_type`、以及额外 labels。
3. WHEN 查询命中记录，THE `dataserver` SHALL 返回 JSON，字段包含 `records` 数组；每条记录含写入时的业务字段与标签。
4. THE 查询请求 SHALL 支持 `limit`，默认 100，最大 1000。
5. IF 过滤条件中的时间范围非法（起点晚于终点），THE `dataserver` SHALL 返回 HTTP 400，JSON 含 `error` 与 `code`。

### Requirement 13: 采集项配置与下发

**User Story:** AS 运维人员, I want 在 dataserver 以采集项为单位配置采集和入库并由 GSE 下发, so that 每条采集链路可独立维护。

#### Acceptance Criteria

1. THE 采集项 SHALL 包含：`item_id`、目标 `agent_id` 列表（至少一个）、名称、类型、启用开关、采集端配置、入库配置。
2. THE 采集项类型 SHALL 为 `metrics_host`、`log_file`、`log_k8s_stdout` 三者之一。
3. THE 日志类采集端配置 SHALL 包含：匹配模式（文件路径 glob，或精确 namespace + Pod 名 glob）、开始标记（`head`/`tail` 与 `start_n`）、批次上限、上报间隔、清洗配置（包含/排除正则与提取规则）。
4. THE 入库配置 SHALL 包含保存周期（天），缺省 1 天。
5. WHEN 某采集项启用开关为关闭，THE Agent SHALL 跳过该采集项；已接入数据仍可查询，保存周期继续生效。
6. THE Agent 进程配置（Server 地址、`agent_id`、token、心跳）SHALL 使用本地 TOML，并允许 `GSE_` 前缀环境变量覆盖对应项。
7. WHEN Agent 进程配置文件缺失或 TOML 无法解析，THE Agent SHALL 以非零退出码退出，并向标准错误输出错误原因。
8. THE 采集项 SHALL 持久化在 GSE Server sqlite 表 `collect_items`，以 `item_id` 为主键。
9. THE `dataserver` 配置 SHALL 包含数据目录、SQL HTTP 监听、Prom 查询 HTTP 监听、可选的前端静态目录 `http_web_dir`、以及 GSE 管理口基址 `gse_admin_url`。
10. WHEN 运维在 dataserver 前端新建或保存采集项，THE `dataserver` SHALL 调用 GSE Server HTTP 写入 `collect_items`。
11. WHEN GSE Server 写入采集项成功，THE GSE Server SHALL 向目标列表中所有在线 Agent 下发各 Agent 应执行的采集项列表。
12. WHEN Agent 认证成功，THE Agent SHALL 向 GSE Server 拉取目标列表包含自己的采集项并应用。
13. WHEN Agent 收到新的采集项列表，THE Agent SHALL 在不重启进程的情况下按新列表运行采集器。
14. IF 没有任何采集项的目标列表包含某 Agent，THE 该 Agent SHALL 不采集。
15. THE `dataserver` SHALL 按采集项保存周期删除该 `item_id` 下超过周期的已接入记录。
16. WHEN 运维删除采集项，THE GSE Server SHALL 删除该配置并通知目标 Agent 停止该采集项；已接入数据保留至原保存周期到期后再删除。

### Requirement 14: 错误与可观测性

**User Story:** AS 运维人员, I want 接入失败和查询失败有明确输出, so that 可以区分采集、选路与存储问题。

#### Acceptance Criteria

1. THE 接入与查询错误响应 SHALL 使用 JSON，并包含机器可读的 `code` 字段。
2. THE `code` SHALL 使用已有稳定值：`invalid_argument`、`unavailable`、`query_failed`、`not_found`。
3. WHEN Agent 完成一次接入，THE Agent SHALL 写一条日志，包含 `agent_id`、`data_type`、`batch_id`、`ingest_url`、`accepted` 条数或失败 `code`。
4. WHEN Agent 丢弃最旧批次，THE Agent SHALL 向标准错误输出 `data_type` 与丢弃条数。
5. THE `dataserver` SQL HTTP 端口的 `GET /health` SHALL 在接入接口可接受请求时返回 `status` 为 `ok`。

### Requirement 15: v1 范围边界

**User Story:** AS 开发者, I want 明确本期交付边界, so that 实现范围与存储层、控制面已有规格对齐。

#### Acceptance Criteria

1. THE v1 可运行采集器 SHALL 覆盖 `metrics_host`、`log_file`、`log_k8s_stdout` 三类采集项。
2. THE v1 `dataserver` SHALL 接受并查询 `metrics`、`logs`、`apm`、`ebpf` 四类信封。
3. THE 下列能力 SHALL 列入后续范围：APM 采集器、eBPF 采集器、`disk_usage` 与 `net_bytes` 采集、dataserver 自登记与心跳、接入令牌校验、查询鉴权、跨数据面集群路由、未确认批次落盘 WAL、GSE 控制台数据面登记页。
4. THE 控制面现有认证、心跳、会话、作业下发与 `ping` 信令 SHALL 保持可用。
5. THE 本 feature 的存储写入 SHALL 调用已有 `TimeSeriesStore` 与 `LogStore` 接口，沿用 `dataplane-layered-storage` 的本地引擎绑定。
6. THE v1 GSE Server 在存在多个 `online` 数据面服务时 SHALL 将 `service_id` 排序后，用 `agent_id` 的哈希对个数取模选出一条 `ingest_url`；同一 `agent_id` 在同一 online 集合上得到同一实例。
7. THE 后续范围内的 APM 采集器与 eBPF 采集器 SHALL 以 `/.monkeycode/specs/apm-tracing/`、`/.monkeycode/specs/ebpf-observability/`、共享的 `/.monkeycode/specs/observability-data-model/` 与存储侧的 `/.monkeycode/specs/dataplane-ts-retention/` 为设计基线；本 feature 的 Requirements 3、6、12 只定义 `apm`/`ebpf` 信封与兼容行为，新链路的记录模型、查询语义与聚合指标保留以上述四份设计为准。

### Requirement 16: dataserver 前端

**User Story:** AS 运维人员, I want 在 dataserver 自带页面查看采集链路并检索数据, so that 查询与接入上下文在数据面进程内闭环。

#### Acceptance Criteria

1. THE `dataserver` SHALL 在 SQL HTTP 端口按配置 `http_web_dir` 托管自有前端静态资源。
2. WHEN `http_web_dir` 已配置，THE `dataserver` SHALL 对未匹配 API 的 GET 请求回退 `index.html`。
3. IF `http_web_dir` 未配置，THE `dataserver` SHALL 仍提供接入与查询 API。
4. THE 前端 SHALL 提供采集链路页，列出采集项：名称、类型、目标 Agent 列表、启用状态、最近接入时间、最近一批 `accepted` 条数。
5. THE 链路页 SHALL 提供「新建采集项」。
6. THE 链路页每一行 SHALL 提供启用开关、「编辑」「查看详情」「数据检索」「删除」。
7. WHEN 运维点击「编辑」或「新建采集项」，THE 前端 SHALL 打开采集项表单（多选 Agent、类型、匹配模式、开始标记、攒批、清洗含提取规则、保存周期），保存时经 dataserver 写入 GSE。
8. WHEN 运维点击「查看详情」，THE 前端 SHALL 只读展示该采集项全部字段与最近接入状态。
9. WHEN 运维切换启用开关，THE 前端 SHALL 保存 `enabled` 并经 dataserver 写入 GSE。
10. WHEN 运维点击「删除」并确认，THE 前端 SHALL 调用删除接口。
11. WHEN 运维点击「数据检索」，THE 前端 SHALL 跳到对应查询页并带上 `agent_id` 与 `data_id=item_id`：`metrics_host` 到指标页，日志类到日志页。
12. THE 三个页面的数据刷新 SHALL 由运维手动触发。
13. THE 前端 SHALL 提供指标查询页：默认按时间范围与 `agent_id` 展示 `cpu_usage` 与 `mem_usage`；并提供可展开的 PromQL 输入，提交后调用 `query_range`。
14. THE 前端 SHALL 提供日志检索页，`data_type` 可切换 `logs`、`apm`、`ebpf`（缺省 `logs`），调用 `POST /v1/logs/search` 并展示命中记录。
15. THE 前端与接入、查询、链路、采集项 API SHALL 同源，使用 SQL HTTP 同一端口。
16. WHEN 一批记录接入成功（`ok` 或 `partial`），THE `dataserver` SHALL 更新该 `item_id` 对应流的最近接入时间，供链路页读取。
17. THE `dataserver` SHALL 提供 `GET /v1/streams`，返回本实例当前流列表。
18. THE `dataserver` SHALL 提供采集项列表与读写接口，并转发到 GSE Server 的 `collect_items` HTTP。

### Requirement 17: K8s Pod 标准输出采集

**User Story:** AS 运维人员, I want 按 Pod 名模糊匹配采集容器标准输出, so that 应用日志不必先落文件。

#### Acceptance Criteria

1. WHEN Agent 收到类型为 `log_k8s_stdout` 且启用的采集项，THE Agent SHALL 调用 Kubernetes API 采集该命名空间内匹配 Pod 的容器标准输出（与 `kubectl logs` 同一路径：apiserver 的 pod log 接口，`follow=true`）。
2. THE 命名空间 SHALL 为精确值，必填。
3. THE Pod 名模式 SHALL 支持 `*` 与 `?` glob（单层）。
4. IF 采集项填写了容器名，THE Agent SHALL 只采集该容器；IF 容器名为空，THE Agent SHALL 采集该 Pod 下全部容器。
5. THE 开始标记 `tail` 与 `start_n` SHALL 映射为 API 的 `tailLines`；`head` 从当前日志流可读起点开始。
6. THE 该采集项 SHALL 使用与 `log_file` 相同的攒批、清洗与入库配置。
7. THE 每条记录的 `source` SHALL 标识命名空间、Pod 名与容器名，`data_id` SHALL 为该采集项 `item_id`。
8. THE Agent SHALL 优先使用 in-cluster ServiceAccount；若不在集群内，THE Agent SHALL 使用采集项 `kubeconfig` 路径，缺省为 `~/.kube/config`。
9. THE Agent SHALL 每 30 秒重新 list Pods：对新匹配的 Pod 开始 follow，对不再匹配的 Pod 停止 follow。
