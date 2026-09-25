# vectorman

可自托管的 aiops 平台：**采集 + 数据平面 + 查询 + 界面**闭环。采集端（GSE Agent/Server）负责纳管与取数，
数据平面（dataserver）负责落库与查询，前端（dataplane / console）负责查看与配置。

不绑定特定 aiops 生态：五类存储抽象为稳定接口（文件 / 时序 / 日志检索 / KV / 关系型），v1 用本地引擎实现，
后续可把同一接口接到 Redis、S3、Elasticsearch/Doris、InfluxDB/VictoriaMetrics、MySQL/PostgreSQL。

## 能力清单

| 能力 | 说明 | 界面 |
| --- | --- | --- |
| 主机指标 | 采集项 `metrics_host`，落时序库，Prometheus 查询协议（selector、范围、`sum/avg/max/min` + `by`） | `/metrics`（含自定义 PromQL） |
| 日志检索 | 采集项 `log_file` / `log_k8s_stdout`，tantivy + jieba 全文索引，支持过滤与关键词 | `/logs` |
| trace | 采集项 `apm_otlp`（OTLP/HTTP 经 Agent 中转）或 eBPF 推导，span 摘要 + 明细 + 配对成边 | `/traces`、`/traces/:id`（瀑布） |
| 服务拓扑 | 边指标按 `(桶, 源服务, 目标服务)` 聚合，eBPF 与 OTLP 两路可分开看或合并 | `/topology` |
| APM 指标 | 服务 RED（请求/错误/耗时）与端点表 | `/apm` |
| eBPF 可观测 | 采集项 `ebpf_network` / `ebpf_tcp` / `ebpf_process` / `ebpf_syscall`：连接边、TCP 异常、进程生命周期、文件与 syscall 延迟、慢调用原始事件 | `/ebpf` |
| 服务名归一 | 静态映射（进程名/前缀/Pod 前缀/CIDR）→ 端点表 → `unknown-<ip>`；eBPF 与 APM 共用 | `/settings`（含别名 CRUD） |
| 采集链路 | Agent 注册与心跳、采集项下发/启停、流索引、eBPF 能力状态 | `/` |
| 作业与台账 | Host/Agent/AccessPoint/DataPlane 台账、作业提交/查询/重做、模板、文件传输 | console：`/hosts` `/agents` `/jobs` … |
| 运维 CLI | `dpc`（dataserver 全接口）、`vmctl`（gse-server 台账与作业） | — |

## 组件

```text
bins/dataserver   数据平面：接入 + 五类存储 + 查询（SQL HTTP / Prom HTTP / 自监控 HTTP）
bins/dpc          dataserver 运维命令行（health/sql/query/logs/ts/traces/edges/ebpf-events/…）
bins/gse-server   GSE 调度端：Agent 会话、心跳、采集项下发、作业与台账（RPC + HTTP 管理口）
bins/gse-agent    GSE 执行端：部署在被观测机器，主动外连，执行采集项与作业
bins/vmctl        gse-server 的 HTTP 客户端 CLI
bins/console      桌面门户（聚合其它组件入口）

frontend/apps/dataplane   数据平面界面（采集链路/指标/日志/trace/拓扑/APM/eBPF/设置）
frontend/apps/console     管控界面（主机/Agent/接入点/Agent 配置/作业/模板）
frontend/apps/{job,node,desktop}  其它门户入口

crates/dataplane-*     五类存储与接入（core/file/kv/sql/ts/log/ingest/apm + adapter-* 占位）
crates/gse-*           GSE 协议、Server/Agent 核心与 eBPF 用户态（gse-agent-ebpf）
crates/ebpf-abi        eBPF 内核态与用户态共享 ABI（no_std）
crates/gse-ebpf-programs  eBPF 内核态程序（独立工作区外，nightly + bpf-linker 构建）
crates/vectorman-*     版本号与自监控指标
```

## 快速开始（本地体验）

```bash
# 1) 数据平面
cargo build --workspace
./target/debug/dataserver --config config.toml.example   # 或先 cp 成 config.toml 再改
./target/debug/dpc health

# 2) 前端：二选一
#    a. 开发模式（热更新，已配 /v1 代理到 127.0.0.1:8081）
cd frontend && npm ci && npm run dev:dataplane    # → http://localhost:5175
#    b. 单进程托管：构建 dist 后在 dataserver 配置里加 http_web_dir = "<...>/apps/dataplane/dist"
cd frontend && npm run build:dataplane            # → 浏览器打开 http://<host>:8081/

# 3) 取数：起 gse-server + gse-agent，然后在「采集链路」页建采集项
#    （metrics_host / log_file / log_k8s_stdout / apm_otlp / ebpf_*）
```

安装包部署（含 systemd 单元与前端 dist）：

```bash
packaging/build-package.sh                              # 产出 vectorman-<版本>-linux-x86_64.tar.gz
packaging/deploy/install.sh all --dest /opt/vectorman --with-systemd
/opt/vectorman/deploy/ctl.sh dataserver start   # 单组件：start|stop|status|restart
```

服务端默认端口：dataserver `8081`（SQL/接入/UI）、`9090`（Prom 查询）、`9091`（自监控）；
gse-server `7100`（RPC）、`7101`（台账 HTTP）、`7102`（自监控）。

## 接口速查

| 组件 | 路由 |
| --- | --- |
| dataserver | `POST /v1/ingest`（采集接入）、`POST /v1/sql`、`POST /v1/logs/search`、`POST /v1/traces/search`、`GET /v1/traces/{id}`、`POST /v1/edges/search`、`POST /v1/ebpf/events/search`、`GET /v1/ebpf/capability`、`GET /v1/streams`、`GET|POST /v1/apm/service-aliases`、`GET /v1/apm/services`、`GET /v1/ts/stats`、`POST /v1/ts/delete`、`/health` |
| dataserver（Prom） | `GET /api/v1/query`、`GET /api/v1/query_range` |
| gse-server | `/api/gse/{hosts,agents,access-points,dataplanes,collect-items,agent-configs,jobs,job-templates,job-files}`、`/health` |

`/v1/collect-items*` 在 dataserver 上是**反代到 GSE 管理口**（需 `gse_admin_url`）。
鉴权中间件已装配，v1 默认关闭（`NoopAuth`）。

## eBPF 可观测的前置条件

- Linux；内核 **≥ 5.8**；可读 `/sys/kernel/btf/vmlinux`（内核带 BTF）；
- 以 **root**（或带 `CAP_BPF` + `CAP_PERFMON`）运行 gse-agent；
- 内核态程序不硬编码内核结构体偏移：偏移来自 BTF 与 tracepoint `format`，**任一项取不到即该项不采集**（不按猜测值跑）；
- 不满足时**只降级 eBPF 本身**：Agent 上报 `agent_ebpf_capability`（含原因），其它采集项照常工作，前端 `/ebpf` 页会显示原因。

## 已知限制

- **DNS 延迟**（`ebpf_dns`）与 **CPU profile / 火焰图**（`ebpf_cpu_profile`）尚未实现；
- 采集侧目前只覆盖 **IPv4**；`ebpf_process` 取 16 字节进程名（不发完整 `cmdline`）；
- syscall 采集里**只有 `openat` 带路径**（read/write/fsync 没有路径来源），路径仅出现在慢调用事件里且截断 256 字节；
- 时序聚合指标的删除周期配置有**下限 60 秒**；
- 跨进程并发写同一 `data_path` 不做文件锁，由调用方保证；
- 前端没有真实浏览器 e2e：现有验证是 jsdom 渲染 + 真实接口（能挡住字段漂移，覆盖不到 CSS/布局）。

## 数据路径

`data_path` 为目录时：

| 引擎 | 相对位置 |
| --- | --- |
| 文件 | `{data_path}/files/` |
| 时序 | `{data_path}/ts/` |
| 日志 | `{data_path}/logs/` |
| KV | `{data_path}/kv.redb` |
| SQL | `{data_path}/sql.sqlite` |

## 规格与设计文档

- 数据平面分层存储：`.monkeycode/specs/dataplane-layered-storage/`
- 时序保留策略：`.monkeycode/specs/dataplane-ts-retention/`
- 可观测数据模型：`.monkeycode/specs/observability-data-model/`
- APM（trace/拓扑/APM 指标）：`.monkeycode/specs/apm-tracing/`
- eBPF 可观测：`.monkeycode/specs/ebpf-observability/`（含 `todo.md`：遗留项与已知边界）
- GSE 采集与数据面接入：`.monkeycode/specs/gse-dataplane-ingest/`
- 中文介绍文档：`docs/vectorman概述.md`、`docs/gse能力介绍.md`
