ID: IT-VMCTL-01
依赖: 无外部中间件（gse-server 临时 sqlite + dataserver 本地 redb/tsink，端口与目录全部临时生成）
命令: cargo test -p vmctl --test it_vmctl -- --nocapture
负责人: pit

# IT-VMCTL-01：真 `vmctl` 二进制 × 真 gse-server 台账 × 真 dataserver 流索引

覆盖问题：`agents status` / `agents doctor` 对 gse-server 返回字段的解析此前**只有手写 JSON 夹具**。
夹具里的字段名是我们自己写的，真实字段漂移测不出来 —— `4a91826` 就是现场：`ebpf_tcp` 被按
「eBPF 家族都出边记录」猜成 `ebpf_edges`，而它实际只在 `metrics` 上报，于是一个健康的采集项被报成
`not_reporting`。

本用例的被测对象是**真 `vmctl` 二进制**，输入全部来自真进程：spec / 会话 / 心跳来自真 `gse-server`
台账，流索引来自真 `dataserver`；断言直接读 `/v1/streams` 与 `/api/gse/agents*` 台账，不与夹具比对。

## 前置数据

| 组件 | 形态 | 配置 / 临时资源 |
| --- | --- | --- |
| gse-server | 真进程 `gse-server`（`GSE_SERVER_CONFIG` 指向临时 toml） | agent 口 / 管理口 / 自监控口均为 `free_port()` 现取；台账 sqlite 在临时目录；`auth_enabled=true`、`dataplane_probe_interval_secs=1`、`heartbeat_interval_secs=1`、`heartbeat_timeout_secs=30` |
| dataserver | 真进程 `dataserver --config <临时 toml>` | `data_path=<临时目录>/data`（预创建）；`kv_backend=redb`、`ts_backend=tsink`；SQL / Prom / 自监控口随机；`gse_admin_url` 指向 gse-server 管理口；`apm_enabled=false`、`self_metrics_interval_secs=0` |
| gse-agent | 真进程 `gse-agent`（`GSE_AGENT_CONFIG` 指向临时 toml） | `server_addr` 指向 gse-server agent 口；`agent_id=agent-it-vmctl`、`token=it-vmctl-token`、`host_id=host-it-vmctl`；`heartbeat_interval_secs=1`、`otlp_enabled=false` |
| 台账 agent | 经 gse-server 管理口 `POST /api/gse/agents` 登记 | 一台 `agent-it-vmctl`（认证前必须先登记：auth 只 UPDATE status） |
| 台账 dataplane | `POST /api/gse/dataplanes` | `service_id=ds-it-vmctl`、`ingest_url=<dataserver SQL 口>`、`query_url=<dataserver Prom 口>`，轮询到 `status=online` |
| 期望 spec | `PUT /api/gse/agents/agent-it-vmctl/spec` | 4 个采集项：`item-it-vmctl-metrics`（`metrics_host`，`interval_secs=1`，启用）、`item-it-vmctl-tcp`（`ebpf_tcp`，60s，启用）、`item-it-vmctl-net`（`ebpf_network`，60s，启用）、`item-it-vmctl-log`（`log_file`，`enabled=false`） |
| 流索引（real agent） | 真 `gse-agent` 采集 `metrics_host` → dataserver `/v1/ingest` | `metrics` / `item-it-vmctl-metrics` |
| 流索引（eBPF 两条） | 测试用 HTTP 走 dataserver **真实接入路径** `/v1/ingest` 各发 1 批 | `metrics` / `item-it-vmctl-tcp`（与真实现一致：`ebpf_tcp` 只出 metrics）；`ebpf_edges` / `item-it-vmctl-net`（`ebpf_network` 是家族里唯一出边记录的）。**故意不给 `item-it-vmctl-tcp` 发 `ebpf_edges`** |

三个真进程都通过 `Command::env_remove` 清掉继承来的 `DATASERVER_*` / `DP_*` / `GSE_*`，再注入本用例的配置。
eBPF 项在非 root / 无 BTF 的机器上前置校验会失败，agent 只降级**该项**并打日志（`ebpf item … disabled`），
spec 仍然生效、其它采集项照常上报 —— 这正是 `4a91826` 的现场条件，不构成跳过。

## 执行步骤

1. `cargo build -p gse-server -p gse-agent -p dataserver`（测试内自动执行；`vmctl` 由 `CARGO_BIN_EXE_vmctl` 提供）。
2. 建临时目录，创建 `<tmp>/data`、`<tmp>/jobs`。
3. 现取 7 个回环随机端口：gse-server（agent / 管理 / 自监控）、dataserver（SQL / Prom / 自监控）、agent OTLP。
4. 写 `<tmp>/gse-server.toml` 并启动 gse-server 真进程。
5. 写 `<tmp>/dataserver.toml` 并启动 dataserver 真进程。
6. 轮询 `GET {gse_admin}/health`、`GET {ds_sql}/health` 至 200。
7. `POST {gse_admin}/api/gse/agents` 登记 `agent-it-vmctl`。
8. `POST {gse_admin}/api/gse/dataplanes` 登记 `ds-it-vmctl`；轮询 `GET /api/gse/dataplanes/ds-it-vmctl` 至 `status=="online"`。
9. `PUT {gse_admin}/api/gse/agents/agent-it-vmctl/spec` 写入上表 4 个采集项。
10. 写 `<tmp>/gse-agent.toml` 并启动 gse-agent 真进程。
11. 轮询 `GET /api/gse/agents/agent-it-vmctl` 至 `status=="online"` 且 `last_heartbeat_at` 非空。
12. 轮询 `GET …/spec` 至 `sync_status=="synced"` 且 `applied.spec.items` 恰为 4 项。
13. 轮询 `GET {ds_sql}/v1/streams` 至出现 `metrics` / `item-it-vmctl-metrics`（真 agent 上报入库）。
14. `POST {ds_sql}/v1/ingest` 发两条 eBPF 流（`metrics` / tcp、`ebpf_edges` / net），轮询 `/v1/streams` 至两条都出现。
15. 从 `GET …/spec` 的 `desired.spec.items` 读期望采集项集合（不写死），从 `/v1/streams` 读流台账。
16. 跑 `vmctl --url {gse_admin} --data-url {ds_sql} agents doctor agent-it-vmctl`，断言退出码与表格。
17. 跑 `vmctl --url {gse_admin} --data-url {ds_sql} agents status agent-it-vmctl`，断言退出码与逐行字段；与直接读 `/v1/streams`、`/api/gse/agents` 的结果比对。
18. 停掉 agent 进程，等 `/v1/streams` 的 `accepted`/`last_seen_micros` 连续两次采样不变（冻结计数），再跑第 17 步比对。

## 期望结果

- 步骤 6/7/8/9/14：都返回 2xx。
- 步骤 11：台账 `status=online`、`last_heartbeat_at` 非空。
- 步骤 12：`sync_status=synced`，`applied.spec.items` 恰为 4 项（真 agent 解析并生效了下发内容）。
- 步骤 15（前置自检，失败即说明用例失去判别力）：
  - 台账里**没有** `ebpf_edges` / `item-it-vmctl-tcp`；
  - 台账里有 `metrics` / `item-it-vmctl-tcp`；
  - 台账里有 `ebpf_edges` / `item-it-vmctl-net`。
- 步骤 16（doctor，agent 仍在线）：
  - 退出码 0，输出含 `session_state: online`（且与 `GET /api/gse/agents` 里该 agent 的 `session_state` 一致）、`summary: ok`；
  - 采集项表格 4 行，`item-it-vmctl-tcp` 行为 `data_type=metrics`、`verdict=reporting`。
- 步骤 17/18（status，计数已冻结）：
  - 退出码 0，输出含 `spec: synced`、`summary: ok`；
  - 表格行数 = 台账采集项数（4），`item_id` 集合与 `desired.spec.items` 排序后完全一致；
  - 每个启用项：`(data_type, item_id)` 必须在 `/v1/streams` 里真实存在，`accepted` 与台账**完全相等**，
    `verdict=reporting`，`last_seen` 非空，且台账 `last_seen_micros` 距今 ≤ 60s；
  - 停用项 `item-it-vmctl-log`：`data_type=logs`、`verdict=disabled`、`accepted=0`；
  - 定点断言（`4a91826`）：`item-it-vmctl-tcp` 必须是 `data_type=metrics` + `verdict=reporting`
    （按 `emits_edges` 猜映射的实现会报 `ebpf_edges` / `not_reporting`，此断言必挂）；
    `item-it-vmctl-net` 必须是 `data_type=ebpf_edges`；`item-it-vmctl-metrics` 必须是 `data_type=metrics`。
- 任一步超时（上限 180s）或子进程提前退出：panic 并打印 gse-server / dataserver / gse-agent 的 stdout/stderr 尾部。
- 无 `#[ignore]`、无 feature gate、无「缺二进制就 return」：`cargo test` 默认即跑。

## 清理方式

- 三个子进程由 `Proc` 的 `Drop` 执行 `kill()` + `wait()`；测试中途停 agent 用 `Chain::stop`（同样是 kill + wait）。
- 临时目录由 `TempRoot` 的 `Drop` 递归删除（gse-server 台账 sqlite、dataserver redb/tsink、agent 作业目录）。
- 随机端口随进程退出释放。

## 关联需求或事故编号

`4a91826` —— `agents status` 把健康的 `ebpf_tcp` 采集项报成 `not_reporting`（字段/映射漂移，夹具掩盖）。
