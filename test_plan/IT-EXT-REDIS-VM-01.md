# IT-EXT-REDIS-VM-01 · 外置存储后端（Redis + VictoriaMetrics）语义与全链路

| 项 | 值 |
|---|---|
| ID | `IT-EXT-REDIS-VM-01` |
| 优先级 | P0（缺变量必须失败） |
| 测试文件 | `bins/dataserver/tests/external_storage_e2e.rs` |
| 命令 | `cargo test -p dataserver --test external_storage_e2e`（或 `scripts/test-integration.sh`） |
| 依赖 | 真实 Redis + 真实 VictoriaMetrics（`DP_E2E_*` 变量，见下） |
| 负责人 | mike |

## 这条覆盖什么

外置后端（Redis 做 KV、VictoriaMetrics 做时序）与本地后端（redb / tsink）的**语义一致性**，
以及经真实 HTTP 接入链路（`POST /v1/ingest` → Redis + VM → `/api/v1/query` 回读）的端到端。

适配器把调用口径带偏是这类改造最典型的回归：本地后端全绿、切到外置后端后
`scan_prefix` 少了前缀还原、`prune_prefix_before` 跨 SCAN 分批漏删、VM 的 dedup 让点
「查不到」等等。这些只有对着真后端跑才暴露。

## 前置数据

环境变量（缺一个就失败，不跳过）：

| 变量 | 含义 |
|---|---|
| `DP_E2E_REDIS_URL` | 必填。`redis://[:password@]host:port/db` |
| `DP_E2E_VM_INSERT_URL` | 必填。VM 写入基址（集群版 `vminsert`，单机版同 select） |
| `DP_E2E_VM_SELECT_URL` | 缺省回落为 `DP_E2E_VM_INSERT_URL`（单机版只给一个地址即可） |
| `DP_E2E_VM_SINGLE_NODE` | 设了即按单机版解释地址（不带 `/insert` / `/select` 前缀） |
| `DP_E2E_VM_DELETE_AUTH_KEY` | 可选。vmselect 的 `-deleteAuthKey`；不设则删除路径只验错误分支 |
| `DP_E2E_VM_DELETE_ENABLED` | 可选。允许「按时间窗删除」退化成整序列删除 |

推荐用 `scripts/test-integration.sh` 跑：它负责从 `.env.integration` 读这些变量、做
TCP 连通性前置、拒绝 `TEST_ENV=prod`，缺东西时非零退出并给修复提示。

数据前置：库里可以是脏的。测试自己用一个按进程隔离的前缀（`e2e-{pid}-` 与
`e2e/series/{pid}` 之类）并把写入的东西删掉，不要求空库。

## 执行步骤

1. `redis_kv_semantics_match_local_engine`：Redis KV 的 CRUD / 错误码（删不存在的键报
   `not_found`）/ 二进制键值 / `scan_prefix` 只返回本前缀且键还原成调用方形状 /
   `prune_prefix_before` 按 8 字节小端微秒比较、旧删新留坏值也删、跨 SCAN 分批（1200+ 键）。
2. `redis_batch_ops_match_single_ops`：`set_many` / `exists_many` 跨默认 chunk（500）的
   批量结果与逐条一致；空批是 no-op（MGET/MSET 不接受零参数，适配器要自己挡）。
3. `vm_write_query_delete_roundtrip`：写入 → PromQL 可见 → 删除。按 60 秒点间距写，
   避开 cloud3 `-dedup.minScrapeInterval=30s` 的查询期合并（dedup 是**可见性**问题，不是分辨率问题，
   见 `docs/external-storage.md` §5）。未配 `DP_E2E_VM_DELETE_AUTH_KEY` 时验删除的错误口径。
4. `local_tsink_and_vm_agree_on_same_points`：同一批点同时写本地 tsink 与 VM，同一 PromQL
   查询结果集必须一致（毫秒对齐后）。
5. `redb_baseline_still_works`：对照组，同一套语义跑本地 redb，保证适配器没把调用口径带偏。
   这条**不依赖外部中间件**，所以缺变量时它仍然通过。
6. `ingest_over_http_lands_in_redis_and_victoriametrics`：起真实路由（Redis + VM 作后端），
   `POST /v1/ingest` 落库，`/v1/ts/stats` 看到序列增长，`/api/v1/query` 回读到带标签的点。

## 期望结果

- 6 个测试全绿（对着真实 Redis + VM）。
- 缺 `DP_E2E_*` 时：5 个依赖外部后端的测试**失败**，panic 信息形如
  `缺少环境变量 DP_E2E_REDIS_URL：本用例要求真实外置后端，不能静默跳过。…`；
  `redb_baseline_still_works` 仍然通过。

## 清理方式

- Redis：`prefix = e2e-{pid}-`，`prune_prefix_before(..., i64::MAX)` 收尾；
- VM：删除测试写入的序列（未开删除鉴权时残留少量测试序列，属已知限制，不影响后续运行——
  序列名按 pid 隔离）；
- 本地文件：`tempfile::TempDir` 随作用域删除。

## 关联需求或事故

- `docs/external-storage.md` §5「语义差异」、§7「实测对比（2026-11-10，cloud3）」——
  本场景把这些实测结论固化成可重复的断言。
- 2026-10-08 tsink 分片 tombstone 竞态（`Cargo.toml` 里 `[patch.crates-io]` 的 pin 原因）：
  属于本地 tsink 侧问题，外置后端不受影响；本场景用 tsink 作对照基线时按 60 秒间距写，
  不受该竞态干扰。
- 历史缺陷：本节 6 处 `eprintln!("skip: …"); return;` 让这条链路长期「离线全绿」，
  实际一次都没在 CI 里跑过。2026-11-10 改为缺变量即 `panic!` 并点名变量；
  同日 CI 加 Redis + VictoriaMetrics service，这条进 PR 门禁真跑
  （默认端口 `redis://127.0.0.1:6379/0`、`http://127.0.0.1:8428`）。
