# IT-VM-SELF-02 trace 接入限流自监控（`dataserver_apm_ingest_throttled_*`）→ VictoriaMetrics 直查

`ID: IT-VM-SELF-02` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_*）` ／
`命令: cargo test -p dataserver --test it_vm_self_throttle -- --nocapture` ／ `负责人: mike`

`http.rs::ingest` 对 trace 批次做写入保护：同一秒内超过 `apm_ingest_max_batches_per_sec` 的批次
直接回 `429 + unavailable`，并把「被挡下的批次数 / 被挡下的记录数」记进
`dataserver_apm_ingest_throttled_batches_total` / `_throttled_records_total`。
这两个计数器此前只在 `/metrics` 上被间接依赖，没有任何用例证明它们**真被触发过**、也证明不了
**它们也进了 VM**（Agent 侧的限流告警就是查 VM 里的这两条）。

`SelfMetrics` 的计数器是懒创建的：没有真实限流就没有序列。所以本用例不塞值，而是真发到被 429 挡下。

## 前置数据

| 项 | 值 |
|---|---|
| 环境变量 | `DP_E2E_VM_INSERT_URL`（必填，缺失即 panic 点名）、`DP_E2E_VM_SINGLE_NODE=1` |
| 被测进程 | `target/debug/dataserver`，配置写在 `TempDir` |
| APM | `apm_enabled = true`（限流器对 trace 分支生效，且 span 要能进接入路径） |
| 限流 | `apm_ingest_max_batches_per_sec = 1`（同一秒的第二批必然 429；夹具新增 `Options::with_apm_ingest_limit`，默认 0 = 不限，与改动前一致） |
| 自监控 | `self_metrics_interval_secs = 1`（必须 > 0，否则 `LocalTsSink` 根本不会被调用） |
| 后端 | 时序 = 真实 VictoriaMetrics；KV = `redb`；日志 = tantivy |
| 隔离口径 | 所有 VM 查询都带 `instance="<本进程 SQL 口地址>"`，与并行实例的同名序列分开 |

## 执行步骤

1. `vm::Vm::require()` + `wait_ready`，用 `Options::local().with_vm(&vm).with_apm(1).with_self_metrics(1).with_apm_ingest_limit(1)` 起真进程。
2. 以**每批 1 条 span** 的 trace 信封循环 `POST /v1/ingest`（最多 60 次），直到出现第一个 `429`。
   不能只发 2 批就断言：批限流是固定秒窗，跨秒边界会放行。
3. 断言 `429` 的报文里含 `throttled`（证明进的是限流分支，而不是别的拒绝路径），记录实测 429 次数。
4. `GET <metrics 口>/metrics`，取两个计数器的值：应等于 429 次数（每批 1 条记录，两个计数器必然相等，
   互为印证）。
5. 在 VM 原生口按 `instance` + 指标名查询，等值等于第 4 步的数字；并断言样本的
   `component="dataserver"`、`instance` 等于本进程 SQL 口地址。

## 期望结果

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 限流真被触发 | 循环里出现 `429`，且报文含 `throttled` |
| 2 | 批次数口径正确 | `/metrics` 的 `_throttled_batches_total` == 实测 429 次数 |
| 3 | 记录数口径正确 | `/metrics` 的 `_throttled_records_total` == 实测 429 次数（1 批 1 记录） |
| 4 | 落 VM | VM 原生口按 `instance` 查到两条序列，值一字不差 |
| 5 | 标签未漂 | VM 样本保留 `component=dataserver`，`instance` == 本进程 SQL 口地址 |

**反向验证（实测）**：把第 2 条断言的期望改成 `rejected + 1` 后重跑，红输出：
```
thread 'apm_ingest_throttle_self_metrics_land_in_victoriametrics' panicked at bins/dataserver/tests/it_vm_self_throttle.rs:99:5:
自监控的限流批次数应等于实测 429 次数 1，实际 1
test result: FAILED. 0 passed; 1 failed
```
说明这条断言不是恒真（把期望改一位就红）。还原后重跑通过。

## 清理方式

`TempDir` 随作用域删除；dataserver 在 `Dataserver::Drop` 里 `kill` + `wait`。
VM 上的序列按 `instance="127.0.0.1:<随机端口>"` 隔离，不需要也无法主动删除（无删除鉴权，且
时间窗删除在 VM 后端被明确跳过）。

## 关联需求或事故编号

- 实现：`bins/dataserver/src/http.rs::ingest`（trace 分支的 `apm_limiter` → 429 + 两个计数器）、
  `bins/dataserver/src/limits.rs::BatchLimiter`（固定秒窗）、`bins/dataserver/src/main.rs::AppState`
  （`apm_limiter` 恒存在，`max_per_sec = 0` 即不限）。
- 设计依据：`/.monkeycode/specs/apm-tracing/requirements.md` Requirement 10（采样在应用侧、
  dataserver 只做写入保护：超限 429 由 Agent 退避重试，不丢批）。
- 与 `IT-VM-SELF-01` 互补：那条验「自监控文本 == VM 值」，这条验**限流这条业务路径**产出的
  两个计数器确实被触发并落到 VM。
