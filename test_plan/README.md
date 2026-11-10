# 集成测试场景目录（test_plan）

本目录是**集成测试的唯一事实来源**：每个场景一份 md（前置数据 / 执行步骤 / 期望结果 /
清理方式 / 关联需求或事故），配套一个真实可执行的 Rust 测试。索引、md、测试函数三者
必须一一对应。

## 三条规矩（违反即不算落地）

1. **不进目录的不写**：没有 md 的测试不进索引，没有测试的 md 也不进索引。不留「待补」空号。
2. **禁止静默跳过**：不用 `#[ignore]`、不用 feature gate、不缺前置条件就 `return`。
   缺什么必须**失败**并点名缺的是哪个变量/哪个文件——静默跳过等于假绿。
3. **禁止恒真断言**：断言必须有可能挂。「返回 200 就算过」「数组非空就算过」不叫覆盖。

## 跑法

```bash
cp .env.integration.example .env.integration   # 填真实的 Redis / VictoriaMetrics 地址
scripts/test-integration.sh
```

前置条件不满足时脚本直接非零退出并打印修复提示，覆盖四种情况：没有 `.env.integration`、
必填变量为空、TCP 连不通、`TEST_ENV=prod`（拒绝指向生产）。

不需要外部中间件的场景可以单独跑，也可以进 PR 门禁：

```bash
cargo test -p dataserver --test boot_smoke
cargo test -p dataserver --test it_chain -- --nocapture
```

## 索引

| ID | 场景 | 依赖 | 命令或关联 | 负责人 |
|---|---|---|---|---|
| IT-DS-BOOT-01 | dataserver **真进程**启动链路：`--config` → 后端装配 → `GET /health` → `POST /v1/ingest` → `/v1/ts/stats` 与 `/api/v1/query` 回读；另含 3 种非法后端配置必须非 0 退出且 stderr 点名变量 | 无（redb + tsink 本地后端，随机端口 + 临时目录） | `cargo test -p dataserver --test boot_smoke`<br>场景：[IT-DS-BOOT-01.md](IT-DS-BOOT-01.md) | mike |
| IT-EXT-REDIS-VM-01 | 外置存储后端：Redis KV 语义与本地引擎一致、VictoriaMetrics 写入/查询/删除、tsink 与 VM 同点同结果、`POST /v1/ingest` 经真实 Redis+VM 落库后 `/api/v1/query` 回读；缺 `DP_E2E_*` 必须失败 | 真实 Redis + VictoriaMetrics（`scripts/test-integration.sh` 做连通性前置） | `cargo test -p dataserver --test external_storage_e2e`<br>场景：[IT-EXT-REDIS-VM-01.md](IT-EXT-REDIS-VM-01.md) | mike |
| IT-CHAIN-01 | 三**真进程**链路：gse-agent → gse-server → dataserver，agent 注册并心跳上线 → 下发采集项在 agent 侧生效 → metrics 入时序库 → Prom 即时查询回读带 `agent_id`/`item_id` 标签的点 | 无外部中间件（redb + tsink，端口与目录全部临时生成）；测试内自动 `cargo build -p gse-server -p gse-agent` | `cargo test -p dataserver --test it_chain -- --nocapture`<br>场景：[IT-CHAIN-01.md](IT-CHAIN-01.md) | pit |
