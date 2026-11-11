# IT-VM-APM-01 OTLP span → `apm_service_*` / `apm_edge_*` 分钟指标 → VictoriaMetrics 直查

`ID: IT-VM-APM-01` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_*）` ／
`命令: cargo test -p dataserver --test it_vm_apm -- --nocapture` ／ `负责人: pit`

RED 指标是 APM 页面的唯一数据源，但它的三个口径都很容易在重构里被悄悄改掉：
**只有 `span_kind=server` 才计入**、**计数按 `status` 拆分而耗时按 `(service, operation)` 合并**、
**分位数用最近秩**。这些在单元测试里能过，但「点到底有没有写进 VictoriaMetrics」只有 VM 自己的
查询口能证明——dataserver 的 `/api/v1/query` 是代理读，写路径坏了它照样返回。

同一条 client span 还会进边聚合：进不了服务 RED（`apm_service_*` 是服务侧），但**一定**派生一条
`apm_edge_*{source=otlp}`——这条路径只能靠 VM 直查证明，见「关联」的三条代码事实。

## 前置数据

| 项 | 值 |
|---|---|
| 环境变量 | `DP_E2E_VM_INSERT_URL`（必填，缺失即 panic 点名）、`DP_E2E_VM_SELECT_URL`（缺省回落 insert）、`DP_E2E_VM_SINGLE_NODE=1` |
| 被测进程 | `target/debug/dataserver`（`env!("CARGO_BIN_EXE_dataserver")`），配置由夹具现写在 `TempDir` |
| 后端 | 时序 = 真实 VictoriaMetrics（`ts_backend = "victoriametrics"`）；KV = 本地 `redb`；日志 = 本地 tantivy |
| APM | `apm_enabled = true`、`apm_agg_interval_secs = 1`（等一个 tick 即可，不用等自然分钟） |
| 隔离前缀 | `util::unique_prefix("apm")` → `tp-apm-{pid}`，落在 `service` 标签上；并行/重复跑互不干扰 |
| 分钟桶 | `bucket = floor_minute(now) - 300s`：桶一定「已关闭」，VM instant 查询也仍在回看窗口内 |
| 写入内容 | 4 条 OTLP span（`collector=otlp`），service 均为 `{prefix}-svc`：`GET /orders` server ok/ok/error，duration 1000/3000/5000 微秒（start 在 bucket+1/2/3s）；`POST /downstream` client **error**，duration 9000（start 在 bucket+3.5s）——client 记 error 是为了让边级错误数也确实落到 VM（边的派生与 status 无关，calls 仍为 1） |
| 查询时刻 | `time = bucket/1e6 + 120`（桶内第 120 秒） |

## 执行步骤

1. `vm::Vm::require()` + `wait_ready`：缺 `DP_E2E_VM_INSERT_URL` 直接 panic 并点名变量，不静默跳过。
2. `Dataserver::start(dir, Options::local().with_vm(&vm).with_apm(1), BIN)`。
3. 用 `data::traces_envelope` 一次 `POST /v1/ingest` 写入那 4 条 span（`ingest_ok` 断言 `failures` 为空）。
4. 等 `apm_service_requests_total{service,operation="GET /orders",span_kind="server",source="otlp",status="ok"}` 在 `time` 上等于 2；断言返回点带 `source=otlp`、`span_kind=server`。
5. 同条件 `status="error"` 等于 1；`apm_service_errors_total`（**独立 measurement**，聚合时对 error 额外写一条）同标签同样等于 1。
6. `apm_service_duration_micros{service,operation="GET /orders",span_kind="server",source="otlp",field=…}`：`avg=3000`（9000/3）、`max=5000`、`p95=5000`（最近秩 `ceil(0.95*3)=3`，排序后第 3 个）；`field` 集合**恰好**是 `avg/p50/p95/p99/max`（多一个少一个都不行），`p50=3000`（`ceil(0.5*3)=2`，与 `max` 的 5000 区分）。
7. `apm_service_requests_total{service,operation="POST /downstream"}` 必须**查不到任何样本**（client span 不进服务 RED）。
8. 同一条 client span 派生边：`apm_edge_requests_total{src_service={service},dst_service="unknown",span_kind="server",source="otlp"}` 等于 1（配对不到 server、也没有 `server.address`/`net.peer.*`，对端兜底 `unknown`，桶关闭后由 `flush()` 补落）。
9. `apm_edge_duration_micros{同上,field="avg"}` 等于 9000：边只有 `avg`/`p95` 两个 field，calls=1 时 `avg` 就是那条 client span 的耗时。
10. 同上的 `field="p95"` 也等于 9000（calls=1 时 p95 退化成单样本）；而 `field="sum"` 必须**查不到样本**——aggregator 根本不写 `sum`，拿它做断言会永远为空。
11. `apm_edge_errors_total{同上}` 等于 1：client span 记 error → 边级错误数一条。
12. `query_range` 在 `start=end=bucket` 求值一次，必须取到 `p95` 的点——即三个 server span 合并成一个点、时间戳是桶起点（instant 查询回的是求值时刻，测不出这一层）。
13. 断言子进程 stderr 里没有 `aggregate failed` / `observe_span failed`。

## 期望结果

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | ok 计数 == 2 | VM `status="ok"` 样本值 2 |
| 2 | error 计数 == 1 | VM `status="error"` 样本值 1 |
| 3 | error 另有独立 measurement | `apm_service_errors_total{…,status="error"}` == 1（不是 `requests_total` 的别名） |
| 4 | 耗时合并口径 | 同一 `(service, operation, span_kind)` 下 `avg=3000`、`max=5000`、`p95=5000`、`p50=3000` |
| 5 | 耗时 field 集合封闭 | `field` 去重排序后恰为 `avg/max/p50/p95/p99` |
| 6 | client span 不计入 | `operation="POST /downstream"` 的查询结果为空 |
| 7 | 点标签可区分来源 | 点带 `source=otlp`、`span_kind=server` |
| 8 | client span 派生边 | `apm_edge_requests_total{src_service=svc,dst_service="unknown",span_kind="server",source="otlp"}` == 1 |
| 9 | 边耗时 field 集合 | `apm_edge_duration_micros{同上}` 只有 `avg`/`p95`：`avg=p95=9000`，`field="sum"` 查不到任何样本 |
| 10 | 边级错误数 | `apm_edge_errors_total{同上}` == 1（client span 是 error） |
| 11 | 点写在分钟桶起点 | `start=end=bucket` 的 `query_range` 能取到点 |
| 12 | 聚合无报错 | stderr 不含 `aggregate failed` / `observe_span failed` |

**反向验证（落地时实测）**：把第 1 条的期望值从 `2` 改成 `3`，用例在 60s 超时后失败：

```
等待 VM 查询 apm_service_requests_total{service="tp-apm-…-svc",operation="GET /orders",span_kind="server",source="otlp",status="ok"} 满足断言 超时（60s）
test apm_service_red_lands_in_victoriametrics ... FAILED
```

改回 `2` 后恢复绿色——说明这条断言不是恒真，抓的正是「ok 计数被多算/漏算」。

边那条同理：把第 8 条的 `1` 改成 `2`（或把对端从 `unknown` 改成 `client`）→ 断言失败，
证明边派生确实是断言在盯，而不是「查询返回空就算过」。

另外四条也是改期望值实测变红的（红输出见 commit message）：`apm_edge_errors_total` 1→2、
边 `p95` 9000→1000、`field="sum"` 的期望从「空」改成「非空」，三条都是 `left == right`
或 `边耗时只有 avg/p95` 断言失败，还原后绿；服务侧 `p50` 3000→5000 则是 60s 超时失败。

## 清理方式

`tempfile::TempDir` 随作用域删除（配置、`redb`、tantivy 索引）；dataserver 子进程在
`Dataserver::Drop` 里 `kill` + `wait`。VM 侧写入的序列名由 `tp-apm-{pid}` 隔离，
残留点不影响后续运行（VM 无删除鉴权时无法按序列删除，属已知限制）。

## 关联需求或事故编号

- 实现：`crates/dataplane-apm/src/aggregator.rs`（`apm_service_*` / 边指标落点）、`crates/dataplane-apm/src/red.rs`（分钟样本与最近秩分位数）、`crates/dataplane-apm/src/edge.rs`（client span → 边）。
- 边派生的三个代码事实（易被误改，断言按它们写）：
  1. `span_kind` 记的是**被调用侧**：`edge.rs` 的 `record()`（`EdgeKey` 与 `observe_edge`）和桶关闭兜底的
     `flush()` 都写死 `"server"`，所以一条 client span 派生的边是 `span_kind="server"` 而不是 `client`。
  2. `apm_edge_duration_micros` **只有 `avg` 和 `p95` 两个 field，没有 `sum`**（`aggregator.rs` 的边样本分支）；
     calls=1 时 `avg` 等于单条 client span 的耗时。查 `field="sum"` 恒为空，不能拿来当断言。
  3. 没有 `server.address` / `net.peer.name` / `net.peer.ip` 时对端兜底为 `unknown`（`edge.rs` 的 `fallback_dst`），
     该桶关闭后由 `flush()` 的第 1 步补落，不是配对那一刻就写。
- 设计：`/.monkeycode/specs/apm-tracing/design.md`「ApmAggregator」。
- 关键取舍：计数按 `status` 拆、耗时按 `(service, operation, span_kind)` 合并——同一
  measurement 下 `field` 不是序列身份，分状态写会互相覆盖（`aggregator.rs` 头部注释）。
- `test_plan/README.md` 第 6 条：「落进 VM」只认 VM 原生查询口。
