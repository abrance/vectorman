# IT-VM-SELF-06 接入计数自监控（`vectorman_ingest_*` / `dataserver_ingest_*`）→ VictoriaMetrics 直查

`ID: IT-VM-SELF-06` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_*）` ／
`命令: cargo test -p dataserver --test it_vm_self_ingest -- --nocapture` ／ `负责人: mike`

`http.rs::ingest` 每次受理一批都记四条自监控：

| 序列 | 语义 |
|---|---|
| `vectorman_ingest_records_accepted_total` | 本批受理的记录数（`reply.accepted`） |
| `vectorman_ingest_records_failed_total` | 本批被拒的记录数（`reply.failures.len()`） |
| `dataserver_ingest_batches_total{data_type,status}` | 批次计数，`status` ∈ `ok` / `partial` |
| `dataserver_ingest_records_total{data_type,result}` | 记录计数，`result` ∈ `accepted` / `invalid` |

后两条是需求 12.4「eBPF 的批次数 / 记录数 / 非法记录数」的落地口径（`data_type=~"ebpf.*"` 即 eBPF），
**按标签拆分**正是这两个序列的全部价值，所以本用例既断言值、也断言标签值没被吞掉。

## 前置数据

| 项 | 值 |
|---|---|
| 环境变量 | `DP_E2E_VM_INSERT_URL`（必填，缺失即 panic 点名）、`DP_E2E_VM_SINGLE_NODE=1` |
| 被测进程 | `target/debug/dataserver`（APM 关闭，只需要接入 + 自监控） |
| 自监控 | `self_metrics_interval_secs = 1` |
| 后端 | 时序 = 真实 VictoriaMetrics；KV = `redb`；日志 = tantivy |
| 触发数据 | 第 1 批：2 条合法 `metrics` 记录（`status=ok`）；第 2 批：1 条合法 + 1 条 `record_id` 为空（`invalid_record`，整批 200 但 `status=partial`、1 条 failure） |
| 隔离口径 | VM 查询带 `instance="<本进程 SQL 口地址>"`；带标签序列再加 `data_type` / `status`（`result`） |

## 执行步骤

1. `vm::Vm::require()` + `wait_ready`；`Options::local().with_vm(&vm).with_self_metrics(1)` 起真进程。
2. 发第 1 批（2 条合法）：断言 `accepted == 2`、`status == "ok"`。
3. 发第 2 批（1 合法 + 1 `record_id=""`）：断言 `accepted == 1`、`status == "partial"`、`failures` 长度 1。
4. `GET /metrics`，按六条口径取样本并断言：3 收 / 1 拒，`batches_total{metrics,ok} == 1`、
   `{metrics,partial} == 1`、`records_total{metrics,accepted} == 3`、`{metrics,invalid} == 1`。
5. VM 原生口按 `instance` + 标签再次查询这六条，值必须一字不差；并逐条断言样本仍带着
   `data_type` / `status`（`result`）标签——标签丢了「按 data_type 拆分」就名存实亡。

## 期望结果

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 全合法批 | `accepted=2`、`status=ok` |
| 2 | 混合批 | `accepted=1`、`status=partial`、`failures=1` |
| 3 | `/metrics` 累计值 | 收 3 / 拒 1；批次 ok=1、partial=1；记录 accepted=3、invalid=1 |
| 4 | 落 VM | 同六条序列在 VM 上值一致（按 `instance` 过滤） |
| 5 | 标签未漂 | 带标签序列的 `data_type` / `status` / `result` 逐个相等 |

**反向验证（实测）**：把 `/metrics` 那组的期望 `3.0` 改成 `4.0` 后重跑，红输出：
```
thread 'ingest_counters_self_metrics_land_in_victoriametrics' panicked at bins/dataserver/tests/it_vm_self_ingest.rs:119:9:
/metrics 上 vectorman_ingest_records_accepted_total[] 期望 4，实际 3
test result: FAILED. 0 passed; 1 failed
```
说明断言真的在比较值（不是「查得到就绿」）。还原后重跑通过。

## 清理方式

`TempDir` 随作用域删除；dataserver 在 `Dataserver::Drop` 里 `kill` + `wait`。
`metric` 名带 `util::unique_prefix()`（含进程号），与外层并行用例的序列不冲突；自监控序列按
`instance` 隔离，不需要也无法主动删除。

## 关联需求或事故编号

- 实现：`bins/dataserver/src/http.rs::ingest`（`vectorman_ingest_records_accepted_total` /
  `_failed_total` / `dataserver_ingest_batches_total{data_type,status}` /
  `dataserver_ingest_records_total{data_type,result}`）、`crates/dataplane-ingest/src/lib.rs`
  （`status` 由 `failures.is_empty()` 决定；`require_record_id` 产生 `invalid_record`）。
- 需求 12.4：eBPF 批次数 / 记录数 / 非法记录数按 `data_type` 拆分。
- 与 `IT-VM-INGEST-01` 互补：那条验**数据点**落 VM，这条验**接入计数**落 VM（且标签齐全）。
