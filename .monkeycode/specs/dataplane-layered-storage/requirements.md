# Requirements Document

> **已归档**（2026-09-26 文档整理）：本 feature 已实现并合入 main，本文件压缩为需求索引——完整 EARS 条款见 git 历史（本文件重写前的最后一个版本），design.md 全文保留为实施细节档案。

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

- AS 开发者, I want 一个分层的 Rust workspace 与两个二进制, so that 存储实现与网络入口可以独立演进。
- 验收：THE dataplane 仓库 SHALL 使用 Cargo workspace 组织 crates；THE workspace SHALL 提供名为 `apiserver` 的二进制 crate。
### Requirement 2: 五类存储接口

- AS 嵌入调用方, I want 五类统一的异步存储接口, so that 业务代码与具体引擎解耦。
- 验收：THE dataplane SHALL 提供 `FileStore`、`TimeSeriesStore`、`LogStore`、`KvStore`、`RelationalStore` 五个 async trait；THE 存储接口的方法 SHALL 使用 tokio 异步签名。
### Requirement 3: 本地引擎绑定

- AS 嵌入调用方, I want v1 有可运行的本地实现, so that 无需外部数据库即可落盘。
- 验收：THE `FileStore` 本地实现 SHALL 将对象写入配置指定的操作系统目录；THE `TimeSeriesStore` 本地实现 SHALL 使用 tsink 持久化数据点。
### Requirement 4: 数据目录

- AS 运维人员, I want 用一个可配置路径存放全部本地数据, so that 备份与迁移只需处理这一处。
- 验收：THE apiserver SHALL 从配置读取单个数据路径，该路径为文件或目录二者之一；WHEN 配置的数据路径为目录且目录不存在，THE apiserver SHALL 在启动时创建该目录。
### Requirement 5: 文件存储接口

- AS 嵌入调用方, I want 按相对路径读写对象, so that 可以在本地目录上模拟对象存储。
- 验收：THE `FileStore` SHALL 提供 `put`、`get`、`delete`、`head`、`list` 五个异步方法；WHEN 调用方执行 `put`，THE `FileStore` SHALL 按相对路径写入字节内容，并保存 content-type 与 size。
### Requirement 6: KV 存储接口

- AS 嵌入调用方, I want 字节键值的高频读写, so that 本地缓存与元数据可以落在 redb 上。
- 验收：THE `KvStore` SHALL 提供 `get`、`set`、`delete`、`exists`、`scan_prefix` 五个异步方法；THE `KvStore` 的键与值 SHALL 使用字节序列。
### Requirement 7: 关系型存储接口与 SQL HTTP

- AS API 调用方, I want 用 JSON HTTP 执行 SQL, so that 可以用任意 HTTP 客户端查询 sqlite。
- 验收：THE `RelationalStore` SHALL 提供执行单条 SQL 语句并返回列名与行数据的异步方法；THE apiserver SHALL 在 SQL HTTP 端口提供 `POST /v1/sql`。
### Requirement 8: 时序存储与 Prom 查询

- AS 嵌入调用方与查询客户端, I want 按 Influx 形状写入、按 Prometheus 形状查询, so that 写入模型与 Prom 查询 API 可以同时成立。
- 验收：THE `TimeSeriesStore` 写入方法 SHALL 接受 measurement 名、tags、fields 与时间戳；WHEN v1 写入一个数据点，THE 调用方 SHALL 为该数据点提供恰好一个数值型 field。
### Requirement 9: 日志检索接口

- AS 嵌入调用方, I want 按时间、级别、关键词和标签检索日志, so that 后续可以接到 Elasticsearch 而不改调用方式。
- 验收：THE `LogStore` 的一条日志记录 SHALL 包含字段：`id`、`timestamp`、`level`、`message`、`labels`（字符串到字符串的映射）；THE `LogStore` SHALL 对 `message` 使用 jieba 分词后写入 tantivy 索引。
### Requirement 10: apiserver 进程与端口

- AS 运维人员, I want 单进程多端口, so that SQL 与 Prom 查询可以分别对接现有客户端。
- 验收：THE apiserver SHALL 在同一进程内监听 SQL HTTP 端口与 Prom 查询 HTTP 端口；THE 两个监听地址 SHALL 从 TOML 配置读取，并允许被环境变量覆盖。
### Requirement 11: 配置

- AS 运维人员, I want TOML 配置与环境变量覆盖, so that 同一套二进制可以在不同环境启动。
- 验收：THE apiserver SHALL 默认读取当前工作目录下的 `config.toml`；WHEN 启动参数提供 `--config <path>`，THE apiserver SHALL 读取该路径的 TOML 文件。
### Requirement 12: dpc 运维 CLI

- AS 运维人员, I want 用 dpc 探测 apiserver, so that 不必手写 curl 即可做健康检查与简单查询。
- 验收：THE dpc SHALL 通过 HTTP 访问 apiserver，不在本进程内直接打开数据路径；THE dpc SHALL 提供 `health` 子命令，分别请求 SQL HTTP 端口与 Prom 查询端口的 `GET /health`，并在标准输出打印两个端口的结果。
### Requirement 13: v1 网络边界

- AS 开发者, I want v1 明确哪些协议不开放, so that 仓库骨架与实现范围一致。
- 验收：THE v1 apiserver SHALL 对外提供 SQL HTTP 与 Prom 查询 HTTP 两类网络接口；THE v1 的 `KvStore`、`FileStore`、`LogStore` 以及时序写入 SHALL 以 Rust 嵌入接口提供给同一进程调用方。
### Requirement 14: 错误与可观测性

- AS 运维人员, I want 启动失败和接口错误有明确输出, so that 可以定位配置与查询问题。
- 验收：WHEN 任一本地引擎在启动阶段初始化失败，THE apiserver SHALL 以非零退出码退出，并向标准错误输出引擎名称与失败原因；THE SQL HTTP 与 Prom 查询 HTTP 的错误响应 SHALL 使用 JSON 体，并包含机器可读的 `code` 字段。
