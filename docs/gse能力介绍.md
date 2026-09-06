# GSE 能力介绍

## 1. 定位

GSE（Global Scheduling Engine，全局调度引擎）是 vectorman 的底座调度组件，负责集中管理 Agent 会话并提供信令的双向上下行通道。上层模块（Task / File / Proc / Data）可复用本通道完成远程命令、文件传输、进程托管与数据采集。

v0.1 实现了最小闭环：连接建立 → 身份认证 → 心跳保活 → 会话管理 → 信令双向上下行，并以 `ping/pong` 指令验证端到端链路。

## 2. 组件与代码结构

| 二进制 / crate | 职责 | 位置 |
| --- | --- | --- |
| `gse-server` | 调度端进程入口，加载配置并启动 `Server` | `bins/gse-server` |
| `gse-agent` | 执行端进程入口，加载配置并启动 `run` | `bins/gse-agent` |
| `gse-server-core` | server 核心库：配置、会话注册表、认证/心跳/信令与存活检测 | `crates/gse-server-core` |
| `gse-agent-core` | agent 核心库：外连、重连、认证、心跳、指令执行 | `crates/gse-agent-core` |
| `gse-proto` | 两端共享 DTO 与错误码映射 | `crates/gse-proto` |

传输层采用 geminio-rs（singchia/geminio 的 Rust 移植），单 TCP 连接承载双向 RPC。Agent 永远主动外连 Server，无公网入口的机器也能被管理。

## 3. 功能清单

- **Agent 主动外连与自动重连**：Agent 启动即 dial Server；断线后按指数退避 1s→60s 自动重连，重连成功后重新认证并恢复心跳。
- **身份认证**：agent-id + token；Server 维护 `[agents]` 静态 token 表。认证失败 Agent 停止重连并以非零退出码结束。
- **心跳保活与存活检测**：Agent 每 30s 一次 `heartbeat`（可配置）；Server 在超时窗口（默认 90s）内无任何消息则将会话按 Online→Checking→Offline 推进。
- **会话管理**：内存注册表，key 为 agent-id，保证同一 agent-id 至多一个活跃会话；支持列表、按 id 查询、在线状态查询。
- **信令双向上下行**：上层模块通过 `send_command` 下发指令（下行 `exec` RPC），Agent 产生回执经上行通道返回，`Send` 与应答通过指令 id 关联。
- **链路验证指令**：`ping` 回 `pong`；未知指令返回 `ok:false` 并携带指令名。
- **配置**：TOML 文件 + `GSE_` 前缀环境变量覆盖；配置缺失或非法时以退出码 1 结束，stderr 含 `config_invalid`。

## 4. 接口

### 4.1 RPC 接口（geminio 方法级）

两端通过 geminio 双向 RPC 通信，载荷为 JSON 序列化的 DTO：

| 方向 | 方法 | 请求 | 应答 |
| --- | --- | --- | --- |
| Agent → Server | `auth` | `AuthRequest` | `AuthReply` |
| Agent → Server | `heartbeat` | `Heartbeat` | 空 Bytes |
| Server → Agent | `exec` | `Command` | `Receipt` |

### 4.2 gse-server-core 进程内 API

同进程 API，v1 无网络协议，供上层模块直接调用（`crates/gse-server-core/src/server.rs`、`session.rs`）：

```rust
// Server：绑定、运行、查询与下发
Server::bind(cfg: ServerConfig) -> Result<(Arc<Server>, SocketAddr), Error>
Server::run() -> Result<(), Error>
Server::sessions() -> Vec<Session>                       // 会话快照
Server::send_command(agent_id: &str, name: &str,
                     payload: Bytes) -> Result<Receipt, GseError>

// SessionRegistry：会话注册表
SessionRegistry::new()
SessionRegistry::insert(session) -> Option<Session>      // 同名替换，返回旧会话
SessionRegistry::get(&str) -> Option<Session>
SessionRegistry::remove(&str) -> Option<Session>
SessionRegistry::list() -> Vec<Session>
SessionRegistry::touch(&str, now) -> bool                // 刷新 last_seen
SessionRegistry::advance_all(now, window)                // 推进状态机
SessionRegistry::set_state(&str, state) -> bool          // 强制同步状态

// Session / SessionState
Session { agent_id, end, state, last_seen_micros, connected_at_micros }
SessionState::{Online, Checking, Offline, Closed}
```

`send_command` 行为：目标无会话或状态非 Online 时返回 `GseError{code:"unavailable"}`；RPC 超时（60s）同样返回 `unavailable`；序列化错误返回 `GseError{code:"rpc_error"}`。

### 4.3 gse-agent-core 编程接口

```rust
run(cfg: AgentConfig) -> Result<(), String>   // 认证失败 Err，调用方非零退出
load_config(path: &str) -> Result<AgentConfig, String>
AgentConfig { server_addr, agent_id, token, heartbeat_interval_secs }
```

`AgentError::AuthFailed` 表示认证被拒（停止重连）；`ConnError` 触发指数退避重连。

### 4.4 DTO（crates/gse-proto）

| 类型 | 字段 | 说明 |
| --- | --- | --- |
| `AuthRequest` | `agent_id`, `token` | 认证请求 |
| `AuthReply` | `ok`, `reason` | 认证应答，拒绝时带原因 |
| `Heartbeat` | `agent_id`, `ts_micros` | 周期心跳 |
| `Command` | `id`, `name`, `payload` | 下行指令，`payload` 为 Bytes |
| `Receipt` | `command_id`, `ok`, `message` | 上行回执，`command_id` 关联原指令 |
| `GseError` | `code`, `message` | 错误载荷，与 `DataplaneError` 互相转换 |

### 4.5 CLI 与配置接口

```bash
# 配置路径由环境变量指定，未设置时默认读取当前目录 gse-server.toml / gse-agent.toml
GSE_SERVER_CONFIG=/path/gse-server.toml ./gse-server
GSE_AGENT_CONFIG=/path/gse-agent.toml ./gse-agent
```

- 进程退出码：正常 0；配置缺失/TOML 非法 1（stderr 含 `config_invalid` 与路径）；bind 失败 1；Agent 认证被拒 1。

## 5. 配置项

### gse-server（crates/gse-server-core/src/config.rs）

| 配置项 | 默认值 | 环境变量 |
| --- | --- | --- |
| `listen` | `0.0.0.0:7100` | `GSE_SERVER_LISTEN` |
| `auth_enabled` | `true` | `GSE_SERVER_AUTH` |
| `agents`（[agents] 表） | 空 | - |
| `heartbeat_interval_secs` | `30` | - |
| `heartbeat_timeout_secs` | `90` | `GSE_SERVER_HEARTBEAT_TIMEOUT` |

```toml
listen = "0.0.0.0:7100"
auth_enabled = true
heartbeat_interval_secs = 30
heartbeat_timeout_secs = 90

[agents]
web-01 = "token-a"
```

### gse-agent（crates/gse-agent-core/src/config.rs）

| 配置项 | 默认值 | 环境变量 |
| --- | --- | --- |
| `server_addr` | `127.0.0.1:7100` | `GSE_AGENT_SERVER` |
| `agent_id` | `agent-1` | `GSE_AGENT_ID` |
| `token` | 空 | `GSE_AGENT_TOKEN` |
| `heartbeat_interval_secs` | `30` | `GSE_AGENT_HEARTBEAT` |

## 6. 会话状态机与生命周期

```
Authenticating → Online（认证成功；同 agent-id 新认证替换旧会话）
Authenticating → Closed（认证失败，Server 不建会话，Agent 停止重连退出）
Online → Checking → Offline（超时窗口内无消息，由 liveness 每 5s 推进）
Online / Checking → Closed（连接关闭或会话被移除）
```

- 会话注册表不变量：同一 agent-id 至多一个活跃会话，重复认证直接替换。
- liveness：`heartbeat` 或任意下行响应更新 `last_seen`；超时窗口默认 90s（Agent 侧 3 次心跳窗口内必须恢复）。

## 7. 错误处理

| 场景 | 行为 |
| --- | --- |
| 认证失败 / agent-id 未登记 | Server 回 `AuthReply{ok:false}`；Agent 退出码 1，不重连 |
| Agent 连不上 Server | 指数退避重连 1–60s，持续失败期间不执行任务 |
| 指令目标离线或无会话 | `send_command` 返回 `GseError{code:"unavailable"}` |
| 未知指令（非 ping） | Agent 回 `Receipt{ok:false, message:"unknown command: <name>"}` |
| RPC 超时（60s） | `GseError{code:"unavailable"}` |
| 配置文件缺失或非法 | 退出码 1，stderr 含 `config_invalid` 与路径 |

## 8. 运行部署

```bash
cargo build --release -p gse-server -p gse-agent
GSE_SERVER_CONFIG=./gse-server.toml ./target/release/gse-server
GSE_AGENT_CONFIG=./gse-agent.toml ./target/release/gse-agent
```

配置示例见 `bins/gse-server/gse-server.toml.example` 与 `bins/gse-agent/gse-agent.toml.example`。tag `v*` 触发 CI 打包发布，安装包按组件统一目录布局（每组件一个子目录，内部 `bin/` + `conf/`）分发。

## 9. 测试覆盖

- 单元测试：DTO 序列化往返与错误码互转（gse-proto）；会话状态机、唯一性、touch/advance_all（gse-server-core）；配置解析与环境变量覆盖（server/agent）。
- 集成测试（`crates/gse-server-core/tests/e2e.rs`）：auth 成功/拒绝/未登记、心跳、ping/pong、未知指令、离线下发 `unavailable`、同 agent-id 唯一会话、断线自动重连恢复。

## 10. 规划中的扩展

- **GSE Task**：远程命令编排、下发与结果回收。
- **GSE File**：Server 与 Agent 间大文件分发与下载。
- **GSE Proc**：Agent 机器进程托管式生命周期管理。
- **GSE Data**：采集数据全链路传输、路由分发与管道管理。
- **Proxy / p-agent**：网络隔离场景的非直连区域中转与执行端。