# IT-DS-BOOT-01 · dataserver 真进程启动链路

| 项 | 值 |
|---|---|
| ID | `IT-DS-BOOT-01` |
| 优先级 | P0（PR 门禁） |
| 测试文件 | `bins/dataserver/tests/boot_smoke.rs` |
| 命令 | `cargo test -p dataserver --test boot_smoke` |
| 依赖 | 无外部中间件（redb + tsink 本地后端） |
| 负责人 | mike |

## 这条覆盖什么

`e2e.rs` 是**进程内**托管 axum router 的测试，它覆盖不到「真实二进制怎么把自己装起来」这一段：
配置文件解析、`DATASERVER_*` / `DP_*` 环境变量覆盖、后端选择与初始化、端口监听、非法配置
的退出码与报错。这一段出错的后果是**部署当场起不来**，或者更糟——起得来但后端选错了
（写入落不到预期存储，且没有任何报错）。

所以本场景跑的是编出来的二进制（`env!("CARGO_BIN_EXE_dataserver")`），不是 router。

## 前置数据

- 一个临时目录，里面：
  - `config.toml`：`data_path` 指向临时目录下的 `data/`；`kv_backend = "redb"`、
    `ts_backend = "tsink"`；`apm_enabled = false`；`self_metrics_interval_secs = 0`；
    `apm_clean_interval_secs = 0`、`apm_agg_interval_secs = 0`（关掉后台任务，避免干扰断言）；
    `[sql_http]` / `[prom_http]` / `[metrics_http]` 各绑 `127.0.0.1:<随机端口>`。
  - 三个端口都由测试先 `bind("127.0.0.1:0")` 取空闲端口再释放。
- 进程环境：把 `apply_env()` 会读的**全部** `DATASERVER_*` / `DP_*` 变量 `env_remove` 掉。
  不清的话，开发机上残留的 `DATASERVER_TS_BACKEND=victoriametrics` 会把配置文件里的值
  悄悄改掉，这条测试就不测它自己声称测的东西了。

## 执行步骤

### A. 正常启动与数据回读

1. 写 `config.toml`，`Command::new(binary).arg("--config").arg(config)` 起子进程。
2. 轮询 `GET /health`，最多 40 秒（连接被拒是中途状态，不算失败）；返回
   `200` 且 body 含 `"status":"ok"` 视为就绪。超时则杀掉子进程，**打印它的 stdout/stderr** 再失败。
3. `GET /v1/ts/stats` → `series_count == 0`（空库）。
4. `POST /v1/ingest`（metrics envelope，`record_id = "boot-1"`，`measurement = "boot_cpu_usage"`，
   `tags = {"src":"boot_smoke"}`，`field_name = "value"`，`field_value = 42.5`）→ `200` 且 body 含
   `"accepted":1`。
5. `GET /v1/ts/stats` → `series_count >= 1`。
6. `GET /api/v1/query?query=boot_cpu_usage&time=<点的秒级时间戳>` → `status == "success"`，
   `data.result` 恰好 1 条，`value[1] == 42.5`，`metric.src == "boot_smoke"`。

> 时间戳取「当下整秒」（`now_micros() / 1e6 * 1e6`）。两个坑：整秒是为了让秒粒度的
> `time=` 参数不落在点之前；取当下是为了不被 `ts_retention` 窗口过滤掉。

### B. 非法后端配置（三个子场景，逐条独立跑）

配置里追加一项（必须写在**第一个表头之前**，否则会被归到 `[metrics_http]` 表，
而 `HttpListenConfig` 会静默忽略未知字段，导致「非法配置」其实合法、进程正常起来）：

| 子场景 | 配置 | 期望 |
|---|---|---|
| kv_backend 非法 | `kv_backend = "mysql"` | `exit != 0`；stderr 含 `unsupported kv_backend` 与 `kv_backend` |
| kv_backend=redis 缺地址 | `kv_backend = "redis"`（不给 `redis_url`） | `exit != 0`；stderr 含 `kv_backend=redis requires redis_url` 与 `DATASERVER_REDIS_URL` |
| ts_backend=victoriametrics 缺地址 | `ts_backend = "victoriametrics"`（不给 vm url） | `exit != 0`；stderr 含 `ts_backend=victoriametrics`、`vm_insert_url`、`vm_select_url`、`DATASERVER_VM_INSERT_URL`、`DATASERVER_VM_SELECT_URL` |

每个子场景用 `try_wait()` 轮询 + 30 秒看门狗等进程退出。**不能用 `Command::output()`**：
它没有超时，一旦「非法配置」其实合法、进程开始服务，测试会永久挂住（本场景开发时实测踩过一次）。

## 期望结果

- A 部分全部断言通过，且子进程在 `Server` 的 `Drop` 里被 `kill + wait`（不留孤儿进程）。
- B 部分三个子场景都以非 0 退出码结束，且 stderr 精确点名了缺的键/变量——**不是**只退出非 0
  就算过（那会把「TOML 拼错」和「后端配置校验生效」混为一谈）。

## 清理方式

`tempfile::TempDir` 随作用域删除（含 `data/` 下的 redb / sqlite / tsink 目录）；
子进程在 `Drop` 里 `kill + wait`。失败路径也会走 `Drop`，不需要手工清。

## 关联需求或事故

- 无独立事故编号，新增覆盖。
- 对应的产品语义：`docs/external-storage.md` §1「配置」、§2「切换步骤」——外置后端全由
  `kv_backend` / `ts_backend` + 地址开关控制，配错必须在启动期就红，不能静默降级。
- 相关既有覆盖：`bins/dataserver/tests/e2e.rs`（进程内 router 链路），本场景补进程级那一段。
