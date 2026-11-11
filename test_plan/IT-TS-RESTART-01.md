# IT-TS-RESTART-01 本地时序后端重启一致性（SIGTERM → 同 `data_path` 重开）

`ID: IT-TS-RESTART-01` ／ `依赖: 无外部中间件（redb + tsink 本地后端）` ／
`命令: cargo test -p dataserver --test ts_restart -- --nocapture` ／ `负责人: mike`

**为什么有这条**：tsink 的三次竞态修复之后一直没有 reopen 回归测试。运维口径里 WAL 回放
是 4–6 分钟量级（cloud3 实测），也就是说「重启后数据还在」只能靠测试保证，人肉观察不起作用。
`replay_bench.rs` 读的是拷贝出来的 tsink 目录、且带 `#[ignore]`，进不了门禁，覆盖不到
「真进程优雅停机 → 真进程重开」这条路径。

## 前置数据

| 项 | 值 |
|---|---|
| 二进制 | `target/debug/dataserver`（测试用 `env!("CARGO_BIN_EXE_dataserver")` 取，不写死路径） |
| 配置文件 | 临时目录里现写，两次启动**各自一份**（端口每次重新取，避免 TIME_WAIT），但 `data_path` 指向**同一个**目录 |
| 后端 | `kv_backend = "redb"`、`ts_backend = "tsink"`（不依赖 Redis / VictoriaMetrics） |
| 环境变量 | 清空 `DATASERVER_*` / `DP_*` 全部 39 个变量，避免开发机残留把 `dp_data_path` / 端口改掉 |
| 写入内容 | `ts_restart_cpu`（值 0.5…4.5）与 `ts_restart_mem`（值 100.5…104.5），各 5 个点，1 秒间隔，标签 `{src: ts_restart, host: restart-host}`，时间戳取「当下整秒往前退 5 秒」 |
| 端口 | `127.0.0.1:0` 现取，`sql_http` / `prom_http` / `metrics_http` 三个各一个 |

## 执行步骤

1. 起第一个 `dataserver --config <config-1>`（下称 boot#1），轮询 `GET /health` 直到
   `{"status":"ok"}`（上限 60s，超时 panic 并打印子进程 stdout/stderr 尾部）。
2. `POST /v1/ingest` 写入一组 `metrics` 信封：10 条记录（2 条序列 × 5 个点），断言应答
   `accepted == 10`。
3. 记录快照：`GET /v1/ts/stats` 的 `series_count`，以及
   `GET /api/v1/query_range?query=<measurement>&start=<base>&end=<base+4s>&step=1`
   的样本向量（时间戳 + 值，逐点）。
4. 向 boot#1 发 **SIGTERM**（`libc::kill(pid, SIGTERM)`，不用 `Child::kill()` 的 SIGKILL，
   因为要验的就是优雅停机路径），等它自己退出（上限 30s），断言退出码 **0**。
5. 用同一份 `data_path`、新端口起 boot#2，同样等 `/health` 就绪。
6. 轮询快照直到与步骤 3 的 `series_count` 和两条序列的点集**逐点完全相等**
   （上限 120s；WAL 回放慢时只是重试，判定标准不放宽）。
7. `GET /api/v1/query?query=ts_restart_cpu&time=<最后一个点>` 断言即时查询回读到重启前
   最后一个点的值。
8. 再写入一组**值域不同**的点（200.5…204.5，`record_id` 前缀 `after-`），断言：
   新点按原值读回；老窗口内仍然是重启前那份点集（新写入没有覆盖历史）。

## 期望结果（可判定）

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 首次写入被全部接收 | `accepted == 10` |
| 2 | 重启前两条序列各 5 个点 | `len == 5`，值精确等于 0.5…4.5 / 100.5…104.5 |
| 3 | SIGTERM 后进程自己退出 | `status.success()`（退出码 0），30s 内 |
| 4 | 重启后序列数不变 | `series_count` 与重启前**相等**（不是 ≥） |
| 5 | 重启后点集等价 | 两条序列的 `(ts, value)` 向量与重启前**逐点相等** |
| 6 | 即时查询可用 | `result[0].value[1] == 重启前最后一个点的值` |
| 7 | 重启后可继续写 | 新写入 `accepted == 10`，且新点按原值读回 |
| 8 | 历史点未被改动 | 老窗口查询结果与重启前快照相等 |
| 9 | 标签跨重启保留 | 回读的 `metric.src == "ts_restart"`、`metric.host == "restart-host"` |

**反向验证（写这条测试时实测过）**：把 boot#2 的 `data_path` 换成一个空目录，
第 4 条立刻挂成 `series_count 必须与重启前相同（2），实际 0`——说明这些断言不是恒真。

## 清理方式

`tempfile::TempDir` 在测试结束（含 panic）时自动删数据目录与配置文件；两个子进程都在各自
`Server` 的 `Drop` 里 `kill` + `wait`，不留孤儿。失败路径下 panic 前会先打印子进程
stdout/stderr 尾部 40 行。

## 关联需求或事故编号

- tsink 三次竞态修复（reopen 相关）——本测试是它们的回归护栏。
- 运维口径：cloud3 上 WAL 回放 4–6 分钟，重启期间数据不可丢。
- 与 `replay_bench.rs`（`#[ignore]`、读拷贝目录、只做性能对比）互补：那条测回放吞吐，
  这条测「真进程停机 → 重开」的数据一致性，且能进门禁。
