# IT-API-TRACES-01 `POST /v1/traces/search` 与 `GET /v1/traces/{trace_id}`

`ID: IT-API-TRACES-01` ／ `依赖: 无（redb + tsink + tantivy + sqlite，APM 打开，临时目录）` ／
`命令: cargo test -p dataserver --test it_api_traces -- --nocapture` ／ `负责人: mike`

**为什么有这条**：trace 列表/详情是 APM 页面的全部数据来源，但 tp 里没有任何一条用例从外部证明
「摘要聚合算对了」「过滤/排序/分页对得上」「详情能拉全 span」。`span_count` 少算一条、`duration_micros`
错用根 span 耗时、详情对不存在的 id 返回 200 空体——这三种缺陷都不会被写入侧用例发现。

**为什么必须等**：摘要是 APM 后台任务每秒 `flush_due` 写进 `apm_trace_summary` 的派生数据，
`/v1/ingest` 返回 200 时摘要可能还没落库。所有断言先轮询到「三条摘要齐全且 A 的 `span_count == 2`」
再判定，避免把「还没来得及 flush」当成「功能坏了」或「功能对了」。

## 前置数据

| 项 | 值 |
|---|---|
| 二进制 / 后端 | `Options::local().with_apm(1)`：redb + tsink + sqlite，`apm_enabled = true`、聚合间隔 1s、`apm_min_duration_micros_for_detail = 0`（夹具固定） |
| 唯一前缀 | `unique_prefix("api-traces")` = `tp-api-traces-<pid>`，落在 service 名与信封标签上 |
| trace A | 2 个 span：根 `GET /a`（server, ok, 500ms）+ 子 `db query`（client, ok, 100ms，`parent_span_id` = 根）。根 @ `now-10min`，子 @ `+10ms` |
| trace B | 1 个 span：`POST /b`（server, **error**, 2s）@ `+60s` |
| trace C | 1 个 span：`GET /c`（server, ok, 100ms）@ `+120s` |
| trace_id | 32 位小写 hex，pid 作种子；span_id 16 位 hex |
| 写入路径 | 一次 `POST /v1/ingest`（`data_type=traces`，4 个 span） |
| 时间窗 | `from_ts = 起点-1s`、`to_ts = 起点+300s`，所有列表查询都带 |
| 端口 | `127.0.0.1:0` 现取；`TempDir` + `Dataserver::Drop` 兜底 |

## 执行步骤

1. 起真 dataserver（APM 打开），一次 ingest 写入 4 个 span。
2. 轮询 `POST /v1/traces/search`（全窗口）直到 `total == 3` 且 A 的 `span_count == 2`、`duration == 500000`。
3. 全量比对三条摘要的 `trace_id / start_ts / duration_micros / status / span_count`，
   以及 A 的 `root_service / root_operation / services`。
4. 过滤：`service`、`status`、`min_duration_micros`、以及互相排除的组合。
5. 排序：`sort=duration_micros&order=asc`；分页：`sort=start_ts&order=asc&limit=1&offset=1` 与全量第 2 条对齐。
6. 详情：存在的 A → 2 个 span（父子与顺序）；不存在的 32 位 hex → 真实状态码；非法格式 → 400。
7. 反证锚点：窗口整体挪到三条之外 → `total == 0`。

## 期望结果（可判定）

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 摘要落库 | 轮询到 `total == 3` 且 A `span_count == 2`、`duration_micros == 500000` |
| 2 | 摘要逐字段精确 | 默认排序（`start_ts DESC`）下 `rows == [C(100000), B(2000000, error), A(500000, span 2)]` |
| 3 | 根 span 与 services | A 的 `root_service == svc-a`、`root_operation == "GET /a"`、`services == [svc-a]` |
| 4 | `service` 过滤 | `service=svc-a` → `total == 1` 且是 A |
| 5 | `status` 过滤 | `status=error` → `total == 1` 且是 B |
| 6 | `min_duration_micros` 过滤 | `>= 1000000` → 只有 B（A=0.5s、C=0.1s 被排除） |
| 7 | 组合互相排除 | `service=svc-c&status=error` → `total == 0` |
| 8 | 排序 | `duration_micros asc` → `[C, A, B]` |
| 9 | 分页不重不漏 | `start_ts asc` 全量 = `[A, B, C]`；`limit=1&offset=1` 的 `traces[0]` 与全量第 2 条逐字段相等，`total` 仍为 3 |
| 10 | 详情返回全部 span | A：`summary.span_count == 2`、`len(spans) == 2`、`partial == false`、`expected_span_count == 2` |
| 11 | 详情父子与顺序 | `spans[0]` 是根（`parent_span_id == ""`，`duration_micros == 500000`）；`spans[1]` 是子（`parent_span_id == 根 span_id`，100000） |
| 12 | 不存在 trace 的真实状态码 | **404**，`code == "not_found"`，`error` 里点名 trace_id |
| 13 | 非法 trace_id | **400**，`code == "invalid_argument"`（先校验 32 位 hex，再查库） |
| 14 | 反证锚点 | 窗口挪到 `to_ts` 之后 → `total == 0` 且 `traces == []` |

**关于 `duration_micros` 的口径（实测，不是按直觉猜的）**：摘要的 `duration_micros = max_end_ts - start_ts`，
是**整条 trace 的时间跨度**，不是根 span 的耗时。A 的根 span 耗时也是 500ms，两者恰好相同，
所以另有 C（100ms）与 B（2s）来区分「按 trace 跨度」与「按根 span」两种实现。

**反向验证（实测）**：把期望里 A 的 `duration_micros` 从 `500000` 改成 `500001`，立即失败：

```
assertion `left == right` failed: 默认排序（start_ts DESC）下三条摘要应逐字段相等: {"total":3,"traces":[...]}
  left: [... Row { trace_id: "…5d11", start_ts: …, duration_micros: 500000, status: "ok", span_count: 2 }]
 right: [... Row { trace_id: "…5d11", start_ts: …, duration_micros: 500001, status: "ok", span_count: 2 }]
```

还原后重新通过。

## 清理方式

`TempDir` 随测试结束删除（sqlite 摘要表、tantivy 明细索引、redb 去重键全在里面）；
`Dataserver::Drop` 里 `kill` + `wait`。trace_id 与 service 名都带 pid，不会污染同库数据。

## 关联需求或事故编号

- `crates/dataplane-apm/src/query.rs`：列表/详情的 WHERE、排序、分页与 `not_found`/`invalid_argument` 口径。
- `crates/dataplane-apm/src/accumulator.rs`：`duration_micros = max_end - start`、`status = error_count > 0 ? error : ok`。
- `test_plan/README.md` 第 4 条（只认后端 HTTP 契约，不碰前端）。
