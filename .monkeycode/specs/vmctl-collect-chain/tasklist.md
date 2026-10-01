# Task List

> 本 feature 是**纯客户端**：服务端三条 spec 路由与数据面两条读路由都已实现于 `main`。
> 因此每一步的验收都要求能在**不发写请求**的前提下证明（单测用 `FakeTransport` 计数、
> 集成用 `--dry` 路径或只读命令）。
>
> 需求见 `requirements.md`（本版已整体重新定范围，2026-09-30），设计见 `design.md`。

## 0. 实现前先确认（不做完不动代码）

- [x] 0.1 重新核对服务端路由与响应形状**仍在 `main` 上未变**：
      `GET /api/gse/agent-specs`、`GET|PUT /api/gse/agents/{id}/spec`、
      `POST /api/gse/agents/{id}/spec/apply`、`GET /api/gse/agents/{id}`；
      以及数据面 `GET /v1/streams`、`GET /v1/ebpf/capability`、`GET /health`。
      逐一 `curl` 本机或 cloud3，把实测响应（含 `sync_status` / `diff` / `not_enforced` 的真实取值）
      **存成夹具常量**，供 4.5 使用。对应需求 R3 / R4 / R5。
- [x] 0.2 核对 `crates/gse-agent-core/src/collect/*.rs` 的 kind → data_type 映射未变
      （`ebpf.rs` 的 `EbpfSink` 三个方法、`metrics.rs` / `logfile.rs` / `k8s.rs` / `otlp.rs` 的 push 调用点）。
      若与 `design.md` 的 `KIND_DATA_TYPES` 不一致，**先改 design 再写代码**。对应设计「kind → data_type 映射」。
- [x] 0.3 确认 `bins/vmctl/src/lib.rs` 的 `Transport` / `Output` / `FakeTransport` 是可直接复用的
      （`Output::ok` / `Output::err` 私有构造函数是否够用；`FakeTransport` 是否在 `#[cfg(test)]` 内）。
      对应设计「Transport 复用」节。
- [x] 0.4 确认 `vmctl` 现有输出风格（`jobs list` 的表格填充方式、`url=... reason=... code=...` 错误格式），
      本 feature 的表格与错误信息**照抄**，不新造风格。对应需求 R7。
- [x] 0.5 确认 `bins/vmctl/Cargo.toml` 确实无需新依赖
      （`serde_json` / `clap` / `ureq` 已在；并发用 `std::thread::scope`，不引 async runtime）。对应设计 P2。

## 1. 骨架与参数（需求 R1、R2）

- [x] 1.1 `main.rs` 的 `Cli` 增加 `--data-url`（缺省 `http://127.0.0.1:8081`，
      与 `dpc --sql-url` 一致），并在 `main()` 里构造 `DataClient`。
- [x] 1.2 `Command::Agents` 下新增五个变体：`Specs`、`Spec { cmd }`、`Status`、`Doctor`；
      `AgentsCmd::Spec` 再套一层 `SpecCmd { Get, Put, Apply }`。
      参数：`specs --table --agent-id`、`spec get <agent_id>`、
      `spec put <agent_id> -f <path> | --json <json>`、`spec apply <agent_id>`、
      `status <agent_id> --table`（默认已是表格，`--table` 只为口径统一）、`doctor <agent_id>`。
- [x] 1.3 `--help` 验收：`vmctl --help` **仍然只有** `health` / `hosts` / `agents` / `jobs` 四个顶层命令；
      `vmctl agents --help` 列出六个；`vmctl agents spec --help` 列出三个。
      写成单测（`Cli::command().debug_assert()` 或 `try_parse_from` + 断言）。
- [x] 1.4 回归：`hosts` / `jobs` / `health` / `agents list|get` 的请求路径、请求体、输出格式不变。
      用既有单测守住（跑一遍确认零改动）。

- **检查点 A**：`cargo test -p vmctl` 全绿；`--help` 三级断言通过。

## 2. 透传类命令（需求 R3、R4）

- [x] 2.1 `lib.rs` 新增 `Client::agents_specs()` / `agents_spec_get(id)` / `agents_spec_put(id, body)` /
      `agents_spec_apply(id)` / `agent_get(id)`；四者走 `base_url`，`agent_get` 走 HTTP `GET`，转发服务端正文。
      错误口径复用既有 `ureq::Error::Status` 分支（stderr `url=... reason=... code=...`）。
- [x] 2.2 `spec.rs`：`read_json_object(source: &SpecSource) -> Result<serde_json::Value, String>` ——
      `-f` / `--json` 二选一（都给 / 都不给都报错），读文件失败 / 非 JSON / 非对象各有可读错误信息，
      **不补默认值、不裁剪字段**。对应设计 Pitfall「`spec put` 不能补默认值」。
- [x] 2.3 `spec.rs`：`agent_specs_rows(body) -> Option<Vec<SpecsRow>>` 与 `render_specs_table`；
      解析失败返回 `None` → 调用方透传原正文并退出码 0。`--agent-id` 过滤解析失败时透传全量 + stderr 提示。
      对应设计 P3。
- [x] 2.4 `spec.rs`：`render_spec_get_table(view)` —— `agent_id` / `session_state` / `sync_status` /
      `desired.revision` / `applied.revision` / `applied.outcome` / `applied.not_enforced` /
      `desired.spec.items` 条数 / `diff` 是否为空；`desired` / `applied` / `diff` 为 null 时该列留空不报错。
- [x] 2.5 单测：`-f` + `--json` 同时给 → 报错且 `FakeTransport` 调用次数为 0；
      `-f` 指向缺失文件 / 非 JSON / JSON 数组 → 报错且不发请求；`spec get` 404 → 退出码 1 且 stderr 含服务端正文。
- [x] 2.6 单测：响应是数组 / 缺 `agent_id` → `agent_specs_rows` 返回 `None`，命令退出码 0 且 stdout 为原正文。
- [x] 2.7 单测：错误信息**不含**请求体内容（构造含 `"token":"s3cret"` 的 body，断言 stderr 里没有 `s3cret`）。
      对应需求 R7 与设计 Pitfall「token 只在 desired 里出现」。

- **检查点 B**：五个命令的透传与错误路径都有单测；`spec put` 的本地校验**先于**任何网络调用。

## 3. 生效核验（需求 R5）

- [x] 3.1 `status.rs`：`KIND_DATA_TYPES` 常量（八类 kind → data_type 数组）+ 未知 kind → `unknown_kind`。
- [x] 3.2 `status.rs`：`classify(stream: Option<&StreamRow>, interval_secs: u64, now_micros: i64) -> Verdict`
      **纯函数**（不读时钟）。阈值 `max(3 * interval, 60)` 秒；`interval` 归一（缺失 / 非数字 / ≤0 → 15）。
- [x] 3.3 `status.rs`：`expand_items(desired) -> Vec<(item_id, kind, enabled, interval_secs)>` 与
      `join_streams(items, streams, agent_id, now)` —— 按 `(agent_id, data_type, data_id == item_id)` join，
      一个 item 展开多行。对应设计「kind → data_type 映射」。
- [x] 3.4 `status.rs`：`render_status(...)` —— 逐 `(item_id, data_type)` 一行（`item_id` / `data_type` /
      `last_seen` 相对年龄 / `accepted` / verdict），**额外**一行 spec 同步状态；
      `enabled = false` 的行标 `disabled` 且不参与退出码；`sync_status != synced` 标 `dirty`。
- [x] 3.5 `status.rs`：退出码判定 —— 全部 enabled 项 `reporting` **且** `sync_status == "synced"` → 0，否则 1。
      数据面不可达 → 全部 `unknown` + 末行原因 + 退出码 1。对应需求 R5 末两条验收。
- [x] 3.6 单测（判定边界）：无 stream / 恰好等于阈值 / 阈值 -1µs / 阈值 +1µs / `last_seen` 在未来 /
      `interval_secs = 0` / `interval_secs` 缺失 —— 七组都用 `classify` 直接断言。
- [x] 3.7 单测（映射）：八类 kind 逐个断言展开结果；未知 kind 断言 `unknown_kind` 且**不影响**退出码。
- [x] 3.8 单测（缺字段）：`desired = null` → 报「未保存过 spec」而非空表当成功；
      `collector` 缺 `interval_secs` → 按 15 秒算阈值。
- [x] 3.9 实现 P2 的并发取数：`std::thread::scope` 两个 `join`（GSE spec + dataserver streams）。
      断言：加一个 1 秒延迟的 `FakeTransport`，总耗时应 < 1.8 秒（证明是并发不是串行）。

- **检查点 C**：`status` 的判定函数有七组边界单测；`dirty` 与 `stale` 的区分有单测守住。

## 4. 链路诊断（需求 R6）

- [x] 4.1 `doctor.rs`：五个段落函数各自返回 `Section { title, lines, ok: Option<bool> }`，
      任一失败不 `?`、不 return，只把该段标 `unknown` 并把原因收进 summary。
- [x] 4.2 `doctor.rs`：段 1（Agent 台账）404 → **立即退出**退出码 1，后续段一个请求都不发
      （单测断言 `FakeTransport` 调用次数为 1）。
- [x] 4.3 `doctor.rs`：`summary:` 末行 —— 逐条列失败段与原因；全绿时打一行 `summary: ok`。
- [x] 4.4 `doctor.rs`：退出码 = 段1 `session_state == online` && 段5 health ok &&
      段2 `sync_status == synced` && 段3 全部 enabled 项 `reporting`。**不看** `status` 字段。
      对应设计 P4 与 Pitfall「`sync_status` 与 `status` 不是一回事」。
- [x] 4.5 单测（真实夹具）：用 0.1 存的真实响应构造全绿场景与「Agent offline」场景，
      断言退出码与 summary 文案；断言 render 里 `data_type` 行**同时**含 `item_id`
      （防 `ebpf_process` 与 `metrics_host` 两行看起来一样）。
- [x] 4.6 单测：段 4（capability）/ 段 5（health）不可达时该段标 `unknown`，其余段仍输出完整，
      summary 记明原因，退出码 1。
- [x] 4.7 单测：doctor **不发** `POST` / `PUT` —— 用 `FakeTransport` 断言全部调用方法为 `GET`
      （记录 `(method, url)` 二元组后逐条断言）。对应需求 R6 末条验收。

- **检查点 D**：doctor 五段各自有「不可达」用例；Agent 404 短路有计数断言。

## 5. 输出与文档

- [x] 5.1 表格列宽用空格填充到固定宽度（照抄 `jobs list` 现状），不用 `\t`。对应设计 Pitfall。
- [x] 5.2 `README.md` 的 `vmctl` 章节补五条命令与一个完整示例
      （`spec put -f spec.json` → `spec apply` → `status`，含期望输出与退出码含义）。
- [x] 5.3 `README.md`「已知限制」确认三条已在（管理口鉴权默认关闭、dataserver 默认 `NoopAuth`、
      eBPF 需内核 ≥5.8 + BTF + 权限）；未在则补。对应需求 R8 已知限制 1–3。
- [x] 5.4 `.monkeycode/specs/README.md` 索引表把本 feature 的状态由「🟡 ACTIVE（规格完成，待实施）」
      改为「✅ 已实现」并更新截至日期与一句话。
- [x] 5.5 ~~`dpc` 是否为 `data` 子命名空间留注释：在 `bins/dpc/src/main.rs` 顶部加一行说明
      「dataserver 查询的 CLI 入口在此；`vmctl data` 已评估并决定不做（见 vmctl-collect-chain 修订记录）」，
      避免以后重复立项讨论。**不改 `dpc` 行为**。

- **检查点 E**：文档与实现一致；`README` 示例真跑一遍确认可复制。

## 6. 收口

- [x] 6.1 `cargo fmt --all -- --check`、`cargo clippy --all-targets --all-features -- -D warnings`、
      `cargo test --all-features` 全绿。
- [x] 6.2 集成测试接环境变量开关：`VECTORMAN_E2E_URL` / `VECTORMAN_E2E_DATA_URL` 都设置时运行，
      否则跳过（与 `ebpf-live.test.tsx` 同一约定）。本机起真服务跑一次：
      `specs` / `spec get` / `status` 三条，记录实测输出。
- [x] 6.3 真机验收（cloud3 或 testbkee）：五条命令逐个执行，记录退出码与关键输出；
      含一条**刻意失败**的用例（例如对未保存 spec 的 Agent 跑 `status`，断言报错文案与退出码 1）。
- [x] 6.4 把 6.3 的实测结论追加到本文件下方「实现完成情况」一节。
- [x] 6.5 若实现中发现需求 / 设计口径错误，**先改 `requirements.md` / `design.md` 再改代码**，
      并在修订记录里写明原因（本仓库已多次踩「照抄过期口径」的坑）。

- **检查点 F**：CI 全绿；真机五条命令结论落盘；本 feature 的每条验收都有对应实现或明确的不做理由。

## 8. 实现完成情况（2026-09-30）

**已实现并验证**：§0 全部核对、§1–§4 全部、§5.1–§5.4、§6.1/§6.2。

- 交付五个命令：`agents specs` / `agents spec get|put|apply` / `agents status` / `agents doctor`；
  顶层子命令仍为四个（`health` / `hosts` / `agents` / `jobs`），有单测断言。
- 新增文件：`bins/vmctl/src/{spec.rs,status.rs,doctor.rs}`；`lib.rs` 加 `DataClient` 与五个方法；
  `Cargo.toml` 加 `test-support` feature（供二进制测试复用 `Mock`）。
- 质量基线：`cargo test --all-features` 全绿（vmctl 48 个用例：lib 23 + bin 25）；
  `cargo clippy --all-targets --all-features -- -D warnings` 0 告警；`cargo fmt --all -- --check` 通过。

### 实测（本机起 gse-server 27101 + dataserver 27201，真夹具）

夹具用**真实响应**抓取（不是手写），存为 `main.rs` 的 `REAL_SPEC_VIEW` / `REAL_SPECS_LIST` /
`REAL_AGENTS_LIST` / `REAL_AGENT_ONE` 常量。

| 命令 | 实测 | 退出码 |
| --- | --- | --- |
| `agents specs --table` | 2 行（agent-1 有 spec、agent-2 无），`items` 列为 3 / 空 | 0 |
| `agents spec get agent-1 --table` | 11 个键值行，`applied.*` 与 `diff` 全空（`null`） | 0 |
| `agents spec put agent-1 -f spec.json` | 200；`desired.revision` 由 `6c4338bc` 变为 `2943c620` | 0 |
| `agents spec apply agent-1`（离线） | `{"code":"agent_offline","error":"agent agent-1 无会话"}` | 1 |
| `agents spec get ghost` | `{"code":"not_found","error":"agent ghost 没有 spec"}` | 1 |
| `agents status agent-1` | 3 行 `(item_id, data_type)`：reporting / stale / disabled + `dirty` 行 | 1 |
| `agents doctor agent-1` | 5 段全输出，summary 逐条列未通过原因 | 1 |
| `agents doctor ghost` | 短路，只发 2 个请求（单台 + 列表） | 1 |

### 实施中发现并修正的三个口径错误（均已回写 design.md / requirements.md）

1. **`updated_at` / `reported_at` 不是 ISO 时间**，是台账序列字符串（`1790831429653846-7` = `{unix_micros}-{seq}`）。
   设计初稿按 ISO 写，已改为「只透传显示、不解析成时间」。
2. **`GET /api/gse/agents/{id}` 返回裸 `Agent`，不含 `session_state`**；会话口径只在列表端点上。
   初稿让 `doctor` 只查单台 → 实测输出 `session_state: `（空），把「在线」误判成「不在线」。
   已改为两个端点并发查，并加单测 `doctor_takes_session_state_from_list_not_single_agent`。
3. **`GET /api/gse/agents*` 明文回 `token`**（未脱敏）。新增一条需求：`status` / `doctor`
   不得透传该响应，只渲染字段白名单；加单测 `doctor_does_not_leak_plaintext_token`。

### 实施中发现的两个自身缺陷（已修）

- **顺序回放的 Mock 在并发下随机失败**：`doctor` 并发发请求，顺序不确定。
  改为**按 URL 后缀匹配**的 `Mock::routed`（长后缀优先）。这是测试基础设施的坑，值得记：
  并发代码不能用顺序脚本化 mock。
- **`doctor` 先并发再判 404 会白花四次往返**：实测定到 6 个请求。
  改为「先查台账主体（2 个并发） → 404 立即返回 → 才发余下 4 个」，
  单测 `doctor_short_circuits_when_agent_missing` 断言 `call_count() == 2`。

### 未完成（明确边界）

- **§5.5**：`dpc` 顶部加注释说明「`vmctl data` 已决定不做」—— 未做（`dpc` 本版冻结，
  只改注释不留价值，规格修订记录已承载该结论）。
- **§6.3 真机验收**：本机三件套实测已完成（上表）；cloud3 / testbkee 真集群复跑待做
  （与 `observability-hardening` 的集群验证同批执行更省事）。

## 9. 真集群验收结果（2026-10-01，cloud3 k3s）

镜像 `ghcr.io/abrance/vectorman-server:v1.3.7-38c1edf`（digest `sha256:8d9348c2…`），
经 `kubectl set image` 升级三件套；Agent 侧 `v1.3.6` 未动（本 feature 不改 Agent）。

### 五条命令逐个实测

| 命令 | 实测结果 | 退出码 |
| --- | --- | --- |
| `agents specs --table` | 4 台 Agent，全部 `synced`（`items` 1/1/4/1） | 0 |
| `agents spec get ser539375215934 --table` | `session_state: online`、`applied.outcome: applied`、`diff: empty` | 0 |
| `agents spec put cloud2-agent -f` | 200；`sync_status` 由 `synced` → **`stale`**；revision 变更 | 0 |
| `agents spec apply cloud2-agent` | 返回 `ack`；5 秒后 `sync_status` 回到 `synced` | 0 |
| `agents status cloud2/debian12/testbkee` | 三台 VM 全绿，`summary: ok` | **0** |
| `agents status ser539375215934` | 3 reporting / 1 not_reporting（见下） | 1 |
| `agents doctor ser539375215934` | 5 段完整：会话 `online`、`job_channel_available: true`、`capability reported: 4` | 1 |
| `agents status ghost-agent` | 404 + `agent ghost-agent 没有 spec`（刻意失败用例） | 1 |

### 「不补默认值」的实测证据（本 feature 最关键的正确性属性）

对 `cloud2-agent` 只提交 `{"params":{"max_concurrent_jobs":2}}`，其余 params 一个不给：

| 字段 | 提交前 | 提交后 | 结论 |
| --- | --- | --- | --- |
| `max_concurrent_jobs` | 1 | **2** | 改到了 |
| `allowed_interpreters` | `["bash","sh","python3"]` | 同左 | **未被清空** |
| `heartbeat_interval_secs` | 30 | 30 | 未变 |
| `otlp_listen` | `0.0.0.0:4318` | 同左 | 未变 |

若 CLI 照自己的默认值补全，`allowed_interpreters` 会被重置成默认集、
Agent 上原有的三解释器配置会静默丢失 —— 真集群证实了这条口径是对的。
验收后已按备份**完整还原**（7 个字段逐项核对一致）。

### 抓到的 bug：`ebpf_tcp` 期望 data_type 写错（已单开 PR #116）

`ser539375215934` 有 4 个 eBPF 采集项。`agents status` 实测：

```
item-…-0  ebpf_edges  25   reporting
item-…-1  ebpf_edges   0   not_reporting   ← 误报
item-…-2  metrics    621   reporting
item-…-3  metrics    497   reporting
```

该 item 的 kind 是 `ebpf_tcp`，它**只产出指标**：`EbpfItemKind::emits_edges()`
（`crates/gse-agent-ebpf/src/attach.rs:61`）只对 `Network` 为真 —— 它和 `ebpf_network`
用不同 map（`TCP_AGG` vs `CONN_AGG`），两边都发边记录会因 sqlite 主键覆盖写而互相丢数据。
它在 `metrics` 上 `accepted=11`，是**健康的**。

用真集群抓下的 spec + streams 复算两版映射：初版 1 个 `not_reporting` → 修正后 4/4 `reporting`。

**为什么单测与本机实测都没抓到**：本机 scratch 环境只造了 `metrics` 与 `ebpf_edges`
两种流，没有 `ebpf_tcp` 的真实上报；我当初推映射的依据是「各采集器的 push 调用点」，
但 `network` 与 `tcp` 共用同一条 `run_loop`（`collect/ebpf.rs` 的 `_ =>` 分支）、
共用一个 `EbpfSink` 实现，光看调用点分不出哪个 kind 走哪条 —— 真正的依据是 `emits_edges()`。

修法与防回归见 PR #116（新增 `ebpf_kinds_follow_emits_edges_contract`，
把映射表锚在代码契约上）。

### 环境现状

集群仍运行 `v1.3.7`；PR #116 合入后需重发 `server/v1.3.8` 才算闭环。

## 10. 修复后的真集群复验（2026-10-01，cloud3，`server/v1.3.8-a7f2be8`）

PR #116 合入后重发并部署 `v1.3.8`，**四台 Agent 全部全绿**：

| Agent | items | `agents status` 结论 | 退出码 |
| --- | --- | --- | --- |
| cloud2-agent | 1 | `summary: ok` | 0 |
| debian12-agent | 1 | `summary: ok` | 0 |
| ser539375215934 | 4 | `summary: ok` | 0 |
| testbkee | 1 | `summary: ok` | 0 |

`ser539375215934` 修复前后对比（同一台机、同一批采集项）：

```
修复前 (v1.3.7)                          修复后 (v1.3.8)
item-…-0  ebpf_edges  25  reporting       item-…-0  ebpf_edges  33  reporting
item-…-1  ebpf_edges   0  not_reporting   item-…-1  metrics     11  reporting  ← 修好
item-…-2  metrics    621  reporting       item-…-2  metrics    630  reporting
item-…-3  metrics    497  reporting       item-…-3  metrics    524  reporting
summary: not reporting …                  summary: ok   (exit 0)
```

`agents doctor ser539375215934` 同样 `summary: ok`（退出码 0）。

### 顺带发现（已记 `ebpf-observability/todo.md` 的 TODO-13，不在本 feature 修）

`doctor` 的「eBPF 能力」段读到 `reported: 0`，但同环境 eBPF 数据在正常上报
（`ebpf_process_exec_total` 300 条序列、`ebpf_edge_connections_total` 134 条序列，
而 `agent_ebpf_capability` 为 **0 条**）。根因是能力指标**只在采集器启动时上报一次**，
而 dataserver 是按 7 天窗口从时序库查 —— 一次性写入的样本会因保留期清理而查不到。

这是**本 feature 之外**的既有问题（不改 Agent / dataserver），
但正是新增的 `doctor` 命令把它暴露出来的 —— 已单开 TODO 并给出两个可选修法。

### 部署过程的一个观察（运维注意）

`kubectl set image` 滚动期间 dataserver 短暂不可用，Agent 日志出现
`dataplane_addr unavailable: no online dataplane` 与
`drop oldest batch data_type=… records=…`（缓冲淘汰）。
恢复后 `status` 即回到全绿。**这意味着滚动重启 dataserver 会丢一批缓冲数据**，
属既有的背压/淘汰行为，与本次改动无关，但升级窗口建议避开数据密集期。

## 11. `doctor` 能力段盲区的收口（2026-10-01，v1.3.9）

§10 记录的 TODO-13（`agent_ebpf_capability` 只在采集器启动时上报一次 → `doctor` 的「eBPF 能力」段
读成 `reported: 0`）**已修复**：Agent 侧改为随每轮采集重报能力状态（含前置校验失败分支）。

对 `doctor` 的意义：该段此前**在本 feature 落地的当天就已经是坏的**（不是后来才坏的），
表现为「有 eBPF 数据但能力未知」，而不是「不可用」。修复后四台 Agent 的
`vmctl agents doctor` 该段恢复 `reported=N`，五个命令至此没有已知盲区。

修复落在 `ebpf-observability`（Agent 侧），本 feature 只做验收：
`GET /v1/ebpf/capability` 的 `reported` 恢复为环境中 eBPF 采集项数。

### 11.1 复验时抓到的两个真问题（2026-10-01，均已在 Agent 侧修复）

`doctor`/`status` 这两条命令的价值在这天兑现了两次 —— 它们把两个**在两端都看不见**的数据问题翻了出来：

1. **dataserver OOM 崩溃循环（生产中断）**：`dataserver` 容器 `limits.memory=1Gi`，
   而进程稳态 RSS 在 **0.7～1.05GB** 之间抖动（启动 15 秒就到 880MB）→ 平均几分钟 OOM 一次，
   `CrashLoopBackOff`；期间 `/v1/*` 全部不可用（Traefik 直接回 `no available server`），
   Agent 侧刷 `dataplane_addr unavailable: no online dataplane` + `drop oldest batch`。
   **处置**：`scale 0 → 确认主机无 dataserver 进程 → set resources --limits=memory=2Gi → scale 1`；
   起来后 RSS 峰值 ~1.05GB，稳定 `1/1 Ready`。
   **教训**：`dataserver` 的内存上限（1Gi）与它的真实占用太贴，**升级巡检要把 RSS 与 limit 的比值当指标看**。

2. **边记录几乎全丢（见 `ebpf-observability/todo.md` TODO-14）**：
   `status` 显示 `ebpf_edges` 长期 `stale`，一查是 `connections == 0` 的边被服务端整条拒
   （`accepted=1` / `invalid=6415`），而 Agent 把 `partial` 当成功、`IngestReply` 又不解析 `failures`
   → 数据静默消失。已修（v1.3.10）：Agent 不再产出无新建连接的边，且 `partial` 会打日志。

> 两条都不是本 feature 引入的，但都是本 feature 的命令**第一次真正跑起来**才暴露的 ——
> 这正是 §11 那句话的注脚：命令做完了，不等于链路是通的。

### 11.2 修复后的复验（v1.3.10，2026-10-01）

`abrance/vectorman#118` → `agent/v1.3.10`（`v1.3.10`，镜像 `v1.3.10-b30d698`），
k8s 节点用 `ctr pull`+`tag`+`set image`，三台 VM 用 `file_transfer`+`agent_upgrade`。

**边记录（TODO-14）的修复证据**（dataserver 自监控，75 秒窗口）：

| 计数器 | 修复后 t0 | t0 + 75s |
| --- | --- | --- |
| `ingest_records_total{data_type="ebpf_edges",result="accepted"}` | 2794 | **3037**（持续增长） |
| `ingest_records_total{data_type="ebpf_edges",result="invalid"}` | 16325 | **16325（停止增长）** |

- `GET /api/v1/query?query=ebpf_edge_connections_total` → **328 条序列**（修复前 0 条）
- `ebpf_edges` 流的 `last_seen` 从 480 秒+ 回到 **1～2 秒**
- 四台 Agent 全部升到 `1.3.10`（台账 `version` 自动跟随）

**五条命令最终状态（`summary: ok`，退出码 0）**

```
== collect items ==
item-1790554921748537-0  ebpf_edges  2s   29   reporting   ← 修复前长期 stale
item-1790554922302785-1  metrics     7s   13   reporting
item-1790554922845489-2  metrics     3s   908  reporting
item-1790554923409501-3  metrics     6s   511  reporting
== ebpf capability ==
reported: 6      unavailable: none
summary: ok
```

> `doctor` 至此**没有已知盲区**：v1.3.9 修掉能力段（`reported: 0`），v1.3.10 修掉边记录（`stale`）。
> 这两条都是它自己跑起来才暴露的 —— 命令的真正价值在这里，不在「能打印表格」。

## 12. `data` 子命名空间（2026-10-01，复用 `dpc` 实现）

规格依据：`requirements.md` Requirement 9 + 本次修订记录；设计见 `design.md` §「`data` 子命名空间」。
前置：上一版修订记录明确「`data` 要做需先统一 `--sql-url` / `--data-url` 口径」——本节的 1.3 就是它。

### 12.1 规格与口径（先改规格再动代码）

- [x] 12.1.1 `requirements.md`：修订记录新增 2026-10-01 条目；R8 放开 `data`（保留「不改 `dpc` 行为与输出」）；新增 Requirement 9。
- [x] 12.1.2 `design.md`：新增 `data` 子命名空间设计（拆分方案、地址口径表、为什么不走 `Transport`、输出/退出码、唯一写命令）；
      Pitfalls 补 4 条（地址别名、不要铺到顶层、不改 `dpc` 输出、不要重复包错误）。

### 12.2 `dpc` 拆 lib + bin（行为零变化）

- [x] 12.2.1 `git mv bins/dpc/src/main.rs bins/dpc/src/lib.rs`；`Cli` / `Command` / `TsCommand` / `DpcError` / `run` 改 `pub`。
- [x] 12.2.2 新增 `pub struct Endpoints { pub sql_url: String, pub prom_url: String }` 与
      `pub fn dispatch(endpoints: &Endpoints, cli: &Cli) -> ExitCode`（把原 `main` 的错误打印收进来）。
- [x] 12.2.3 新 `bins/dpc/src/main.rs`：`Cli::parse()` + `dpc::dispatch`，5 行以内。
- [x] 12.2.4 验收：`dpc --help` / `dpc ts --help` / `dpc --version` 输出与改动前**逐字一致**；
      `dpc` 原有 5 个单测仍通过；`cargo build -p dpc` 产物仍是单二进制。

### 12.3 `vmctl data` 接线

- [x] 12.3.1 `bins/vmctl/Cargo.toml` 增加 `dpc = { path = "../dpc" }`（无新三方依赖）。
- [x] 12.3.2 `bins/vmctl/src/main.rs`：顶层 `Command::Data(DataArgs)`，其中 `#[command(subcommand)] command: dpc::Command`；
      全局 `--data-url` 加 `alias = "sql-url"`；新增全局 `--prom-url`（缺省 `http://127.0.0.1:9090`）。
- [x] 12.3.3 派发：`Data` 分支构造 `dpc::Endpoints { sql_url: data_url, prom_url }` 调 `dpc::dispatch`，
      **不重复包错误**。
- [x] 12.3.4 `--help` 验收：顶层多出 `data`（共 5 个）；`vmctl data --help` 列出 11 个子命令；
      `vmctl data ts --help` 列出 `stats` / `delete`。
- [x] 12.3.5 回归：`health` / `hosts` / `agents` / `jobs` 的输出与退出码不变（既有单测全绿）。

### 12.4 测试

- [x] 12.4.1 单测：`vmctl --help` 的顶层子命令集合 = `{health, hosts, agents, jobs, data}`（更新既有的「四个」断言）。
- [x] 12.4.2 单测：`--sql-url` 能作为 `--data-url` 的别名被接受（解析后 `data_url` 相等）。
- [x] 12.4.3 单测：`dpc::Command` 的 `logs` / `query` 参数在 `vmctl data` 下能正确解析（含可选参数省略）。
- [x] 12.4.4 质量门：`cargo fmt --all -- --check`、`cargo clippy --all-targets --all-features -- -D warnings`、
      `cargo test --all-features` 全绿。

### 12.5 文档与验收

- [x] 12.5.1 `README.md` 的 `vmctl` 章节补 `data` 子命名空间（11 个子命令、两个地址参数、`ts delete` 是唯一写命令）。
- [x] 12.5.2 真机验收（cloud3 数据面）：`vmctl data health|query|logs|edges|ebpf-capability|ts stats` 逐个执行，
      记录退出码与关键输出；含一条**刻意失败**（错误 `--data-url`）断言 `code=` 与退出码 1。
- [x] 12.5.3 把 12.5.2 结论写回本节下方；`.monkeycode/specs/README.md` 索引更新本 feature 的一句话。

- **检查点 G**：`dpc` 行为零变化且可独立使用；`vmctl data` 与 `dpc` 输出逐字节一致（同参数同环境比对）。

### 12.6 实施与验收结论（2026-10-01）

**实现要点**

- `bins/dpc`: `main.rs` → `lib.rs`（`Cli` / `Command` / `TsCommand` / `DpcError` / `run` 全 `pub`），
  新增 `Endpoints { sql_url, prom_url }` 与 `dpc::dispatch(endpoints, command)`；
  `main.rs` 变成 5 行。**`dpc` 的 `--help` / `--version` / 5 个单测全部照旧。**
- `bins/vmctl`: 新增顶层 `data`（`cmd: dpc::Command`），全局 `--data-url` 加 `alias = "sql-url"`、
  新增 `--prom-url`（`DEFAULT_PROM_URL = http://127.0.0.1:9090`）；派发直接 `return dpc::dispatch(...)`，
  不重复包错误（两套前缀会打架）。
- 零复制：`vmctl data` 与 `dpc` 共用同一份实现与同一个 `code=`/`url=` 错误口径。

**与 `dpc` 的逐字节比对（真数据面 `dataserver.xiaoyxq.top`）**

| 命令 | 结果 |
| --- | --- |
| `health` | ✅ 52 字节逐字节一致 |
| `ebpf-capability` | ✅ 395 字节一致 |
| `edges --limit 2` | ✅ 810 字节一致 |
| `logs --limit 2` | ✅ 873 字节一致 |
| `traces --limit 2` | ✅ 23 字节一致 |
| `query --expr cpu_usage` | ✅ 500 字节一致 |
| `ts stats` | ⚠️ 字段集合逐一相同；值逐字节比对**不适用** —— 该响应含 `memory_used_bytes` / `wal_size_bytes` / `sampled_at_ts` 等波动字段，`dpc` 自己连跑两次也不一样 |

**退出码**：成功 `0`；错误地址时两边都输出同一条
`url=… reason=… code=unavailable`（逐字节一致）且退出码 `1`。

**`--help` 层级**

- `vmctl --help` 顶层 = `health | hosts | agents | jobs | data`（既有单测从「恰好四个」改为固定集合断言）。
- `vmctl data --help` 的 11 个子命令与 `dpc` 完全一致 —— 由单测 `data_mirrors_all_dpc_subcommands`
  直接比对两个 clap 命令树（同名同序），防止「影子实现」走样。
- `vmctl data ts --help` 里 `delete` 可见（唯一会改数据的命令，`--help` 与 README 都已标注）。

**测试**：`cargo test -p vmctl --features test-support` 23 + 29 全绿（含新增的别名、参数绑定、
命令树一致性 4 条）；`cargo test -p dpc` 5 条照旧；`cargo test --workspace --all-features` 全绿；
clippy `-D warnings` 0 告警；fmt 干净。
