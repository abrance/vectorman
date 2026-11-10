# IT-API-SQL-01 `POST /v1/sql` 的建表 / 占位参数 / 回读契约

`ID: IT-API-SQL-01` ／ `依赖: 无（redb + sqlite 本地后端，临时目录）` ／
`命令: cargo test -p dataserver --test it_api_sql -- --nocapture` ／ `负责人: mike`

**为什么有这条**：`/v1/sql` 是 dataserver 唯一的 SQL 出口（APM 摘要、别名、eBPF 聚合、运维查数
都经它），但此前只有「夹具顺手调一次」的隐性依赖，没有任何用例断言它的**契约**：列名与顺序、
类型往返（INTEGER 不变成字符串、NULL 保持 null）、以及三条错误路径的状态码口径。

**为什么必须端到端**：`SqlRequest` 的反序列化、参数 JSON→`SqlValue` 的映射、
错误码到 HTTP 状态的映射分别在三处（`http.rs::sql_exec`、`dataplane-core/src/sql.rs`、
`http.rs::map_err`），进程内直接调 store 会把这三层全部绕过去。

## 前置数据

| 项 | 值 |
|---|---|
| 二进制 | `target/debug/dataserver`（`env!("CARGO_BIN_EXE_dataserver")`） |
| 后端 | `Options::local()`：`kv_backend = "redb"`、`ts_backend = "tsink"`、APM 关闭、`data_path` = `TempDir` |
| 表名 | `tp_api_sql_<pid>`：`unique_prefix()` 带 `-`，SQLite 会把 `-` 当减号，所以自己拼下划线名 |
| 行数据 | 2 行：`(1, "alpha", 1.5, NULL)` / `(2, "beta", 0.25, "note-b")` —— INTEGER / REAL / TEXT / NULL 各至少一次 |
| 参数 | 全部走 `?1..?4` 占位符，不做字符串拼接 |
| 端口 | `127.0.0.1:0` 现取（`sql_http` / `prom_http` / `metrics_http`） |
| 清理 | `TempDir` 随测试结束删除；`Dataserver::Drop` 里 `kill` + `wait` |

## 执行步骤

1. 起真 `dataserver`，等 `/health` `status=ok`。
2. `CREATE TABLE {table} (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL, note TEXT)`。
3. 两次 `INSERT ... VALUES (?1,?2,?3,?4)`，分别带 NULL 与 TEXT 的 note。
4. `SELECT id, name, score, note FROM {table} ORDER BY id`，逐字段比对。
5. 错误路径三条：
   - 请求体缺 `sql` 字段（只剩 `params`）；
   - `SELEC 1`（语法错）；
   - 参数放 `{"foo":"bar"}` 与 `[1,2,3]` 两种非法 JSON 形态。
6. 反面对照：`SELECT ?1` + `{"b64":"aGk="}`（唯一被放行的 object 形式）应原样往返。
7. `DROP TABLE`，再查同一张表；并查 `sqlite_master` 确认没有残留。

## 期望结果（可判定）

| # | 断言 | 判定方式 |
|---|---|---|
| 1 | 建表成功应答为空结果 | `{"columns":[],"rows":[]}` 逐字段相等 |
| 2 | 列名与顺序精确 | `columns == ["id","name","score","note"]` |
| 3 | 行逐行逐值相等 | `rows == [[1,"alpha",1.5,null],[2,"beta",0.25,"note-b"]]` |
| 4 | 类型往返 | `rows[0][0].as_i64() == Some(1)` 且 `as_str()` 为 `None`；`rows[0][2].as_f64() == Some(1.5)`；`rows[0][3].is_null()` |
| 5 | 缺 `sql` 字段 | HTTP **400**，`code == "invalid_argument"` |
| 6 | SQL 语法错 | HTTP **422**，`code == "query_failed"`（不是 400，也不是 500） |
| 7 | 参数 object / array | HTTP **400**，`code == "invalid_argument"` |
| 8 | blob 参数仍可用 | `SELECT ?1` + `{"b64":"aGk="}` → `rows == [[{"b64":"aGk="}]]` |
| 9 | DROP 后表确实消失 | 再查该表 → 422 `query_failed`；`sqlite_master` 里 `name = {table}` 的行数为 0 |

**关于 422**：`sql_exec` 把**所有** `state.sql.execute` 的错误（含 `SQLITE_ERROR` 语法错）统一映射成
422，只有参数映射失败才是 400。「SQL 错都当 400」的直觉在这里是错的，断言必须写实际值。

**反向验证（实测）**：把期望值里的 `1.5` 改成 `1.75`，立即失败：

```
assertion `left == right` failed: 回读结果应逐行逐值等于写入值
  left: Array [Array [Number(1), String("alpha"), Number(1.5), Null], ...]
 right: Array [Array [Number(1), String("alpha"), Number(1.75), Null], ...]
```

还原后重新通过，说明第 3 条不是恒真。

## 清理方式

`DROP TABLE` 在本用例里既是清理也是断言；表名带 pid，不会碰到同库其它表。`TempDir` 与
`Dataserver::Drop` 兜底删目录/杀进程（本用例不重启进程，`Drop` 是唯一退出口）。

## 关联需求或事故编号

- `test_plan/README.md` 第 4 条：查询口径只认后端 HTTP 契约，不碰前端。
- 与 `IT-API-TRACES-01` / `IT-API-APM-01` 共用同一套 `/v1/sql` 底层：那两条间接依赖它，
  这条把契约单独钉住，避免「SQL 口坏了但夹具还是绿的」。
