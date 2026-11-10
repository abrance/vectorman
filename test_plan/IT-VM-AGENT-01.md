# IT-VM-AGENT-01 真 agent 采集的主机指标经 dataserver 落进 VictoriaMetrics

`ID: IT-VM-AGENT-01` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_INSERT_URL / DP_E2E_VM_SINGLE_NODE）` ／
`命令: cargo test -p dataserver --test it_vm_agent -- --nocapture` ／ `负责人: mike`

**为什么有这条**：`IT-CHAIN-01` 验的是「agent → dataserver → Prom 能查到」，用的是本地 tsink；
`IT-VM-INGEST-01` 验的是「手造的 HTTP 记录能落进 VM」。两者之间那一段——**真 agent 按采集项
自己采、自己上报、dataserver 以 VM 为 ts 后端写出**——此前没有任何用例覆盖。这一段断了的表现
是「控制台能看历史、查不到新数据」，最容易在换外置时序库时被漏掉。

三个断言各防一种真实故障：

| 断言 | 挡住什么 |
|---|---|
| VM 原生口能查到 `cpu_usage{agent_id,item_id}` 且值在 `0..=100` | 采集没起来 / 上报 URL 错 / 百分比换算坏（`/proc` 解析或两轮快照相减写错） |
| dataserver 代理口能看到同一条序列（标签与值一致） | 写路径与读路径口径漂移（写进 VM 的标签和查询侧拼的标签对不上） |
| `<data_path>/ts` 不存在或为空 | **ts 后端没真的切到 VM**：两边都写会把「VM 写失败」永久掩盖成绿 |

## 前置数据

| 项 | 值 |
|---|---|
| 二进制 | `target/debug/dataserver`（`env!("CARGO_BIN_EXE_dataserver")`）+ 真 `gse-server` / `gse-agent`（夹具内 `cargo build -p gse-server -p gse-agent`） |
| dataserver 后端 | `kv_backend = "redb"`、`ts_backend = "victoriametrics"`（`Options::local().with_vm(&vm)`）、`gse_admin_url` 指向 gse 管理口 |
| VM | `DP_E2E_VM_INSERT_URL` 必填（缺则 panic 点名），`DP_E2E_VM_SINGLE_NODE=1`，`DP_E2E_VM_SELECT_URL` 缺省回落 insert |
| gse-server | 临时 sqlite 台账；`auth_enabled = true`、`heartbeat_interval_secs = 1`、`dataplane_probe_interval_secs = 1` |
| agent | `agent_id = agent-it-vm-agent-<pid>`、`token = it-vm-agent-token-<pid>`、`heartbeat_interval_secs = 1`、OTLP 关闭 |
| 采集项 | 1 条 `metrics_host`，`item_id = item-it-vm-agent-<pid>`，`collector.interval_secs = 1` |
| 隔离 | 指标名是产品固定的 `cpu_usage`（不能改），所以隔离落在 `agent_id` / `item_id` 的 pid 后缀上：同一台机器前后两次运行、或 CI 复用同一个 VM，都看不到对方的序列 |
| 端口 | 全部 `127.0.0.1:0` 现取（gse agent 口 / 管理口 / 自监控口，dataserver 三面） |
| 等待上限 | 120s（agent 首轮快照只记不发，第二轮才出点，≈2s；留足编译与启动抖动） |

## 执行步骤

1. `Vm::require()` + `wait_ready`（`/health`）；缺变量直接失败点名。
2. 起真 `gse-server` 并等 `/health`；起真 `dataserver`（VM 后端 + gse 反代）并等 `/health`。
3. 台账：`POST /api/gse/agents` 登记唯一 agent → `POST /api/gse/dataplanes` 登记数据面
   （`ingest_url = ds.base()`、`query_url = ds.prom()`）→ 轮询到 `status = online`。
4. `PUT /api/gse/agents/{id}/spec` 下发 1 条 1s 的主机指标采集项。
5. 起真 `gse-agent`，轮询到台账 `status = online` 且有心跳，再轮询到 `sync_status = synced`
   且 `applied.items[0].item_id` 等于下发值。
6. 轮询 VM 原生口 `cpu_usage{agent_id="…",item_id="…"}` 到非空（120s），逐样本断言标签与值域。
7. 用 dataserver 代理口 `GET /api/v1/query` 查同一 PromQL，比对「标签集合 + 值」快照。
8. 断言 `<data_path>/ts` 不存在或为空。`<data_path>` 取 `ds.dir()/data`，而 `data_path/ts`
   是 `tsink` 后端的固定子目录名（见 `crates/dataplane-ts/src/lib.rs:533`）。
9. 三个进程都还活着（全程无异常退出）。

## 期望结果（可判定）

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | agent 上线 | 台账 `status == "online"` 且 `last_heartbeat_at` 非空 |
| 2 | 采集项生效 | `sync_status == "synced"` 且 `applied.items[0].item_id == <下发的 item_id>` |
| 3 | 数据进 VM | VM 原生口该 PromQL 样本数 `> 0` |
| 4 | 标签正确 | 每个样本 `agent_id` / `item_id` 等于台账里的值（`==`，不是「非空」） |
| 5 | 值域正确 | 每个样本值落在 `0..=100`（百分比） |
| 6 | 读写口径一致 | 代理口与 VM 的「标签集合 + 值」排序后**完全相同** |
| 7 | 没走本地 tsink | `<data_path>/ts` 不存在，或目录条目数 `== 0` |
| 8 | 全程无异常退出 | 结束时三个子进程 `try_wait() == None` |

**反向验证（写这条用例时实测过）**：把查询里的 `item_id` 改成 `item-it-vm-agent-wrong-<pid>`
（即「期望的采集项写错」），等待立刻变成
```
等待 VM 查询 cpu_usage{agent_id="agent-it-vm-agent-1209783",item_id="item-it-vm-agent-wrong-1209783"} 满足断言 超时（20s）
test result: FAILED. 0 passed; 1 failed; 0 ignored
```
说明断言不是恒真（反证时把等待上限临时降到 20s 以便快速看到红，还原后按 120s 重跑通过）。

## 清理方式

`tempfile::TempDir` 在测试结束（含 panic）时删掉 gse 台账 sqlite、agent 配置、dataserver 数据目录；
三个子进程在夹具的 `Drop` 里 `kill` + `wait`，不留孤儿、不留被占用的 sqlite / redb。
VM 上的序列带 `<pid>` 后缀，不清理也不与其它用例冲突。

## 关联需求或事故编号

- 「落进 VM」的唯一证据是 VM 原生查询口：`test_plan/README.md` 第 6 条规矩。
- 与 `IT-CHAIN-01` 互补：那条走本地 tsink 验「agent → dataserver → Prom」的标签链路，这条把 ts
  后端换成 VM 并额外证明**没有本地 tsink 目录**。
- 与 `IT-VM-INGEST-01` 互补：那条验手造记录落 VM，这条验真 agent 采集落 VM。
- 指标名不能带 `-`（VM 会 400）；`cpu_usage` 是产品固定名，隔离只能靠标签，
  这也是本用例用 `<pid>` 后缀拼 `agent_id` / `item_id` 的原因。
