# Requirements Document

## Introduction

本 feature 为 vectorman 长期运行的服务端组件补齐自监控。每个组件在独立 metrics 口对外提供 Prometheus 文本格式指标接口。`dataserver` 另外将本进程样本按周期写入自有时序存储，供现有 PromQL 查询。其它组件的样本抓取与入库由后续采集逻辑承担。

自监控覆盖进程运行时、HTTP 请求与组件业务量。Agent 主机采集（`metrics_host` 的 `cpu_usage` / `mem_usage`）保持现有路径，与本 feature 的组件自监控分开。

v1 范围：`gse-server`、`dataserver`、`console`。`gse-agent`、`vmctl`、`dpc` 不在本 feature 范围。

已确认决策：三个服务端均在独立 `metrics_listen` 口提供 `GET /metrics`；组件只暴露接口；`dataserver` 将本进程样本写入本进程 TSDB。

## Glossary

- **服务端组件**：长期运行并对外提供 HTTP 的二进制，v1 为 `gse-server`、`dataserver`、`console`。
- **自监控**：服务端组件采集自身运行与业务量指标。
- **普罗指标接口**：独立 metrics 监听口上的 `GET /metrics`，响应体为 Prometheus text exposition format（`text/plain; version=0.0.4`）。
- **时序存储（TSDB）**：dataserver 进程内的 `TimeSeriesStore`，对外经现有 Prometheus 查询 HTTP 检索。
- **组件指标名**：自监控指标的 measurement / metric 名称，统一前缀 `vectorman_`。
- **component**：组件角色标签，取值 `gse-server`、`dataserver`、`console`。
- **instance**：组件实例标签，取值为该进程业务 HTTP 监听地址。
- **主机采集指标**：Agent `metrics_host` 上报的 `cpu_usage`、`mem_usage` 等，不属于自监控。
- **落盘周期**：`dataserver` 将本进程指标快照写入时序存储的间隔。

## Requirements

### Requirement 1: 服务端组件覆盖

**User Story:** AS 平台运维, I want 所有长期运行的服务端组件都具备自监控, so that 控制面、数据面与门户的健康状态可以统一观察。

#### Acceptance Criteria

1. THE `gse-server` SHALL 启用自监控。
2. THE `dataserver` SHALL 启用自监控。
3. THE `console` SHALL 启用自监控。
4. THE 自监控 SHALL 在对应组件进程启动后自动开始采集。

### Requirement 2: 普罗指标接口

**User Story:** AS 外部 Prometheus 或抓取器, I want 从每个服务端组件拉取标准文本指标, so that 可以接入现有监控体系。

#### Acceptance Criteria

1. THE 每个服务端组件 SHALL 绑定独立的 metrics HTTP 监听地址，并在该地址提供 `GET /metrics`。
2. WHEN 调用方请求该接口, THE 该组件 SHALL 返回 HTTP 200 与 Prometheus text exposition format 正文。
3. THE 响应 `Content-Type` SHALL 为 `text/plain; version=0.0.4; charset=utf-8`。
4. THE 普罗指标接口 SHALL 无需鉴权即可读取。
5. WHEN 组件进程正在运行, THE 普罗指标接口 SHALL 在每次请求时返回当前内存中的最新快照。

### Requirement 3: 指标身份标签

**User Story:** AS 查询调用方, I want 每条自监控样本带上组件与实例身份, so that 多实例部署时可以按组件过滤。

#### Acceptance Criteria

1. THE 每条自监控样本 SHALL 携带标签 `component`，取值为 `gse-server`、`dataserver`、`console` 之一。
2. THE 每条自监控样本 SHALL 携带标签 `instance`，取值为该进程业务 HTTP 监听地址。
3. THE 普罗指标接口正文中的标签集 SHALL 与 `dataserver` 写入时序存储的 tags 一致。

### Requirement 4: 进程运行时指标

**User Story:** AS 运维人员, I want 看到每个服务端组件自身的 CPU、内存与存活时长, so that 能判断进程是否过载或刚重启。

#### Acceptance Criteria

1. THE 服务端组件 SHALL 暴露 `vectorman_process_cpu_seconds_total`，值为该进程累计 CPU 秒。
2. THE 服务端组件 SHALL 暴露 `vectorman_process_resident_memory_bytes`，值为该进程 RSS 字节数。
3. THE 服务端组件 SHALL 暴露 `vectorman_process_uptime_seconds`，值为该进程自启动以来的秒数。
4. THE 进程运行时指标 SHALL 在每次普罗指标接口请求时刷新。
5. THE `dataserver` 进程运行时指标 SHALL 在每次落盘时刷新。

### Requirement 5: HTTP 请求指标

**User Story:** AS 运维人员, I want 看到每个服务端组件的 HTTP 请求量与耗时, so that 能定位接口变慢或错误率升高。

#### Acceptance Criteria

1. THE 服务端组件 SHALL 暴露计数器 `vectorman_http_requests_total`，标签含 `method`、`path`、`status`。
2. THE `path` 标签 SHALL 使用路由模板（例如 `/api/gse/agents/{agent_id}`）。
3. THE 服务端组件 SHALL 暴露直方图 `vectorman_http_request_duration_seconds`，标签含 `method`、`path`。
4. WHEN 一次 HTTP 请求完成, THE 服务端组件 SHALL 更新上述计数器与直方图。
5. THE HTTP 请求指标 SHALL 覆盖该组件对外提供的业务 HTTP 端口。
6. THE metrics 监听口上的请求 SHALL 排除在 `vectorman_http_requests_total` 之外。

### Requirement 6: 组件业务量指标

**User Story:** AS 运维人员, I want 看到各组件特有的业务量, so that 自监控能反映控制面会话与数据面接入是否正常。

#### Acceptance Criteria

1. THE `gse-server` SHALL 暴露 `vectorman_gse_agents_online`，值为当前台账状态为 `online` 的 Agent 数量。
2. THE `gse-server` SHALL 暴露 `vectorman_gse_sessions`，值为当前内存会话数。
3. THE `dataserver` SHALL 暴露 `vectorman_ingest_records_accepted_total`，值为接入成功记录累计数。
4. THE `dataserver` SHALL 暴露 `vectorman_ingest_records_failed_total`，值为接入失败记录累计数。
5. THE `console` SHALL 暴露 `vectorman_console_apps`，值为当前目录中的应用数量。

### Requirement 7: dataserver 写入自有时序存储

**User Story:** AS 平台运维, I want dataserver 把本进程自监控样本写入自有时序存储, so that 无需外部 Prometheus 也能用现有查询口回看数据面健康。

#### Acceptance Criteria

1. THE `dataserver` SHALL 按落盘周期将本进程自监控样本写入本进程时序存储。
2. THE 写入的 measurement 名称 SHALL 与普罗指标名一致。
3. THE 写入的 tags SHALL 包含 `component` 与 `instance`，并包含该指标在普罗接口中的其余标签。
4. THE 默认落盘周期 SHALL 为 60 秒。
5. THE `gse-server` 与 `console` SHALL 仅通过普罗指标接口对外提供自监控快照。

### Requirement 8: 用现有 PromQL 查询 dataserver 自监控

**User Story:** AS 查询调用方, I want 用现有 Prometheus 查询 HTTP 检索 dataserver 自监控, so that 控制台与 `dpc query` 无需新协议。

#### Acceptance Criteria

1. WHEN 调用方对 dataserver 执行 `GET /api/v1/query?query=vectorman_process_uptime_seconds`, THE dataserver SHALL 返回已落盘的本进程自监控即时值。
2. WHEN 调用方对 dataserver 执行 `GET /api/v1/query_range` 且表达式为自监控指标名, THE dataserver SHALL 返回本进程自监控在对应时间范围内的样本。
3. THE 自监控样本与主机采集指标 SHALL 可在同一时序存储中共存。

### Requirement 9: 与主机采集隔离

**User Story:** AS 查询调用方, I want 组件自监控与 Agent 主机采集使用不同指标名, so that 查询不会把进程 RSS 和主机 mem_usage 混在一起。

#### Acceptance Criteria

1. THE 自监控指标名 SHALL 使用前缀 `vectorman_`。
2. THE 现有主机采集指标名 `cpu_usage` 与 `mem_usage` SHALL 保持不变。
3. THE `dataserver` 自监控写入 SHALL 使用独立的 `data_id` 取值 `self`。

### Requirement 10: 配置

**User Story:** AS 部署人员, I want 用配置控制普罗接口监听与 dataserver 落盘周期, so that 不同环境可以改端口或改周期。

#### Acceptance Criteria

1. THE 每个服务端组件 SHALL 提供配置项 `metrics_listen`，绑定独立 metrics HTTP 口，路径固定为 `GET /metrics`。
2. THE `gse-server` `metrics_listen` 缺省 SHALL 为 `127.0.0.1:7102`。
3. THE `dataserver` `metrics_listen` 缺省 SHALL 为 `127.0.0.1:9091`。
4. THE `console` `metrics_listen` 缺省 SHALL 为 `127.0.0.1:7201`。
5. WHERE `dataserver` 配置了前端目录, THE SQL/Web 口 `GET /metrics` SHALL 继续返回 SPA 页面。
6. THE `dataserver` SHALL 提供配置项 `self_metrics_interval_secs`，缺省 60，用于本进程样本落盘周期。
7. THE 上述配置 SHALL 支持对应组件现有前缀的环境变量覆盖（`GSE_`、`DP_`、`CONSOLE_`）。

### Requirement 11: 故障隔离

**User Story:** AS 平台运维, I want 自监控失败不影响业务, so that 抓取或落盘异常不会拖垮调度与接入。

#### Acceptance Criteria

1. IF 进程运行时采样失败, THE 服务端组件 SHALL 跳过本轮该指标并继续提供业务 HTTP 与普罗指标接口。
2. IF `dataserver` 时序写入失败, THE `dataserver` SHALL 记录错误并在下一落盘周期重试，业务接口保持可用。
3. IF 普罗指标接口序列化失败, THE 服务端组件 SHALL 返回 HTTP 500 且正文含错误说明，其它路由保持可用。

### Requirement 12: 测试

**User Story:** AS 开发者, I want 自监控有自动化测试, so that 接口格式与落盘路径可回归。

#### Acceptance Criteria

1. THE 每个服务端组件 SHALL 具备测试：请求普罗指标接口返回 200，正文含 `vectorman_process_uptime_seconds` 与标签 `component`。
2. THE `dataserver` SHALL 具备测试：自监控落盘后 `query` 能命中 `vectorman_process_uptime_seconds{component="dataserver"}`。
3. THE `dataserver` SHALL 具备测试：SQL/Web 口 `GET /metrics` 在配置了前端目录时仍返回 SPA 页面。
