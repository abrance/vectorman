# dataplane

可嵌入的本地数据平面。文件、时序、日志检索、KV 与关系型五类存储被抽象为稳定接口，v1 用本地引擎实现；后续可将同一接口接到 Redis、S3、Elasticsearch/Doris、InfluxDB/VictoriaMetrics、MySQL/PostgreSQL。

## 组件

```text
bins/apiserver   对外提供 SQL HTTP 与 Prometheus 查询 HTTP（单进程两端口）
bins/dpc         运维命令行，仅通过 HTTP 访问 apiserver
crates/dataplane-core       错误码、配置、数据路径、鉴权 trait、SQL 类型
crates/dataplane-file       FileStore + 本地目录引擎
crates/dataplane-kv         KvStore + redb 引擎
crates/dataplane-sql        RelationalStore + sqlite 引擎
crates/dataplane-ts         TimeSeriesStore + tsink 引擎 + Prom 投影
crates/dataplane-log        LogStore + tantivy/jieba 引擎
crates/dataplane-adapter-*  v1 占位适配器（postgres/mysql/redis/elasticsearch/doris/influxdb/victoriametrics）
```

## v1 开放接口

`apiserver` 默认监听两个端口（`config.toml` 或 `DP_*` 环境变量可改，见 `config.toml.example`）：

| 端口 | 协议 | 路由 |
| --- | --- | --- |
| `0.0.0.0:8081` | SQL HTTP | `GET /health`、`POST /v1/sql` |
| `0.0.0.0:9090` | Prometheus 查询 | `GET /health`、`GET /api/v1/query`、`GET /api/v1/query_range` |

- `POST /v1/sql` 请求体 `{"sql":"<statement>","params":[<values>]}`，响应 `{"columns":[],"rows":[]}`。
- Prom 端口 JSON 形状对齐 Prometheus：顶层 `status` 与 `data`；失败时 `status=error` 且带 `errorType`/`error`。查询语言为子集（selector、label matcher、时间范围、`sum/avg/max/min` + `by`），未实现的函数返回 `unimplemented`。
- 鉴权中间件已装配，v1 默认关闭（`NoopAuth`）。

KV、文件、日志检索与时序写入在 v1 通过 Rust 嵌入接口（同一进程内的 storage crate）提供，不对外暴露网络协议。文件检索等外部访问请直接依赖对应 crate。

### 后续可增加的协议

- Redis RESP（对接 `KvStore`）
- S3 兼容 HTTP（新增文件适配器 / 复用 `FileStore`）
- Elasticsearch REST（对接 `LogStore`）
- Influx 写入 HTTP（对接 `TimeSeriesStore`）

## 快速开始

```bash
# 构建
cargo build --workspace

# 启动 apiserver（默认 ./data 目录，勿与其它进程共用）
./target/debug/apiserver

# 健康检查
./target/debug/dpc health

# 执行 SQL
./target/debug/dpc sql --stmt 'SELECT 1'

# Prometheus 即时查询
./target/debug/dpc query --expr 'cpu_usage'
```

## 数据路径

`data_path` 配置为目录时，各引擎数据位于：

| 引擎 | 相对位置 |
| --- | --- |
| 文件 | `{data_path}/files/` |
| 时序 | `{data_path}/ts/` |
| 日志 | `{data_path}/logs/` |
| KV | `{data_path}/kv.redb` |
| SQL | `{data_path}/sql.sqlite` |

单文件数据路径仅支持 sqlite，apiserver 需要目录模式。跨进程并发写同一数据路径 v1 不做文件锁，由调用方保证。

## 规格与实施计划

设计文档见 `.monkeycode/specs/dataplane-layered-storage/`（requirements / design / tasklist）。