# IT-VM-EBPF-PROC-01 eBPF 进程分钟聚合（`ebpf_process_*_total`）→ dataserver → VictoriaMetrics 直查

`ID: IT-VM-EBPF-PROC-01` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_*）` ／
`命令: cargo test -p dataserver --test it_vm_ebpf_proc -- --nocapture` ／ `负责人: pit`

`ebpf_process_exec_total` / `ebpf_process_exit_total` / `ebpf_process_fork_total` 是 Agent 侧
`ProcessMinuteAccumulator::drain_closed`（`crates/gse-agent-ebpf/src/lib.rs:673`）唯一的进程指标出口：
只吐**已关闭**的分钟桶，`value == 0` 的那一条**不产出点**。这条验的就是这三个真名从 Agent 产出
到 VM 可查这一整段，以及 `pid` / `cgroup_id` / `process_name` 三个维度真的成了 VM 的序列标签。

**为什么不用 `IT-VM-PROCESS-01` 代替**：那条验的是 `service` 维度补全，用的 measurement 是
`ebpf_process_cpu_usage` —— 产品**不产出**这个名字（是合成名）。拿它当覆盖会绿得毫无意义：
三个真名一个都没查过。

**没有起真 eBPF（口径说明）**：`ebpf_process_*` 需要 Agent 先用 `aya` 加载内核态程序并挂
`tracepoint/sched/sched_process_{exec,exit,fork}`，条件见下节「未覆盖部分」。本用例调的是产品
产出函数本身（不是手搓 JSON），再走真 dataserver `/v1/ingest`，所以 **Agent 之外的每一跳都是真进程**。

## 前置数据

| 项 | 值 |
|---|---|
| 二进制 | `target/debug/dataserver`（`env!("CARGO_BIN_EXE_dataserver")`），配置写在 `TempDir` |
| dataserver 后端 | `kv_backend = "redb"`、`ts_backend = "victoriametrics"`（`Options::local().with_vm(&vm)`） |
| APM | **关闭**：`service` 补全只影响 `ebpf_process_*`，本用例不验它，关掉可让标签集合最小化 |
| VM | `DP_E2E_VM_INSERT_URL` 必填（缺则 panic 点名）、`DP_E2E_VM_SINGLE_NODE=1` |
| 生成器 | `crates/test-support/src/agent.rs::ebpf_process_records`，内部调 `ProcessMinuteAccumulator::observe` + `drain_closed` |
| 隔离 | `agent_id = tp-ep-<pid>-agent`、`item_id = tp-ep-<pid>-item`、`host_id = tp-ep-<pid>-host`（信封会覆盖记录自带标签，所以前缀落在信封上） |
| 分钟桶 | `floor_minute(now) - 60s`：`drain_closed` 只吐「已关闭」的桶，取前一个整分钟 |
| 样本 1 | pid 4242、cgroup_id 7、`process_name = tp-ep-<pid>-1`、`(exec, exit, fork) = (3, 1, 2)` |
| 样本 2 | pid 4343、cgroup_id 8、`process_name = tp-ep-<pid>-2`、`(5, 0, 0)` —— exit / fork 应当**查不到序列** |
| 断言口径 | 全部按 `agent_id` + `process_name` 前缀过滤，**不查任何全局量**（同一台 VM 上还有别的用例在写） |
| 等待上限 | 60s（VM 批量 flush 50ms，实测到点 ≈1s） |

## 执行步骤

1. `vm::Vm::require()` + `wait_ready`；起真 dataserver（VM 后端、APM 关闭）。
2. `agent::ebpf_process_records(agent_id, now, samples)` 产出点；**先断言点数为 4**
   （busy 三条 + quiet 只有 exec），这一步红了说明产品侧的「0 值不产出」口径变了。
3. `POST /v1/ingest`（`data_type=metrics`，信封 `agent_id`/`data_id`/`host_id` 都是唯一前缀）。
4. `exec` / `exit` / `fork` 三条各查一次（`{agent_id, process_name="…-1"}`）：值必须分别是 3 / 1 / 2。
5. 取 `exec` 的样本，逐个断言 `pid=4242`、`cgroup_id=7`、`item_id` 等于信封里的值
   （后者证明信封覆盖生效，记录里的旧 `agent_id`/`item_id` 不会留下来）。
6. 边界：`quiet` 进程的 `exit` / `fork` 必须查不到任何序列（0 值跳过），而它的 `exec` 是 5。
7. 计数：`exec{process_name=~"tp-ep-<pid>-.*"}` 恰好 **2** 条序列（两条进程各一条，没有互相覆盖）。
8. `<data_path>/ts` 条目数为 0（ts 后端真的是 VM，不是两边都写）。

## 期望结果（可判定）

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | Agent 侧点数 | `drain_closed` 产出 4 条，`exit`/`fork` 为 0 的那条被跳过 |
| 2 | 三个真名都落库 | `ebpf_process_exec_total`=3、`_exit_total`=1、`_fork_total`=2（同一 `process_name`） |
| 3 | 维度成了序列标签 | 样本带 `pid="4242"`、`cgroup_id="7"`、`process_name` |
| 4 | 信封覆盖记录标签 | `item_id` 等于信封 `data_id`，不是记录里的旧值 |
| 5 | 0 值不产出点 | `quiet` 的 `_exit_total`/`_fork_total` 查不到序列；`_exec_total`=5 |
| 6 | 无交叉覆盖 | `_exec_total{process_name=~"<前缀>-.*"}` 恰好 2 条序列 |
| 7 | 没走本地 tsink | `<data_path>/ts` 条目数 `== 0` |

**反向验证（落地时实测）**：把 `exec` 的期望值从 `3.0` 改成 `4.0`：

```
thread 'ebpf_process_totals_land_in_victoriametrics' panicked at bins/dataserver/tests/it_vm_ebpf_proc.rs:91:9:
assertion `left == right` failed: raw=[{"metric":{"__name__":"ebpf_process_exec_total","agent_id":"tp-ep-1742966-agent","cgroup_id":"7","host_id":"tp-ep-1742966-host","item_id":"tp-ep-1742966-item","pid":"4242","process_name":"tp-ep-1742966-1"},"value":[1794340248,"3"]}]
  left: 3.0
 right: 4.0
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.42s
```

改回 `3.0` 后恢复绿色。顺带证明：VM 把 `container_id=""` / `pod_uid=""` 这类**空标签值丢掉了**
（样本里没有这两个键），所以断言里不能写「标签必须等于空串」。

## 未覆盖部分（明确不可达）

| 项 | 需要什么 | 建议归属 |
|---|---|---|
| 内核态 `sched_process_*` tracepoint 挂载与 per-CPU map 读取 | root + `CAP_BPF`（或 `CAP_SYS_ADMIN`）、内核 ≥ 5.x 且 `CONFIG_DEBUG_INFO_BTF=y`、`/sys/kernel/btf/vmlinux` 可读 | 有特权的 e2e 环境（真内核上跑 `gse-agent`）或 `crates/gse-agent-ebpf` 单元测试 |
| `container_id` / `pod_uid` 反查（`process_context_by_pid`） | 容器运行时 socket / cgroup 反查表，TP 无 | 同上；本用例只用空上下文，标签值空 → VM 不落 |

## 清理方式

`TempDir` 随作用域删除；dataserver 在 `Dataserver::Drop` 里 `kill` + `wait`。
VM 侧序列由 `agent_id = tp-ep-<pid>-agent` 隔离，不清理也不与其它用例冲突。

## 关联需求或事故编号

- 产品实现：`crates/gse-agent-ebpf/src/lib.rs:673`（`drain_closed`）、`:780`（`interval_records`）。
- 夹具：`crates/test-support/src/agent.rs::ebpf_process_records`（调产品函数，不手搓 JSON）。
- 与 `IT-VM-PROCESS-01` 互补：那条验 `service` 维度补全（合成 measurement），这条验三个**真名**落库。
- `process_name` 来自内核 `comm`（`TASK_COMM_LEN` = 16 字节），超长会被截断成**另一个**标签值；
  用例里 `assert!(name.len() < 16)` 挡住这个坑。
