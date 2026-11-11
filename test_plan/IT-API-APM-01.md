# IT-API-APM-01 `GET /v1/apm/services` 与 `/v1/apm/service-aliases` CRUD

`ID: IT-API-APM-01` ／ `依赖: 无（redb + tsink + tantivy + sqlite，APM 打开，临时目录）` ／
`命令: cargo test -p dataserver --test it_api_apm -- --nocapture` ／ `负责人: mike`

**为什么有这条**：服务清单与静态映射是「eBPF 边归一服务名」的输入，改错一行会让拓扑页整片显示
`unknown-<ip>`。此前没有任何用例覆盖：清单是否真从端点半表读出、重复请求是否稳定、
别名的 upsert 语义（覆盖还是 409）、PUT 能否改主键、删除后的 404。

**为什么必须是真 span**：`apm_service_endpoint` 由 `ApmSink::observe_span → EndpointRegistry.flush`
写入，是 span 接入的派生数据。直接往 sqlite 里塞一行会把「端点半表登记」整条路径绕过去。

## 前置数据

| 项 | 值 |
|---|---|
| 二进制 / 后端 | `Options::local().with_apm(1)`：redb + tsink + sqlite，`apm_enabled = true`、聚合间隔 1s |
| 唯一前缀 | `unique_prefix("api-apm")` = `tp-api-apm-<pid>` |
| 服务名 | `<p>-svc`（唯一）；由一条 `GET /healthz`（server, ok, 1ms）span 登记，@ `now-5min` |
| 别名匹配值 | `<p>-proc`，`match_kind = process_prefix` → `alias_id = "process_prefix:<p>-proc"` |
| 写入路径 | `POST /v1/ingest`（`data_type=traces`）；端点由 APM 后台任务每秒 flush |
| 端口 | `127.0.0.1:0` 现取；`TempDir` + `Dataserver::Drop` 兜底 |

## 执行步骤

1. 起真 dataserver（APM 打开），ingest 一条带唯一 service 的 span。
2. 轮询 `GET /v1/apm/services` 直到本用例服务出现；核对实例数与实例字段。
3. 再请求一次，与上一次逐字段比较（稳定性）。
4. 别名 CRUD：`POST` → 列表含它 → `PUT` 改 service/enabled/note → 列表反映新值 →
   `POST` 同匹配条件（覆盖）→ `DELETE` → 列表不含 → 再 `DELETE`（404）。
5. 错误路径：`PUT` 改 `match_value`（400）；`POST` 非法 `match_kind`（400）。

## 期望结果（可判定）

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 清单能看到该服务 | 轮询到 `services[].service == <p>-svc` |
| 2 | 实例精确 | `instance_count == 1`、`instances.len() == 1`、`collector == "otlp"`、`last_seen_ts > 0` |
| 3 | 重复请求稳定 | 两次响应里本用例服务行 `==` 逐字段相等 |
| 4 | POST 建别名 | HTTP **201**；`alias_id == "process_prefix:<p>-proc"`、`service == "svc-alias-one"`、`enabled == true`、`note` 原样 |
| 5 | 列表含它 | `?match_kind=process_prefix` 里该 `alias_id` 恰好 1 条，`service` 一致 |
| 6 | PUT 改名 | HTTP 200；`service == "svc-alias-two"`、`enabled == false`、`note == "renamed"` |
| 7 | PUT 不能改主键 | 改 `match_value` → **400** `invalid_argument` |
| 8 | 重复 POST 是**覆盖**不是 409 | HTTP **201**、`alias_id` 不变、`service == "svc-alias-three"`、未传 `enabled` 时缺省 `true`；列表里仍只有 1 条 |
| 9 | DELETE 后消失 | 200 `{"deleted":true,"alias_id":...}`；列表里 `alias_id` 不再出现 |
| 10 | 重复 DELETE | **404** `not_found` |
| 11 | 非法 match_kind | 400 `invalid_argument` |

**关于「重复建」的真实行为（读 handler 确认，并实测）**：`alias_create` 直接调
`upsert_alias`，SQL 是 `INSERT ... ON CONFLICT(alias_id) DO UPDATE`，所以重复 POST **覆盖并仍返回 201**，
不是 409。用例按这个实际行为断言，并在 md 里记下来，避免以后有人「顺手改成 409」把前端依赖打断。

**反向验证（实测）**：把 PUT 后的期望 `service` 从 `svc-alias-two` 改成 `svc-alias-other`，立即失败：

```
assertion `left == right` failed: PUT 应改掉 service
  left: String("svc-alias-two")
 right: String("svc-alias-other")
```

还原后重新通过。

## 清理方式

别名在本用例内 `DELETE`（这也是被断言的步骤之一）；`TempDir` 随测试结束删除，
`Dataserver::Drop` 里 `kill` + `wait`。服务名与匹配值都带 pid，不会与同库其它数据互相影响。

## 关联需求或事故编号

- `bins/dataserver/src/http.rs::alias_create/alias_update/alias_delete`：201 / 覆盖 / 主键不可变 / 404 口径。
- `crates/dataplane-apm/src/alias.rs::alias_id`：`"{match_kind}:{match_value}"` 是派生主键。
- `crates/dataplane-apm/src/endpoint.rs`：服务清单来自 span 登记（`service.instance.id` / pod / 服务名本身）。
- `test_plan/README.md` 第 4 条（只认后端 HTTP 契约，不碰前端）。
