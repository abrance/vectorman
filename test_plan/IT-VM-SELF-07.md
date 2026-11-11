# IT-VM-SELF-07 eBPF 边保留期删除自监控（`dataserver_ebpf_edges_deleted_total`）→ VictoriaMetrics 直查

`ID: IT-VM-SELF-07` ／ `依赖: 真实 VictoriaMetrics（DP_E2E_VM_*）+ 真 gse-server` ／
`命令: cargo test -p dataserver --test it_vm_self_ebpf_edges -- --nocapture` ／ `负责人: mike`

`main.rs` 的全局清理循环每轮跑完 `run_cleanup` 后，只有**真删掉了 eBPF 边记录**
（`report.ebpf_edges_deleted > 0`）才 `inc_counter("dataserver_ebpf_edges_deleted_total")`。
这条序列此前只有「读代码判断可覆盖」的记录、没有任何用例：它要求 live 采集项列表（`gse_admin_url`
+ `kind` 以 `ebpf` 开头）、过期边、以及一个完整 `ts_clean_interval_secs` 周期三者同时成立。

本用例把「计数器出现」与「边真的被删」两件事分开断言，避免出现「计数 +1 但数据还在」
（或反过来「数据没了、计数没动」）这种自监控失真：

1. ingest 后先证明边真在库里（否则清理删 0 行、计数器永远是 0，绿就没有意义）；
2. 真等一个 60s 清理周期，VM 上 `dataserver_ebpf_edges_deleted_total{instance=…} >= 1`；
3. 同周期的 `dataserver_ts_clean_runs_total >= 2`（启动 tick + 删到边的那一轮），证明计数来自
   清理循环本身，而不是别处塞进来的；
4. 删完之后边检索归零（计数器是「删了几行」的证据，不是「跑过一轮」的证据）。

## 前置数据

| 项 | 值 |
|---|---|
| 环境变量 | `DP_E2E_VM_INSERT_URL`（必填，缺失即 panic 点名）、`DP_E2E_VM_SELECT_URL`（缺省回落 insert）、`DP_E2E_VM_SINGLE_NODE=1` |
| 被测进程 | `target/debug/dataserver`（`env!("CARGO_BIN_EXE_dataserver")`），配置写在 `TempDir` |
| 台账进程 | 真 `gse-server`（`Gse::start`，临时目录 + 随机端口；`build_bins()` 自动 `cargo build -p gse-server -p gse-agent`） |
| 采集项 | `PUT /api/gse/agents/{agent}/spec`：1 条 `kind=ebpf_network`、`enabled=true`、`storage.retention_days=3`（eBPF 缺省值，显式写出来是为了不靠猜测）；`item_id` **必须**等于边记录的 `data_id` |
| 触发数据 | 1 条边：`bucket_start = floor_minute(now) - 4 天`（超过 3 天保留期），`src_service`/`dst_service` 带 `util::unique_prefix()` 隔离 |
| 后端 | 时序 = 真实 VictoriaMetrics；KV = 本地 `redb`；日志 = 本地 tantivy；SQL（边台账）= 本地 sqlite；APM 打开（`/v1/edges/search` 在 `apm_enabled=false` 时返回 503「apm is disabled」） |
| 自监控 | `self_metrics_interval_secs = 1` |
| 清理周期 | `ts_clean_interval_secs = 60`（产品侧下限就是 60s，写更小也按 60s 跑） |
| 隔离口径 | VM 查询带 `instance="<本进程 SQL 口地址>"`；边检索用 `src_service` + 显式时间窗 |

## 执行步骤

1. `vm::Vm::require()` + `wait_ready`；`Gse::start(<tmp>/gse)` 起真台账。
2. `Options::local().with_vm(&vm).with_apm(1).with_gse_admin(gse.admin_url()).with_self_metrics(1).with_ts_clean(60)`
   起真 dataserver。
3. `gse.register_agent(...)` 后 `gse.put_spec(...)` 下发 `ebpf_network` 采集项（真实 spec，不塞假 live 列表）。
4. `POST /v1/ingest` 一条 4 天前的 `ebpf_edges` 批（`data_id = item_id`），`ingest_ok` 断言无失败记录。
5. `POST /v1/edges/search`（`source=ebpf`、`src_service`、`from_ts/to_ts` 覆盖 4 天前那个桶）断言
   `total == 1` —— **前置**：边真的进库了。时间窗必须显式给：接口缺省只查最近 1 小时，
   用缺省窗口会把「在库里」误判成「没有」。
6. `self_metrics::wait_at_least(vm, dataserver_ebpf_edges_deleted_total{instance}, 1.0, 180s)`：
   真等一个完整清理周期（启动 tick 时边还没进库，真删发生在一个周期之后）。
7. `self_metrics::wait_at_least(vm, dataserver_ts_clean_runs_total{instance}, 2.0, 180s)`。
8. 再查一次边检索，断言 `total == 0`。
9. 负向断言：`dataserver_ts_clean_errors_total` 不该出现（懒创建：没 +1 就没有序列），
   且 stdout 的 `retention cleanup:` 行里有 `ebpf_edges_deleted=` 字段（证明真走到边保留分支）。

## 期望结果

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | spec 真的下发成功 | `PUT .../spec` 返回 2xx（夹具内部断言），台账列表能看到该 `item_id` |
| 2 | 边真的进库 | `POST /v1/edges/search` 在显式时间窗内 `total == 1` |
| 3 | 删除计数落 VM | `dataserver_ebpf_edges_deleted_total{instance="<本进程 SQL 口>"} >= 1` |
| 4 | 计数来自清理循环 | 同周期 `dataserver_ts_clean_runs_total{instance=…} >= 2` |
| 5 | 边真的被删 | 同一时间窗再次检索 `total == 0` |
| 6 | 没故障 | `/metrics` 上没有 `dataserver_ts_clean_errors_total`；stderr 无 `retention cleanup: ` 报错；stdout 有 `ebpf_edges_deleted=` |

**反向验证（实测）**：把第 3 步的期望从 `1.0` 改成 `2.0`（本用例只造了一条过期边，计数不可能到 2），
重跑红输出：
```
thread 'ebpf_edges_deleted_total_lands_in_victoriametrics' panicked at crates/test-support/src/util.rs:41:13:
等待 VM 查询 dataserver_ebpf_edges_deleted_total{instance="127.0.0.1:36133"} 满足断言 超时（180s）
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 181.17s
```
说明断言真的在比较值（不是「查得到就绿」）。还原成 `1.0` 后重跑通过（约 92s）。

## 清理方式

`TempDir` 随作用域删除（dataserver 数据目录、gse-server 的 sqlite 台账都在里面）；
dataserver / gse-server 在各自的 `Drop` 里 `kill` + `wait`。
VM 上的自监控序列按 `instance="127.0.0.1:<随机端口>"` 隔离，不需要也无法主动删除；
过期边本来就是要被删掉的对象，删完即 0 行。

## 关联需求或事故编号

- 实现：`bins/dataserver/src/main.rs`（清理循环里 `report.ebpf_edges_deleted > 0` 才
  `inc_counter("dataserver_ebpf_edges_deleted_total")`）、`bins/dataserver/src/cleanup.rs::run_cleanup` /
  `apply_retention`（`LiveItem::is_ebpf()` → `ebpf_retention::delete_edges_before`）、
  `crates/dataplane-apm/src/ebpf_retention.rs`（eBPF 缺省保留 3 天、按 `bucket_start` 分批删）。
- 需求 14.1-14.3：eBPF 采集项保留期与边清理的自监控。
- 与 `IT-VM-SELF-04` 互补：那条验 eBPF **聚合**与清理**轮次**，这条验清理**真删了边**并计数；
  `IT-VM-SELF-04` 的「不可达项」表已把本名字移出（可覆盖，只是需要 gse 真进程链）。
- 口径注意：`dataserver_ebpf_edges_deleted_total` 是**行数**累加（一批可能 > 1），不是「轮次」；
  「跑了几轮」看 `dataserver_ts_clean_runs_total`。
