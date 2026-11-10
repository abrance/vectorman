# IT-VM-SELF-05 APM 保留期任务自监控（`dataserver_apm_retention_*` / `*_deleted_total` / `_data_bytes`）→ VictoriaMetrics 直查

`ID: IT-VM-SELF-05` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_*）` ／
`命令: cargo test -p dataserver --test it_vm_self_retention -- --nocapture` ／ `负责人: mike`

`main.rs` 只在 `apm_enabled && apm_clean_interval_secs > 0` 时挂 APM 保留期任务（周期下限 60s），
每轮成功则 `dataserver_apm_retention_runs_total` +1、按报告累加 `_details_deleted_total` /
`_summaries_deleted_total` / `_edges_deleted_total`，并无条件 set `dataserver_apm_data_bytes`
（`report.bytes_after`）。这条路径此前一次都没被触发过。

本用例真造过期数据，让任务真删：

| 删除对象 | 来源 | 计数 |
|---|---|---|
| span 明细（日志引擎里 `data_type=traces`） | 一条 4 天前的 server span | `_details_deleted_total` |
| trace 摘要 `apm_trace_summary.start_ts` | 同一个 span 被 sink 刷盘后写入 | `_summaries_deleted_total` |
| 边摘要 `apm_edge_summary.bucket_start` | 一条 4 天前的 client span 派生 OTLP 边后刷盘写入 | `_edges_deleted_total` |

## 前置数据

| 项 | 值 |
|---|---|
| 环境变量 | `DP_E2E_VM_INSERT_URL`（必填，缺失即 panic 点名）、`DP_E2E_VM_SINGLE_NODE=1` |
| 被测进程 | `target/debug/dataserver`，配置写在 `TempDir` |
| APM | `apm_enabled = true`、`apm_agg_interval_secs = 1` |
| 保留任务 | `apm_clean_interval_secs = 60`（夹具新增 `Options::with_apm_clean`，默认仍为改动前的 0 = 不挂任务） |
| 自监控 | `self_metrics_interval_secs = 1` |
| 保留窗口 | 缺省 `apm_retention_days_default = 3`、`apm_endpoint_retention_days = 30` |
| 触发数据 | 两条 span，`start` 都在 **4 天前**的分钟桶内（一定早于 3 天保留窗口） |
| 隔离口径 | VM 查询带 `instance="<本进程 SQL 口地址>"`；4 天前的聚合点按桶内时刻求值（instant 查询默认回看窗口只有几分钟） |

## 执行步骤

1. `vm::Vm::require()` + `wait_ready`；`Options::local().with_vm(&vm).with_apm(1).with_self_metrics(1).with_apm_clean(60)` 起真进程。
2. `POST /v1/ingest` 两条 4 天前的 span（server + client），断言全部受理。
3. 按桶内时刻在 VM 确认 `apm_service_requests_total{...}` 出现——先证明「刷盘真写出了摘要」，
   否则保留任务没有可删的行，计数器会永远为 0（假绿）。
4. 等 `_details_deleted_total` / `_summaries_deleted_total` / `_edges_deleted_total` 在 VM 上 `>= 1`
   （`inc_counter` 对 0 提前返回，所以「出现」即「真删了东西」；等待上限 180s，跨过一个完整周期）。
5. 等 `_retention_runs_total >= 2`（启动 tick 看不到数据，真删发生在第二个周期）。
6. 等 `dataserver_apm_data_bytes` 出现，并断言 `> 0`（`data_path` 已建好子目录）。
7. 负向证据：`dataserver_apm_retention_errors_total` 不在 `/metrics` 中，`stderr` 无 `apm retention failed`。

## 期望结果

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 摘要真写出 | VM 里查到 4 天前桶上的 `apm_service_requests_total` |
| 2 | 明细真删 | `dataserver_apm_details_deleted_total{instance=本进程} >= 1` |
| 3 | 摘要真删 | `dataserver_apm_summaries_deleted_total{instance=本进程} >= 1` |
| 4 | 边摘要真删 | `dataserver_apm_edges_deleted_total{instance=本进程} >= 1` |
| 5 | 轮次 | `dataserver_apm_retention_runs_total{instance=本进程} >= 2` |
| 6 | 容量 gauge | `dataserver_apm_data_bytes{instance=本进程} > 0` |
| 7 | 无错误 | `_retention_errors_total` 不在 `/metrics` 中，`stderr` 无失败行 |

**反向验证（实测）**：把第 2 条下界从 `1.0` 改成 `1_000_000.0`（等待上限临时缩到 5s）后重跑，红输出：
```
thread 'apm_retention_self_metrics_land_in_victoriametrics' panicked at crates/test-support/src/util.rs:41:13:
等待 VM 查询 dataserver_apm_details_deleted_total{instance="127.0.0.1:35341"} 满足断言 超时（5s）
test result: FAILED. 0 passed; 1 failed
```
还原后重跑通过（含跨周期删除，用时约 91s）。

## 覆盖范围与不可达项

可达并已断言：`dataserver_apm_retention_runs_total`、`_details_deleted_total`、
`_summaries_deleted_total`、`_edges_deleted_total`、`dataserver_apm_data_bytes`（5 条）。

不可达（**未覆盖**）：

| 名字 | 原因 | 需要什么才能覆盖 |
|---|---|---|
| `dataserver_apm_retention_errors_total` | 只在 `retention.run` 返回 `Err` 时 +1：按天过期阶段要走 `evict_older_than`（sql 删除 / `log.reclaim_space` / ts 删除），VM 后端下按时间窗的 ts 删除被**跳过**（`supports_time_window_delete() = false`），健康后端上这些都不会失败 | 日志引擎或 sql 故障注入；tsink 后端能造 ts 错误但那样自监控写 tsink、VM 侧无法断言 |

用负向断言兜住「没故障时不该出现」。

## 清理方式

`TempDir` 随作用域删除；dataserver 在 `Dataserver::Drop` 里 `kill` + `wait`。
被删的数据本来就在本用例自己的 `TempDir` 与 `apm_*` 表里，进程退出即消失；VM 上只留下
`instance` 隔离的自监控序列与几个 4 天前的派生点，随 VM 保留期过期。

## 关联需求或事故编号

- 实现：`bins/dataserver/src/main.rs`（`apm_enabled && apm_clean_interval_secs > 0` 的保留任务分支）、
  `crates/dataplane-apm/src/retention.rs::run` / `evict_older_than`（details / summaries / edges 三段删除，
  以及 VM 后端跳过按时间窗 ts 删除的原因）。
- 与 `IT-VM-APM-01` 互补：那条验派生指标口径，这条验「保留任务真的在删、并且它自己的自监控落 VM」。
- 注意：ts 侧的墓碑（`report.ts_tombstoned`）在这条路径上对 VM 后端是 **0**，不要把它当断言。
