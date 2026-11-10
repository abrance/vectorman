# IT-VM-INGEST-01 接入的 metrics 真的落进 VictoriaMetrics

`ID: IT-VM-INGEST-01` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_INSERT_URL / DP_E2E_VM_SINGLE_NODE）` ／
`命令: cargo test -p dataserver --test it_vm_ingest -- --nocapture` ／ `负责人: mike`

**为什么有这条**：`ts_backend = "victoriametrics"` 时，接入线程把点交给异步批量刷写器，
HTTP 200 只代表「收下了」，不代表「发出去了」。`it_chain` / `ts_restart` / `it_dedup` 全部用
本地 tsink，**没有一条用例能从外部证明点真的到了 VM**。VM 侧标签补全（`agent_id` /
`item_id` / `host_id`）、序列数增量、重复投递不重复入库、重启后数据仍在，此前都只有人工验证。

**为什么不能拿 dataserver 的 `/api/v1/query` 当证据**：它是代理读，走的是同一份后端但允许
缓存/内存态短路；「代理口查得到、VM 查不到」正是最典型的漏写故障。所以这条用例的主证据
只取 **VM 原生查询口** `{select}/prometheus/api/v1/query`，代理口只用来做「两侧口径一致」的对照。

## 前置数据

| 项 | 值 |
|---|---|
| 二进制 | `target/debug/dataserver`（`env!("CARGO_BIN_EXE_dataserver")`） |
| 后端 | `kv_backend = "redb"`、`ts_backend = "victoriametrics"`（`Options::local().with_vm(&vm)`） |
| VM | `DP_E2E_VM_INSERT_URL` 必填（缺则 panic 点名）；`DP_E2E_VM_SINGLE_NODE=1` 走单机版路径；`DP_E2E_VM_SELECT_URL` 缺省回落 insert 地址 |
| 刷写参数 | `vm_flush_interval_ms = 50`、`vm_batch_size = 100`：让 5 条点一轮就发出，不用等满批 |
| 指标名 | `util::metric_name(unique_prefix("vm-ingest"), "gauge")`。`unique_prefix` 带 `-`（`tp-vm-ingest-<pid>`），**必须过滤**成 `[A-Za-z0-9_:]`，否则 VM 以 `invalid metric name` 400 拒写 |
| 时间戳 | `floor_minute(now) - 120s`，5 条**同一时刻**（`step_micros = 0`），标签 `series=<prefix>`、`idx=0..4` → 5 条独立序列 |
| 值 | 1.0 / 2.0 / 3.0 / 4.0 / 5.0 |
| 查询时刻 | `time = ts/1e6 + 1`（秒）；即时查询在写入点之后 1s，避免边界抖动 |
| 端口 | `127.0.0.1:0` 现取（`sql_http` / `prom_http` / `metrics_http`） |
| 清理 | `TempDir` 删目录；`Dataserver` 的 `Drop` 里 `kill` + `wait` |

## 执行步骤

1. `Vm::require()` 读环境变量并 `wait_ready`（`/health`，20s 上限）；缺变量直接失败点名。
2. 起真 `dataserver`，等到 `/health` `status=ok`；记录 `GET /v1/ts/stats` 的 `series_count` 作为基线。
3. `POST /v1/ingest` 一次写入 5 条记录（`data::metric_series`），信封里 `agent_id` / `data_id`(=item) / `host_id` 都是本用例唯一值。
4. 轮询 VM 原生口 `query=<metric>&time=<ts+1s>`，直到样本数 `== 5`（60s 上限）。
5. 比较 dataserver 代理口 `GET /api/v1/query?query=<metric>&time=<ts+1s>` 与 VM 原生口的结果。
6. 轮询 `/v1/ts/stats`，等 `series_count` 增量达到 5，再断言**恰好等于 5**。
7. 查一个从未写入的指标名，期望 0 样本。
8. 同批 `record_id` 换 `batch_id` 重发一次。
9. `SIGTERM` → 退出码 0 → 同 `data_path` 重启（`Dataserver::restart()` 内部断言退出码），再查 VM。

## 期望结果（可判定）

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 首轮全部受理 | `accepted == 5` 且 `failures == []` |
| 2 | 点真的到了 VM | VM 原生口样本数 `== 5`（不是 ≥） |
| 3 | 值集合正确 | 排序后值向量 **逐点等于** `[1,2,3,4,5]` |
| 4 | 标签补全正确 | 每个样本的 `agent_id` / `item_id` / `host_id` 等于信封值，且记录自带 `idx` 标签保留 |
| 5 | 两个查询口一致 | 代理口与 VM 的「标签集合 + 值」排序后**完全相同** |
| 6 | 序列数按实际增量增长 | `/v1/ts/stats` 的 `series_count` 增量 **== 5** |
| 7 | 没写过的名字查不到 | 未写入指标名样本数 `== 0` |
| 8 | 重复投递不重复入库 | 重发后 VM 样本数与快照不变、`series_count` 不变 |
| 9 | 重启不影响已落库数据 | `SIGTERM` 退出码 0；重启后 VM 快照与重启前**逐点相同** |

**关于 `accepted`**：重复记录**仍会** `+1`（`apply_one` 里去重命中走 `Ok(())`），所以第 8 条
只把 `accepted` 当作「请求被受理」的旁证，去重的硬证据是 VM 样本数/序列数不变。

**关于第 6 条的口径**：VM 的 `series_count` 是 `status/tsdb` 的 `totalSeries`，**整个实例**的
总量，不是本用例窗口内的量。所以这条 `== 5` 成立的前提是「同一个测试二进制里没有第二个往
同一台 VM 写序列的用例」（cargo 按 target 串行跑，二进制之间不并发）。以后往 `it_vm_ingest.rs`
里加用例时必须串起来考虑，否则这条会因别的序列而假红。

**反向验证（写这条用例时实测过）**：把写入条数从 5 改成 6（`metric_series(..., COUNT + 1, ...)`），
立即失败：
```
assertion `left == right` failed: 首轮 ingestion 应全部受理: {"accepted":6,"batch_id":"batch-1","failures":[],"status":"ok"}
  left: Some(6)
 right: Some(5)
```
说明断言不是恒真；还原后重新通过。

## 清理方式

`tempfile::TempDir` 在测试结束（含 panic）时删目录；`Dataserver` 的 `Drop` 里 `kill` + `wait`，
第 9 步已经 `SIGTERM` 等它退出过，`drop` 只是兜底。VM 上的序列带 `tp-vm-ingest-<pid>` 前缀，
不清理也不会与其它用例冲突（同一 pid 复用才会撞名，此时值相同不影响判定）。

## 关联需求或事故编号

- 「落进 VM」这条证据链的规范：`test_plan/README.md` 第 6 条规矩（只认 VM 原生查询口）。
- 与 `IT-EXT-REDIS-VM-01` 互补：那条验「Redis KV 语义 + VM 写入/查询/删除 + 与 tsink 同点同结果」，
  这条验「接入线程 → 批量刷写器 → VM」这一段，且不依赖 Redis。
- 指标名不能带 `-`：VM 对 `invalid metric name` 返回 400，`util::metric_name` 就是为此抽出来的。
