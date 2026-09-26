# Task List

> 本 feature 修复 `gse-server` 的会话生命周期缺失。**每一步的验收都要求可复现**：
> 会话类 bug 的难点在于「看起来修好了」，因此凡是行为变更都要求有测试，且测试在移除修复后必须失败。

## 0. 前置：确认 geminio 的能力（不做完不动代码）

- [x] 0.1 **（已完成，2026-09-26）**读 geminio 源码（rev `ff4363f`），结论：

      · **`End` 没有 `closed()` / `on_close()`**，但有等价能力 ——
        `EndListener::accept()` 返回 `(End, EndDrivers)`，其中
        `EndDrivers.hub_driver: JoinHandle<Result<(), mux::Error>>`
        在**连接驱动退出时 resolve**（源码 `end_tcp.rs:159` 的注释明确）。
        **即：服务端能感知连接结束**，只是当前代码把它丢弃成 `_drivers`。
      · **连接身份用 `End::client_id() -> u64`**（握手时分配，源码注释
        「Stable peer identity assigned during the conn-layer handshake」；
        listener 用 `AtomicU64` 单调分配），**无需自造 `conn_id`**。
      · **意外收获**：geminio **自带连接层心跳**（`crates/conn/src/heartbeat.rs`），
        默认 `Heartbeat::Seconds5`、`closewait = interval × 6 = 30s`，
        超时后 `Tick::Timeout → Event::Error → 拆连接`。
        **这解释了为什么连接 30 秒内就被拆了**（日志里也确实如此），
        而会话留了 3.5 小时 —— 因为**没人监听 hub_driver**。

      结论已写回 `design.md` 决策 1、2、6。
- [x] 0.2 **（已完成，2026-09-26）**确认 `DialOptions` 只有
      `client_id / heartbeat / meta / timeout` —— **不暴露底层 socket**，
      `End` 也不暴露。因此**无法**在 dial 后设 TCP keepalive。
      **但不需要**：geminio 连接层心跳（30s）已比系统 TCP keepalive（600s）快得多。
      方案**不降级**，改为依赖既有机制 + 监听 `hub_driver`。
      `DialOptions.heartbeat` 保留为可调旋钮。结论已写回 `design.md` 决策 6，
      并据此**修订了 Requirement 4 的验收标准**。

- **检查点 A ✅**：两个 API 问题都有明确答案，且**都指向「不降级、不需退化方案」** ——
  连接结束通知有（`hub_driver`）、连接身份有（`client_id`）。
  keepalive 虽做不到，但既有机制更快，故 Requirement 4 改为依赖它。

## 1. 会话清理（Requirement 1）

- [ ] 1.1 `Session` 加 `client_id: u64` 字段（**直接取 `end.client_id()`**，不自造）
- [ ] 1.2 `SessionRegistry::remove_if_client(agent_id, client_id) -> bool`：仅当注册表中该 agent 的会话仍属于该 `client_id` 时移除
- [ ] 1.3 `server.rs:167` 的 `Ok((end, _drivers))` 改为把 `drivers` 交给 `handle_conn`；
      `handle_conn` 在注册完全部 handler 后 **await `drivers.hub_driver`**（连接结束的信号），
      结束后执行清理：`remove_if_client` + `close` + `ledger.mark_offline`
- [ ] 1.4 清理路径记录日志（`agent_id` + 结束原因），不得静默
- [ ] 1.5 测试：连接断开后注册表中无该会话、台账为 `offline`
- [ ] 1.6 测试：**重连接管** —— 旧连接结束不影响新连接建立的同名会话
      （构造：conn_a 建会话 → conn_b 建同名会话 → conn_a 结束 → 断言会话仍在且属于 conn_b）
- [ ] 1.7 验证测试有效性：**移除 1.2/1.3 的修复后，1.5 与 1.6 必须失败**
- [ ] 1.8 `handle_conn` 里 await `hub_driver` 的 task 不得阻塞 accept 循环
      （现状已是 `tokio::spawn`，改造后仍需 spawn）；确认没有把 await 放进 accept 分支

- **检查点 B**：`cargo test -p gse-server-core` 全绿；1.7 的「移除修复后失败」已实际验证过
  （记录移除哪一行、失败信息）。

## 2. 心跳重建会话（Requirement 2）

- [ ] 2.1 `handle_heartbeat` 改为接收 `end: &End` 与 `conn_id`
- [ ] 2.2 心跳语义：
      · 会话存在且非 `Closed` → `touch`
      · 会话不存在 → `insert(Session::new(...))` + 日志（区别于 touch）
      · 会话存在且 `Closed` → **不复活**
      · 连接未认证 → 不作为
- [ ] 2.3 测试：无会话时心跳能建会话
- [ ] 2.4 测试：`Closed` 会话不被心跳复活
- [ ] 2.5 测试：未认证连接的心跳不建会话
- [ ] 2.6 验证测试有效性：移除重建逻辑后 2.3 必须失败

- **检查点 C**：心跳的 4 种情形各有测试覆盖；`Closed` 的终止语义有明确断言。

## 3. 状态可见（Requirement 3）

- [ ] 3.1 `GET /api/gse/agents` 响应元素新增 `session_state`
      （`online | checking | offline | closed | absent`）与 `job_channel_available: bool`
- [ ] 3.2 `job_channel_available` 仅当 `session_state == "online"` 为 `true`
- [ ] 3.3 测试：台账 `status = online` 但会话 `absent` 时，响应能体现差异
      （**这是本次事故的形态，必须有测试钉住**）
- [ ] 3.4 `mark_lost_by_agent` 的错误措辞区分 `agent offline`（心跳超窗）与
      `session unavailable`（心跳在窗内但会话不可用）
- [ ] 3.5 `dispatch_job` 的三个失败分支（早退 / RPC 失败 / 超时）给不同措辞
- [ ] 3.6 前端 Agent 列表展示会话状态（不得只显示 `status`）
      —— 前端测试覆盖新字段渲染

- **检查点 D**：能通过 API 区分「心跳在线 + 会话可用」与「心跳在线 + 会话不可用」；
  错误措辞不再把连接问题说成 Agent 离线。

## 4. Agent 侧连接活性（Requirement 4）

- [ ] 4.1 **不引入 TCP keepalive**（`DialOptions` 不暴露 socket）。改为：
       · 确认 geminio 连接层心跳在 agent 侧同样生效（`DialOptions::default()` 的 `Seconds5`）
       · 在 agent 配置里保留 `DialOptions.heartbeat` 的可调性（若现场 30s 太敏感可调）
       · 注释说明「检测半死连接依赖 geminio 连接层心跳，不是 TCP keepalive」
- [ ] 4.2 重连退避在**连接成功建立后重置**（当前 `backoff` 在 `run()` 里跨 `connect_once` 持续增长）
- [ ] 4.3 心跳 RPC 失败时立即进入重连（现状已如此，加测试钉住）
- [ ] 4.4 测试：退避重置行为（可测的纯函数抽取）
- [ ] 4.5 若改动了 `collect` 相关的重连（`collect/mod.rs:400`）需一并检查语义一致性

- **检查点 E**：Agent 侧死连接发现时间从 10 分钟级降到 2 分钟级（或记录退化后的实际值）。

## 5. 端到端验证（真集群）

- [ ] 5.1 把修复部署到 cloud3，观察三个 Agent 的会话状态字段是否符合预期
- [ ] 5.2 **故障注入验证**：在 testbkee 上手工断开 agent 的连接（或 `kill -STOP` 模拟半死），
      断言作业下发**立即**失败并给出正确错误措辞，而不是静默 `lost`
- [ ] 5.3 **恢复验证**：连接恢复后作业下发自动可用，无需重启 `gse-server`
      （这是本次事故的核心症状，必须有此验证）
- [ ] 5.4 记录 testbkee 的重连频率与 keepalive 生效情况，写回 `design.md`

- **检查点 F**：5.2 与 5.3 都通过 —— 即「不需要重启 server」就能从连接故障中恢复。

## 6. 文档同步（Requirement 6）

- [ ] 6.1 `observability-hardening/tasklist.md` 指向本 feature（1.18 的排查发现 → 本 feature 修复）
- [ ] 6.2 `gse-server-agent/design.md` 或 `requirements.md` 补注：会话生命周期在本 feature 中修正
- [ ] 6.3 `README.md` 的「已知限制」检查是否需要更新
- [ ] 6.4 把本次事故的排查路径（心跳正常但作业失败 → `Send-Q` 判定半死）保留在 `design.md`

- **检查点 G**：后续维护者能只读文档就理解根因与结论，不必重跑排查。

## 7. 收口

- [ ] 7.1 跑齐 `cargo fmt --all -- --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-features`
- [ ] 7.2 前端 `npm run typecheck` + `npm test`
- [ ] 7.3 同步 `todo.md`（若引入了新的已知限制）

- **检查点 H**：CI 全绿；本 feature 的所有验收标准都有对应实现或明确记录的不做理由。
