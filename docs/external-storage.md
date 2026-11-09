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
- `ts_retention_days` 在外置模式下用于 `storage_stats` 上报；真正的保留由
  vmstorage 的 `-retentionPeriod` 决定（两边要对齐，否则 `/v1/ts/stats` 会显示一个
  与服务端不一致的值）。v1.3.16 起清理任务在 VM 后端**跳过**按时窗的 `delete_series`
  （能力位 `supports_time_window_delete=false`），不再每小时报错，见 §9.2。
- `ts_cardinality_limit` / `ts_memory_limit_bytes` / `ts_wal_size_limit_bytes` 在
  `ts_backend=victoriametrics` 下无效（这些是本地引擎的闩锁）。

## 2. 切换步骤

0. **上线前先验 VM 去重窗口**（硬前置，§5）：同一条序列连写 `t`、`t+5s`、`t+40s`
   三个点，range 查询必须回 **3 个点**。回 2 个 = vmstorage 的
   `-dedup.minScrapeInterval` 不是 0，**停下，不要往下切**：

   ```bash
   P='bench_dedup_probe{src="cutover"}'
   now=$(date +%s)
   for off in 0 5 40; do
     curl -s -X POST "$VM_INSERT/insert/0/prometheus/api/v1/import/prometheus" \
       --data-binary "$P $off $(( (now+off)*1000 ))"
   done
   sleep 5
   curl -s "$VM_SELECT/select/0/prometheus/api/v1/query_range" \
     --data-urlencode "query=$P" \
     --data-urlencode "start=$((now-60))" --data-urlencode "end=$((now+120))" \
     --data-urlencode "step=1" | jq '.data.result[0].values | length'   # 必须是 3
   ```

1. **前置条件先满足**（§5，尤其 Redis 的 AOF 与容量）。前置没做完就切 = 重启丢
   `ingest/` 去重键、写满即硬失败。
2. 先只切时序（风险小、可独立回滚）：
   `DATASERVER_TS_BACKEND=victoriametrics` + 两个 VM URL，重启 dataserver。
3. 观察：
   - `dpc ts stats` → `series_count`、`background_errors_total`、`degraded`
   - `dataserver_ts_background_errors_total`（自监控）不涨
   - VM 侧 `vm_rows_inserted_total` 随接入量增长
4. 再切 KV：`DATASERVER_KV_BACKEND=redis` + `DATASERVER_REDIS_URL/PASSWORD`，重启。
5. 验收：接入一条数据 → `/v1/prom/query` 查得到；`/v1/streams` 能列出。
   去重键剪枝与日志/边保留**不再受时序删除结果影响**（v1.3.16 起三阶段解耦，§9.2）。
   验收这一项：日志里 `retention cleanup:` 行的 `ingest_dedup_deleted` 应该 >0，
   同时 `failed_phases=0`；再看 `redis-cli info memory` 的 `used_memory` 与 `dbsize`
   是否停止单调上行。

## 3. 回滚步骤

改回 `DATASERVER_TS_BACKEND=tsink` / `DATASERVER_KV_BACKEND=redb` 并重启即可。

- 老数据目录 `data_path/ts`、`data_path/kv.redb` 全程没被修改，回滚后接着用。
- **回滚会丢掉「切换期间写进外置存储」的那部分数据**：时序在 VM 里，KV 的去重键在
  Redis 里。无缝回滚需要在回滚前把这两段补回本地，当前没有反向导出工具（§4）。
  外置模式下 `ts/` 从未被打开，所以回滚后的本地时序**停在切换时刻**。
- 回滚前把 `vm_*` / `redis_*` 连接留在配置里不影响启动（后端未选中就不建连）。

## 4. 迁移：外置模式**不迁移**历史时序（回填工具当前不存在）

结论：**切换只改后端选择，历史时序留在本地，新后端里一条都查不到。**
之前写的「选 WAL 重放、用新版 dataserver 冷启动把老 WAL 重放进 VM」在代码里
并不存在，按现状作废：

- `bins/dataserver/src/main.rs`（`ts_backend` 分支）按配置**只构造一个** store，
  `tsink` / `victoriametrics` 二选一，互斥。
- `crates/dataplane-adapter-victoriametrics` 从不打开 `ts/`，只走 vminsert / vmselect。
- 全仓没有 replay / migrate / backfill 入口：
  `rg 'fn (replay|migrate|backfill|import_from|dump)'` 只命中 gse-server 的 ledger
  迁移与 agent 的 `spec_apply`。`bins/dataserver/tests/replay_bench.rs` 是**测试基准**，
  不进产品二进制，也不在镜像的默认路径里。

线上实测后果（2026-11-10 切换后）：本地 4411 序列的历史留在 `data_path/ts/`
（129MB WAL，未删未改），切换后 `/v1/prom/query` 只能查到**切换之后**写进 VM 的点。

丢数据边界（按实际行为，必须接受）：

- **切换时刻之前的所有历史点**：只在本地 `ts/` 里，VM 不可见，`/v1/prom/query`、
  `/v1/ts/stats` 的 `series_count` 都看不到它们。数据**没丢**，但当前没有回填路径
  （§4.2）。
- 回滚到 `ts_backend=tsink` 能重新看到全部本地历史；但**切换期间写进 VM 的点
tsink 里没有**，回滚后这段时间是空白（除非再做一次 VM→本地反向导出，同样未实现）。
- `retain/` / `spec-live/` / `stream/` 这三类 KV **必须迁移或重建**，否则：
  `retain/` 丢了 → 已删采集项的数据不会被清理（多留，不丢）；
  `spec-live/` 丢了 → 下一轮清理会把所有采集项误判成「刚回来」，只影响一轮对齐；
  `stream/` 丢了 → `/v1/streams` 面板在下次上报前是空的（展示，不丢数据）。
  这三类都是**小集合**（`spec-live/`/`retain/` 是采集项数，`stream/` 是千级），
  用 `scan_prefix` + `set` 逐条搬即可；这是目前唯一可照做的搬迁。
- `ingest/` 不迁移：接受一次重复入库窗口。切换后 Redis 里没有去重键，Agent 重发
  窗口内的记录会被再受理一次，VM 侧同 `(series, timestamp)` 覆盖写，**不产生重复点**。
- **时序保留不迁移**：VM 侧 `-retentionPeriod=30d` 与 `ts_retention_days=30` 已对齐。
  v1.3.16 起 dataserver 在 VM 后端**完全不做**按时窗的时序删除（只跳过、不报错），
  所以「采集项保留期短于全局保留期」这件事在外置时序后端下**不被强制执行**，见 §9.2。

### 4.1 操作含义（切换清单按这个口径）

切换时**不要**执行任何「回放 `ts/` 进 VM」的动作 —— 没有这个能力，`replay_bench`
只用于出基准数字。要么接受历史不可查，要么先做 §4.2 的回填工具。
历史要不要保，属于业务决策（`ts/` 目录可以先拷一份备份，体积 129MB）。

### 4.2 若要做回填：最小可行路径评估（**未实现，仅评估**）

推荐路径：**新增一个独立 bin（`bins/ts-backfill`），不做进 dataserver**。

1. 读侧：用 `dataplane-ts` 的 tsink 只读路径逐序列读出 `(measurement, tags, ts, value)`。
   关键未验证点：tsink 的 `StorageBuilder::build()` 会**重放并可能改写** WAL，
   回填必须在目录**拷贝**上跑（停写后拷，129MB 秒级），或确认 tsink 有只读打开能力。
2. 写侧：直接复用 `dataplane-adapter-victoriametrics` 的 `write`（攒批 + 队列 + 背压
   已有），或直接发 `/insert/{t}/prometheus/api/v1/import/prometheus`。
3. 幂等：VM 对同 `(series, timestamp)` 是覆盖写，回填可重跑；配合「按 series 记录水位」
   落一个本地文件即可断点续传。
4. 窗口处理：回填只写 `ts <= 拷贝时刻` 的点，避免覆盖切换后新写入的同时间戳点。

工作量（按现有代码估，未实施）：

| 项 | 内容 | 估时 |
|---|---|---|
| 读侧 + 转换 | 已有 `replay_bench` 的实现可直接抄（§7.5 的流式版本） | 1 人日 |
| 断点续传 / 水位 / `--dry-run` / 内存上限 | 新增 | 1 人日 |
| 验证 | 线上 4411 序列 / 129MB WAL 全量回填 + 抽样对账（点数、时间范围、首尾值） | 1 人日 |
| 合计 | | **约 3 人日** |

吞吐预期：写入段已有实测 101s / 5.39M 点（53.2k records/s），历史规模（129MB WAL、
4411 序列）比基准数据小，瓶颈在逐点读 tsink（基准里真实源 32.7k records/s），
端到端量级是**分钟级，不是小时级**。

能否复用 `dpc` 的 `ts` HTTP 接口：

- `ts stats`：**可以**，用于回填前后核对 `series_count` 与时间边界。
- `ts delete`：**不能当导入用**（删除 ≠ 导入）。只在「回填写脏了、要按 matcher 整序列删掉重来」
  时有用，且要求 `vm_delete_auth_key` + 有 matcher（§6）。
- `dpc` 没有 bulk import 端点；走 `/v1/ingest` 逐条导入会写 `ingest/` 去重键（污染前缀、
  把历史混成新接入、无法与真实接入区分），不建议。

### 4.3 Open questions

- tsink 能否只读打开而不重放/改写 WAL？（决定回填是否必须先拷目录）
- 回填后微秒→毫秒的精度损失实际影响多少点（同毫秒多点在 VM 侧覆盖，未测）。
- 回填期间是否要限速（避免 VM 写入打满影响同集群的实时接入）。
- 是否要把「历史不可查」在 UI 上显式提示（否则用户会当成数据丢失报障）。

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

1. **`-dedup.minScrapeInterval` 必须是 `0` —— 这是硬前置条件，不是建议。**
   非 0 时同序列落在窗口内的点会在**查询**时被合并（后写胜出）。实测
   `=30s` 时：同序列写 `t`、`t+5s`、`t+40s` 三个点，range 查询只回 2 个
   （`t` 与 `t+5s` 合并）。dataserver 写的是秒级数据 → **静默丢样本**，
   而且写入侧、`vm_rows_inserted_total`、`background_errors_total` 全部正常，
   只有查询时才看得出来。
   现状（2026-11-10 由 tim 改并核对）：vmstorage args 已为
   `--dedup.minScrapeInterval=0`。**任何人改回非 0 都等于把外置时序降成
   ≥30s 分辨率**，上线巡检里有一步专门验证（§2 第 0 步）。
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
| 采样分辨率 | 任意 | 受 vmstorage `-dedup.minScrapeInterval` 限制 | 现为 `0`（= 不去重），秒级数据完整保留；改回非 0 会静默丢样本，见 §5 |
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

### 7.5 去重批量化（`set_many` / `exists_many`）实测（2026-11-10，cloud3 集群内）

接入路径原来是每记录两次 Redis 往返（`exists` + `set`），5.39M 条补数会被 RTT 卡死。
现在：整批一次 `exists_many`（`MGET`）、标记攒 512 条一次 `set_many`（`MSET`）。
trait 的两个方法都有缺省实现（逐条循环），所以 redb 与现有调用方零改动；
redb 侧覆写成「一个读事务 / 一个写事务」，Redis 侧覆写成 `MGET` / `MSET`。

同一份 `ts/` 拷贝、同一实例（ClusterIP 直连，不经隧道），`bench_ingest_replay_throughput`：

| 口径 | 样本 | 墙钟 | 速率 |
|---|---|---|---|
| **改后·全量**：合成点集（3342 序列 / 1s 间隔），信封 1000 条 | **5,391,255 条** | **101.256s**（apply 101.254 + flush 0.002） | **53,244 records/s** |
| 改后·真实源（源 `ts/` 拷贝流式读出） | 200,000 条 | 6.112s（apply 6.105 + flush 0.007） | 32,722 records/s |
| **改前**：逐条 `exists` + `set`（同一 Redis、同一前缀，只打 KV） | 20,000 条 | 9.930s | **2,014 records/s** |

- **539 万条从「约 45 分钟」降到 101 秒**（按改前的 2,014 records/s 折算 5.39M ≈ 2,676s）。
  改后两条路径都是 `background_errors_total=0`、`degraded=false`。
- 53.2k records/s 仍低于 VM 直写的 113.8k 点/s，**新的瓶颈是接入路径自身**
  （`serde_json::Value` → `MetricsRecord` 反序列化 + tags 克隆，约 19µs/条），不是 Redis，也不是 VM。
  真实源那条 32.7k 更低，多出来的是从 tsink 逐点读取与转换的成本（迁移时才会遇到）。
- `prune_prefix_before`：200,000 键 3.606s = 55.5k keys/s；539 万键 72.294s = **74.6k keys/s**。
  去重键清理（每天一轮）不是瓶颈。
- Redis 占用：20 万键 20.3MB → 约 **102 B/key**，539 万键 ≈ **550MB**（`maxmemory 2gb` 够，盯 `used_memory`）。
- **跑法有硬性要求**：这条基准会把源 `ts/` 目录整个重放进内存做对照，必须在有内存上限的
  cgroup 里跑（`systemd-run --scope -p MemoryMax=6G -p MemorySwapMax=0`），
  `SUMMARY` 里的 `peak_rss_kb` 是自查字段。2026-11-10 第一版把 5.39M 条记录先攒进
  `Vec<Value>`（峰值 11.7GB），在 15G 无 swap 的生产节点上触发节点级 OOM，
  dataserver 被连带 OOMKilled —— 现已改为「边读源边凑信封、一次只持有一个信封」。

**`SCAN MATCH` 是整键空间扫描**：`prune_prefix_before`/`scan_prefix` 每轮都会遍历
整个 DB（COUNT 500），键越多越慢（77k keys/s 的量级下，百万键 ≈ 13s/轮）。
清理周期（`ts_clean_interval`，默认 ≥60s）下可接受，但别把它放进请求路径。

---

## 8. 切换清单（可直接照敲）

前提：镜像 `ghcr.io/abrance/vectorman-server:v1.3.15-<sha>` 已出（`server/v1.3.15` tag），
`cops/apps/vectorman/k8s.yaml` 里的 dataserver 用的是它。

### 8.1 切之前必须核过（任一条不过就停）

```bash
# ① VM 必须不去重：同序列 t / t+5s / t+40s 要回 3 个点（§2 第 0 步那段 curl，期望 3）
sudo k3s kubectl -n victoria-metrics get sts \
  vmcluster-victoria-metrics-cluster-vmstorage \
  -o jsonpath='{.spec.template.spec.containers[0].args}' | tr ',' '\n' | grep dedup
#   期望： "--dedup.minScrapeInterval=0"

# ② 两个 secret 在 vectorman ns（secret 不跨 ns）：
sudo k3s kubectl -n vectorman get secret dataserver-redis dataserver-vm

# ③ Redis 持久化与容量（重启会丢 ingest/ 去重键 = 重复入库）
sudo k3s kubectl -n redis-test exec redis-0 -- sh -c \
  'redis-cli -a $REDIS_PASSWORD --no-auth-warning config get appendonly; \
   redis-cli -a $REDIS_PASSWORD --no-auth-warning info memory | grep -E "used_memory_human|maxmemory_human|maxmemory_policy"; \
   redis-cli -a $REDIS_PASSWORD --no-auth-warning dbsize'
#   期望：appendonly yes、noeviction、used_memory 远小于 2G、dbsize=0（首次切换）

# ④ 节点余量：外置模式启动不回放 WAL，切换本身几乎不占内存（实测 Ready <2s、RSS ~16MB）
free -g   # available ≥ 4（无需为 WAL 回放预留）
```

### 8.2 完整 env 清单（`apps/vectorman/k8s.yaml` 的 dataserver 容器）

```yaml
          env:
            - name: DP_DATA_PATH
              value: /app/dataserver/data
            # --- KV 后端：redb → Redis ---
            - name: DATASERVER_KV_BACKEND
              value: redis
            - name: DATASERVER_REDIS_URL
              value: redis://redis.redis-test.svc:6379/0
            - name: DATASERVER_REDIS_PREFIX
              value: "dp:"
            - name: DATASERVER_REDIS_PASSWORD
              valueFrom:
                secretKeyRef:
                  name: dataserver-redis      # ns vectorman 里的副本
                  key: REDIS_PASSWORD
            # --- 时序后端：tsink → VictoriaMetrics ---
            - name: DATASERVER_TS_BACKEND
              value: victoriametrics
            - name: DATASERVER_VM_INSERT_URL
              value: http://vmcluster-victoria-metrics-cluster-vminsert.victoria-metrics.svc:8480
            - name: DATASERVER_VM_SELECT_URL
              value: http://vmcluster-victoria-metrics-cluster-vmselect.victoria-metrics.svc:8481
            - name: DATASERVER_VM_TENANT
              value: "0"
            - name: DATASERVER_VM_SINGLE_NODE
              value: "0"          # 集群版：URL 自带 /insert|/select 前缀
            - name: DATASERVER_VM_BATCH_SIZE
              value: "5000"       # 单批点数
            - name: DATASERVER_VM_FLUSH_INTERVAL_MS
              value: "200"        # 攒批最长等待
            - name: DATASERVER_VM_QUEUE_CAPACITY
              value: "500000"     # 回放/补数时 20 万会撞背压（实测 3349 次/5.39M），给到 50 万
            - name: DATASERVER_VM_WHOLE_SERIES_DELETE
              value: "0"          # 保持「按时间窗删除一律拒绝」
            - name: DATASERVER_VM_DELETE_AUTH_KEY
              valueFrom:
                secretKeyRef:
                  name: dataserver-vm
                  key: VM_DELETE_AUTH_KEY
```

`config.toml`（ConfigMap `dataserver-conf`）**不用改**：`data_path` 仍指向原目录
（外置模式下 `ts/`、`kv.redb` 不再写入，但也不删）；`ts_memory_limit_bytes` /
`ts_wal_size_limit_bytes` / `ts_cardinality_limit` 在外置模式下无效，留着无害。

### 8.3 执行顺序（写中断 <1 分钟：外置模式启动不回放 WAL）

实测（2026-11-10 两次 rollout）：Killing → Started 都是秒级，合计约 **30s**。
运维侧之前按「WAL 回放 4–6 分钟」估的停机是错的 —— 那是本地 tsink 后端的成本。

```bash
NS=vectorman
# ① 停写（Recreate 策略，先缩到 0 再改配置，避免滚动期两个实例同时写）
sudo k3s kubectl -n $NS scale deploy/dataserver --replicas=0
sudo k3s kubectl -n $NS wait --for=delete pod -l app=dataserver --timeout=120s

# ② 备份老 tsink 目录（只拷，不打开；打开会重放并改写）。129MB，秒级。
#    备份不是迁移：外置模式不会读它，回填工具当前也不存在（§4）。
SRC=/app/dataserver/data/ts           # PVC 上的路径，用一次临时 pod 挂同一个 PVC 更省事
cp -a "$SRC" /tmp/backup-ts-$(date +%H%M)

# ③ 上新配置（env 见 8.2）
sudo k3s kubectl -n $NS apply -f apps/vectorman/k8s.yaml
sudo k3s kubectl -n $NS rollout status deploy/dataserver --timeout=600s   # 期望 <10s Ready
```

**`ingest/` 不迁移的后果**（已知并接受）：切换后 Redis 里没有任何去重键，Agent 重发窗口
内的记录会被再受理一次 → VM 侧同 `(series, timestamp)` 覆盖写，**不产生重复点**；
但 `stream/`、`retain/`、`spec-live/` 三类小集合要按 §4 用 `scan_prefix` + `set` 手工搬，
否则 `/v1/streams` 面板在下次上报前是空的、已删采集项的数据会多留一天。

### 8.4 部署后自检（命令 + 期望输出）

```bash
POD=$(sudo k3s kubectl -n vectorman get pod -l app=dataserver -o name | head -1)

# ① 存活
sudo k3s kubectl -n vectorman exec $POD -- curl -s localhost:8081/health
#    {"status":"ok"}

# ② 时序后端自报（关键：memory_used/wal 必须是 0，错误计数必须是 0）
sudo k3s kubectl -n vectorman exec $POD -- curl -s localhost:8081/v1/ts/stats
#    series_count>0  memory_used_bytes=0  wal_size_bytes=0
#    background_errors_total=0   degraded=false   retention_days=30

# ③ 写一条查一条（秒级，确认不被 dedup 压掉）
NOW=$(date +%s)
sudo k3s kubectl -n vectorman exec $POD -- curl -s -X POST localhost:8081/v1/ingest \
  -d "{\"batch_id\":\"cutover-$NOW\",\"data_type\":\"metrics\",\"data_id\":\"cutover\",
       \"agent_id\":\"cutover\",\"host_id\":\"cutover\",\"sent_at_micros\":$((NOW*1000000)),
       \"records\":[{\"record_id\":\"cutover-$NOW\",\"timestamp\":$((NOW*1000000)),
       \"measurement\":\"cutover_probe\",\"tags\":{\"src\":\"cutover\"},
       \"field_name\":\"value\",\"field_value\":1}]}"
#    {"accepted":1,"status":"ok","failures":[]}
sleep 35   # 越过 vmselect 的 -search.latencyOffset（默认 30s）
curl -s "$VM_SELECT/select/0/prometheus/api/v1/query?query=cutover_probe" | jq '.data.result|length'
#    ≥1

# ④ 去重仍然生效：同 record_id 再发一次 → accepted 计入，但 VM 点数不增
#    （对比 /select/0/.../series?match[]=cutover_probe 的返回条数前后一致）

# ⑤ 自监控：dataserver_ts_background_errors_total 增量必须是 0；degraded 会粘滞，不要看它
sudo k3s kubectl -n vectorman exec $POD -- curl -s localhost:9091/metrics | grep -E "background_errors_total|dropped"
```

### 8.5 回滚

```bash
# ① 把 8.2 的两组 env 删掉（或把 BACKEND 改回 redb / tsink），重启即可
sudo k3s kubectl -n vectorman set env deploy/dataserver DATASERVER_KV_BACKEND=redb DATASERVER_TS_BACKEND=tsink
# ② 老数据目录全程没动，回滚后接着用；Redis 的键留着不用清（回滚后没人读）
# ③ VM 里的数据**不要删**：再次切回外置可直接复用，只是切换窗口内本地新写的点缺失
```

回滚会丢掉「切换期间写进外置存储」的那部分：时序在 VM、去重键在 Redis，两边都没有反向
导出工具（§4）。

### 8.6 预期数字（2026-11-10 实测/推算，标注清楚）

| 项 | 值 | 来源 |
|---|---|---|
| dataserver Ready（外置后端） | **<2s**（实测 0.76s），本地 tsink 是 340s | 实测 |
| 接入吞吐（批量去重 + VM） | **53,244 records/s** | 实测 5,391,255 条（峰值 RSS 25MB） |
| 其中 VM 直写上限 | 113,782 点/s | 实测 5.39M 点 |
| 改前（逐条去重往返） | 2,014 records/s（5.39M 折算 ≈ 2,676s） | 实测 20k 条样本 |
| Redis 键数/内存 | 5.39M 键 ≈ **550MB**（102 B/key 实测，`maxmemory 2gb` 够，需盯 `used_memory`） | 实测 20 万键 + 折算 |
| VM 磁盘日增量 | 5.39M 点写入后 vmstorage 目录 2MB → 3MB（merge 未收敛，仅量级参考）；20Gi PVC 余量充足 | 实测（粗） |
| 迁移回放耗时（5.39M 点） | **不适用**：外置模式不迁移历史、无回填入口（§4）；上表 101s 是基准里的写入段，不是切换动作 | 实测（基准） |

---

## 9. 运维观察（2026-11-10 线上）

### 9.1 外置模式启动不回放 WAL

切换后 rollout 的 Killing → Started 是秒级（两次合计约 30s），Ready **<2s**，RSS 空载
~16MB。**「重启要等 WAL 回放 4–6 分钟」只适用于 `ts_backend=tsink`。**

### 9.2 每小时一条 `retention cleanup` / `apm retention failed` —— 已修复（v1.3.16）

现象（v1.3.15，外置模式下每小时各一条）：

```
retention cleanup: invalid_argument: victoriametrics 只能整序列删除（VM 删除接口不接受 start/end）；
  命中 0 条序列，本次按时间窗 [0, ...) 的删除被拒绝。...
apm retention failed: invalid_argument: victoriametrics 拒绝整序列删除无 label matcher 的选择...
```

来源（代码事实）：

| 路径 | 入参 | v1.3.15 结果 |
|---|---|---|
| `cleanup::apply_ts_retention`（`bins/dataserver/src/cleanup.rs`） | 有 `item_id` matcher + 时间窗，`whole_series_delete=false` | 明确拒绝（`invalid_argument`） |
| `dataplane_apm::retention` 的 ts 墓碑（`crates/dataplane-apm/src/retention.rs`） | 只有 `measurement`、无 matcher | 明确拒绝（无 matcher 一律拒绝，即使 `whole_series_delete=true`） |

`ts_retention_enforced=false` **消不掉**这两条：它只作用于 tsink 的写入期保留校验
（`main.rs` → `TsRetentionConfig` → `with_retention_enforced`），两条清理路径都不读它。

v1.3.15 的真实代价（不是日志噪声）：`run_cleanup` 里 `apply_ts_retention(...)?` 是 `?`
传播，一项报错就让同轮的 `apply_retention`（日志/边保留）与 `ingest/` 去重键
`prune_prefix_before` 全部跳过。VM 后端下只要存在「采集项 `retention_days` < 全局
`ts_retention_days`」或任一到期 `retain/{item_id}`，清理周期就**永久空转**：日志不清、
去重键不收敛 → 趋近 `maxmemory 2gb`（`noeviction` → 接入 5xx）。线上 11 分钟
`ingest/` 键 199 → 3942 就是这么来的。APM 侧同理会中断容量淘汰与磁盘回收。

**v1.3.16 修法**：

1. `TimeSeriesStore` 新增能力位 `supports_time_window_delete()`（默认 `true`，tsink 及其它
   后端行为不变），VM 适配器覆写为 `false`。
2. 能力位为 `false` 时跳过所有「按时间窗裁尾部」的删除：`apply_ts_retention` 的
   per-item 段与 `retain/` 段、`ApmRetention` 的聚合点墓碑。VM 上只保留真正可做的
   整系列删除，不再每小时发注定被拒的请求。
3. 单个采集项删除失败不再 `?` 传播：记 warn、计入 `TsCleanReport::failed_items`，
   本轮继续。`run_cleanup` 的三个阶段（时序 / 日志+边 / 去重键）互相解耦，任一阶段
   失败只作废自己，`CleanupReport::failed_phases` 如实计数（不再吞成 0）。
4. 观测：`retention cleanup:` 行新增 `ts_skipped_items` / `ts_failed_items` /
   `failed_phases`；`failed_phases > 0` 时计 `dataserver_ts_clean_errors_total`。

回归覆盖：`cleanup::tests`（删除必错的后端下 `ingest/` 键真被删、日志保留照跑、返回
`Ok` 且计数正确；无时间窗能力的后端零删除请求）与
`apm::tests::retention_skips_ts_tombstone_when_backend_has_no_window_delete`。

残余（需业务决定）：时间窗语义在 VM 上**本来就不存在**，跳过只是不再报错——VM 后端下
「采集项保留期短于全局保留期」这件事不再被强制执行；要它生效得靠 VM 自身的
`-retentionPeriod`，或等 tsink→VM 回填/整系列删除策略落地（见 §4.1）。
