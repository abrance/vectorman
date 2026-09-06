# 需求实施计划

- [x] 1. workspace 接入与 crate 骨架
   - 在 `Cargo.toml` 的 members 中加入 `bins/gse-server`、`bins/gse-agent`、`crates/gse-proto`
   - 在 `[workspace.dependencies]` 声明 `geminio = { git = "https://github.com/singchia/geminio-rs" }`、`serde`、`serde_json`、`tokio`
   - 建立三个 crate 的最小 `main`/`lib` 骨架，保证 `cargo check --workspace` 通过
   - 对应设计「技术选型 / Components」中的组织决策

- [x] 2. 实现 gse-proto 共享 DTO
   - [x] 2.1 定义 `AuthRequest`、`AuthReply`、`Heartbeat`、`Command`、`Receipt`、`GseError`，全部派生 `Serialize`/`Deserialize`
   - [x] 2.2 实现 `GseError` 与 `DataplaneError` 的互相转换
   - [x] 2.3 编写 DTO 序列化往返单元测试

- [x] 3. 实现 gse-server 连接与会话骨架
   - [x] 3.1 用 `EndListener::bind` 绑定监听地址，循环 `accept`，每个连接 spawn 独立任务
   - [x] 3.2 实现内存会话注册表 `SessionRegistry`：key 为 agent-id，值为 `Session`（End、状态、last_seen、connected_at），保证 same agent-id 至多一个活跃 `End`
   - [x] 3.3 实现会话状态机 Online / Checking / Offline / Closed 的转换逻辑
   - [x] 3.4 编写会话唯一性与状态转移单元测试

- [x] 4. 实现认证
   - [x] 4.1 从配置加载 `[agents]` token 表，实现 `auth` RPC handler：校验 agent-id 与 token
   - [x] 4.2 认证成功后创建会话并注册 End；失败回 `AuthReply{ok:false}` 并关闭该连接
   - [x] 4.3 编写认证成功/失败/未登记 agent-id 的测试

- [x] 5. 实现心跳与存活检测
   - [x] 5.1 实现 `heartbeat` RPC handler，更新会话 last_seen
   - [x] 5.2 实现 liveness monitor：超时窗口 90 秒内无消息则会话由 Online→Checking→Offline，并记录日志
   - [x] 5.3 编写心跳刷新与超时状态迁移的测试

- [x] 6. 实现信令下行与上行回执
   - [x] 6.1 实现 `send_command(agent_id, name, payload)`：通过会话 End 发起 `exec` RPC 并设置超时
   - [x] 6.2 目标离线或无会话时返回 `GseError{code: "unavailable"}`
   - [x] 6.3 将 `exec` RPC 返回值解析为 `Receipt` 返回给调用方
   - [x] 6.4 编写 ping/pong 与离线下发的测试

- [x] 7. 实现 gse-agent
   - [x] 7.1 用 `dial` 连接 Server，断线后按指数退避 1 秒到 60 秒自动重连
   - [x] 7.2 连接建立后先发起 `auth` RPC；认证失败则停止重连并以非零退出码结束
   - [x] 7.3 注册 `exec` handler：`ping` 回 `pong`，未知指令回 `ok:false` 并带指令名
   - [x] 7.4 心跳循环：每 30 秒发起 `heartbeat` RPC，重连成功后恢复
   - [x] 7.5 编写重连与认证失败退出的测试

- [x] 8. 配置与环境变量
   - [x] 8.1 实现 server 配置：监听地址、auth 开关、agents token 表、心跳区间与超时窗口；环境变量前缀 `GSE_`
   - [x] 8.2 实现 agent 配置：server 地址、agent-id、token、心跳区间；环境变量前缀 `GSE_`
   - [x] 8.3 配置缺失或 TOML 非法时退出码 1，stderr 含 `config_invalid` 与路径
   - [x] 8.4 编写配置解析与环境变量覆盖测试

- [x] 9. 检查点 - 本地验证链路
   - 本地编译并手工跑通 gse-server + gse-agent 的 auth 与 ping/pong，确保测试通过；如有疑问请询问用户

- [x] 10. 端到端集成验证
   - [x] 10.1 编写两个进程的集成测试/脚本：启动 server 与 agent，验证认证、心跳、ping/pong 全链路
   - [x] 10.2 在 CI 中接入 gse 集成测试步骤（rust-ci 的 `cargo test --all-features` 已覆盖 e2e）