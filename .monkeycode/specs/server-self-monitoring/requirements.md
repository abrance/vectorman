# Requirements Document

> **已归档**（2026-09-26 文档整理）：本 feature 已实现并合入 main，本文件压缩为需求索引——完整 EARS 条款见 git 历史（本文件重写前的最后一个版本），design.md 全文保留为实施细节档案。

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

- AS 平台运维, I want 所有长期运行的服务端组件都具备自监控, so that 控制面、数据面与门户的健康状态可以统一观察。
- 验收：THE `gse-server` SHALL 启用自监控；THE `dataserver` SHALL 启用自监控。
### Requirement 2: 普罗指标接口

- AS 外部 Prometheus 或抓取器, I want 从每个服务端组件拉取标准文本指标, so that 可以接入现有监控体系。
- 验收：THE 每个服务端组件 SHALL 绑定独立的 metrics HTTP 监听地址，并在该地址提供 `GET /metrics`；WHEN 调用方请求该接口, THE 该组件 SHALL 返回 HTTP 200 与 Prometheus text exposition format 正文。
### Requirement 3: 指标身份标签

- AS 查询调用方, I want 每条自监控样本带上组件与实例身份, so that 多实例部署时可以按组件过滤。
- 验收：THE 每条自监控样本 SHALL 携带标签 `component`，取值为 `gse-server`、`dataserver`、`console` 之一；THE 每条自监控样本 SHALL 携带标签 `instance`，取值为该进程业务 HTTP 监听地址。
### Requirement 4: 进程运行时指标

- AS 运维人员, I want 看到每个服务端组件自身的 CPU、内存与存活时长, so that 能判断进程是否过载或刚重启。
- 验收：THE 服务端组件 SHALL 暴露 `vectorman_process_cpu_seconds_total`，值为该进程累计 CPU 秒；THE 服务端组件 SHALL 暴露 `vectorman_process_resident_memory_bytes`，值为该进程 RSS 字节数。
### Requirement 5: HTTP 请求指标

- AS 运维人员, I want 看到每个服务端组件的 HTTP 请求量与耗时, so that 能定位接口变慢或错误率升高。
- 验收：THE 服务端组件 SHALL 暴露计数器 `vectorman_http_requests_total`，标签含 `method`、`path`、`status`；THE `path` 标签 SHALL 使用路由模板（例如 `/api/gse/agents/{agent_id}`）。
### Requirement 6: 组件业务量指标

- AS 运维人员, I want 看到各组件特有的业务量, so that 自监控能反映控制面会话与数据面接入是否正常。
- 验收：THE `gse-server` SHALL 暴露 `vectorman_gse_agents_online`，值为当前台账状态为 `online` 的 Agent 数量；THE `gse-server` SHALL 暴露 `vectorman_gse_sessions`，值为当前内存会话数。
### Requirement 7: dataserver 写入自有时序存储

- AS 平台运维, I want dataserver 把本进程自监控样本写入自有时序存储, so that 无需外部 Prometheus 也能用现有查询口回看数据面健康。
- 验收：THE `dataserver` SHALL 按落盘周期将本进程自监控样本写入本进程时序存储；THE 写入的 measurement 名称 SHALL 与普罗指标名一致。
### Requirement 8: 用现有 PromQL 查询 dataserver 自监控

- AS 查询调用方, I want 用现有 Prometheus 查询 HTTP 检索 dataserver 自监控, so that 控制台与 `dpc query` 无需新协议。
- 验收：WHEN 调用方对 dataserver 执行 `GET /api/v1/query?query=vectorman_process_uptime_seconds`, THE dataserver SHALL 返回已落盘的本进程自监控即时值；WHEN 调用方对 dataserver 执行 `GET /api/v1/query_range` 且表达式为自监控指标名, THE dataserver SHALL 返回本进程自监控在对应时间范围内的样本。
### Requirement 9: 与主机采集隔离

- AS 查询调用方, I want 组件自监控与 Agent 主机采集使用不同指标名, so that 查询不会把进程 RSS 和主机 mem_usage 混在一起。
- 验收：THE 自监控指标名 SHALL 使用前缀 `vectorman_`；THE 现有主机采集指标名 `cpu_usage` 与 `mem_usage` SHALL 保持不变。
### Requirement 10: 配置

- AS 部署人员, I want 用配置控制普罗接口监听与 dataserver 落盘周期, so that 不同环境可以改端口或改周期。
- 验收：THE 每个服务端组件 SHALL 提供配置项 `metrics_listen`，绑定独立 metrics HTTP 口，路径固定为 `GET /metrics`；THE `gse-server` `metrics_listen` 缺省 SHALL 为 `127.0.0.1:7102`。
### Requirement 11: 故障隔离

- AS 平台运维, I want 自监控失败不影响业务, so that 抓取或落盘异常不会拖垮调度与接入。
- 验收：IF 进程运行时采样失败, THE 服务端组件 SHALL 跳过本轮该指标并继续提供业务 HTTP 与普罗指标接口；IF `dataserver` 时序写入失败, THE `dataserver` SHALL 记录错误并在下一落盘周期重试，业务接口保持可用。
### Requirement 12: 测试

- AS 开发者, I want 自监控有自动化测试, so that 接口格式与落盘路径可回归。
- 验收：THE 每个服务端组件 SHALL 具备测试：请求普罗指标接口返回 200，正文含 `vectorman_process_uptime_seconds` 与标签 `component`；THE `dataserver` SHALL 具备测试：自监控落盘后 `query` 能命中 `vectorman_process_uptime_seconds{component="dataserver"}`。
