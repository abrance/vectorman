# IT-API-LOGS-01 `POST /v1/logs/search` 的过滤契约与空结果边界

`ID: IT-API-LOGS-01` ／ `依赖: 无（redb + tsink + tantivy 本地后端，临时目录）` ／
`命令: cargo test -p dataserver --test it_api_logs -- --nocapture` ／ `负责人: mike`

**为什么有这条**：日志检索是数据面最常用的读路径，但 tp 里此前只在别的场景里「顺带写几条日志」，
从没断言过任何一条过滤条件真的生效。`data_type` / `agent_id` / `level` / `message_query` /
`trace_id` / 时间窗 / `labels` / `limit` 八个维度里任何一个是空实现，旧用例都会绿。

**为什么两条边界要单列**：检索页最怕两种退化——「查不到的时候报 500」和「查不到的时候返回全量」。
`collect_hits_paged` 的「预算耗尽 → 报错、真没有 → 空列表」这条分界只有端到端能验。

## 前置数据

| 项 | 值 |
|---|---|
| 二进制 / 后端 | `Options::local()`：redb + tsink + tantivy，`data_path` = `TempDir`，APM 关闭 |
| 唯一前缀 | `unique_prefix("api-logs")` = `tp-api-logs-<pid>`，落在 agent / host / data_id / record_id 上 |
| 第一批（A） | 信封 `agent=<p>-agent-a`、`host=<p>-host-a`、`data_id=<p>-item-a`；两条记录：`info "checkout started"` @ `now-10min`、`error "checkout timeout after 30s"` @ `+1s`；标签 `trace_id=<pid 的 32 位 hex>`、`app=shop` |
| 第二批（B） | 信封 `agent=<p>-agent-b`、`host=<p>-host-b`、`data_id=<p>-item-b`；一条记录：`warn "database connected"` @ `now-8min`；标签 `app=db` |
| 写入路径 | 真 `POST /v1/ingest`（`data_type=logs`），`ingest_ok` 断言 `failures == []` |
| 记录构造 | `data::log_record_with_labels`（本次新增：`log_record` 的 labels 恒为空，无法表达 trace_id / 自定义标签） |
| 端口 | `127.0.0.1:0` 现取；清理靠 `TempDir` + `Dataserver::Drop` |

## 执行步骤

1. 起真 dataserver，两批日志经 `/v1/ingest` 写入（`log.append` 内部 `commit`，写完即可检索，无需轮询）。
2. 基线：不带任何条件，确认库里**只有**这 3 条。
3. 逐条件检索：`data_type`（logs / metrics）、`agent_id`、`host_id`、`data_id`、`level`、
   `message_query`、`trace_id`、`from_ts`/`to_ts`、`labels`、组合条件。
4. `limit=1` / `limit=2` 精确截断。
5. 边界：不存在的 `trace_id`、时间窗外、`from_ts > to_ts`。

## 期望结果（可判定）

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 基线 3 条 | 无条件检索 `records.len() == 3`（独立进程 + 独立目录） |
| 2 | `data_type` 生效 | `logs` → 3 条；`metrics` → 空 |
| 3 | `agent_id` / `host_id` / `data_id` 生效 | A 的三个维度各自只返回 `[a2, a1]`，B 的各自只返回 `[b1]` |
| 4 | `level` 精确匹配 | `error` → `[a2]`；`warn` → `[b1]`；`ERROR` → 空（大小写敏感） |
| 5 | `message_query` 词项 AND | `timeout` → `[a2]`；`connected` → `[b1]`；`"timeout connected"` → 空（两词分属两条） |
| 6 | `trace_id` 生效 | `[a2, a1]`（只有 A 批记录带该标签，走提升索引字段） |
| 7 | 时间窗生效且闭区间 | `[ts_a1, ts_a2]` → `[a2, a1]`；`[ts_b1, ts_b1]` → `[b1]` |
| 8 | `labels` post-filter | `app=shop` → `[a2, a1]`；`app=db` → `[b1]`；`app=shop` + 不存在的键 → 空 |
| 9 | 组合条件 | `agent_a` + `level=error` + `message_query=timeout` + `app=shop` → `[a2]` |
| 10 | `limit` 精确截断 | `agent_a&limit=1` → `[a2]`；`limit=2` → `[a2, a1]`（结果按 timestamp **倒序**） |
| 11 | 空结果不报错 | 不存在的 `trace_id` → 200 且 `records == []`；时间窗外 → 空 |
| 12 | 参数校验 | `from_ts > to_ts` → **400** `invalid_argument`（不是空结果） |

**口径差异（实测，已在用例里断言，不是猜测）**：
- `/v1/logs/search` 的响应体只有 `records`，**没有 `total` 字段**。所以「total 为 0」在本接口
  无从断言，断言的是 `records` 为空且 `total` 键不存在（避免以后有人加了字段却语义不一致）。
- `message_query` 是 jieba 分词后的 **AND 词项匹配**，不是字符串子串匹配。所以用例只断言整词，
  不断言「词的一部分」（例如 `time` 不保证命中 `timeout`）。

**反向验证（实测）**：把 `level=error` 的期望值从 `[a2]` 改成 `[a1]`，立即失败：

```
assertion `left == right` failed
  left: ["tp-api-logs-<pid>-a2"]
 right: ["tp-api-logs-<pid>-a1"]
```

还原后重新通过。

## 清理方式

`TempDir` 随测试结束删除（日志索引、sqlite、redb 全在里面）；`Dataserver::Drop` 里 `kill` + `wait`。
不回写、不删索引，本用例是只读检索。

## 关联需求或事故编号

- `test_plan/README.md` 第 4 条（查询口径只认后端 HTTP 契约）。
- `crates/dataplane-log` 的「labels 全扫描预算」告警（`label filter needs a full scan`）：
  本用例的 `labels` 过滤都在小数据集上，若以后数据量变大触发预算上限，应改成 429/400 而不是
  静默截断——这条用例是以后加边界断言的锚点。
