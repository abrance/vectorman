# IT-DEDUP-01 接入去重的键数上界与清理路径（真进程 → 直读 `kv.redb` → `prune_prefix_before`）

`ID: IT-DEDUP-01` ／ `依赖: 无外部中间件（redb + tsink 本地后端）` ／
`命令: cargo test -p dataserver --test it_dedup -- --nocapture` ／ `负责人: mike`

**为什么有这条**：`ingest/{record_id}` 去重键只写不删就会单调增长。v1.3.15 线上 11 分钟里
`ingest/` 键数从 199 涨到 3942——`run_cleanup` 里 `apply_ts_retention(...)?` 是 `?` 传播，
一项报错就让同轮的去重键 `prune_prefix_before` 整个跳过，键数不再收敛，趋近 Redis
`maxmemory 2gb`（`noeviction`）后就是接入 5xx。这条路径此前只有「监控上看键数」这种事后手段，
没有任何可判定的回归断言。

**为什么不能只验 HTTP 侧**：`IngestReply.accepted` 对重复记录**仍然 +1**（`apply_one` 里重复
走 `Ok(())`）；而 tsink 对「同序列同时刻」是覆盖写，重复投递在点数上也看不出来。所以
「点数没涨」「accepted 正常」都**不足以**证明去重在生效。唯一直接的证据是去重键本身：
停机后打开同一个 `kv.redb`，`scan_prefix("ingest/")` 数出来。

## 前置数据

| 项 | 值 |
|---|---|
| 二进制 | `target/debug/dataserver`（测试用 `env!("CARGO_BIN_EXE_dataserver")` 取，不写死路径） |
| 配置文件 | 临时目录里现写一份；`data_path` 指向同一个临时目录（目录模式下 kv = `<data_path>/kv.redb`） |
| 后端 | `kv_backend = "redb"`、`ts_backend = "tsink"`（不依赖 Redis / VictoriaMetrics） |
| 环境变量 | 清空 `DATASERVER_*` / `DP_*` 全部 39 个变量，避免开发机残留把 `data_path` / KV 后端改掉 |
| 清理周期 | `ts_clean_interval_secs = 3600`：本测试只跑几秒，避免后台清理提前把 `ingest/` 键删掉 |
| 写入内容 | 测量 `it_dedup_gauge`，3 条唯一 record（`dedup-r0/r1/r2`），标签 `{src: it_dedup, host: dedup-host}`，值为 0.5 / 1.5 / 2.5，时间戳「当下整秒往前退 3 秒」起、1 秒间隔 |
| 重复轮数 | 5 轮；每轮的记录、测量、值、时间戳**完全相同**，只换 `batch_id`（模拟 Agent 换批次号重发） |
| 端口 | `127.0.0.1:0` 现取，`sql_http` / `prom_http` / `metrics_http` 各一个 |

## 执行步骤

1. 起 `dataserver --config <config>`，轮询 `GET /health` 直到 `{"status":"ok"}`（上限 60s，
   超时 panic 并打印子进程 stdout/stderr 尾部 40 行）。
2. 第 1 轮：`POST /v1/ingest` 写入 3 条记录，断言 `accepted == 3`；轮询
   `GET /api/v1/query_range?query=it_dedup_gauge&start=<base>&end=<base+2s>&step=1`
   直到回读到 3 个点（上限 30s），断言点集**精确等于** `[(base,0.5),(base+1s,1.5),(base+2s,2.5)]`；
   记录 `GET /v1/ts/stats` 的 `series_count`。
3. 第 2…5 轮：重发**同一批**记录（`batch_id` 不同、`record_id` 相同），每轮断言：
   `accepted == 3`（**注意：这不是去重证据**，当前实现对重复记录仍 +1）、`status == "ok"`、
   点数仍是 3 且点集与第 1 轮完全相等、`series_count` 与第 1 轮相同。
4. 向进程发 **SIGTERM**（`libc::kill`，不用 SIGKILL，因为下一步要拿 `kv.redb` 的文件锁），
   等它自己退出（上限 30s），断言退出码 **0**。
5. 断言停机后 `<data_path>/kv.redb` 存在，用 `dataplane_kv::RedbKvStore::new` 打开它
   （打不开就 panic 并点名路径），`scan_prefix(b"ingest/")`：
   - 键数**恰好 3**，`record_id` 集合逐个等于 `{dedup-r0, dedup-r1, dedup-r2}`；
   - 每个值都是 **8 字节小端微秒**且落在接入窗口内（写空值会被清理端当「旧版本空值」直接删，
     等价于去重键永不生效）。
6. 前置自检：`scan_prefix(b"stream/")` 至少 1 个键（否则说明落库链路没走通，后面的断言没有意义）。
7. `prune_prefix_before(b"ingest/", now_micros() + 1_000_000)`（未来时间 = 保留期已过，
   与真实清理端同一调用）：
   - 返回值**恰好 3**；再数一次 `ingest/` 为 **0**；
   - `stream/` 的键数与剪枝前**相等**（前缀边界没写错）。
8. 清理：`TempDir` 删目录，`Server::Drop` 里 `kill` + `wait`。

## 期望结果（可判定）

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 首轮全部受理 | `accepted == 3` |
| 2 | 首轮回读的正是这批点 | 点集与 `[(base,0.5),(base+1s,1.5),(base+2s,2.5)]` **逐点相等** |
| 3 | 重复投递不再新增点 | 第 2…5 轮后点数 `== 3`（**不是** ≥），点集不变 |
| 4 | 重复投递不再新增序列 | 每轮 `series_count` 与首轮**相等** |
| 5 | 停机走优雅路径 | SIGTERM 后 30s 内自己退出，退出码 `0` |
| 6 | 去重键数 == 唯一 record 数 | `scan_prefix("ingest/")` 键数 `== 3`（`5 × 3 = 15` 就是没去重） |
| 7 | 键的 `record_id` 对得上 | 排序后逐个等于 `[dedup-r0, dedup-r1, dedup-r2]` |
| 8 | 键值满足清理约定 | 每个值 `len == 8`，解码出的微秒落在 `(base-1h, now+1min]` |
| 9 | 剪枝返回删除数 == 键数 | `prune_prefix_before("ingest/", now+1s) == 3`，之后重扫为 `0` |
| 10 | 前缀边界正确 | 剪枝前后 `stream/` 键数相等（且 ≥1） |

**反向验证（写这条测试时实测过）**：把每轮 `record_id` 改成 `{id}-{round}`（即让去重不再命中），
第 6 条立刻挂成
`ingest/ 键数必须等于唯一 record 数（3），实际 15 个: ["dedup-r0-1", …, "dedup-r2-5"]` ——
说明这条断言不是恒真，且它抓的正是「N 轮重复投递 = N 倍键数」这个真实故障形态。
注意：**只把 `ROUNDS` 改成 1 不会变红**（循环体为空，断言依然成立），不要用那种改法当反证；
要反证必须破坏「同一批 record 重发」这个前提本身。

## 清理方式

`tempfile::TempDir` 在测试结束（含 panic）时删掉数据目录与配置；子进程在 `Server::Drop`
里 `kill` + `wait`，不留孤儿，也不会留下被占用的 `kv.redb`。第 5 步已经 `SIGTERM` 等它
退出过，`drop(server)` 只是兜底。失败路径下 panic 前会先打印子进程 stdout/stderr 尾部 40 行。

## 关联需求或事故编号

- **v1.3.15 线上事故**：`run_cleanup` 的 `?` 传播让 `ingest/` 去重键剪枝整轮跳过，线上 11 分钟
  键数 199 → 3942，趋近 Redis `maxmemory 2gb`（`noeviction` → 接入 5xx）。
  见 `docs/external-storage.md` §「三阶段解耦」。
- 保留期配置 `ingest_dedup_retention_days`（默认 3 天，0 = 不清理）：本测试的第 9 条就是
  `run_cleanup` 里那个 `prune_prefix_before(INGEST_PREFIX, cutoff)` 调用的等价验证。
- 与 `crates/dataplane-kv/src/lib.rs::tests::prune_prefix_before_deletes_expired_and_legacy_only`
  互补：那条测「值解析与过期判定」的单元语义（本地 `KvStore`），这条测「真进程写进去的键
  能被真时间戳剪掉、且键数上界等于唯一 record 数」。
