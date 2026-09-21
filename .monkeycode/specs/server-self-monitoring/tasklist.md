# 需求实施计划

 - [x] 1. 新建 `crates/vectorman-metrics`
   - [x] 1.1 workspace 加入 crate：Registry、进程采样、HTTP 中间件、`GET /metrics`、文本快照
    - 对应 Requirement 2、3、4、5、9、11
   - [x] 1.2 单元测试：编码含 `vectorman_process_uptime_seconds` 与 `component`；中间件计数；快照标签与文本一致；histogram 含 `_bucket`
    - 对应 Requirement 12.1、设计 Test Strategy

 - [x] 2. 检查点 - vectorman-metrics 测试通过
  - 确保所有测试通过,如有疑问请询问用户

 - [x] 3. dataserver 接入
   - [x] 3.1 配置 `metrics_http.listen` 缺省 `127.0.0.1:9091`、`self_metrics_interval_secs` 缺省 60；环境变量 `DP_METRICS_HTTP_LISTEN` / `DP_SELF_METRICS_INTERVAL`
    - 对应 Requirement 7、10
   - [x] 3.2 独立 metrics 口、SQL/Prom 口 HTTP 埋点、ingest 计数器、60s 直写 TSDB
    - 对应 Requirement 1.2、2、5、6.3、6.4、7、8、11
   - [x] 3.3 httptest：metrics 口 200；web_dir 时 SQL 口 `/metrics` 仍为 SPA；flush 后 query 命中 `component="dataserver"`
    - 对应 Requirement 12.2、12.3、10.5

 - [x] 4. gse-server 接入
   - [x] 4.1 配置 `metrics_listen` 缺省 `127.0.0.1:7102`；环境变量 `GSE_SERVER_METRICS_LISTEN`
    - 对应 Requirement 10.1、10.2、10.7
   - [x] 4.2 独立 metrics 口、管理口 HTTP 埋点、scrape 刷新 `vectorman_gse_agents_online` / `vectorman_gse_sessions`
    - 对应 Requirement 1.1、5、6.1、6.2、7.5
   - [x] 4.3 httptest：`GET /metrics` 含 `vectorman_gse_sessions` 与 `component="gse-server"`
    - 对应 Requirement 12.1

 - [x] 5. console 接入
   - [x] 5.1 配置 `metrics_listen` 缺省 `127.0.0.1:7201`；环境变量 `CONSOLE_METRICS_LISTEN`
    - 对应 Requirement 10.1、10.4、10.7
   - [x] 5.2 独立 metrics 口、业务口 HTTP 埋点、scrape 刷新 `vectorman_console_apps`
    - 对应 Requirement 1.3、5、6.5、7.5
   - [x] 5.3 httptest：`GET /metrics` 含 `vectorman_console_apps` 与 `component="console"`
    - 对应 Requirement 12.1

 - [x] 6. 检查点 - 相关 crate 测试通过
  - 确保所有测试通过,如有疑问请询问用户
