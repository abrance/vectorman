# IT-VM-EBPF-TCP-01 eBPF 边 TCP 重传/重置计数 → `ebpf_tcp_retrans_total` / `ebpf_tcp_resets_total` → VictoriaMetrics 直查

`ID: IT-VM-EBPF-TCP-01` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_*）` ／
`命令: cargo test -p dataserver --test it_vm_ebpf_tcp -- --nocapture` ／ `负责人: mike`

`IT-VM-EBPF-01`（pit）覆盖了连接数/字节/失败三个桶，但 `tcp_retrans` / `tcp_resets` 这两个
字段当时**造不出非零样本**：`test-support` 的 `ebpf_edge` 把它俩硬编码成 0。这条补上，并把
聚合侧两个最容易写错的口径锁死：

| 口径 | 代码事实（`crates/dataplane-apm/src/ebpf_metrics.rs`） |
|---|---|
| 0 值边 | `if row.tcp_retrans > 0 { … or_default() += … }` → **不产出点**，VM 里连序列都没有（不是「查到 0」） |
| 计数语义 | 聚合键 `(分钟桶, src_service, dst_service)`；同桶多行**累加**，跨桶**各算各的**（每桶写一个点，不是跨桶累加的 counter） |
| 标签集 | 只写 `src_service` / `dst_service`（`status` / `source` / `reason` 都没有） |

## 前置数据

| 项 | 值 |
|---|---|
| 环境变量 | `DP_E2E_VM_INSERT_URL`（必填，缺失即 panic 点名）、`DP_E2E_VM_SELECT_URL`（缺省回落 insert）、`DP_E2E_VM_SINGLE_NODE=1` |
| 被测进程 | `target/debug/dataserver`（`env!("CARGO_BIN_EXE_dataserver")`），配置写在 `TempDir` |
| 后端 | 时序 = 真实 VictoriaMetrics；KV = 本地 `redb`；日志 = 本地 tantivy |
| APM | `apm_enabled = true`、`apm_agg_interval_secs = 1` |
| 隔离前缀 | `tp-ebpf-tcp-{pid}` → `agent` / `src` / `dst` / `dst-zero` |
| 分钟桶 | `bucket = floor_minute(now) - 300s`，`bucket_next = bucket + 60s` |
| 边 A（桶 1） | src `{p}-src`:`10.33.0.1:41001` → dst `{p}-dst`:`10.44.0.1:8080`，tcp，connections 5，`tcp_retrans=7`、`tcp_resets=2`，字节/耗时全 0，hist `[0,0,5]` |
| 边 B（桶 1） | 同一对 `(src_service, dst_service)`，src_port 换成 `41002`（`record_id` 才能不同、不被去重），connections 4，`tcp_retrans=3`、`tcp_resets=0`，hist `[0,0,4]` |
| 边 C（桶 2） | src `{p}-src`:`10.33.0.1:41001` → dst `{p}-dst`:`10.44.0.1:8080`，connections 2，`tcp_retrans=4`、`tcp_resets=1`，hist `[0,0,2]` |
| 边 D（桶 1，全 0） | src `{p}-src` → dst `{p}-dst-zero`:`10.44.0.2:9090`，connections 2，`tcp_retrans=0`、`tcp_resets=0` |
| 查询时刻 | `at = bucket + 30s`、`at_next = bucket_next + 30s`（**必须落在本桶内**，见「期望结果」第 6 条） |

造数用新增夹具 `data::ebpf_edge_tcp(…, tcp_retrans, tcp_resets)`；`data::ebpf_edge` 的签名与语义
不动（现在内部转调 `ebpf_edge_tcp(…, 0, 0)`，产出的 JSON 与改动前逐字段相同，已有 3 个用例继续用它）。

## 执行步骤

1. `vm::Vm::require()` + `wait_ready`；`Options::local().with_vm(&vm).with_apm(1)` 起真 dataserver。
2. `apm::wait_watermark(&ds)`：等第一轮 tick 写出 `ebpf_metrics_watermark`
   （聚合器首次运行把游标对齐到当前已关闭的分钟且**不回溯**）。
3. `POST /v1/ingest`（`data_type=ebpf_edges`）写入桶 1 的两条边 + 桶 2 的一条边 + 全 0 的那条边。
4. **再** `apm::rewind_ebpf_watermark(&ds, bucket - 60s)`。顺序不能反：先拨游标再 ingest 时，
   中间插进一轮 tick 会「扫到 0 行也把游标推到上界」，这批边就永久落在游标后面。
5. 在 `at` 上断言（全部走 VM 原生口）：
   - `ebpf_tcp_retrans_total{src,dst}` == 10（7+3，同桶累加）、`ebpf_tcp_resets_total{src,dst}` == 2；
   - 两个样本的标签集**恰好**是 `__name__` / `src_service` / `dst_service`；`src_service`、`dst_service` 值逐个相等。
6. 在 `at_next` 上断言：同两条序列 == 4 / 1（若是跨桶累加的 counter 这里会是 14 / 3）。
7. 0 值边：`{…dst_service="{p}-dst-zero"}` 两个 measurement 都**查不到样本**（而不是值为 0）。

## 期望结果

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 同桶多边累加 | `ebpf_tcp_retrans_total{src,dst}` == 10、`ebpf_tcp_resets_total{src,dst}` == 2 |
| 2 | 标签集只有两个业务维度 | 回读样本 label keys 排序后 == `["__name__","dst_service","src_service"]` |
| 3 | 标签值正确 | `src_service` / `dst_service` 等于构造时的值 |
| 4 | 每桶独立计数 | `at_next` 上 == 4 / 1（不是 14 / 3） |
| 5 | 0 值不产点 | `dst-zero` 两个 measurement 都 `is_empty()`（正样本已落库后再查，排除「还没写进去」） |

**反向验证（写这条用例时实测）**：

1. 把桶 1 的期望值从 `10.0` 改成 `11.0` → 60s 超时，红输出：
```
等待 VM 查询 ebpf_tcp_retrans_total{src_service="tp-ebpf-tcp-1487904-src",dst_service="tp-ebpf-tcp-1487904-dst"} 满足断言 超时（60s）
test ebpf_tcp_retrans_resets_land_in_victoriametrics ... FAILED
```
2. 给期望标签集多加一个 `status` → 立刻失败（证明标签集断言不是恒真）：
```
assertion `left == right` failed: `ebpf_tcp_retrans_total` 的标签集只该有 src_service/dst_service，实际: {"__name__": "ebpf_tcp_retrans_total", "dst_service": "tp-ebpf-tcp-…-dst", "src_service": "tp-ebpf-tcp-…-src"}
  left: ["__name__", "dst_service", "src_service"]
 right: ["__name__", "dst_service", "src_service", "status"]
```

**踩坑记录（真实发生）**：第一版把查询时刻写成 `bucket + 120s`，即落在**下一桶之后**。
instant 查询返回「<= time 的最新样本」，于是桶 1 读到的是桶 2 的 4，断言 10 必然超时。
现在两个时刻都取本桶 + 30s。

## 清理方式

`TempDir` 随作用域删除；dataserver 在 `Dataserver::Drop` 里 `kill` + `wait`。
VM 侧序列带 `tp-ebpf-tcp-{pid}` 标签，不需要（也无法在无删除鉴权时）主动删除。

## 关联需求或事故编号

- 实现：`crates/dataplane-apm/src/ebpf_metrics.rs:200-217`（`if > 0` 的产出条件与
  `(bucket, src, dst)` 累加键）、`:285-306`（两个 measurement 的标签）。
- 用户要求「TP 的用例都不要通过前端，提前做好可复用的资源生成函数」：新增
  `crates/test-support/src/data.rs::ebpf_edge_tcp`，后续任何要造重传/重置样本的场景直接复用。
- 与 `IT-VM-EBPF-01` 互补：那条验连接/字节/失败，这条验重传/重置与「0 值不产点」。
