ID: IT-CHAIN-01
依赖: 无外部中间件（本地 redb + tsink，端口与目录全部临时生成）
命令: cargo test -p dataserver --test it_chain -- --nocapture
负责人: pit

# IT-CHAIN-01：gse-agent → gse-server → dataserver 三真进程端到端链路

覆盖「Agent 上线 → 采集项下发并生效 → metrics 上报入时序库 → Prom 带标签回读」这条主链路。
与 `bins/dataserver/tests/e2e.rs` 的区别：那边是进程内托管 router，本场景三个组件都是**真进程**，
跨进程走 geminio 会话与 HTTP。

## 前置数据

| 组件 | 形态 | 配置 / 临时资源 |
| --- | --- | --- |
| gse-server | 真进程 `gse-server`（`GSE_SERVER_CONFIG` 指向临时 toml） | agent 口 / 管理口 / 自监控口均为 `free_port()` 现取；台账 sqlite 在 `tempfile::tempdir()` 下；`auth_enabled=true`、`dataplane_probe_interval_secs=1`、`heartbeat_timeout_secs=30` |
| dataserver | 真进程 `dataserver --config <临时 toml>` | `data_path=<临时目录>/data`（目录需预先创建）；`kv_backend=redb`、`ts_backend=tsink`；SQL / Prom / 自监控口随机；`gse_admin_url` 指向 gse-server 管理口；`apm_enabled=false`、`self_metrics_interval_secs=0` 降低噪声 |
| gse-agent | 真进程 `gse-agent`（`GSE_AGENT_CONFIG` 指向临时 toml） | `server_addr` 指向 gse-server agent 口；`agent_id=agent-it-chain`、`token=it-chain-token`；`heartbeat_interval_secs=1`；`otlp_enabled=false` |
| 台账数据 | 经 gse-server HTTP 管理口写入 | 1 台 agent（`agent-it-chain`）、1 条 dataplane（`ds-it-chain`）、1 条 spec（1 个 `metrics_host` 采集项，`interval_secs=1`，`item_id=item-it-chain`） |

三个进程都通过 `Command::env_remove` 清掉继承来的 `DATASERVER_*` / `DP_*` / `GSE_*`，再注入本用例的配置，
避免外部环境污染测试。

## 执行步骤

1. `cargo build -p gse-server -p gse-agent`（测试内自动执行；`dataserver` 由 `CARGO_BIN_EXE_dataserver` 提供）。
2. 建临时目录，创建 `<tmp>/data`。
3. 现取 7 个回环随机端口：gse-server（agent / 管理 / 自监控）、dataserver（SQL / Prom / 自监控）、agent OTLP。
4. 写 `<tmp>/gse-server.toml` 并启动 gse-server 真进程。
5. 写 `<tmp>/dataserver.toml` 并启动 dataserver 真进程。
6. 轮询 `GET {gse_admin}/health`、`GET {ds_sql}/health` 至 200。
7. `POST {gse_admin}/api/gse/agents` 登记 `agent-it-chain`（token `it-chain-token`）。
8. `POST {gse_admin}/api/gse/dataplanes` 登记数据面：`ingest_url={ds_sql}`、`query_url={ds_prom}`；
   轮询 `GET /api/gse/dataplanes/ds-it-chain` 至 `status=="online"`（探活打 `{ingest_url}/health`）。
9. `PUT {gse_admin}/api/gse/agents/agent-it-chain/spec` 写入 1 个 `metrics_host` 采集项（`interval_secs=1`）。
10. 写 `<tmp>/gse-agent.toml` 并启动 gse-agent 真进程。
11. 轮询 `GET /api/gse/agents/agent-it-chain` 至 `status=="online"` 且 `last_heartbeat_at` 非空。
12. 轮询 `GET /api/gse/agents/agent-it-chain/spec` 至 `sync_status=="synced"` 且 `applied.spec.items[0].item_id=="item-it-chain"`。
13. 轮询 `GET {ds_prom}/api/v1/query?query=cpu_usage&time=<当前秒>` 至返回非空结果。

## 期望结果

- 步骤 6：两个 `/health` 都返回 HTTP 200。
- 步骤 7/8/9：均返回 2xx。
- 步骤 11：台账 `status=online`（认证成功 + 心跳落库），`last_heartbeat_at` 非空。
- 步骤 12：`sync_status=synced`，且 agent 回执里的采集项就是下发的 `item-it-chain`（证明 spec 被真 agent 解析并生效）。
- 步骤 13：`status=="success"`，`data.result` 非空；第一个 series：
  - `metric.agent_id == "agent-it-chain"`
  - `metric.item_id == "item-it-chain"`
  - `value[1]` 可解析为数字，且落在 `0..=100`（CPU 使用率）
- 测试末尾（步骤 13 之后）收尾自检：三个子进程都仍在运行（`try_wait()` 为 `None`）。
- 任一步超时（上限 120s）或子进程提前退出：测试 panic，并打印 gse-server / dataserver / gse-agent 的
  stdout/stderr 尾部。

## 清理方式

- 三个子进程由 `Proc` 的 `Drop` 执行 `kill()` + `wait()`，测试成功、失败、panic 都回收，不留孤儿进程。
- 临时目录由 `tempfile::tempdir()` 在测试结束时递归删除（台账 sqlite、redb、tsink、日志）。
- 随机端口随进程退出释放。

## 关联需求或事故编号

无，新增覆盖。
