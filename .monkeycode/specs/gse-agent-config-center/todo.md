# Agent 配置中心：遗留项清单（TODO）

本文档记录 `gse-agent-config-center` 交付后**仍未完成、未验证或已决定不做**的事项。设计基线见同目录
`requirements.md` / `design.md` / `tasklist.md`；这里只写「还欠什么、为什么欠、怎么补、怎么验收」。
实施与发布过程（含 12～16 节的修复与验收）在 `tasklist.md`。

状态日期：2026-09-30。主体已上线：服务端 `v1.3.3-3e7353f`、agent `v1.3.4`，
四台 Agent（cloud2 / debian12 / testbkee / k8s 节点 `ser539375215934`）均在报数据。

## 一、交付现状（一句话口径）

| 能力 | 状态 |
| --- | --- |
| 一台 Agent 一份 spec（`params` + `items`）、保存≠下发、手动 `apply` | ✅ 上线 |
| Agent 认证后自动拉取（重连即收敛） | ✅ 上线（**v1.3.4 修过重连丢采集器，见 tasklist 15**） |
| 逐字段生效核验（`outcome` / `not_enforced` / `sync_status` / diff） | ✅ 上线 |
| 未实现字段（`cpu_limit_percent` / `mem_limit_percent` / `log_level`）只记录不生效 | ✅ 上线（UI 标「未实现（仅记录）」） |
| 管理口密码鉴权 | ⚠️ 代码已实现（v1.3.1），**生产未开启** → TODO-3 |

## 二、TODO 清单

### TODO-1 台账 `agents.version` 不刷新（控制台显示的版本永远是登记时那个）

- **现象**：`GET /api/gse/agents` 显示 `cloud2-agent` / `debian12-agent` / `testbkee` = `1.1.0`、
  `ser539375215934` = `1.2.0-rc1`，而目标机上实测三台都是 `1.3.4`（`sha256sum` + `--version`）。
  排查时据此判断版本会得出完全错误的结论（本次就误导过一次）。
- **原因**：`gse_proto::AuthRequest` 只有 `agent_id` / `token` 两个字段，**没有 version**；
  `ledger::upsert_agent` 虽然会更新 `version`（`ON CONFLICT ... version = excluded.version`），
  但 Agent 上线路径里没有任何地方带 version 调用它 —— 只有登记接口（`http.rs` 的 `PUT /api/gse/agents`）
  才写。于是 `agents.version` 停在登记那一刻。
- **修法**：`AuthRequest` 增加 `version: String`（`#[serde(default)]`，兼容尚未升级的旧 Agent）；
  `handle_auth` 认证成功后按 `agent_id` 刷新 `agents.version`（与 `status` / `last_heartbeat_at` 一起写，
  避免多一次读改写）；`vmctl agents` 与 console 节点页不需要改（它们读的就是这个字段）。
- **验收**：升级任意一台 Agent（或改 `--version` 重启）后，`GET /api/gse/agents` 的 `version`
  在**一个心跳周期（30s）内**变成新版本；旧版 Agent（不带 version 字段）仍能认证成功且不把已记录的版本清空。
- **成本**：proto 一处、server 一处、两条测试（带 version / 不带 version）；需要同时发 server 与 agent。

### TODO-2 作业台账的 `kind` 一律存成 `"script"`

- **现象**：`POST /api/gse/jobs` 用 `insert_job` 时 `..Default::default()` 落库，而真正派发用的是请求里的
  `kind` —— 于是 `agent_upgrade` / `file_transfer` 作业在 `GET /api/gse/jobs` 里都显示成 `script`，
  排查升级类作业时对不上号（本次升 3 台 Agent 时每个作业都显示 `script`）。
- **修法**：`insert_job` 传入真实 `kind`；补一条断言（提交 `agent_upgrade` 后查台账，`kind` 一致）。
- **验收**：提交三种 kind 的作业，台账里 `kind` 与提交值一致。

### TODO-3 管理口密码鉴权在生产未开启

- **现状**：`GSE_SERVER_ADMIN_PASSWORD` / `DATASERVER_GSE_ADMIN_PASSWORD` 代码支持（v1.3.1，
  空值=不鉴权），但 cops 的 `k8s.yaml` 未注入（`envFrom: secretRef`），`/opt/cops/secrets/vectorman.env`
  里也没有这两个键 —— 目前 `/api/gse/*` 裸奔（仅靠 NodePort/网络边界）。
- **修法**：cops 侧加 secret 注入 + 两个键；`vmctl` 用 `--password` / `VECTORMAN_PASSWORD`；
  开启后先验证 `401` / `WWW-Authenticate` 行为与 console 的填入口径。
- **注意**：开启会同时影响 console 前端（它直连 `/api/gse/*`），需要确认前端能把密码带上。

### TODO-4 通过配置中心轮换 token 会让 k8s Secret 失效

- **现象**：k8s 节点 Agent 的 token 来自 Secret `gse-agent-token`；若用 `PUT .../spec` 下发新 token
  （服务端有 `agents.prev_token` 双凭据宽限），Agent 当前进程会切到新 token，
  但 **Secret 里还是旧的** → 该 pod 一旦重启就认证失败。
- **修法**（任选）：轮换后同步更新 Secret（运维动作，写进 runbook）；或把「Secret 里的 token 才是权威」
  作为约定，禁止用 spec 下发 token（在下发接口对 k8s 类 Agent 拒绝 `token` 字段）。
- **验收**：轮换 token 后重启 pod 仍能认证（或接口明确拒绝该操作并给出原因）。

### TODO-5 三台 VM Agent 只下发了 `metrics_host`，未下发 eBPF 采集项

- **现状**：`cloud2-agent` / `debian12-agent` / `testbkee` 各一份 `metrics_host`（15s / 7d）；
  eBPF 四类采集只下在 k8s 节点（迁移来的 4 个 item）。这三台是 VM，跑 eBPF 应该更顺。
- **前置**：先确认目标机 eBPF 前置条件（BTF / 内核版本 / 权限），
  Agent 的 `agent_ebpf_capability` 指标会给出「不可用」的原因，先看它再下发。
- **验收**：下发后 `{agent_id="…"}` 能查到当前样本（与本次 metrics_host 同样的验收方式）。

### TODO-6 tsink 启动期 ENOENT：定性已知，未定位到具体文件（无害）

- **现象**：dataserver 启动时偶发 `background_errors_total` 增长，
  `last_background_error = flush worker error: IO error: No such file or directory (os error 2)`；
  错误文本**不带路径**（tsink 的 worker 监督器只包一层 `"{worker} worker error: {err}"`）。
- **已知**：本机用**生产数据完整副本**（926 段 / 7561 序列）+ 打开自监控 + 真实写入 + `strace` 复跑
  **复现不出来**；strace 里可见的 ENOENT 全是**可选文件探测**
  （`lane_blob/.compaction-replacements`、`series_index.delta.bin`、`.rollups/*.json`、`logs/meta.json`）。
  生产表现为启动瞬间 1～3 次、之后长时间不再增长。
- **影响**：自 v1.3.2 关掉 `background_fail_fast` 后**只涨计数器、不影响写入**；此前它会永久停写
  （实测静默丢 3 小时 41 分数据）。
- **运维判断法**：看到计数器在启动后增长，**先验证写入闭环**（`POST /v1/ingest` + query 查回），
  **不要因此重启 pod**（重启会再来一次同样的启动期噪声）。
- **要彻底定位**：需要给 tsink 的 worker 错误补上文件路径（fork/patch 上游），或在宿主机上用
  `inotifywait`/`fatrace` 盯住 data path 观察一次重启 —— 目前不值得，故留作 TODO。

## 三、已决定不做（非欠账，避免以后重复提）

| 项 | 决定 | 依据 |
| --- | --- | --- |
| `cpu_limit_percent` / `mem_limit_percent` / `log_level` 真实生效 | 本期不做，只记录 +「未实现（仅记录）」标注 | 需求阶段确认；`NOT_ENFORCED_FIELDS` + `not_enforced` 回执保留位置 |
| k8s Agent 镜像预装 bash / python3 | 不装，只带 busybox `sh`，解释器配置成 `["sh"]` | 用户决定「容器里就用 sh」；需要时 `apk add` 再评估镜像体积 |
| 作业表单解释器下拉按目标 Agent 的 `allowed_interpreters` 动态给值 | 不做，手选 `sh` 即可 | 用户决定 |
| Agent 侧本地配置文件里的采集项 | 彻底移除，items 只归 spec 中心 | 设计决策；因此 `desired=null` 的 Agent 不采集（需要下发 spec） |
