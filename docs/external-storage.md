# dataserver 外置存储后端（Redis + VictoriaMetrics）

把 dataserver 内嵌的本地引擎换成 cloud3 现成的中间件：

| 数据面 | 本地（缺省，行为不变） | 外置 |
|---|---|---|
| KvStore（`ingest/` 去重键、`retain/`、`spec-live/`、`stream/`） | redb `kv.redb` | Redis（`redis-test` ns） |
| TimeSeriesStore | tsink `ts/`（WAL + 段文件） | VictoriaMetrics 集群版（`victoria-metrics` ns） |

切换只改环境变量/配置，**数据目录不动**，所以回滚就是改回来重启。

---

## 1. 配置

```toml
# --- KV 后端 ---
kv_backend = "redis"                 # redb（缺省）| redis
redis_url = "redis://redis.redis-test.svc:6379/0"
redis_password = ""                  # 空 = 不带 AUTH；实际用 env 注入
redis_prefix = "dp:"                 # 键前缀，必须与其它业务隔离

# --- 时序后端 ---
ts_backend = "victoriametrics"       # tsink（缺省）| victoriametrics
vm_insert_url = "http://vmcluster-victoria-metrics-cluster-vminsert.victoria-metrics.svc:8480"
vm_select_url = "http://vmcluster-victoria-metrics-cluster-vmselect.victoria-metrics.svc:8481"
vm_tenant = "0"                      # dataserver 无多租户
vm_single_node = false               # true = 单机版 VM（无 /insert|/select 前缀）
vm_batch_size = 5000                 # 单批点数
vm_flush_interval_ms = 200           # 攒批最长等待
vm_queue_capacity = 200000           # 队列点数；满即背压（写返回 unavailable）
vm_delete_auth_key = ""              # vmselect 的 -deleteAuthKey；空 = 禁止删除
vm_whole_series_delete = false       # VM 只能整序列删；见 §6「删除」
```

对应环境变量（覆盖 TOML）：

| 配置 | 环境变量 |
|---|---|
| `kv_backend` | `DATASERVER_KV_BACKEND` |
| `redis_url` | `DATASERVER_REDIS_URL` |
| `redis_password` | `DATASERVER_REDIS_PASSWORD` |
| `redis_prefix` | `DATASERVER_REDIS_PREFIX` |
| `ts_backend` | `DATASERVER_TS_BACKEND` |
| `vm_insert_url` | `DATASERVER_VM_INSERT_URL` |
| `vm_select_url` | `DATASERVER_VM_SELECT_URL` |
| `vm_tenant` | `DATASERVER_VM_TENANT` |
| `vm_single_node` | `DATASERVER_VM_SINGLE_NODE=1` |
| `vm_batch_size` | `DATASERVER_VM_BATCH_SIZE` |
| `vm_flush_interval_ms` | `DATASERVER_VM_FLUSH_INTERVAL_MS` |
| `vm_queue_capacity` | `DATASERVER_VM_QUEUE_CAPACITY` |
| `vm_delete_auth_key` | `DATASERVER_VM_DELETE_AUTH_KEY` |
| `vm_whole_series_delete` | `DATASERVER_VM_WHOLE_SERIES_DELETE=1` |

`data_path` / `kv_cache_bytes` / `ts_retention*` 保留原语义：

- 外置模式下 `data_path/ts`、`data_path/kv.redb` **不再写入**（但也不会被删），
  `sql.sqlite` / `logs/` / `files/` 仍用本地目录 —— 这次只替换 KV 与时序。
- `kv_cache_bytes` 只在 `kv_backend=redb` 生效。
- `ts_retention_days` 在外置模式下只用于 `storage_stats` 上报；真正的保留由
  vmstorage 的 `-retentionPeriod` 决定（两边要对齐，否则 `/v1/ts/stats` 会显示一个
  与服务端不一致的值）。
- `ts_cardinality_limit` / `ts_memory_limit_bytes` / `ts_wal_size_limit_bytes` 在
  `ts_backend=victoriametrics` 下无效（这些是本地引擎的闩锁）。

## 2. 切换步骤

1. **前置条件先满足**（§5，尤其 Redis 的 AOF 与容量）。前置没做完就切 = 重启丢
   `ingest/` 去重键、写满即硬失败。
2. 先只切时序（风险小、可独立回滚）：
   `DATASERVER_TS_BACKEND=victoriametrics` + 两个 VM URL，重启 dataserver。
3. 观察：
   - `dpc ts stats` → `series_count`、`background_errors_total`、`degraded`
   - `dataserver_ts_background_errors_total`（自监控）不涨
   - VM 侧 `vm_rows_inserted_total` 随接入量增长
4. 再切 KV：`DATASERVER_KV_BACKEND=redis` + `DATASERVER_REDIS_URL/PASSWORD`，重启。
5. 验收：接入一条数据 → `/v1/prom/query` 查得到；`/v1/streams` 能列出；
   等一轮清理（`ts_clean_interval`）看 `ingest_dedup_deleted` 有数。

## 3. 回滚步骤

改回 `DATASERVER_TS_BACKEND=tsink` / `DATASERVER_KV_BACKEND=redb` 并重启即可。

- 老数据目录 `data_path/ts`、`data_path/kv.redb` 全程没被修改，回滚后接着用。
- **回滚会丢掉「切换期间写进外置存储」的那部分数据**：时序在 VM 里，KV 的去重键在
  Redis 里。若要无缝回滚，需要在回滚前把这两段补回本地（当前没有反向导出工具，
  见 §4 的迁移取舍）。
- 回滚前把 `vm_*` / `redis_*` 连接留在配置里不影响启动（后端未选中就不建连）。

## 4. 迁移：为什么选「WAL 重放」而不是写一次性迁移工具

结论：**选 WAL 重放（把 `ts/` 目录拷走，用新版 dataserver 冷启动把老 WAL 重放进
VM），不写 redb→Redis / tsink→VM 的迁移工具。**

理由：

1. 回放已经降到分钟级（上一轮 `c7a8754`，O(series)→O(batch)，1,691,215 点 48.6s
   引擎自报），一次性工具的开发+验证成本高于它省下的时间。
2. 写迁移工具要再实现一遍「读 redb 全表 + 读 tsink 全部段 + 断点续传 + 幂等」，
   等于把本地引擎的读路径再用一次，且没有第二个使用场景。
3. `ingest/` 去重键**不需要迁移**：它的语义是「防接入重试重复」，重建代价是
   **允许一次重复入库**，不是丢数据；窗口内重复的数据量远小于迁移工具的复杂度。

丢数据边界（必须接受）：

- 迁移窗口内（拷贝 `ts/` 之后到 VM 追平之前）新产生的点，走的是新后端，不受影响；
  但 `ts/` 拷贝之后本地不再写入，所以**本地时序与 VM 之间会有一段只存在于 VM 的点**
  —— 这就是「切换时刻」，需要按 §2 的顺序（先切时序、观察、再切 KV）把窗口压到
  一次重启。
- `retain/` / `spec-live/` / `stream/` 这三类 KV **必须迁移或重建**，否则：
  `retain/` 丢了 → 已删采集项的数据不会被清理（多留，不丢）；
  `spec-live/` 丢了 → 下一轮清理会把所有采集项误判成「刚回来」，只影响一轮对齐；
  `stream/` 丢了 → `/v1/streams` 面板在下次上报前是空的（展示，不丢数据）。
  这三类都是**小集合**（`spec-live/`/`retain/` 是采集项数，`stream/` 是千级），
  用 `scan_prefix` + `set` 逐条搬即可；这也是唯一建议手写脚本搬的部分。
- `ingest/` 不迁移：接受一次重复入库窗口。
- **时序保留不迁移**：VM 侧 `-retentionPeriod=30d` 与 `ts_retention_days=30` 已对齐，
  外置模式下 dataserver **不再**自己算全局保留（`storage_stats.retention_*` 只做上报）；
  唯一还在 dataserver 侧执行的是 `retain/{item_id}` 触发的「采集项删除 → 清数据」，
  在 VM 后端退化为整序列删除（§6）。

## 5. cloud3 前置条件（运维 tim 负责，代码侧不自动改）

### Redis（`redis-test` ns，`redis:7.4`）— 已由运维硬化（2026-11-10）

| 项 | 现状态 | 说明 |
|---|---|---|
| 持久化 | `appendonly yes` + `appendfsync everysec` | 重启留存已实测：写 key → 删 pod → 新 pod → 值仍在，`aof_last_write_status:ok` |
| 容量 | PVC 8Gi | pod 重建不丢 |
| 内存 | `maxmemory 2gb`，policy **noeviction** | 去重键不能被淘汰，policy 不许改成 LRU |
| limits | 3Gi / 2 核 | |
| 认证 | secret `redis-test/redis-secret` key `REDIS_PASSWORD` | dataserver 用 `DATASERVER_REDIS_PASSWORD` 注入 |

**写满是硬失败，不是慢**：`noeviction` 触顶时 Redis 回
`OOM command not allowed when used memory > 'maxmemory'`。适配器把它映射成
`unavailable`（可重试，带退避），不是永久失败 —— 否则 `ingest/` 去重键一写就打穿整条
接入链路。单测：`crates/dataplane-adapter-redis` 的 `oom_is_retryable_unavailable`。

容量估算：当前 `kv.redb` 403MB（`ingest/` 去重键占绝大部分，按天清理）。
`redis_prefix=dp:` 下 2GB 够用，但要盯 `used_memory`：`ingest/` 清理周期
（`ingest_dedup_retention_days`）不变时，峰值为「一天的接入去重键」。

### VictoriaMetrics（`victoria-metrics` ns，集群版 v1.155.0）— 已由运维硬化

| 项 | 现状态 | 说明 |
|---|---|---|
| 写入 / 查询 | vminsert `:8480` / vmselect `:8481` | 集群版路径前缀 `/insert/{t}/...`、`/select/{t}/...` |
| 保留期 | `vmstorage -retentionPeriod=30d` | 与 `ts_retention_days=30` 已对齐；dataserver 不再自己算保留 |
| 容量 | vmstorage PVC 10Gi | 按实测写入速率再定，不预先扩容 |
| 删除 | v1.155 **没有 `-deleteData`**，只有 `-deleteAuthKey` | 已通过 secret `victoria-metrics/vm-delete-auth`（key `deleteAuthKey`）生效；接口实测：无 key/错 key → 401，对 key → 204，删后查询为空 |
| 删除路径 | `POST /delete/{tenant}/prometheus/api/v1/admin/tsdb/delete_series?authKey=...` | **不是** `/select/...`；`authKey` 只能走 query arg（集群内明文 HTTP，接受） |

**两个必须知道的 VM 行为（实测）：**

1. **`-dedup.minScrapeInterval=30s` 会让 1s 级数据在查询时被合并。**
   实测：同序列写 `t`、`t+5s`、`t+40s` 三个点，range 查询只回 2 个（`t` 与 `t+5s`
   合并成后写胜出的一个）。dataserver 的指标是秒级/分钟级自监控，30s 去重会**静默
   丢样本**。要么把 vmstorage 的 `-dedup.minScrapeInterval` 调到 ≤ 最小采样间隔
   （自监控是 1s~15s，建议 `0`），要么接受降采样 —— 这一条不解决，外置时序只能
   承载「≥30s 分辨率」的数据。**这是切生产的阻塞项。**
2. **`-search.latencyOffset`（默认 30s）**：查询当前时间最近 30s 的数据看不到。
   排障时别把「刚写入查不到」当故障。
3. **删除接口不接受 `start`/`end`**（实测 400：
   `delete API does not support specific time ranges ... the series can only be
   deleted completely`）。见 §6「删除」。

### 数据面容器侧

tim 已在 `vectorman` ns 建 VM 删除 key 的副本（secret 不跨 ns），dataserver 部署用
`DATASERVER_VM_DELETE_AUTH_KEY` 注入；Redis 密码来自 `redis-test/redis-secret`。

## 6. 语义差异（切之前必须知道）

| 项 | 本地 | 外置 | 影响 |
|---|---|---|---|
| 时间精度 | 微秒 | 毫秒（VM 固定） | 同毫秒多点互相覆盖（后写胜出）；查询返回毫秒对齐的微秒 |
| `write` 返回值 | 落盘后返回 | **入队**即返回 `Ok` | 进程被 `SIGKILL` 时最多丢 `vm_flush_interval_ms` 的点；`SIGTERM` 会先 flush 再退出 |
| 背压 | 无（本地写阻塞） | 队列满返回 `unavailable` | 接入路径 5xx → Agent 缓冲重试；`vm_queue_capacity` 要 ≥ 峰值批量 |
| 失败可见性 | 写失败即刻 5xx | 批量失败计入 `background_errors_total`/`degraded`，日志 `dropped N points` | 自监控要盯 `dataserver_ts_background_errors_total` 的增量，不要盯 `degraded`（粘滞） |
| `scan_prefix` | 全量 `Vec` | 全量 `Vec`（`SCAN` 累积） | 只给有界集合用：`spec-live/`、`retain/`（采集项数）、`stream/`（千级）。**不要对 `ingest/` 用** |
| `prune_prefix_before` | 范围流式 | `SCAN` + 批量 GET/DEL（流式） | 百万级键可用；`SCAN` 只保证「扫过的都处理」，期间新写入的键可能漏到下轮（下一轮会补） |
| `delete_series` | 写墓碑，支持时间窗 | **只能整序列删**，且必须配 `delete_auth_key` | `whole_series_delete=false`（缺省）时按时间窗的删除请求被**明确拒绝**（`invalid_argument`）；设为 `true` 则整条序列删掉、窗口外的点一起没，并打 warn。**即使用 `true`，无 label matcher 的选择也会被拒绝** —— 那种请求等于「删掉整个指标」，APM 保留期就是这种入参 |
| 采样分辨率 | 任意 | 受 vmstorage `-dedup.minScrapeInterval` 限制（cloud3 现为 30s） | 小于该间隔的点在查询时被合并 → 秒级数据静默丢样本。**阻塞项**，见 §5 |
| 一致性 | 单文件 | 无事务 | `retain/` 写入 + `spec-live/` 删除不是原子的；残留结果是「多留一天」，不丢数据 |

## 7. 实测对比（2026-11-10，cloud3）

数据源：`/opt/vectorman-k8s/dataserver/ts` 的 117MB WAL（`ts/wal`）拷贝。
命令与口径见 `bins/dataserver/tests/replay_bench.rs`。

| 指标 | 本地 tsink | 外置 VM |
|---|---|---|
| 冷启动回放 | 见下 | N/A（VM 里数据已在，启动只建连接） |
| 写入吞吐 | 见下 | 见下 |

> 数字在本次实测后填入（见 PR 描述与 `replay_bench` 输出）。

### 7.1 已测

| 项 | 值 | 口径 |
|---|---|---|
| 本地 tsink 冷启动回放（750MB 目录拷贝，4004 序列 / 5,391,328 点） | 24.6s | `StorageBuilder::build()` + `list_metrics_with_wal()` 墙钟（本机 16 核，NVMe） |
| 生产基线（cloud3，122MB WAL / 3336 序列） | Ready 340s | v1.3.14 实测（ops 提供） |
| 本地逐点写入吞吐（同一 trait 调用） | 32,882 点/s（5.39M 点 / 164.0s） | `TsinkTimeSeriesStore::write` 循环 |
| 外置 dataserver Ready（data_path 里放着完整 784MB `ts/` 目录） | **0.76s** | 外置后端根本不打开 tsink：同一次启动里 `ts/` 一个字节都不读 |
| 外置 dataserver RSS（空载，同上） | **16.5MB** | `ps rss`；对比线上本地后端 RSS 2.21GB、tsink 记账 1.35GB |
| `storage_stats`（VM `status/tsdb`） | 0.36s | vmselect 直连 |
| VM 写入吞吐（经 ssh 隧道，不利条件） | 约 9.6k 点/s | 见 7.2，隧道是瓶颈；那一轮还撞上了当时 `flush()` 的单次超时 bug（已修） |
| `vm_rows_inserted_total{type="prometheus"}` | 跑前 1,832 → 跑后 5,150,708 | 与写入点数核对（差额为仍未 flush 的队列） |

### 7.2 口径说明

- 本机到 cloud3 的 vminsert 走 ssh 隧道，1 个批（5000 点 ≈ 400KB）跨加密链路，
  **吞吐被隧道限制**，不代表集群内表现。集群内的数字见 7.3。
- `-dedup.minScrapeInterval=30s`（cloud3 现状）会让 1s 级数据在**查询**时被合并，
  所以真实数据下「写入点数」和「查询能看到的点数」不是一回事；写入吞吐不受影响。

### 7.3 集群内实测（同一二进制、同一份 `ts/` 拷贝，在 cloud3 本机跑，不经隧道）

数据源：`/opt/vectorman-k8s/dataserver/ts` 的拷贝；**3342 序列 / 5,391,255 点**。

| 指标 | 数值 |
|---|---|
| 本地 tsink 冷启动回放（`StorageBuilder::build()` + 列序列） | **100.9s**（另一轮 119.9s，受宿主负载影响） |
| VM 写入（同一批点，batch=5000 / flush=200ms / 队列 20 万） | **5,387,906 点 / 47.35s = 113,782 点/s**，其中入队 46.4s、最终 flush 0.97s |
| 背压 | 队列满重试 3,349 次（≈0.06%），即队列容量对这批数据偏紧，回放时建议 `vm_queue_capacity` 提到 50 万 |
| `vm_rows_inserted_total{type="prometheus"}` 差值 | 10,539,215 − 5,151,309 = **5,387,906**，与写入点数**逐点一致**（vminsert 侧零丢失） |
| 本轮 `storage_stats` | `background_errors_total=0`、`degraded=false` |
| 本地 tsink 逐点写入（cloud3 上同一 trait 调用） | 24 分钟仍未跑完 5.39M 点（< 3.8k 点/s），已中止；本机 NVMe 上测得 32.9k 点/s |

**结论：外置写入比本地逐点写入快一个数量级以上（113.8k vs <3.8k 点/s，cloud3 同机），
且 Ready 从 340s 降到 0.76s（不再做本地回放）。批量写入达标，改造不是负收益。**

### 7.4 Redis（集群内，经适配器 API）

20 万条 `ingest/` 形状的键（`dp:ingest/{id}` + 8B 小端微秒）：

| 操作 | 数值 |
|---|---|
| `set`（逐键，一次往返） | 64.9s / 20 万 = **3,080 ops/s** |
| `exists`（逐键） | 53.3s / 20 万 = **3,751 ops/s** |
| `prune_prefix_before`（SCAN + 批量 GET/DEL） | 2.59s / 20 万 = **77,113 keys/s** |
| 内存 | 122 B/key（10 万键实测 11.6MB）→ 1M 键 ≈ 122MB；`kv.redb` 403MB 对应 Redis ≈ 400–500MB，`maxmemory 2gb` 够用 |

**已知瓶颈（要评估，不在本次改）**：trait 是「一键一次往返」，所以接入路径的
`exists` + `set`（去重键）在 1.5k 记录/s 以上就会顶到 Redis 的 RTT 上限，
而本地 redb 是微秒级。当前实际接入量（5.39M 点/天 ≈ 62 点/s）远低于这个天花板，
**但 WAL 重放/突发补数会被它卡住**。要提上去需要给 `KvStore` 加 `set_many` / `exists_many`
批量方法（trait 变更，本次未做）。

**`SCAN MATCH` 是整键空间扫描**：`prune_prefix_before`/`scan_prefix` 每轮都会遍历
整个 DB（COUNT 500），键越多越慢（77k keys/s 的量级下，百万键 ≈ 13s/轮）。
清理周期（`ts_clean_interval`，默认 ≥60s）下可接受，但别把它放进请求路径。
