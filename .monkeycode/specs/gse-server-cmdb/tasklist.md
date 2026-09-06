# 需求实施计划

## 1. Ledger 台账模块

- [ ] 1.1 gse-server-core Cargo.toml 增加 `dataplane-sql`、`dataplane-core` workspace 依赖
- [ ] 1.2 实现 `ledger.rs`：`Ledger::new(db)` + `init()`（四表 DDL，幂等建表）
- [ ] 1.3 实现 hosts 表的 `upsert/get/list/remove`
- [ ] 1.4 实现 access_points 表的 `upsert/get/list/remove`
- [ ] 1.5 实现 agents 表与 agent_configs 表的 `upsert/get/list/remove`
- [ ] 1.6 实现运行态方法 `check_auth` / `mark_online` / `mark_heartbeat` / `mark_offline`
- [ ] 1.7 定义 `Host` / `AccessPoint` / `Agent` / `AgentConfig` 结构体（serde）
- [ ]* 1.8 单元测试：建表幂等、四表增删改查、upsert 覆盖、check_auth 三态

## 2. Server 集成

- [ ] 2.1 `ServerConfig` 新增 `db`（默认 `gse-server.db`，`GSE_SERVER_DB` 覆盖）；移除 `agents` 认证表字段（老配置段被自然忽略）
- [ ] 2.2 `ServerConfig` 新增 `http_enabled`（默认 true）、`http_listen`（默认 `127.0.0.1:7101`，`GSE_SERVER_HTTP_LISTEN` 覆盖）
- [ ] 2.3 `Server` 新增 `ledger: Arc<Ledger>`；`bind` 打开库、`init()` 建表、解析 `listen` 后 upsert 自登记接入点
- [ ] 2.4 `handle_auth` 认证数据源切换到 `ledger.check_auth`；通过后 `mark_online`
- [ ] 2.5 `handle_heartbeat` 在刷新会话 last_seen 外回写 `mark_heartbeat`（异步，不阻塞 RPC 应答）
- [ ] 2.6 `run_liveness` 会话推进至 Offline 时调用 `mark_offline`
- [ ] 2.7 导出 `Server.ledger`（`Arc<Ledger>`）供上层模块访问
- [ ]* 2.8 配置测试：`db`/`http_listen` 默认值与环境变量覆盖；含 `[agents]` 的旧配置可正常解析且不使用其做认证

## 3. HTTP 管理接口

- [ ] 3.1 gse-server-core 增加 `axum = "0.8"` 依赖（与 apiserver 一致）与 `http.rs` 模块
- [ ] 3.2 实现 `/health` 与 hosts 表的 GET/POST/GET{id}/DELETE{id} 路由
- [ ] 3.3 实现 access-points、agents、agent-configs 表的路由（agents 查询含运行状态）
- [ ] 3.4 登记接口必填字段校验，失败返 4xx + JSON 错误消息
- [ ] 3.5 `Server::run` 在 `http_enabled` 时以 axum 启动管理服务
- [ ]* 3.6 HTTP 路由测试（tower 请求级断言：200/201/404/400）

## 4. 测试与集成验证

- [ ]* 4.1 认证测试：预登记通过、未登记拒绝、token 错误拒绝、`auth_enabled=false` 直通
- [ ]* 4.2 状态回写测试：认证后 online + last_heartbeat；心跳推进 last_heartbeat；liveness 超时置 offline
- [ ]* 4.3 自登记测试：bind 后 access_points 含本 Server 一行，重复 bind 幂等
- [ ]* 4.4 e2e 集成：在临时 sqlite 库上重放认证/心跳/ping/pong，并断言 agents 表状态变化
- [ ] 4.5 `cargo fmt` / `cargo clippy -D warnings` / 全量 `cargo test --workspace --all-features` 通过

## 5. 收尾

- [ ] 5.1 同步 `docs/gse能力介绍.md` 与 `docs/vectorman概述.md` 中 GSE 章节（台账 + HTTP 管理端口）
- [ ] 5.2 提交并创建 MR 到 main