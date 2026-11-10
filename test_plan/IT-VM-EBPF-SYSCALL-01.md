# IT-VM-EBPF-SYSCALL-01 `ebpf_syscall_duration_micros{field=avg|p95}` 与 `ebpf_syscall_failures_total{op,errno}` → VictoriaMetrics 直查

`ID: IT-VM-EBPF-SYSCALL-01` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_*）` ／
`命令: cargo test -p dataserver --test it_vm_ebpf_syscall -- --nocapture` ／ `负责人: pit`

这两类指标由 **Agent 直接产出**（`crates/gse-agent-ebpf/src/syscall.rs:113` `duration_metrics`、
`:160` `failure_metrics`），dataserver 只补 `service`。本用例调的就是这两个产品函数，验的是
`op` / `field` / `errno` 三个维度真的成了 VM 的序列身份，以及 `p95` 是「直方图槽上界」而不是精确分位数。

**此前的覆盖为什么不算**：只有 `avg` 被查到过，还是 `IT-VM-PROCESS-01` 里为了验 `service` 补全
顺手带的一条，`op` / `field` 两个维度的取值**一次都没断言**。`field` 写反（p95 记成 avg）时，
前端「慢调用 P95」会静默显示成均值，任何现有用例都不会红。

**没有起真 eBPF（口径说明）**：syscall 采集项要挂 `raw_syscalls` tracepoint 并读 per-CPU 直方图 map，
条件见「未覆盖部分」。本用例走「Agent 产出函数 → 真 dataserver `/v1/ingest` → VM」。

## 前置数据

| 项 | 值 |
|---|---|
| 二进制 | `target/debug/dataserver`（`env!("CARGO_BIN_EXE_dataserver")`），配置写在 `TempDir` |
| dataserver 后端 | `kv_backend = "redb"`、`ts_backend = "victoriametrics"` |
| APM | **关闭**：本用例只验 `op`/`field`/`errno`，`service` 补全归 `IT-VM-PROCESS-01` |
| VM | `DP_E2E_VM_INSERT_URL` 必填、`DP_E2E_VM_SINGLE_NODE=1` |
| 生成器 | `crates/test-support/src/agent.rs::ebpf_syscall_duration_records` / `ebpf_syscall_failure_record`；`op` 常量取自 `ebpf-abi`（`SYSCALL_OP_OPENAT`=1、`SYSCALL_OP_READ`=2） |
| 隔离 | `agent_id = tp-es-<pid>-agent`、`item_id = tp-es-<pid>-item`、`host_id = tp-es-<pid>-host` |
| 耗时样本 A | `op=openat`、`process_name = tp-es-<pid>-1`、calls 5、sum 500µs、直方图 5 次全在槽 3 |
| 耗时样本 B | `op=read`、`process_name = tp-es-<pid>-2`、calls 2、sum 900µs、直方图 2 次全在槽 1 |
| 失败样本 | `op=openat`、`errno=13`（EACCES）2 次；`op=openat`、`errno=111`（ECONNREFUSED）1 次 |
| 断言口径 | 全部按 `agent_id` 过滤（每个用例唯一的 pid 前缀），不查任何全局量 |
| 等待上限 | 60s |

`p95` 的算法在 `syscall.rs::histogram_p95`：累积到 95% 的**槽上界** `2^(slot+1)`。槽 3 → 16µs，
槽 1 → 4µs。它是近似值，不能与 OTLP 侧精确 p95 相加。

## 执行步骤

1. `vm::Vm::require()` + `wait_ready`；起真 dataserver（VM 后端、APM 关闭）。
2. `duration_metrics` 产出 A、B 两组（每组 `field=avg` 与 `field=p95` 各一条），`failure_metrics`
   产出两条失败点；**先断言这批是 6 条**（4 + 2），少了说明产品侧口径变了。
3. `POST /v1/ingest`（`data_type=metrics`）。
4. 四条耗时序列逐条断言：`openat/p95=16`、`openat/avg=100`（500/5）、`read/p95=4`、`read/avg=450`（900/2）。
   四个值互不相同，`field` 或 `op` 串位必红。
5. `ebpf_syscall_duration_micros{agent_id=…}` 恰好 4 条序列（2 op × 2 field）——多一条说明
   `field` 写成了同一个值或重复产出。
6. 失败点：`op="openat",errno="13"` == 2、`errno="111"` == 1（errno 是**十进制字符串**，
   不是数值/名称）；`sum(...{op="openat"})` == 3；该 agent 名下共 2 条失败序列。
7. 边界：`op="read"` 没有失败记录 → 查不到序列（`failure_metrics` 对 `count == 0` 返回 `None`）。

## 期望结果（可判定）

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 点数 | 4 条耗时 + 2 条失败 = 6 |
| 2 | `field` 维度区分 | `openat` 上 `p95=16`、`avg=100` 两条独立序列 |
| 3 | `p95` 口径 | 槽 3 → 16µs、槽 1 → 4µs（槽上界 `2^(slot+1)`，不是均值） |
| 4 | `avg` 口径 | `sum/calls`：500/5=100、900/2=450 |
| 5 | `op` 维度区分 | `read` 的 `avg/p95` 与 `openat` 不同序列、不同值 |
| 6 | 序列数 | 该 agent 名下恰好 4 条耗时序列 |
| 7 | `errno` 维度 | `errno="13"`=2、`errno="111"`=1，十进制字符串 |
| 8 | 失败按 op 汇总 | `sum(…{op="openat"})` == 3；共 2 条失败序列 |
| 9 | 0 值不产出点 | `op="read"` 查不到失败序列 |

**反向验证（落地时实测）**：把 `openat` 的 `avg` 期望值从 `100.0` 改成 `101.0`：

```
thread 'ebpf_syscall_metrics_land_in_victoriametrics' panicked at bins/dataserver/tests/it_vm_ebpf_syscall.rs:123:9:
assertion `left == right` failed: raw=[{"metric":{"__name__":"ebpf_syscall_duration_micros","agent_id":"tp-es-1744312-agent","field":"avg","host_id":"tp-es-1744312-host","item_id":"tp-es-1744312-item","op":"openat","process_name":"tp-es-1744312-1"},"value":[1794340284,"100"]}]
  left: 100.0
 right: 101.0
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 30.16s
```

改回 `100.0` 后恢复绿色。

## 未覆盖部分（明确不可达）

| 项 | 需要什么 | 建议归属 |
|---|---|---|
| `raw_syscalls/sys_enter`、`sys_exit` tracepoint 挂载 + per-CPU 直方图 map 读取 | root + `CAP_BPF`、内核 ≥ 5.x、BTF 可用 | 有特权的 e2e 环境（真内核跑 `gse-agent`）或 `gse-agent-ebpf` 单元测试（`syscall.rs` 的 `#[cfg(test)]` 已覆盖差分/求和/直方图） |
| `service` 维度补全（`ebpf_syscall_*` 与 `ebpf_process_*` 共用 `SERVICE_MEASUREMENT_PREFIXES`） | 无特权要求，但要开 APM | `IT-VM-PROCESS-01`（已覆盖，本用例故意 APM 关闭以隔离维度） |
| `slow_io` 事件（路径、阈值） | 内核态慢调用采样 | `IT-VM-EBPF-01` 的原始事件边界 + `syscall.rs` 单元测试 |

## 清理方式

`TempDir` 随作用域删除；dataserver 在 `Drop` 里 `kill` + `wait`。
VM 序列按 `agent_id = tp-es-<pid>-agent` 隔离，无需删除。

## 关联需求或事故编号

- 产品实现：`crates/gse-agent-ebpf/src/syscall.rs:113`（`duration_metrics`）、`:160`（`failure_metrics`）、
  `histogram_p95`（槽上界近似）。
- 夹具：`crates/test-support/src/agent.rs::ebpf_syscall_*`（调产品函数；`op` 用 `ebpf-abi` 常量）。
- 与 `IT-VM-PROCESS-01` 互补：那条只验 `service` 补全，`avg` 是顺路带的；这条把 `op`/`field`/`errno`
  三个维度和 `p95` 口径逐条钉住。
