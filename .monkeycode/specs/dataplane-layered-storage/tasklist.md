# 需求实施计划

- [x] 1. 初始化 Cargo workspace 与共享核心
  - [x] 1.1 创建 workspace 根 `Cargo.toml`、`crates/`、`bins/` 目录与 `config.toml.example`
    - 成员包含 core、五个引擎 crate、七个 adapter、`bins/apiserver`、`bins/dpc`
    - 对应 Requirement 1、Requirement 3.6、Requirement 11
  - [x] 1.2 实现 `dataplane-core`：`DataplaneError`、错误码、`SqlValue`/`SqlResult`、配置结构与 TOML+`DP_*` 覆盖
    - 包含 `AuthN` trait 与 `NoopAuth`
    - 对应 Requirement 2、Requirement 10.8、Requirement 11、Requirement 14
  - [x] 1.3 实现数据路径解析：目录创建、子路径约定、单文件仅 sqlite 的校验
    - 对应 Requirement 4
  - [ ]* 1.4 为配置覆盖顺序与错误码序列化编写单元测试
    - 对应 Requirement 11.4、Requirement 14.2

- [x] 2. 定义五个 async 存储 trait
  - [x] 2.1 在 `dataplane-file` 定义 `FileStore` 与 `Object`/`ObjectMeta`，拒绝 `..` 与绝对路径
    - 对应 Requirement 5
  - [x] 2.2 在 `dataplane-kv` 定义 `KvStore`（bytes 键值）
    - 对应 Requirement 6
  - [x] 2.3 在 `dataplane-sql` 定义 `RelationalStore::execute`
    - 对应 Requirement 7.1
  - [x] 2.4 在 `dataplane-ts` 定义 `TimeSeriesStore`、`TsPoint`、`PromResult`
    - 单 field 约束在类型或 `write` 入口校验
    - 对应 Requirement 8.1、Requirement 8.2、Requirement 8.7
  - [x] 2.5 在 `dataplane-log` 定义 `LogStore`、`LogRecord`、`LogFilter`
    - 时间戳单位为 Unix 微秒
    - 对应 Requirement 9

- [x] 3. 检查点 - 确保 workspace 可编译且 trait 签名稳定
  - 确保所有测试通过,如有疑问请询问用户

- [x] 4. 实现五个本地引擎
  - [x] 4.1 实现 `FileStore` 本地目录引擎（put/get/head/delete/list）
    - 同步 IO 放进 `spawn_blocking`
    - 对应 Requirement 3.1、Requirement 5
  - [x] 4.2 实现 `KvStore` 的 redb 引擎
    - 对应 Requirement 3.4、Requirement 6
  - [x] 4.3 实现 `RelationalStore` 的 sqlite 引擎（单条 SQL + params）
    - 对应 Requirement 3.5、Requirement 7.1
  - [x] 4.4 实现 `TimeSeriesStore` 的 tsink 写入与 Prom 投影查询
    - PromQL 子集：selector、label matcher、时间范围、sum/avg/max/min + by
    - 未实现函数返回 `unimplemented` 且信息含函数名
    - 对应 Requirement 3.2、Requirement 8
  - [x] 4.5 实现 `LogStore` 的 tantivy+jieba 引擎（append/search）
    - 对应 Requirement 3.3、Requirement 9
  - [ ]* 4.6 为各引擎编写 tempdir 往返测试与路径穿越拒绝测试
    - 对应 FileStore/KvStore/LogStore/TimeSeriesStore/RelationalStore 正确性约束

- [x] 5. 检查点 - 确保本地引擎测试通过
  - 确保所有测试通过,如有疑问请询问用户

- [x] 6. 实现七个 adapter 占位 crate
  - [x] 6.1 为 postgres/mysql/redis/elasticsearch/doris/influxdb/victoriametrics 各建 crate
    - 所有存储方法返回 `code=unimplemented`
    - 对应 Requirement 3.6、Requirement 3.7
  - [ ]* 6.2 为每个 adapter 调用任一方法断言 `unimplemented`
    - 对应 Requirement 3.7

- [x] 7. 实现 apiserver
  - [x] 7.1 实现启动流程：读 `--config`、覆盖环境变量、初始化五引擎、失败则非零退出
    - 成功后 stdout 打印 `sql_http=` 与 `prom_http=`
    - 对应 Requirement 4、Requirement 10、Requirement 11、Requirement 14.1、Requirement 14.3
  - [x] 7.2 用 axum 在 `0.0.0.0:8081` 提供 `GET /health` 与 `POST /v1/sql`
    - JSON 契约与 HTTP 400/422 错误码
    - 对应 Requirement 7、Requirement 10.3、Requirement 10.5、Requirement 10.7
  - [x] 7.3 用 axum 在 `0.0.0.0:9090` 提供 `GET /health`、`/api/v1/query`、`/api/v1/query_range`
    - 成功 JSON 顶层 `status`+`data`；未实现函数 `status=error` 且含函数名
    - 对应 Requirement 8.3-8.6、Requirement 10.4、Requirement 10.6
  - [x] 7.4 接入 `NoopAuth` 中间件，默认跳过鉴权
    - 对应 Requirement 10.8
  - [ ]* 7.5 为 SQL 与 Prom HTTP 编写 httptest
    - 对应 Requirement 7、Requirement 8、Requirement 10

- [x] 8. 实现 dpc
  - [x] 8.1 用 clap 实现 `health`/`sql`/`query` 与 `--sql-url`/`--prom-url`
    - 仅 HTTP 访问 apiserver
    - 对应 Requirement 12、Requirement 13
  - [x] 8.2 实现不可达时非零退出与 stderr `url=`/`reason=`
    - 对应 Requirement 12.6
  - [ ]* 8.3 对 mock HTTP 端口测试三个子命令的退出码
    - 对应 Requirement 12

- [x] 9. 检查点 - 确保 apiserver 与 dpc 可运行
  - 确保所有测试通过,如有疑问请询问用户

- [x] 10. 固化 v1 网络边界说明
  - [x] 10.1 在 workspace README 列出 v1 开放接口与后续 Redis/S3/ES/Influx 协议
    - 对应 Requirement 13.3
