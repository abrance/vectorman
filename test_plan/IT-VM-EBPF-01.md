# IT-VM-EBPF-01 eBPF 边台账 → `ebpf_*` / `apm_edge_*{source=ebpf}` 分钟指标 → VictoriaMetrics 直查

`ID: IT-VM-EBPF-01` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_*）` ／
`命令: cargo test -p dataserver --test it_vm_ebpf -- --nocapture` ／ `负责人: pit`

边指标不在 Agent 上算，而是 dataserver 从 `ebpf_edges` 台账按分钟派生（维度里有
`src_service` / `dst_service`，只有服务端能做反查）。这条验的是「台账 → 指标 → VM」整段，
外加两个最容易看错的口径：**`p95` 是直方图槽上界近似**、**游标不回溯历史**。

## 前置数据

| 项 | 值 |
|---|---|
| 环境变量 | `DP_E2E_VM_INSERT_URL`（必填，缺失即 panic 点名）、`DP_E2E_VM_SELECT_URL`（缺省回落 insert）、`DP_E2E_VM_SINGLE_NODE=1` |
| 被测进程 | `target/debug/dataserver`（`env!("CARGO_BIN_EXE_dataserver")`），配置现写在 `TempDir` |
| 后端 | 时序 = 真实 VictoriaMetrics；KV = 本地 `redb`；日志 = 本地 tantivy |
| APM | `apm_enabled = true`、`apm_agg_interval_secs = 1` |
| 隔离前缀 | `tp-ebpf-{pid}` → `agent` / `src` / `dst-a` / `dst-b` 四个标签值 |
| 分钟桶 | `bucket = floor_minute(now) - 300s`；两条边的 `bucket_start_micros` 都等于它 |
| 边 A | src `{p}-src` → dst `{p}-dst-a`（`10.22.0.1:8080`），connections 5、sent 1000、recv 500、failures 1 / `refused`、duration_sum 50000、duration_max 20000、hist `[0,0,0,5]` |
| 边 B | src `{p}-src` → dst `{p}-dst-b`（`10.22.0.2:9090`），connections 3、sent 2000、recv 600、failures 0、duration_sum 9000、duration_max 12000、hist `[0,0,3]` |
| 查询时刻 | `time = bucket/1e6 + 120` |

**两条边必须用不同的 `dst_service`**：同名标签在 VM 里是同一条序列，断言会互相覆盖。

## 执行步骤

1. `vm::Vm::require()` + `wait_ready`；起 `Options::local().with_vm(&vm).with_apm(1)`。
2. `apm::wait_watermark(&ds)`：等第一轮 tick 把 `ebpf_metrics_watermark` 写出来
   （聚合器首次运行会把游标对齐到当前已关闭的分钟且**不回溯**）。
3. `POST /v1/ingest`（`data_type=ebpf_edges`）写入两条边。
4. **再** `apm::rewind_ebpf_watermark(&ds, bucket - 60s)` 把游标拨回去。
   顺序不能反：先拨游标再 ingest 时，中间插进一轮 tick 会「扫到 0 行也把游标推到上界」，
   我们那批边就永远落在游标后面，再也不会被聚合（实测踩过）。
5. 在 `time` 上断言（全部走 VM 原生口）：
   - `ebpf_edge_connections_total{src,dst-a,dst_port=8080}` == 5、`{…dst-b,dst_port=9090}` == 3；
   - `ebpf_edge_bytes_total{src,dst-a,direction="sent"}` == 1000、`direction="recv"` == 500（点带 `protocol="tcp"`）；
   - `ebpf_tcp_failures_total{src,dst-a,reason="refused"}` == 1；
   - `apm_edge_requests_total{src,dst-a,source="ebpf"}` == 5、`{…dst-b}` == 3，且点**没有** `span_kind` 标签
     （VM 不落空标签值；OTLP 侧服务边一定是 `span_kind=server`，这就是两路不混线的证据）；
   - `apm_edge_errors_total{src,dst-a,source="ebpf"}` == 1；
   - `apm_edge_duration_micros{src,dst-a,source="ebpf"}`：`avg=10000`（50000/5）、`max=20000`、`p95=16`（累计到槽 3 达 95%，槽上界 `2^4`）；dst-b：`avg=3000`（9000/3）、`p95=8`（槽 2，`2^3`）。
6. 边界：`POST /v1/ingest`（`data_type=ebpf`）写入一条原始事件（唯一 `record_id`/`process_name`/`event_type`）→
   `POST /v1/ebpf/events/search {"event_type": …}` 必须查到它；
   `POST /v1/edges/search {"src_service": …, "source": "ebpf"}` 的 `total` 与写入事件前**相等**（=2）；
   13 条指标查询的**完整样本（标签+值+时间戳）**与事件前逐条相等。

## 期望结果

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 连接数按 `(src,dst,dst_port)` 分组 | dst-a=5、dst-b=3 |
| 2 | 字节按方向拆分 | dst-a sent=1000、recv=500 |
| 3 | 失败按 `reason` 分组 | dst-a `refused`=1；dst-b 无 failures → 查不到点 |
| 4 | 请求/错误并入 APM 同名 measurement | `apm_edge_requests_total{source="ebpf"}`=5/3、`apm_edge_errors_total`=1 |
| 5 | 耗时口径 | avg = sum/connections、max = 桶内最大、p95 = 直方图槽上界（16 / 8） |
| 6 | 原始事件只进日志引擎 | 事件查得到；边 `total` 不变；指标逐条不变 |

**反向验证（落地时实测）**：把 `connections` 的期望值从 `5` 改成 `6`，用例 60s 超时失败：

```
等待 VM 查询 ebpf_edge_connections_total{src_service="tp-ebpf-…-src",dst_service="tp-ebpf-…-dst-a",dst_port="8080"} 满足断言 超时（60s）
test ebpf_edge_metrics_land_in_victoriametrics ... FAILED
```

改回 `5` 后恢复绿色。

## 清理方式

`TempDir` 随作用域删除；dataserver 在 `Dataserver::Drop` 里 `kill` + `wait`。
VM 侧序列名带 `tp-ebpf-{pid}`，不需要（也无法在无删除鉴权时）主动删除。

## 关联需求或事故编号

- 实现：`crates/dataplane-apm/src/ebpf_metrics.rs`（`aggregate()` / `histogram_p95()` / 游标）、
  `crates/dataplane-apm/src/ebpf_edge.rs`（边落库与服务名反查）。
- 「游标不回溯历史」的单元语义由 `dataplane-apm` 覆盖；这条验的是**聚合结果真落进 VM**。
- `histogram_p95` 是近似值（槽上界低估长尾），前端 tooltip 必须标注，不能与 OTLP 侧精确 p95 相加。
