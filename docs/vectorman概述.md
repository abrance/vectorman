# vectorman 概述

## 1. 项目定位

vectorman 是一个 aiops 平台。愿景是不固定绑定某一种 aiops 生态：集成通用的组件，可以让后续项目基于此轻易扩展。

## 2. 当前状态：dataplane v1（已实现）

v1 交付了可嵌入的本地数据平面，五类存储被抽象为稳定接口并用本地引擎实现，后续可用同一接口对接外部成熟组件。

### 2.1 五类存储与引擎

| 存储接口 | 本地引擎 | 后续可接入 |
| --- | --- | --- |
| 文件存储 `FileStore` | 操作系统目录 | S3 / 对象存储 |
| KV 存储 `KvStore` | redb | Redis |
| 关系型存储 `RelationalStore` | sqlite | MySQL / PostgreSQL |
| 日志检索 `LogStore` | tantivy + jieba | Elasticsearch / Doris |
| 时序存储 `TimeSeriesStore` | tsink | InfluxDB / VictoriaMetrics |

v1 暂不接入 influxdb、es、redis、七牛云 s3、mysql 等成熟组件；对应产品已留占位 adapter crate，方法统一返回 `unimplemented`。

### 2.2 二进制形态

| 二进制 | 职责 |
| --- | --- |
| `bins/apiserver` | 对外提供 SQL HTTP 与 Prometheus 查询 HTTP（单进程两端口） |
| `bins/dpc` | 运维命令行，仅通过 HTTP 访问 apiserver（health / sql / query） |

### 2.3 演进原则

数据管道 + 数据存储合并在一个组件中承载，避免二进制数量膨胀。

## 3. 组件化设计原则

组件指编译好的二进制。同一产品下的服务尽量不拆成多个二进制，优先采用「单二进制 + 子命令」形式，避免二进制过多。

## 4. v1.1 目标

v1.1 的重点是先沉淀一批通用 crate，把「做出组件」的基础能力准备出来：

- 环境变量读取
- 运行时设置
- 每个组件使用关系型数据库时会用到 sqlite，因此每个组件需要独立的运行目录
- 统一目录结构设计：`bin/`、`scripts/`、`config/config.toml` 等

同时整理出第一版基础组件。

## 5. 规划中的组件

### 5.1 cmdb 配置平台

配置管理平台，作为组件基础数据源。

### 5.2 node 节点管理

节点（主机）生命周期与状态管理。

### 5.3 job 作业平台

作业编排与远程执行。建立 ssh 执行作业时使用反向隧道，基于 https://github.com/singchia/geminio-rs 实现 agent 与 server 之间的通信。

以下为完整技术架构的组件拆分，落地时可先实现主要组件：

```
job-frontend
job-gateway
job-analysis
job-backup
job-config-watcher
job-crontab
job-execute
job-file-gateway
job-file-worker-headless
job-logsvr
job-manage
```

### 5.4 全局调度引擎

待拆分的组件清单。目前可确定至少会有 Server 与 Agent 两个二进制组件，是否再拆其他二进制待定；服务端即使拆多个进程，也优先共用同一二进制通过子命令区分。

| 组件名称 | 所属模块 | 角色定位 | 设计目标 |
| --- | --- | --- | --- |
| GSE Cluster（基础平台服务） | GSE 核心 | 底座服务 | 为 GSE 所有上层模块提供统一的 Agent 会话管理与信令上下行通道 |
| GSE Task（任务服务） | GSE 核心 | 任务执行 | 提供远程命令的编排、下发与结果回收能力 |
| GSE File（文件服务） | GSE 核心 | 文件传输 | 提供大文件在 Server 与 Agent 之间的高效分发与下载能力 |
| GSE Proc（进程管理服务） | GSE 核心 | 进程托管 | 对 Agent 机器上的进程进行托管式生命周期管理 |
| GSE Data（数据服务） | GSE 核心 | 数据传输 | 提供海量运维采集数据的全链路传输、路由分发与管道管理 |
| GSE Data Server | GSE Data | 数据路由引擎 | 维护 data_id 路由表，将采集数据精准投递到各消费方 |
| Proxy | GSE 管控 | 非直连区域桥梁 | 在网络隔离场景中充当 Server 与 Agent 之间的中转节点 |
| Agent（直连 Agent） | GSE 管控 | 直连执行端 | 部署在直连区域目标机器上，主动外连 Server，接收指令并执行 |
| p-agent（非直连 Agent） | GSE 管控 | 非直连执行端 | 部署在非直连区域目标机器上，通过 Proxy 中转与 Server 通信 |