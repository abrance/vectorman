# IT-VM-EBPF-SELF-01 Agent eBPF 自监控（`agent_ebpf_*` + `agent_ebpf_capability`）→ VictoriaMetrics 直查

`ID: IT-VM-EBPF-SELF-01` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_*）` ／
`命令: cargo test -p dataserver --test it_vm_ebpf_self -- --nocapture` ／ `负责人: pit`

eBPF 采集出问题时（内核态限流、map 满、map 读失败、上行缓冲淘汰）**不报错**，只体现在这 12 条
自监控指标上；`agent_ebpf_capability` 则是链路页显示「能用/不能用 + 原因」的唯一来源。两者都由
`gse-agent-ebpf/src/lib.rs:780`（`interval_records`，三个采集循环共用）产出，本用例调的就是它，
所以点数、`record_id` 规则、标签集合与线上一致。

**必须按真实落库形态断言的一条**：`agent_ebpf_capability` 的 `field_name` 是 `available`，但 VM
后端只写 `measurement{tags} value ts`（`crates/dataplane-adapter-victoriametrics/src/lib.rs:195`
`encode_point`），**`field_name` 在 VM 里会丢**。所以这里断言的是「指标名 + value(0/1) + 维度」，
不是 `available` 序列，也不能断言 `field_name` 标签；反过来，用例额外断言 `available` 这条序列
**不存在**，把「有人以为它会落成指标名」这个误解钉死。

**没有起真 eBPF（口径说明）**：TP 环境没有 root/`CAP_BPF`，`capability` 的真实形态就是
`capability_ok=false` + `value=0` + 有 `reason`。本用例上报的就是这条**真实降级路径**，
不是把 `degraded=1` 当成「已降级」来美化。

## 前置数据

| 项 | 值 |
|---|---|
| 二进制 | `target/debug/dataserver`（`env!("CARGO_BIN_EXE_dataserver")`），配置写在 `TempDir` |
| dataserver 后端 | `kv_backend = "redb"`、`ts_backend = "victoriametrics"` |
| VM | `DP_E2E_VM_INSERT_URL` 必填、`DP_E2E_VM_SINGLE_NODE=1` |
| 生成器 | `crates/test-support/src/agent.rs::agent_ebpf_interval_records` → `interval_records` + `EbpfStats::set_cpu` |
| 隔离 | `agent_id = tp-eself-<pid>-agent`、`item_id = tp-eself-<pid>-item`、`host_id = tp-eself-<pid>-host` |
| 计数器值 | 按 `stats_metrics` 的输出顺序固定填 `1..=10`：flushes=1、edges=2、metric_points=3、filtered=4、idle_keys=5、read_errors=6、map_overflow_dropped=7、rate_limited=8、buffer_dropped=9、edges_skipped=10 |
| gauge | `agent_ebpf_cpu_percent = 12.34`（`set_cpu` 内部 ×100 存 1234，读回再 /100）、`agent_ebpf_degraded = 1` |
| capability | `kernel_release = tp-eself-<pid>-kernel`、`kernel_ok=true`、`btf_ok=true`、`capability_ok=false`、`reason = tp-eself-<pid>-no-cap-bpf`、`value = 0` |
| 断言口径 | 全部带 `agent_id` 过滤（本用例唯一前缀），不查任何全局量 |
| 等待上限 | 60s |

## 执行步骤

1. `vm::Vm::require()` + `wait_ready`；起真 dataserver（VM 后端）。
2. `interval_records(...)` 产出一轮上报；**先断言是 13 条**（10 计数器 + 2 gauge + capability），
   少了/多了说明产品的「每轮必报」集合变了。
3. `POST /v1/ingest`（`data_type=metrics`）。
4. 10 条计数器逐条查 `{agent_id=…}`，值必须等于 1..=10 中对应的那个（值互不相同，行串位必红）。
5. `agent_ebpf_cpu_percent` ≈ 12.34（容差 1e-9）、`agent_ebpf_degraded == 1`。
6. capability：用**完整标签选择器**（`agent_id`/`item_id`/`kernel_release`/`kernel_ok`/`btf_ok`/
   `capability_ok`/`reason`）查到 1 个样本，`value == 0`；再断言样本**没有** `field_name` 标签，
   且 `available{agent_id=…}` 查不到序列。
7. `count({agent_id=…})` == 13：少一条说明有点没落库，多一条说明重复上报（同一轮发了两次）。

## 期望结果（可判定）

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 点数 | `interval_records` 产出 13 条 |
| 2 | 10 条计数器 | 每条 1 个样本，值为 1..=10（与 `stats_metrics` 顺序对应） |
| 3 | CPU gauge | `agent_ebpf_cpu_percent` == 12.34（×100 存整数后还原） |
| 4 | 降级标记 | `agent_ebpf_degraded` == 1 |
| 5 | capability 落库形态 | 指标名是 `agent_ebpf_capability`，`value=0`，7 个维度都在（含 `reason`） |
| 6 | `field_name` 不进 VM | 样本无 `field_name` 标签；`available` 序列不存在 |
| 7 | 序列集合稳定 | 该 agent 名下恰好 13 条序列 |

**反向验证（落地时实测）**：把 `CPU_PERCENT` 期望值从 `12.34` 改成 `12.35`（输入仍是 12.34）：

```
thread 'agent_ebpf_self_metrics_land_in_victoriametrics' panicked at bins/dataserver/tests/it_vm_ebpf_self.rs:101:5:
assertion failed: (scalar(&cpu, "agent_ebpf_cpu_percent") - CPU_PERCENT).abs() < 1e-9
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 30.15s
```

改回 `12.34` 后恢复绿色。（反证时特意把「采集侧采样值」写成字面量 `12.34`、把期望值留成常量，
否则两边一起改会得到假绿——第一次就踩了这个坑。）

## 未覆盖部分（明确不可达）

| 项 | 需要什么 | 建议归属 |
|---|---|---|
| 计数器**真实增长**（限流丢弃、map 满、map 读失败、缓冲淘汰真的发生） | root + `CAP_BPF`、能塞满 map / 触发令牌桶的内核环境 | 有特权的压测环境；`gse-agent-ebpf` 单元测试已覆盖计数口径 |
| `capability` 的**通过**形态（`value=1`、无 `reason`） | 内核 ≥ 5.x + BTF + root/CAP_BPF | 在真内核上跑 `gse-agent` 的 e2e 环境 |
| `agent_ebpf_cpu_percent` 与真实 CPU 占用的一致性 | 同上（`cpu::read_self_cpu_ticks` 读 `/proc/self/stat`） | 同上；本用例只验 `set_cpu` → 快照 → 落库这段 |

## 清理方式

`TempDir` 随作用域删除；dataserver 在 `Drop` 里 `kill` + `wait`。
VM 序列按 `agent_id = tp-eself-<pid>-agent` 隔离，无需删除。

## 关联需求或事故编号

- 产品实现：`crates/gse-agent-ebpf/src/lib.rs:780`（`interval_records`）、`:797`（`stats_metrics`）、
  `:720`（`capability_metric`）；落库形态在 `crates/dataplane-adapter-victoriametrics/src/lib.rs:195`。
- 「每轮必报」的原因写在 `interval_records` 的注释里：能力状态是**状态**而不是事件，
  dataserver 按 7 天窗口取每条序列的最后一个样本，哪个循环漏报，那类采集项的能力就会随保留期消失。
- 与 `IT-VM-SELF-01` 区分：那条是 **dataserver 自己**的 `/metrics` 快照落 VM，这条是 **Agent eBPF** 的自监控。
