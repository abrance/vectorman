# IT-VM-SELF-03 APM 聚合 / 刷盘自监控（`dataserver_apm_agg_*`、`*_flushed_total`、sink gauge）→ VictoriaMetrics 直查

`ID: IT-VM-SELF-03` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_*）` ／
`命令: cargo test -p dataserver --test it_vm_self_apm -- --nocapture` ／ `负责人: mike`

`main.rs` 里有两类 APM 后台任务：

- **聚合任务**（`apm_enabled && apm_agg_interval_secs > 0`）每轮跑 `ApmAggregator::run_once`，
  成功则 `dataserver_apm_agg_runs_total` +1、`_agg_points_total` 累加写出的点数、`_last_agg_bucket`
  记最后一个桶；每轮结束后无条件 set `_open_groups` / `_dropped_samples` 两个 `RedSamples` gauge。
- **刷盘任务**（APM sink 存在时每秒一轮）把已关闭的 trace 摘要 / endpoint / 边落库，计数
  `_trace_summaries_flushed_total` / `_endpoints_flushed_total` / `_edges_flushed_total`，并每轮
  无条件 set `_live_traces` / `_dirty_traces` / `_paired_edges` / `_pending_spans`。

`IT-VM-SELF-01` 的实例 APM 关闭、也没有聚合任务，所以上面这些序列**一条都没被触发过**。
本用例真跑一遍链路，并且只查本进程 `instance` 的序列。

## 前置数据

| 项 | 值 |
|---|---|
| 环境变量 | `DP_E2E_VM_INSERT_URL`（必填，缺失即 panic 点名）、`DP_E2E_VM_SINGLE_NODE=1` |
| 被测进程 | `target/debug/dataserver`，配置写在 `TempDir` |
| APM | `apm_enabled = true`、`apm_agg_interval_secs = 1`（`Options::with_apm(1)`） |
| 自监控 | `self_metrics_interval_secs = 1` |
| 后端 | 时序 = 真实 VictoriaMetrics；KV = `redb`；日志 = tantivy |
| 触发数据 | 1 条 `span_kind=server`（`GET /agg`，2ms，ok）+ 1 条 `span_kind=client`（`POST /downstream`，4ms，error），起点在**5 分钟前**的分钟桶内（桶已关闭，聚合才会写） |
| 隔离口径 | 所有 VM 查询带 `instance="<本进程 SQL 口地址>"`；聚合点在桶内求值（`at = bucket + 120s`），不能用「现在」求值——VM instant 查询的默认回看窗口约 5 分钟，压在边界上会偶发假红（本轮实测踩到过一次） |

## 执行步骤

1. `vm::Vm::require()` + `wait_ready`；`Options::local().with_vm(&vm).with_apm(1).with_self_metrics(1)` 起真进程。
2. `POST /v1/ingest` 两条 span（server + client），断言全部受理。
3. 在 VM 里按桶内时刻确认 `apm_service_requests_total{...source="otlp"}` 与
   `apm_edge_requests_total{src_service=...}` 都出现了——先证明「聚合真写出了点」，否则后面的
   自监控数值没有来源。
4. 断言聚合组：`_agg_runs_total >= 1`、`_agg_points_total >= 2`（服务点 + 边点）、
   `_last_agg_bucket == <那条 span 的分钟桶微秒>`、`_open_groups` / `_dropped_samples` 在本进程下存在且非负。
5. 断言刷盘组：三个 `*_flushed_total >= 1`；四个 sink gauge（`_live_traces` / `_dirty_traces` /
   `_paired_edges` / `_pending_spans`）在本进程下存在且非负。
6. 负向证据：`_agg_errors_total` / `_flush_errors_total` 在 `/metrics` 上**不存在**（懒创建：没 +1 就没有序列），
   并且 `stderr` 里没有 `aggregate failed` / `flush failed`。

## 期望结果

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 聚合真产出 | VM 里查到 `apm_service_requests_total` 与 `apm_edge_requests_total`（桶内时刻求值） |
| 2 | 聚合轮次 | `dataserver_apm_agg_runs_total{instance=本进程} >= 1` |
| 3 | 聚合点数 | `dataserver_apm_agg_points_total{instance=本进程} >= 2` |
| 4 | 最后桶 | `dataserver_apm_last_agg_bucket == bucket`（桶起点微秒，精确相等） |
| 5 | RedSamples gauge | `_open_groups` / `_dropped_samples` 存在且 `>= 0` |
| 6 | 刷盘计数 | `_trace_summaries_flushed_total` / `_endpoints_flushed_total` / `_edges_flushed_total` 均 `>= 1` |
| 7 | sink gauge | `_live_traces` / `_dirty_traces` / `_paired_edges` / `_pending_spans` 存在且 `>= 0` |
| 8 | 无错误 | `_agg_errors_total` / `_flush_errors_total` 不在 `/metrics` 中，`stderr` 无聚合/刷盘失败 |

**反向验证（实测）**：把第 3 条的期望从 `2.0` 改成 `1_000_000.0`（等待上限临时缩到 5s，只为让
失败快速出现，谓词不变）后重跑，红输出：
```
thread 'apm_agg_and_flush_self_metrics_land_in_victoriametrics' panicked at crates/test-support/src/util.rs:41:13:
等待 VM 查询 dataserver_apm_agg_points_total{instance="127.0.0.1:34141"} 满足断言 超时（5s）
test result: FAILED. 0 passed; 1 failed
```
说明「查询拿到样本」本身不被当成通过条件，值与下界也锁了。还原后重跑通过。

## 覆盖范围与不可达项

可达并已断言：`_agg_runs_total`、`_agg_points_total`、`_last_agg_bucket`、`_open_groups`、
`_dropped_samples`、`_trace_summaries_flushed_total`、`_endpoints_flushed_total`、
`_edges_flushed_total`、`_live_traces`、`_dirty_traces`、`_paired_edges`、`_pending_spans`（12 条）。

不可达（**未覆盖**，不造假）：

| 名字 | 原因 | 需要什么才能覆盖 |
|---|---|---|
| `dataserver_apm_agg_errors_total` | ts 后端是 VM：写点走队列 fire-and-forget，`run_once` 在 VM 后端下不返回错误。tsink 后端能用 `point out of retention` 真造出错误，但那样实例的自监控写 tsink、无法再在 VM 侧断言 | 一个「自监控仍写 VM、聚合写另一个会失败的后端」的双后端开关（产品侧没有），或聚合错误的故障注入钩子 |
| `dataserver_apm_flush_errors_total` | 同上：`flush_due` 落 sqlite + ts，VM 后端下不返回错误 | 同上一行 |

这两条本用例用**负向断言**兜住「没故障时不该出现」，从而保证「出现了就是真故障」。

## 清理方式

`TempDir` 随作用域删除；dataserver 在 `Dataserver::Drop` 里 `kill` + `wait`。
VM 上的自监控序列按 `instance` 隔离；APM 派生点在 5 分钟前的桶上，随 VM 保留期自然过期。

## 关联需求或事故编号

- 实现：`bins/dataserver/src/main.rs`（聚合任务 `aggregator.run_once` 分支、eBPF 同节拍分支、
  sink 刷盘 `flush_due` 分支）、`crates/dataplane-apm/src/aggregator.rs::run_once`、
  `crates/dataplane-apm/src/red.rs::RedSamples`（`groups()` / `dropped_samples()`）。
- 与 `IT-VM-APM-01`（派生指标口径）互补：那条验**点值/标签**，这条验**任务自身的自监控**是否
  被触发并落 VM（看板上「聚合卡住/刷盘失败」的告警就是查这些序列）。
