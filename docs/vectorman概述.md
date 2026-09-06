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
| `bins/gse-server` | GSE 全局调度引擎调度端：Agent 会话管理与信令上下行通道 |
| `bins/gse-agent` | GSE 执行端：部署在目标机器，主动外连 Server，接收指令并执行 |

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

### 5.4 全局调度引擎（GSE）

组件化拆分，至少含 Server 与 Agent 两个二进制；服务端即使拆多个进程，也优先共用同一二进制通过子命令区分。传输层基于 geminio-rs（singchia/geminio 的 Rust 移植）反向隧道，单连接承载双向 RPC。

**已实现（v0.1 最小闭环 + 台账）：**

- `bins/gse-server` + `crates/gse-server-core`：geminio `EndListener` 监听、Agent 主动外连接入
  - 认证：查库认证——agent-id + token 对预登记的 `agents` 台账表校验（配置 `[agents]` 静态表已废弃），失败拒绝且 Agent 停止重连
  - 心跳与存活：heartbeat handler 刷新 last_seen 并回写台账 `last_heartbeat_at`，超时窗口内无消息判离线（Online→Checking→Offline）并同步置台账 `offline`
  - 会话管理：内存注册表，agent-id 至多一个活跃会话
  - 信令下发：`send_command` 经下行 `exec` RPC，RPC 返回即回执；目标离线返回 `unavailable`
  - 资产台账（CMDB）：sqlite 持久化 hosts / access_points / agents / agent_configs 四表，启动自动建表并自登记接入点；提供 HTTP 管理端口（默认 `127.0.0.1:7101`）与同进程 `Ledger` API 做增删改查，删除 Agent 级联清理配置与会话
- `bins/gse-agent` + `crates/gse-agent-core`：dial 外连、指数退避重连（1–60s）、认证失败停止重连并以非零退出码结束、周期心跳、`exec` handler（ping/pong 验证链路）
- `crates/gse-proto`：两端共享 DTO（auth / heartbeat / command / receipt / error）
- 配置：TOML 文件 + `GSE_` 前缀环境变量覆盖；`db`/`http_enabled`/`http_listen` 支持 `GSE_SERVER_DB`/`GSE_SERVER_HTTP_LISTEN` 覆盖；缺失或非法输出 `config_invalid` 并以退出码 1 结束
- 集成测试覆盖认证（含未登记与 auth 关闭直通）、心跳、ping/pong、未知指令、台帐状态回写（online/heartbeat/offline）、HTTP 管理 CRUD、删除级联、自登记幂等

**规划中模块：**

| 组件名称 | 所属模块 | 角色定位 | 设计目标 |
| --- | --- | --- | --- |
| GSE Task（任务服务） | GSE 核心 | 任务执行 | 提供远程命令的编排、下发与结果回收能力 |
| GSE File（文件服务） | GSE 核心 | 文件传输 | 提供大文件在 Server 与 Agent 之间的高效分发与下载能力 |
| GSE Proc（进程管理服务） | GSE 核心 | 进程托管 | 对 Agent 机器上的进程进行托管式生命周期管理 |
| GSE Data（数据服务） | GSE 核心 | 数据传输 | 提供海量运维采集数据的全链路传输、路由分发与管道管理 |
| GSE Data Server | GSE Data | 数据路由引擎 | 维护 data_id 路由表，将采集数据精准投递到各消费方 |
| Proxy | GSE 管控 | 非直连区域桥梁 | 在网络隔离场景中充当 Server 与 Agent 之间的中转节点 |
| p-agent（非直连 Agent） | GSE 管控 | 非直连执行端 | 部署在非直连区域目标机器上，通过 Proxy 中转与 Server 通信 |