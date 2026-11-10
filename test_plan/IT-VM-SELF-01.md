# IT-VM-SELF-01 dataserver 自监控快照（`/metrics`）→ VictoriaMetrics 直查

`ID: IT-VM-SELF-01` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_*）` ／
`命令: cargo test -p dataserver --test it_vm_self -- --nocapture` ／ `负责人: mike`

dataserver 的自监控除了暴露 `/metrics`，还会在 `self_metrics_interval_secs > 0` 时把**每一份快照**
（`SelfMetrics::snapshot()`，就是把 `/metrics` 文本解析成样本列表）逐条写进时序后端
（`main.rs` 里 `LocalTsSink { ts }` + `vectorman_metrics::flush_loop`）。ts 后端换成 VM 之后，
这条「自监控也落 VM」的路径此前没有任何用例覆盖：写没写、值对不对、标签有没有被改写，全靠猜。

这条用例把它锁成三条可判定事实：

1. 自监控样本真的进了 VM（VM 原生口查得到，而不是只看 dataserver 自己的 `/metrics`）；
2. VM 里的**值**与 `/metrics` 文本一致（同一条序列，读文本后不再打该路由，值就固定）；
3. VM 里的**标签集与标签值**与文本一字不差（自监控写入口径一旦多/少一个标签，看板会漂）。

## 前置数据

| 项 | 值 |
|---|---|
| 环境变量 | `DP_E2E_VM_INSERT_URL`（必填，缺失即 panic 点名）、`DP_E2E_VM_SELECT_URL`（缺省回落 insert）、`DP_E2E_VM_SINGLE_NODE=1` |
| 被测进程 | `target/debug/dataserver`（`env!("CARGO_BIN_EXE_dataserver")`），配置写在 `TempDir` |
| 自监控 | `self_metrics_interval_secs = 1`（**必须 > 0**，默认 0 等于关掉 `flush_loop`，`LocalTsSink` 根本不会被调用） |
| 后端 | 时序 = 真实 VictoriaMetrics；KV = 本地 `redb`；日志 = 本地 tantivy；APM 关闭 |
| 锚点序列 | `vectorman_http_requests_total{method="GET",path="/health",status="200"}`，另有公共标签 `component="dataserver"`、`instance="127.0.0.1:<SQL 口端口>"`、`data_id="self"` |
| 制造计数 | 先 `GET /health`（SQL 口）3 次，保证值 > 0（0 值区分不了「没写」和「写对了 0」） |

夹具新增 `Options::with_self_metrics(secs)`（默认 0，与改动前 `self_metrics_interval_secs = 0`
逐字段一致，已有用例不受影响）。

## 执行步骤

1. `vm::Vm::require()` + `wait_ready`；`Options::local().with_vm(&vm).with_self_metrics(1)` 起真 dataserver。
2. `GET /health` 三次（SQL 口），把 `GET /health 200` 的请求计数顶起来。
3. `GET <metrics 口>/metrics`，从文本里解析出 `vectorman_http_requests_total{method="GET",path="/health",status="200"}`
   这一行的**完整标签集与值 V**。此后不再请求 `/health`，该序列的值就冻结在 V。
4. `vm.wait_query(<由第 3 步标签拼成的 selector>, None, 60s, |r| 唯一一个样本值 == V)`。
   这一步同时证明「写进去了」与「值没写错」：只证明前者的话，`value` 写错/截断都会漏过。
5. 断言 VM 样本的标签集 == 文本标签 + `__name__`（排序后逐项相等，多一个少一个都失败）。
6. 断言 `instance == ds.base()` 去掉 `http://`（即 SQL 口监听地址）、`component == "dataserver"`、`data_id == "self"`。
7. 断言 `<data_path>/ts` 不存在或为空（证明没回落到本地 tsink）。
8. 断言时序存储状态采样（`main.rs` 里跟自监控同周期的 `storage_stats()` 分支）的六条序列也落 VM：
   `dataserver_ts_series_count`、`_memory_used_bytes`、`_memory_budget_bytes`、`_wal_size_bytes`、
   `_background_errors_total`、`_degraded`。VM 后端下前三者的内存/WAL 口径恒 0（那是本地 tsink 的概念），
   后两者在没故障时恒 0；`_series_count` 是 VM 的**全实例** `totalSeries`，只断言 `>= 1`，不锁绝对值
   （同一台 VM 上还有别的实例在写，锁死会假红）。

## 期望结果

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 自监控样本进 VM | 用文本标签拼出的 selector 在 VM 原生口查到恰好 1 个样本 |
| 2 | 值一致 | 样本值 == 文本值（差值 < 1e-9） |
| 3 | 标签名/值一致 | VM 样本 `(key,value)` 集合排序后 == 文本标签集合 + `("__name__", 指标名)` |
| 4 | 隔离标签正确 | `component=dataserver`、`data_id=self`、`instance=<SQL 口监听地址>` |
| 5 | 没走本地 tsink | `<data_path>/ts` 不存在，或目录条目数 == 0 |
| 6 | 时序存储状态采样 | 六条 `dataserver_ts_*` 均在本进程 `instance` 下查到；`memory_used_bytes`/`memory_budget_bytes`/`wal_size_bytes`/`background_errors_total`/`degraded` 精确等于 0（VM 后端口径），`series_count >= 1` |

**反向验证（写这条用例时实测）**：把等待条件里的期望值改成 `value + 1.0`
（即「VM 里的值比 `/metrics` 多 1」），60s 超时红输出：
```
等待 VM 查询 vectorman_http_requests_total{method="GET",path="/health",status="200",instance="127.0.0.1:42669",component="dataserver",data_id="self"} 满足断言 超时（60s）
```
说明断言不是恒真：只要求「查得到样本」就会绿，把值也锁上才抓得到写入口径漂移。
还原成 `value` 后重跑通过。

第二条反向验证（时序存储状态采样这一组）把 `dataserver_ts_degraded` 的期望从 `0.0` 改成 `1.0`，红输出：
```
thread 'dataserver_self_metrics_land_in_victoriametrics' panicked at bins/dataserver/tests/it_vm_self.rs:206:27:
dataserver_ts_degraded 期望 1，VM 实际 0: [{"metric":{"__name__":"dataserver_ts_degraded","component":"dataserver","data_id":"self","instance":"127.0.0.1:34541"},"value":[1794340675,"0"]}]
test result: FAILED. 0 passed; 1 failed
```
还原后重跑通过（约 31s）。

## 覆盖范围

这条只覆盖「自监控文本 → VM」这条管道本身（含时序存储状态采样的六条 gauge）。APM / eBPF 聚合、
保留期、限流、接入计数各自的序列需要**真跑对应业务路径**才能出现（计数器懒创建），拆在：
`IT-VM-SELF-02`（限流）、`IT-VM-SELF-03`（APM 聚合/刷盘）、`IT-VM-SELF-04`（eBPF 聚合 + 全局清理）、
`IT-VM-SELF-05`（APM 保留期）、`IT-VM-SELF-06`（接入计数）。

仍**不可达、未覆盖**的名字与原因（需要故障注入或后端双形态）集中在 `IT-VM-SELF-04.md` 的
「不可达项」表：`dataserver_apm_agg_errors_total`、`dataserver_apm_flush_errors_total`、
`dataserver_apm_retention_errors_total`、`dataserver_ebpf_agg_errors_total`、
`dataserver_ts_clean_errors_total`、`dataserver_ts_tombstones_applied_total`、
`dataserver_ebpf_edges_deleted_total`、`dataserver_ingest_dedup_pruned_total`。

## 清理方式

`TempDir` 随作用域删除；dataserver 在 `Dataserver::Drop` 里 `kill` + `wait`。
VM 上的自监控序列以 `instance="127.0.0.1:<随机端口>"` 隔离，不需要（也无法在无删除鉴权时）主动删除。

## 关联需求或事故编号

- 实现：`bins/dataserver/src/main.rs`（`self_metrics_interval_secs > 0` 时挂 `LocalTsSink` +
  `flush_loop`；同一条件下的 `storage_stats()` 采样分支）、`bins/dataserver/src/http.rs::LocalTsSink`
  （逐条 `ts.write`，`field_name = "value"`）、`crates/vectorman-metrics/src/lib.rs::flush_loop` /
  `SelfMetrics::snapshot`。
- 与 `IT-VM-INGEST-01` / `IT-VM-AGENT-01` 互补：那两条验「接入/采集数据」落 VM，这条验
  **dataserver 自身的自监控**落 VM；三条都带 `<data_path>/ts` 为空的边界，共同证明没偷偷回落到本地 tsink。
- 已知口径风险（未改产品代码，只记录）：`flush_loop` 每轮把**全量**快照逐条写一次
  （每个 sample 一次 `ts.write`），序列多时写入放大明显；VM 侧同一序列每轮一个新点，
  这些 counter 在 VM 里是「按秒采样的单调值」，跨进程重启会归零（VM 会当成 counter reset）。
