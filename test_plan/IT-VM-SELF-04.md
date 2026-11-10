# IT-VM-SELF-04 eBPF 聚合自监控 + 全局清理自监控 → VictoriaMetrics 直查

`ID: IT-VM-SELF-04` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_*）` ／
`命令: cargo test -p dataserver --test it_vm_self_ebpf -- --nocapture` ／ `负责人: mike`

两条互不相关的后台路径，共同点是「此前只在 `/metrics` 上存在、从没被断言过」：

- **eBPF 边聚合**（`main.rs`，与 APM 聚合同一个节拍）：写出点时才 `dataserver_ebpf_agg_points_total`
  +点，每个桶 `dataserver_ebpf_last_agg_bucket` set 一次；
- **全局清理**（`ts_clean_interval_secs`，产品侧下限 60s）：每跑完一轮 `dataserver_ts_clean_runs_total` +1。

两者都只在特定开关下才存在，`IT-VM-SELF-01` 的实例两个都没开。

## 前置数据

| 项 | 值 |
|---|---|
| 环境变量 | `DP_E2E_VM_INSERT_URL`（必填，缺失即 panic 点名）、`DP_E2E_VM_SINGLE_NODE=1` |
| 被测进程 | `target/debug/dataserver`，配置写在 `TempDir` |
| APM / 聚合 | `apm_enabled = true`、`apm_agg_interval_secs = 1` |
| 清理 | `ts_clean_interval_secs = 60`（夹具新增 `Options::with_ts_clean`，默认仍写改动前的 3600s） |
| 自监控 | `self_metrics_interval_secs = 1` |
| 触发数据 | 1 条 eBPF 边（`tcp`、2 连接、`bucket_start` 在 5 分钟前的分钟桶、`data_id` 唯一前缀） |
| 游标 | 先 `wait_watermark` 等第一轮 tick 写下 `ebpf_metrics_watermark`，ingest 之后再 `rewind_ebpf_watermark(bucket - 60s)` |
| 隔离口径 | VM 查询带 `instance="<本进程 SQL 口地址>"`；派生点按桶内时刻求值（`at = bucket + 120s`），避免撞 VM instant 查询的默认回看窗口 |

## 执行步骤

1. `vm::Vm::require()` + `wait_ready`；`Options::local().with_vm(&vm).with_apm(1).with_self_metrics(1).with_ts_clean(60)` 起真进程。
2. `apm::wait_watermark(&ds)`（游标必须先存在，否则「首次对齐」会覆盖回拨的值）。
3. `POST /v1/ingest` 一条 `ebpf_edges`；随后 `apm::rewind_ebpf_watermark(bucket - 60s)`。
4. VM 里按桶内时刻确认 `apm_edge_requests_total{src_service=...,source="ebpf"}` 出现（聚合真写了点）。
5. 断言 `dataserver_ebpf_agg_points_total{instance} >= 1`、`dataserver_ebpf_last_agg_bucket{instance} == bucket`。
6. 断言 `dataserver_ts_clean_runs_total{instance} >= 2`：启动 tick 之外**还要再看一个 60s 周期**，
   避免把「刚起进程那一次」当成周期性证据（等待上限 180s）。
7. 负向证据：`dataserver_ebpf_agg_errors_total` / `dataserver_ts_clean_errors_total` 不在 `/metrics` 中，
   `stderr` 无 `ebpf: aggregate failed` / `failed_phases`。

## 期望结果

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | eBPF 聚合真产出 | VM 里查到 `apm_edge_requests_total{source="ebpf"}`（桶内时刻求值） |
| 2 | 聚合点数 | `dataserver_ebpf_agg_points_total{instance=本进程} >= 1` |
| 3 | 最后桶 | `dataserver_ebpf_last_agg_bucket{instance=本进程} == bucket`（精确相等） |
| 4 | 清理在周期性跑 | `dataserver_ts_clean_runs_total{instance=本进程} >= 2`（跨过一个完整 60s 周期） |
| 5 | 两条路径都没出错 | 两个 `*_errors_total` 不在 `/metrics` 中，`stderr` 无对应失败行 |

**反向验证（实测）**：把第 2 条下界从 `1.0` 改成 `1_000_000.0`（等待上限临时缩到 5s）后重跑，红输出：
```
thread 'ebpf_agg_and_ts_clean_self_metrics_land_in_victoriametrics' panicked at crates/test-support/src/util.rs:41:13:
等待 VM 查询 dataserver_ebpf_agg_points_total{instance="127.0.0.1:44311"} 满足断言 超时（5s）
test result: FAILED. 0 passed; 1 failed
```
还原后重跑通过（含 60s 周期的第二轮清理）。

## 覆盖范围与不可达项

可达并已断言：`dataserver_ebpf_agg_points_total`、`dataserver_ebpf_last_agg_bucket`、
`dataserver_ts_clean_runs_total`（3 条）。

不可达（**未覆盖**，不造假）：

| 名字 | 原因 | 需要什么才能覆盖 |
|---|---|---|
| `dataserver_ebpf_agg_errors_total` | VM 后端写点是排队 fire-and-forget，`run_once` 不返回错误；tsink 后端的 `point out of retention` 能真造错，但那种实例的自监控写 tsink，无法在 VM 侧断言 | 双后端开关或聚合错误注入 |
| `dataserver_ts_clean_errors_total` | 只在「某个清理阶段整段失败」时 +1：ts 阶段要 `kv.scan_prefix` 失败、日志/边阶段要 `delete_matching` / `delete_edges_before` 失败、去重阶段要 `prune_prefix_before` 失败。健康后端上这些都不可达 | KV / 日志后端的故障注入开关 |
| `dataserver_ts_tombstones_applied_total` | 需要 ts 后端支持**按时间窗删除**并回报墓碑：VM 后端 `supports_time_window_delete() = false`（VM 删除接口不吃 start/end），且共享 VM 不能开 `-deleteAuthKey` / `whole_series_delete`；tsink 后端能真删，但那种实例的自监控写 tsink，无法在 VM 侧断言 | 一个「自监控写 VM、业务时序写 tsink」的双后端形态 |
| `dataserver_ebpf_edges_deleted_total` | 只有在 `apply_retention` 拿到 **live 采集项列表**（配 `gse_admin_url`，且该 item 的 `kind` 以 `ebpf` 开头）并对该 item 删边时才 +1；本用例没起 gse-server。可覆盖但需要 `Gse` 真进程链（夹具已有 `Gse::put_spec`），属于独立场景 | 一条新场景：fake gse-server 下发 eBPF 采集项 + 过期边 + `ts_clean=60` |
| `dataserver_ingest_dedup_pruned_total` | 去重键按**接入时刻**落 KV，只有 `ingest/{record_id}` 的值早于 `ingest_dedup_retention_days`（默认 3 天）才会被删；接入时间无法回溯，等于要么等 3 天、要么直接往 KV 写旧键（没有对外 API） | KV 直写夹具，或可配置的「去重键 aging」注入 |

`dataserver_ebpf_agg_errors_total` / `dataserver_ts_clean_errors_total` 用负向断言兜住：
「没故障时不该出现」，因此一旦出现就是真故障。

## 清理方式

`TempDir` 随作用域删除；dataserver 在 `Dataserver::Drop` 里 `kill` + `wait`。
VM 上的自监控序列按 `instance` 隔离；eBPF 派生点在 5 分钟前的桶上，随 VM 保留期过期。

## 关联需求或事故编号

- 实现：`bins/dataserver/src/main.rs`（eBPF 聚合任务分支、`ts_clean_interval_secs.max(60)` 的清理循环）、
  `crates/dataplane-apm/src/ebpf_metrics.rs::run_once`（游标 `ebpf_metrics_watermark` 不回溯）、
  `bins/dataserver/src/cleanup.rs::run_cleanup`。
- 与 `IT-VM-EBPF-01`（派生指标口径）互补：那条验**点值/标签**，这条验 **eBPF 聚合任务的自监控**。
- 与 `IT-VM-SELF-01` 互补：那条验自监控文本 == VM 值，这条验两组「只在特定任务下才存在」的序列。
