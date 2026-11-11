# 集成测试场景目录（test_plan）

本目录是**集成测试的唯一事实来源**：每个场景一份 md（前置数据 / 执行步骤 / 期望结果 /
清理方式 / 关联需求或事故），配套一个真实可执行的 Rust 测试。索引、md、测试函数三者
必须一一对应。

## 三条规矩（违反即不算落地）

1. **不进目录的不写**：没有 md 的测试不进索引，没有测试的 md 也不进索引。不留「待补」空号。
2. **禁止静默跳过**：不用 `#[ignore]`、不用 feature gate、不缺前置条件就 `return`。
   缺什么必须**失败**并点名缺的是哪个变量/哪个文件——静默跳过等于假绿。
3. **禁止恒真断言**：断言必须有可能挂。「返回 200 就算过」「数组非空就算过」不叫覆盖。
4. **不碰前端**：用例只走 HTTP / 真进程接口，不跑 `frontend` 的 dev server、不依赖浏览器、
   不把前端渲染结果当断言依据。查询口径就是 `/api/v1/query`、`/v1/logs/search`、
   `/v1/traces/search`、`/v1/edges/search` 这些后端契约。
5. **共用一套夹具**：起 dataserver、造 metrics / traces / eBPF 边 / 日志记录、查 Prom/VM，
   统一用 `crates/test-support`（`vectorman-test-support`）。测试里不再复制 `free_port`、
   写 toml、拼 envelope；新场景只写自己的断言。
6. **「落进 VM」只认 VM 原生查询口**：`vm.query(...)`（`{select}/prometheus/api/v1/query`）
   才算证据，dataserver 的 `/api/v1/query` 是代理读、会掩盖「写没进 VM」的问题。

## 跑法

```bash
cp .env.integration.example .env.integration   # 填真实的 Redis / VictoriaMetrics 地址
scripts/test-integration.sh
```

前置条件不满足时脚本直接非零退出并打印修复提示，覆盖四种情况：没有 `.env.integration`、
必填变量为空、TCP 连不通、`TEST_ENV=prod`（拒绝指向生产）。

不需要外部中间件的场景可以单独跑，也可以进 PR 门禁：

```bash
cargo test -p dataserver --test boot_smoke
cargo test -p dataserver --test it_chain -- --nocapture
cargo test -p dataserver --test ts_restart -- --nocapture
cargo test -p vmctl --test it_vmctl -- --nocapture
cargo test -p dataserver --test it_dedup -- --nocapture
```

CI（`rust-ci.yml` 的 `rust-tests` 作业）用 service 容器起 Redis 7 + 单机版 VictoriaMetrics，
喂满 `DP_E2E_*` 后跑 `cargo test --all-features`：本目录 26 条在 PR 上都是真跑。

## 索引

| ID | 场景 | 依赖 | 命令或关联 | 负责人 |
|---|---|---|---|---|
| IT-DS-BOOT-01 | dataserver **真进程**启动链路：`--config` → 后端装配 → `GET /health` → `POST /v1/ingest` → `/v1/ts/stats` 与 `/api/v1/query` 回读；另含 3 种非法后端配置必须非 0 退出且 stderr 点名变量 | 无（redb + tsink 本地后端，随机端口 + 临时目录） | `cargo test -p dataserver --test boot_smoke`<br>场景：[IT-DS-BOOT-01.md](IT-DS-BOOT-01.md) | mike |
| IT-EXT-REDIS-VM-01 | 外置存储后端：Redis KV 语义与本地引擎一致、VictoriaMetrics 写入/查询/删除、tsink 与 VM 同点同结果、`POST /v1/ingest` 经真实 Redis+VM 落库后 `/api/v1/query` 回读；缺 `DP_E2E_*` 必须失败 | 真实 Redis + VictoriaMetrics（`scripts/test-integration.sh` 做连通性前置） | `cargo test -p dataserver --test external_storage_e2e`<br>场景：[IT-EXT-REDIS-VM-01.md](IT-EXT-REDIS-VM-01.md) | mike |
| IT-CHAIN-01 | 三**真进程**链路：gse-agent → gse-server → dataserver，agent 注册并心跳上线 → 下发采集项在 agent 侧生效 → metrics 入时序库 → Prom 即时查询回读带 `agent_id`/`item_id` 标签的点 | 无外部中间件（redb + tsink，端口与目录全部临时生成）；测试内自动 `cargo build -p gse-server -p gse-agent` | `cargo test -p dataserver --test it_chain -- --nocapture`<br>场景：[IT-CHAIN-01.md](IT-CHAIN-01.md) | pit |
| IT-VMCTL-01 | 真 `vmctl` 二进制 × 真 gse-server 台账 × 真 dataserver 流索引：`agents doctor` / `agents status` 的字段解析直接对台账断言，不碰手写 JSON 夹具；定点覆盖 `4a91826`（`ebpf_tcp` 只出 `metrics`，不是 `ebpf_edges`） | 无外部中间件（gse-server 临时 sqlite + redb/tsink，端口与目录全部临时生成）；测试内自动 `cargo build -p gse-server -p gse-agent -p dataserver` | `cargo test -p vmctl --test it_vmctl -- --nocapture`<br>场景：[IT-VMCTL-01.md](IT-VMCTL-01.md) | pit |
| IT-TS-RESTART-01 | 本地时序后端重启一致性：写入 → 真 **SIGTERM** 优雅停机（退出码 0）→ 同一 `data_path` 重开 → 序列数与点集**逐点等价**，即时查询与标签保留，重启后仍可写且不改动历史点 | 无外部中间件（redb + tsink，端口与目录全部临时生成） | `cargo test -p dataserver --test ts_restart -- --nocapture`<br>场景：[IT-TS-RESTART-01.md](IT-TS-RESTART-01.md) | mike |
| IT-DEDUP-01 | 接入去重（`ingest/{record_id}`）的键数上界与清理路径：同一批 record 5 轮重发 → 点数/序列数不增长 → 真 **SIGTERM** 停机 → 用 `dataplane_kv` 直读同一个 `kv.redb`，`scan_prefix("ingest/")` 键数**恰好等于唯一 record 数**且 `record_id` 逐个对得上 → `prune_prefix_before("ingest/")` 删除数 == 键数、删完为 0、不越界删 `stream/` | 无外部中间件（redb + tsink，端口与目录全部临时生成） | `cargo test -p dataserver --test it_dedup -- --nocapture`<br>场景：[IT-DEDUP-01.md](IT-DEDUP-01.md) | mike |
| IT-VM-INGEST-01 | `ts_backend = victoriametrics` 时接入的 metrics 真的到了 VM：VM 原生口样本数与值集合、`agent_id`/`item_id`/`host_id` 标签补全、代理口与 VM 逐点一致、按 measurement 自数序列数（不依赖全局 `totalSeries`）、同 record 换 batch 重发不增点（`accepted` 不算证据）、**SIGTERM 重启后仍在** | 真实 VictoriaMetrics（`DP_E2E_VM_INSERT_URL` + `DP_E2E_VM_SINGLE_NODE`） | `cargo test -p dataserver --test it_vm_ingest -- --nocapture`<br>场景：[IT-VM-INGEST-01.md](IT-VM-INGEST-01.md) | mike |
| IT-VM-AGENT-01 | 补齐「真 agent 自采 → dataserver 以 VM 为 ts 后端写出」这段：真 gse-server 台账 + 真 gse-agent 按 `metrics_host` 采集项上报，VM 原生口按 `agent_id`/`item_id` 查到 `cpu_usage` / `mem_usage` 且值域合法，代理口同序列一致，`<data_path>/ts` 为空（证明没回落到本地 tsink） | 真实 VictoriaMetrics；测试内自动 `cargo build -p gse-server -p gse-agent` | `cargo test -p dataserver --test it_vm_agent -- --nocapture`<br>场景：[IT-VM-AGENT-01.md](IT-VM-AGENT-01.md) | mike |
| IT-API-SQL-01 | `POST /v1/sql` 的契约：建表 → 占位参数写入 → 回读的列名/顺序与类型往返（INTEGER 不变成字符串、REAL 回浮点、NULL 保持 null），以及三条错误路径（缺字段/非法参数 400、SQL 语法错 422） | 无（redb + sqlite 本地后端，临时目录） | `cargo test -p dataserver --test it_api_sql -- --nocapture`<br>场景：[IT-API-SQL-01.md](IT-API-SQL-01.md) | mike |
| IT-API-LOGS-01 | `POST /v1/logs/search` 八个过滤维度逐个生效（`data_type`/`agent_id`/`level`/`message_query`/`trace_id`/时间窗/`labels`/`limit` 截断），空结果返回 `records: []` 且**没有 `total` 字段**（接口本来就没有，口径以 `records` 为准） | 无（redb + tsink + tantivy 本地后端，临时目录） | `cargo test -p dataserver --test it_api_logs -- --nocapture`<br>场景：[IT-API-LOGS-01.md](IT-API-LOGS-01.md) | mike |
| IT-API-TRACES-01 | `POST /v1/traces/search` 的摘要聚合与服务/状态/耗时过滤、排序、分页不重不漏，`GET /v1/traces/{id}` 详情拉全父子 span；口径按代码断言：`duration_micros = max_end_ts - start_ts`（整条跨度，不是根 span）、任一 span error 即 `error`、不存在 404、非 32 位 hex 400 | 无（redb + tsink + tantivy + sqlite，APM 打开，临时目录） | `cargo test -p dataserver --test it_api_traces -- --nocapture`<br>场景：[IT-API-TRACES-01.md](IT-API-TRACES-01.md) | mike |
| IT-API-APM-01 | `GET /v1/apm/services` 清单从端点半表读出且重复请求稳定；`/v1/apm/service-aliases` 的 CRUD：重复 POST 是**覆盖（仍 201，不是 409）**、PUT 不能改 `match_kind`/`match_value`（400）、删除后 404 | 无（redb + tsink + tantivy + sqlite，APM 打开，临时目录） | `cargo test -p dataserver --test it_api_apm -- --nocapture`<br>场景：[IT-API-APM-01.md](IT-API-APM-01.md) | mike |
| IT-VM-APM-01 | OTLP **server** span → `apm_service_requests_total` / `apm_service_duration_micros` 直查 VM：计数按 `status` 拆分、`avg`/`max`/`p95` 与 `aggregator` 口径一致（`p95` 是最近秩 `ceil(p/100*n)`）、client span **不**进 `apm_service_*`、同桶三 span 合并成一个点且时间戳 = 桶起点；otlp client span 派生 `apm_edge_*`（`source=otlp`、`span_kind` 记被调用侧、无对端时兜底 `unknown`、边耗时只有 `avg`/`p95` 两个 field（无 `sum`）、error span 另落 `apm_edge_errors_total`） | 真实 VictoriaMetrics（`DP_E2E_VM_INSERT_URL` + `DP_E2E_VM_SINGLE_NODE`） | `cargo test -p dataserver --test it_vm_apm -- --nocapture`<br>场景：[IT-VM-APM-01.md](IT-VM-APM-01.md) | pit |
| IT-VM-EBPF-01 | eBPF 边台账 → `ebpf_edge_connections_total` / `ebpf_edge_bytes_total`（sent/recv 分开）/ `ebpf_tcp_failures_total` 与 APM 侧 `apm_edge_*`（`source=ebpf`、无 `span_kind`）直查 VM；边界：原始 `ebpf` 事件只进日志引擎，不派生新边、不改已聚合指标 | 真实 VictoriaMetrics | `cargo test -p dataserver --test it_vm_ebpf -- --nocapture`<br>场景：[IT-VM-EBPF-01.md](IT-VM-EBPF-01.md) | pit |
| IT-VM-PROCESS-01 | 进程指标按 `process_name` 别名补 `service` 维度后落 VM：命中别名补服务名、未命中补 `unknown-<process_name>`、原始 `pid`/`syscall` 标签不被挤掉、`service=""` 一条都不产生；`ebpf_process_*` 与 `ebpf_syscall_*` 两个前缀走同一分支 | 真实 VictoriaMetrics | `cargo test -p dataserver --test it_vm_process -- --nocapture`<br>场景：[IT-VM-PROCESS-01.md](IT-VM-PROCESS-01.md) | pit |
| IT-VM-EBPF-TCP-01 | eBPF 边的 `tcp_retrans`/`tcp_resets` 落 VM：同桶多条边累加、**每个桶各自求和**（不是跨桶累加的 counter，桶 2 断言 4 而非 10+4）、标签集只含 `src_service`/`dst_service`、全 0 值的边在聚合侧 `if > 0` 被跳过 → VM 里**查不到序列**（不是查到 0） | 真实 VictoriaMetrics | `cargo test -p dataserver --test it_vm_ebpf_tcp -- --nocapture`<br>场景：[IT-VM-EBPF-TCP-01.md](IT-VM-EBPF-TCP-01.md) | mike |
| IT-VM-SELF-01 | dataserver 自身 `/metrics` 经 `flush_loop` → `LocalTsSink` 逐样本写进 VM：VM 里的值等于 `/metrics` 文本值、标签集与文本逐项一致（多一个少一个都算错）、`instance` 是本次 SQL 口监听地址、`component=dataserver`/`data_id=self`、`<data_path>/ts` 条目数为 0（证明没回落本地 tsink）；另含 `dataserver_ts_*` 六条存储状态 gauge（`series_count`/`memory_used_bytes`/`memory_budget_bytes`/`wal_size_bytes`/`background_errors_total`/`degraded`） | 真实 VictoriaMetrics；`with_self_metrics(1)` 打开自监控快照（默认 0 = 关闭） | `cargo test -p dataserver --test it_vm_self -- --nocapture`<br>场景：[IT-VM-SELF-01.md](IT-VM-SELF-01.md) | mike |
| IT-VM-SELF-02 | trace 接入限流自监控：循环发单记录 trace 批直到真拿到 429，`dataserver_apm_ingest_throttled_batches_total` == 实测 429 次数、`_throttled_records_total` == 被拒记录数，落 VM 后值与 `component`/`instance` 标签逐项一致 | 真实 VictoriaMetrics；`with_apm_ingest_limit` 把每秒批数上限压到 1 | `cargo test -p dataserver --test it_vm_self_throttle -- --nocapture`<br>场景：[IT-VM-SELF-02.md](IT-VM-SELF-02.md) | mike |
| IT-VM-SELF-03 | APM 聚合 / 刷盘自监控：真 span → 聚合 1s → 刷盘 1s，`apm_agg_runs_total`/`apm_agg_points_total`、`apm_last_agg_bucket` **精确等于桶起点**、`trace_summaries`/`endpoints`/`edges_flushed_total` ≥1、`open_groups`/`dropped_samples`/`live_traces`/`dirty_traces`/`paired_edges`/`pending_spans` 存在且 ≥0；`agg_errors`/`flush_errors` 不出现 | 真实 VictoriaMetrics；`with_apm_clean` 打开聚合与刷盘（默认关闭） | `cargo test -p dataserver --test it_vm_self_apm -- --nocapture`<br>场景：[IT-VM-SELF-03.md](IT-VM-SELF-03.md) | mike |
| IT-VM-SELF-04 | eBPF 聚合 + 全局清理自监控：真 eBPF 边 + 回拨游标，`ebpf_agg_points_total` ≥1、`ebpf_last_agg_bucket` 精确等于桶起点、`ts_clean_runs_total` ≥2（真跨一个 60s 周期）、`ebpf_edges_deleted_total` 有值；两个 `*_errors_total` 不出现且 stderr 无失败行 | 真实 VictoriaMetrics；`with_ts_clean(60)`（产品下限） | `cargo test -p dataserver --test it_vm_self_ebpf -- --nocapture`<br>场景：[IT-VM-SELF-04.md](IT-VM-SELF-04.md) | mike |
| IT-VM-SELF-05 | APM 保留期自监控：4 天前 span/边 → 真跨一个 60s 保留周期，`apm_details_deleted_total`/`summaries_deleted_total`/`edges_deleted_total` 都 ≥1、`retention_runs_total` ≥2、`apm_data_bytes` >0；`retention_errors_total` 不出现 | 真实 VictoriaMetrics；`with_apm_clean(60)` | `cargo test -p dataserver --test it_vm_self_retention -- --nocapture`<br>场景：[IT-VM-SELF-05.md](IT-VM-SELF-05.md) | mike |
| IT-VM-SELF-06 | 接入计数自监控：2 条全合法批 + 1 合法 1 非法批 → 收 3 拒 1、批次 `ok`/`partial` 各 1、记录 `accepted=3`/`invalid=1`，六条序列落 VM 且 `data_type`/`status`/`result` 标签逐个相等 | 真实 VictoriaMetrics；`with_self_metrics(1)` | `cargo test -p dataserver --test it_vm_self_ingest -- --nocapture`<br>场景：[IT-VM-SELF-06.md](IT-VM-SELF-06.md) | mike |
| IT-VM-EBPF-PROC-01 | Agent 已关闭分钟桶的 `ebpf_process_{exec,exit,fork}_total` 经 `/v1/ingest` 落 VM：`pid`/`process_name` 维度保留、值逐个相等、全 0 的进程不产点 | 真实 VictoriaMetrics；夹具直接调产品 `ProcessMinuteAccumulator::drain_closed`，不手搓 JSON | `cargo test -p dataserver --test it_vm_ebpf_proc -- --nocapture`<br>场景：[IT-VM-EBPF-PROC-01.md](IT-VM-EBPF-PROC-01.md) | pit |
| IT-VM-EBPF-SYSCALL-01 | `ebpf_syscall_duration_micros{field="avg"}` 与 `{field="p95"}` 的 `op` 维度、`ebpf_syscall_failures_total{op,errno}` 落 VM，值按产品 `duration_metrics`/`failure_metrics` 口径（p95 为最近秩） | 真实 VictoriaMetrics；夹具直接调产品产出函数 | `cargo test -p dataserver --test it_vm_ebpf_syscall -- --nocapture`<br>场景：[IT-VM-EBPF-SYSCALL-01.md](IT-VM-EBPF-SYSCALL-01.md) | pit |
| IT-VM-EBPF-SELF-01 | Agent eBPF 自监控 13 条（`agent_ebpf_{flushes,edges,metric_points,filtered,idle_keys,read_errors,map_overflow_dropped,rate_limited,buffer_dropped,edges_skipped}_total`、`agent_ebpf_cpu_percent`、`agent_ebpf_degraded`）与 `agent_ebpf_capability` 落 VM：断言**真实降级形态**（`value=0`、`reason` 有值），并钉住 `field_name` 不进 VM、`available` 序列不存在 | 真实 VictoriaMetrics；夹具走产品 `interval_records`/`capability_metric`；内核态采集本身不可达（root+CAP_BPF） | `cargo test -p dataserver --test it_vm_ebpf_self -- --nocapture`<br>场景：[IT-VM-EBPF-SELF-01.md](IT-VM-EBPF-SELF-01.md) | pit |
| IT-VM-SELF-07 | eBPF 边保留期清理自监控落 VM：真 `gse-server` 下发 `kind=ebpf_network` 采集项（`item_id` == 边记录的 `data_id`）→ ingest 一条 4 天前的边（> eBPF 缺省保留 3 天）→ 真等一个 60s 清理周期 → `dataserver_ebpf_edges_deleted_total{instance=…} >= 1`、同周期 `dataserver_ts_clean_runs_total >= 2`、边检索归零（计数是「删了几行」而不是「跑过一轮」的证据）；边检索必须显式给时间窗，接口缺省只查最近 1 小时 | 真实 VictoriaMetrics + 真 `gse-server`（`Gse::start`，临时目录 + 随机端口）；测试内自动 `cargo build -p gse-server -p gse-agent`；APM 必须打开（否则 `/v1/edges/search` 返回 503） | `cargo test -p dataserver --test it_vm_self_ebpf_edges -- --nocapture`<br>场景：[IT-VM-SELF-07.md](IT-VM-SELF-07.md) | mike |
