# vectorman 概述

## 1. 项目定位

vectorman 是一个 aiops 平台。愿景是不固定绑定某一种 aiops 生态：集成通用的组件，可以让后续项目基于此轻易扩展。

## 2. 当前状态：dataplane v1 + 可观测能力（已实现）

v1 交付了可嵌入的本地数据平面，五类存储被抽象为稳定接口并用本地引擎实现，后续可用同一接口对接外部成熟组件。
在此之上已打通「采集 → 数据平面 → 查询 → 界面」的闭环：主机指标、日志检索、trace/APM、服务拓扑与 eBPF 可观测。

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
| `bins/dataserver` | 对外提供 SQL HTTP 与 Prometheus 查询 HTTP（单进程两端口） |
| `bins/dpc` | 运维命令行，仅通过 HTTP 访问 dataserver（health / sql / query） |
| `bins/vmctl` | gse-server HTTP 客户端：Host/Agent 只读查询，作业提交/查询/重做 |
| `bins/gse-server` | GSE 全局调度引擎调度端：Agent 会话管理与信令上下行通道 |
| `bins/gse-agent` | GSE 执行端：部署在目标机器，主动外连 Server，执行采集项与作业 |
| `bins/console` | 桌面门户：聚合各组件入口与 App 目录 |
| `frontend/apps/dataplane` | 数据平面界面：采集链路 / 指标 / 日志 / trace / 拓扑 / APM / eBPF / 设置 |
| `frontend/apps/console` | 管控界面：主机 / Agent / 接入点 / Agent 配置 / 作业 / 模板 |

全部二进制支持 `--help` / `--version`（`-h` / `-V`），且不依赖配置文件。

`--version` 在编译期注入：release 打包（`packaging/build-package.sh`）写入 tag 版本；本地构建优先 `VECTORMAN_VERSION`，否则 `git describe --tags --always --dirty`。

### 2.3 可观测能力（已实现）

| 能力 | 采集来源 | 落地 |
| --- | --- | --- |
| 主机指标 | 采集项 `metrics_host` | 时序库（Prometheus 查询协议） |
| 日志检索 | `log_file` / `log_k8s_stdout` | tantivy + jieba 全文索引，支持过滤与关键词 |
| trace | `apm_otlp`（OTLP/HTTP 经 Agent 中转）；eBPF 推导的 span（`source=ebpf`） | span 摘要 + 明细 + 配对成服务边 |
| 服务拓扑 | 边指标按 `(桶, 源服务, 目标服务)` 聚合，eBPF 与 OTLP 两路可分可合 | `/topology` 页 |
| APM 指标 | span 摘要 → 服务 RED 与端点表 | `/apm` 页 |
| 服务名归一 | 静态映射（进程名/前缀/Pod 前缀/CIDR）→ 端点表 → `unknown-<ip>` | `/settings` 别名 CRUD |
| eBPF | `ebpf_network`（连接边）/ `ebpf_tcp`（重传·RST）/ `ebpf_process`（生命周期）/ `ebpf_syscall`（文件与 syscall 延迟、慢调用） | 边记录落 sqlite，指标落时序库，慢调用/原始事件落日志库 |

eBPF 的前置条件：Linux + 内核 ≥ 5.8 + BTF 可读 + root（或 `CAP_BPF`+`CAP_PERFMON`）；
不满足时只降级 eBPF 本身，并以 `agent_ebpf_capability` 指标说明原因。内核态偏移全部来自
BTF 与 tracepoint `format`，取不到即该项不采集（不按猜测值跑）。

**尚未实现**：DNS 延迟（`ebpf_dns`）、CPU profile 与火焰图（`ebpf_cpu_profile`）。

### 2.4 演进原则

数据管道 + 数据存储合并在一个组件中承载，避免二进制数量膨胀。

## 3. 组件化设计原则

组件指编译好的二进制。同一产品下的服务尽量不拆成多个二进制，优先采用「单二进制 + 子命令」形式，避免二进制过多。

## 4. 后续目标

组件化基础（通用 crate 沉淀、环境变量读取、运行时设置、每组件独立运行目录、统一目录结构
`bin/` / `scripts/` / `config/`）已在 v1 落地并随发布包交付；后续重点：

- 补齐 eBPF 的 DNS 延迟与 CPU profile（火焰图）；
- 把本地引擎按同一接口接到外部成熟组件（ES/Doris、VictoriaMetrics、Redis、S3、MySQL/PG）；
- 真机集群场景的验证：Kubernetes Pod 名反查、多 Agent 大规模下的限流与容量。

## 5. 规划中的组件

> 2026-09-26 状态校正：cmdb（gse-server-cmdb）、node（@vectorman/node）、job（@vectorman/job）、
> GSE Server/Agent v0.1 最小闭环、文件传输、作业模板均已**实现并合入 main**——实现明细见
> `docs/gse能力介绍.md` 与 `.monkeycode/specs/README.md` 索引，不再在此重复。以下仅列**真正未落地**的规划。

### 5.1 GSE 规划中模块

| 组件名称 | 所属模块 | 角色定位 | 设计目标 |
| --- | --- | --- | --- |
| GSE Task（任务服务） | GSE 核心 | 任务执行 | 提供远程命令的编排、下发与结果回收能力 |
| GSE File（文件服务） | GSE 核心 | 文件传输 | 提供大文件在 Server 与 Agent 之间的高效分发与下载能力 |
| GSE Proc（进程管理服务） | GSE 核心 | 进程托管 | 对 Agent 机器上的进程进行托管式生命周期管理 |
| GSE Data（数据服务） | GSE 核心 | 数据传输 | 提供海量运维采集数据的全链路传输、路由分发与管道管理 |
| GSE Data Server | GSE Data | 数据路由引擎 | 维护 data_id 路由表，将采集数据精准投递到各消费方 |
| Proxy | GSE 管控 | 非直连区域桥梁 | 在网络隔离场景中充当 Server 与 Agent 之间的中转节点 |
| p-agent（非直连 Agent） | GSE 管控 | 非直连执行端 | 部署在非直连区域目标机器上，通过 Proxy 中转与 Server 通信 |
