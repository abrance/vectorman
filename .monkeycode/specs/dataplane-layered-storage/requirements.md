# Requirements Document

## Introduction

dataplane 是一套可嵌入的本地数据平面。系统将文件、时序、日志检索、KV 与关系型五类存储抽象为稳定接口，v1 使用本地引擎实现，后续可将同一接口接到 MySQL/PostgreSQL、Redis、Elasticsearch/Doris、InfluxDB/VictoriaMetrics。

v1 交付两个二进制：`apiserver` 对外提供裸 SQL HTTP 与 Prometheus 查询 HTTP；`dpc` 作为运维命令行，通过访问 apiserver 完成健康检查与简单查询。KV、文件、日志检索与时序写入在 v1 以 Rust 嵌入接口提供，不对外暴露 Redis/S3/ES/Influx 网络协议。

## Glossary

- **dataplane**：本仓库与产品名称。
- **apiserver**：对外提供网络接口的二进制进程。
- **dpc**：命令行二进制，通过 HTTP 访问 apiserver。
- **数据目录**：用户配置的单个文件或单个目录，承载全部本地引擎数据。
- **存储接口**：五类存储的 Rust async trait（`FileStore`、`TimeSeriesStore`、`LogStore`、`KvStore`、`RelationalStore`）。
- **本地引擎**：v1 绑定的实现：本地操作系统目录、tsink、tantivy+jieba、redb、sqlite。
- **远程适配器**：后续用于对接外部数据库的实现；v1 仅保留接口与占位 crate。
- **SQL HTTP**：apiserver 上执行 SQL 的 JSON HTTP 接口。
- **Prom 查询 HTTP**：形状对齐 Prometheus 的 `/api/v1/query` 与 `/api/v1/query_range`。
- **鉴权中间件**：可插拔的请求鉴权层；v1 默认关闭。
- **嵌入调用方**：依赖 dataplane 存储 crate、在同一进程内调用存储接口的 Rust 程序。

## Requirements

### Requirement 1: 仓库与二进制形态

**User Story:** AS 开发者, I want 一个分层的 Rust workspace 与两个二进制, so that 存储实现与网络入口可以独立演进。

#### Acceptance Criteria

1. THE dataplane 仓库 SHALL 使用 Cargo workspace 组织 crates。
2. THE workspace SHALL 提供名为 `apiserver` 的二进制 crate。
3. THE workspace SHALL 提供名为 `dpc` 的二进制 crate。
4. THE workspace SHALL 将五类存储接口与本地引擎实现放在可被外部 Rust 项目依赖的 library crates 中。
5. WHEN 构建 workspace，THE 构建系统 SHALL 产出 `apiserver` 与 `dpc` 两个可执行文件。

### Requirement 2: 五类存储接口

**User Story:** AS 嵌入调用方, I want 五类统一的异步存储接口, so that 业务代码与具体引擎解耦。

#### Acceptance Criteria

1. THE dataplane SHALL 提供 `FileStore`、`TimeSeriesStore`、`LogStore`、`KvStore`、`RelationalStore` 五个 async trait。
2. THE 存储接口的方法 SHALL 使用 tokio 异步签名。
3. WHEN 本地引擎底层为同步库，THE 对应实现 SHALL 在 tokio 的 blocking 线程中执行同步调用。
4. THE 每个存储接口 SHALL 使用本文件 Glossary 中的名称，并在 crate 文档中给出同一术语。

### Requirement 3: 本地引擎绑定

**User Story:** AS 嵌入调用方, I want v1 有可运行的本地实现, so that 无需外部数据库即可落盘。

#### Acceptance Criteria

1. THE `FileStore` 本地实现 SHALL 将对象写入配置指定的操作系统目录。
2. THE `TimeSeriesStore` 本地实现 SHALL 使用 tsink 持久化数据点。
3. THE `LogStore` 本地实现 SHALL 使用 tantivy 与 jieba 建立索引并检索。
4. THE `KvStore` 本地实现 SHALL 使用 redb 持久化键值。
5. THE `RelationalStore` 本地实现 SHALL 使用 sqlite 执行 SQL。
6. THE workspace SHALL 为每个目标产品提供独立适配 crate：`dataplane-adapter-postgres`、`dataplane-adapter-mysql`、`dataplane-adapter-redis`、`dataplane-adapter-elasticsearch`、`dataplane-adapter-doris`、`dataplane-adapter-influxdb`、`dataplane-adapter-victoriametrics`。
7. WHEN 构建 workspace，THE 上述适配 crate SHALL 编译通过；其存储方法在 v1 返回统一的 `unimplemented` 错误码。

### Requirement 4: 数据目录

**User Story:** AS 运维人员, I want 用一个可配置路径存放全部本地数据, so that 备份与迁移只需处理这一处。

#### Acceptance Criteria

1. THE apiserver SHALL 从配置读取单个数据路径，该路径为文件或目录二者之一。
2. WHEN 配置的数据路径为目录且目录不存在，THE apiserver SHALL 在启动时创建该目录。
3. WHEN 配置的数据路径无法打开或无法创建，THE apiserver SHALL 以非零退出码退出，并向标准错误输出一条包含该路径的错误信息。
4. THE 五类本地引擎 SHALL 把各自数据放在同一数据路径之下（目录内子路径，或配置指定的单个文件，按引擎需要选择）。

### Requirement 5: 文件存储接口

**User Story:** AS 嵌入调用方, I want 按相对路径读写对象, so that 可以在本地目录上模拟对象存储。

#### Acceptance Criteria

1. THE `FileStore` SHALL 提供 `put`、`get`、`delete`、`head`、`list` 五个异步方法。
2. WHEN 调用方执行 `put`，THE `FileStore` SHALL 按相对路径写入字节内容，并保存 content-type 与 size。
3. WHEN 调用方执行 `get` 且对象存在，THE `FileStore` SHALL 返回字节内容、content-type 与 size。
4. WHEN 调用方执行 `head` 且对象存在，THE `FileStore` SHALL 返回 content-type 与 size，且不要求返回完整正文。
5. WHEN 调用方执行 `list`，THE `FileStore` SHALL 返回指定前缀下的相对路径列表。
6. IF 调用方对不存在的路径执行 `get` 或 `head` 或 `delete`，THE `FileStore` SHALL 返回可区分的“对象不存在”错误。

### Requirement 6: KV 存储接口

**User Story:** AS 嵌入调用方, I want 字节键值的高频读写, so that 本地缓存与元数据可以落在 redb 上。

#### Acceptance Criteria

1. THE `KvStore` SHALL 提供 `get`、`set`、`delete`、`exists`、`scan_prefix` 五个异步方法。
2. THE `KvStore` 的键与值 SHALL 使用字节序列。
3. WHEN 调用方执行 `set`，THE `KvStore` SHALL 覆盖写入该键。
4. WHEN 调用方执行 `scan_prefix`，THE `KvStore` SHALL 返回键以给定前缀开头的键值对。
5. IF 调用方对不存在的键执行 `get`，THE `KvStore` SHALL 返回可区分的“键不存在”错误。

### Requirement 7: 关系型存储接口与 SQL HTTP

**User Story:** AS API 调用方, I want 用 JSON HTTP 执行 SQL, so that 可以用任意 HTTP 客户端查询 sqlite。

#### Acceptance Criteria

1. THE `RelationalStore` SHALL 提供执行单条 SQL 语句并返回列名与行数据的异步方法。
2. THE apiserver SHALL 在 SQL HTTP 端口提供 `POST /v1/sql`。
3. WHEN 客户端向 `POST /v1/sql` 发送 JSON 体 `{"sql":"<statement>","params":[<values>]}`，THE apiserver SHALL 执行该单条语句。
4. WHEN SQL 执行成功，THE apiserver SHALL 返回 JSON，包含 `columns` 数组与 `rows` 数组。
5. WHEN SQL 执行失败，THE apiserver SHALL 返回 JSON 对象，字段为 `error` 与 `code`。
6. IF 请求体缺少 `sql` 字段或 JSON 无法解析，THE apiserver SHALL 返回 HTTP 400 与包含 `error`、`code` 的 JSON 体。

### Requirement 8: 时序存储与 Prom 查询

**User Story:** AS 嵌入调用方与查询客户端, I want 按 Influx 形状写入、按 Prometheus 形状查询, so that 写入模型与 Prom 查询 API 可以同时成立。

#### Acceptance Criteria

1. THE `TimeSeriesStore` 写入方法 SHALL 接受 measurement 名、tags、fields 与时间戳。
2. WHEN v1 写入一个数据点，THE 调用方 SHALL 为该数据点提供恰好一个数值型 field。
3. THE apiserver SHALL 在 Prom 查询端口提供 `GET /api/v1/query` 与 `GET /api/v1/query_range`。
4. WHEN 查询成功，THE apiserver SHALL 返回与 Prometheus HTTP API 相同的 JSON 顶层字段：`status` 与 `data`。
5. THE Prom 查询语言子集 SHALL 支持：指标选择器、label matcher、时间范围，以及 `sum`、`avg`、`max`、`min` 与 `by` 子句。
6. IF 查询包含尚未实现的函数（例如 `rate`），THE apiserver SHALL 返回 `status` 为 `error` 的 JSON，并在错误信息中包含该函数名。
7. THE 查询层 SHALL 将 measurement 映射为指标名，将 tags 映射为 labels，将该数据点的唯一数值 field 映射为样本值。

### Requirement 9: 日志检索接口

**User Story:** AS 嵌入调用方, I want 按时间、级别、关键词和标签检索日志, so that 后续可以接到 Elasticsearch 而不改调用方式。

#### Acceptance Criteria

1. THE `LogStore` 的一条日志记录 SHALL 包含字段：`id`、`timestamp`、`level`、`message`、`labels`（字符串到字符串的映射）。
2. THE `LogStore` SHALL 对 `message` 使用 jieba 分词后写入 tantivy 索引。
3. THE `LogStore` SHALL 提供写入日志记录的异步方法。
4. THE `LogStore` SHALL 提供查询方法，过滤条件包括时间范围、level、message 关键词与 labels。
5. WHEN 查询命中记录，THE `LogStore` SHALL 返回包含上述五个字段的记录列表。

### Requirement 10: apiserver 进程与端口

**User Story:** AS 运维人员, I want 单进程多端口, so that SQL 与 Prom 查询可以分别对接现有客户端。

#### Acceptance Criteria

1. THE apiserver SHALL 在同一进程内监听 SQL HTTP 端口与 Prom 查询 HTTP 端口。
2. THE 两个监听地址 SHALL 从 TOML 配置读取，并允许被环境变量覆盖。
3. THE SQL HTTP 的默认监听地址 SHALL 为 `0.0.0.0:8081`。
4. THE Prom 查询 HTTP 的默认监听地址 SHALL 为 `0.0.0.0:9090`。
5. THE SQL HTTP 端口 SHALL 提供 `GET /health`。
6. THE Prom 查询 HTTP 端口 SHALL 提供 `GET /health`。
7. WHEN `GET /health` 被调用且进程可接受请求，THE apiserver SHALL 返回 HTTP 200 与 JSON 体，字段包含 `status`，值为 `ok`。
8. THE apiserver SHALL 加载鉴权中间件，并在 v1 默认配置下跳过鉴权校验、直接进入业务处理。

### Requirement 11: 配置

**User Story:** AS 运维人员, I want TOML 配置与环境变量覆盖, so that 同一套二进制可以在不同环境启动。

#### Acceptance Criteria

1. THE apiserver SHALL 默认读取当前工作目录下的 `config.toml`。
2. WHEN 启动参数提供 `--config <path>`，THE apiserver SHALL 读取该路径的 TOML 文件。
3. THE 配置文件 SHALL 包含：数据路径、SQL HTTP 监听地址与端口、Prom 查询 HTTP 监听地址与端口。
4. WHEN 存在对应环境变量，THE apiserver SHALL 用环境变量值覆盖 TOML 中的同名项。环境变量前缀为 `DP_`。
5. WHEN 配置文件缺失或 TOML 无法解析，THE apiserver SHALL 以非零退出码退出，并向标准错误输出错误原因。

### Requirement 12: dpc 运维 CLI

**User Story:** AS 运维人员, I want 用 dpc 探测 apiserver, so that 不必手写 curl 即可做健康检查与简单查询。

#### Acceptance Criteria

1. THE dpc SHALL 通过 HTTP 访问 apiserver，不在本进程内直接打开数据路径。
2. THE dpc SHALL 提供 `health` 子命令，分别请求 SQL HTTP 端口与 Prom 查询端口的 `GET /health`，并在标准输出打印两个端口的结果。
3. THE dpc SHALL 提供 `sql` 子命令，向 `POST /v1/sql` 发送一条语句并打印 JSON 响应。
4. THE dpc SHALL 提供 `query` 子命令，向 Prom `GET /api/v1/query` 发送查询并打印 JSON 响应。
5. THE dpc SHALL 通过 `--sql-url` 与 `--prom-url` 指定两个端口的基址。
6. IF apiserver 不可达，THE dpc SHALL 以非零退出码退出，并向标准错误输出目标 URL 与失败原因。

### Requirement 13: v1 网络边界

**User Story:** AS 开发者, I want v1 明确哪些协议不开放, so that 仓库骨架与实现范围一致。

#### Acceptance Criteria

1. THE v1 apiserver SHALL 对外提供 SQL HTTP 与 Prom 查询 HTTP 两类网络接口。
2. THE v1 的 `KvStore`、`FileStore`、`LogStore` 以及时序写入 SHALL 以 Rust 嵌入接口提供给同一进程调用方。
3. THE workspace 文档 SHALL 列出后续可增加的网络协议：Redis RESP、S3 兼容 HTTP、Elasticsearch REST、Influx 写入 HTTP。

### Requirement 14: 错误与可观测性

**User Story:** AS 运维人员, I want 启动失败和接口错误有明确输出, so that 可以定位配置与查询问题。

#### Acceptance Criteria

1. WHEN 任一本地引擎在启动阶段初始化失败，THE apiserver SHALL 以非零退出码退出，并向标准错误输出引擎名称与失败原因。
2. THE SQL HTTP 与 Prom 查询 HTTP 的错误响应 SHALL 使用 JSON 体，并包含机器可读的 `code` 字段。
3. THE apiserver SHALL 将监听地址与端口在启动成功后写入标准输出一行日志。
