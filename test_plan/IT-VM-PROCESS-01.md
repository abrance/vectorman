# IT-VM-PROCESS-01 进程指标的 `service` 维度补全 → VictoriaMetrics 直查

`ID: IT-VM-PROCESS-01` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_*）` ／
`命令: cargo test -p dataserver --test it_vm_process -- --nocapture` ／ `负责人: pit`

Agent 上报的进程指标只有 `process_name`（进程名），`service` 是 dataserver 按静态映射
（`apm_service_alias`，**只对 `ebpf_process_*` / `ebpf_syscall_*` 生效**）补出来的。
VM 的序列身份包含全部标签：补不上就是另一条序列，前端「按服务聚合」会整片空——
所以必须验「补出来的维度跟着点落进了 VM」，而不是只在服务端内存里出现过。

## 前置数据

| 项 | 值 |
|---|---|
| 环境变量 | `DP_E2E_VM_INSERT_URL`（必填，缺失即 panic 点名）、`DP_E2E_VM_SELECT_URL`（缺省回落 insert）、`DP_E2E_VM_SINGLE_NODE=1` |
| 被测进程 | `target/debug/dataserver`（`env!("CARGO_BIN_EXE_dataserver")`），配置现写在 `TempDir` |
| 后端 | 时序 = 真实 VictoriaMetrics；KV = 本地 `redb`；日志 = 本地 tantivy |
| APM | `apm_enabled = true`（`MetricSink` 只在 APM 打开时挂在接入路径上） |
| 隔离前缀 | `tp-proc-{pid}`：`{p}-proc`（建别名的进程）、`{p}-svc`（映射到的服务）、`{p}-unmapped`（没建别名的进程） |
| 别名 | `POST /v1/apm/service-aliases`：`match_kind=process_name`、`match_value={p}-proc`、`service={p}-svc` |
| 写入内容 | 两条 `ebpf_process_cpu_usage`（必须这个前缀才触发补全）：进程名 `{p}-proc` 值 42.0；进程名 `{p}-unmapped` 值 7.0。时间戳 `floor_minute(now) - 120s` |

## 执行步骤

1. `vm::Vm::require()` + `wait_ready`；起 `Options::local().with_vm(&vm).with_apm(1)`。
2. `apm::create_alias(&ds, "process_name", &process, &service)`（非 201 直接失败并打印应答）。
3. 一次 `POST /v1/ingest`（`data_type=metrics`）写入两条记录（`ingest_ok` 断言 `failures` 为空）。
4. 在 VM 原生口断言：
   - `ebpf_process_cpu_usage{service="{p}-svc",process_name="{p}-proc"}` == 42.0，
     且点的 `pid` 仍是 `4242`（补维度不能挤掉原始标签）；
   - `ebpf_process_cpu_usage{service="unknown-{p}-unmapped",process_name="{p}-unmapped"}` == 7.0；
   - `ebpf_process_cpu_usage{service=""}` 查不到任何序列；
   - `ebpf_process_cpu_usage{process_name=~"{p}-.*"}` 恰好 2 条序列（两条记录没有互相覆盖）。

## 期望结果

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 命中别名时补出服务维度 | VM 上 `{service="{p}-svc",process_name="{p}-proc"}` 的值 == 42 |
| 2 | 原始标签不被覆盖 | 同一点 `pid == "4242"` |
| 3 | 未命中补 `unknown-<process_name>` | `{service="unknown-{p}-unmapped"}` 的值 == 7 |
| 4 | 不落空服务维度 | `{service=""}` 结果为空 |
| 5 | 两条记录两条序列 | `process_name=~"{p}-.*"` 的样本数 == 2 |

**反向验证（落地时实测）**：把第 1 条的期望值从 `42.0` 改成 `43.0`，用例 60s 超时失败：

```
等待 VM 查询 ebpf_process_cpu_usage{service="tp-proc-…-svc",process_name="tp-proc-…-proc"} 满足断言 超时（60s）
test process_metric_service_dimension_lands_in_victoriametrics ... FAILED
```

改回 `42.0` 后恢复绿色。

## 清理方式

`TempDir` 随作用域删除（含 `redb` 里的别名表）；dataserver 在 `Dataserver::Drop` 里 `kill` + `wait`。
VM 侧序列名带 `tp-proc-{pid}`，不清理也不影响后续运行。

## 关联需求或事故编号

- 实现：`crates/dataplane-apm/src/sink.rs`（`SERVICE_MEASUREMENT_PREFIXES` / `MetricSink::metric_tags`）、
  `crates/dataplane-ingest/src/lib.rs`（接入路径在去重之后补维度、再写 `TimeSeriesStore`）。
- 约定：未命中不丢维度，补 `unknown-<process_name>`（与边记录的 `unknown-<ip>` 同口径）；
  Agent 曾发 `service: ""`，因此「已有 service」判断把空值当没填（`sink.rs` 注释）。
- `dataplane-apm/src/tests.rs` 覆盖同一逻辑的单元语义；这条验的是**真进程 + 真 VM**。
