# Design Document

## 排查路径（先记录，因为结论依赖它）

### 现象

给 testbkee 升级 `gse-agent` 1.1.0 时，作业下发持续失败：

```json
{"status":"lost","error":"agent offline"}
```

但同一时刻：

```bash
$ curl -sS https://vectorman.xiaoyxq.top/api/gse/agents
[{"agent_id":"testbkee","status":"online","last_heartbeat_at":"1790403042605801"}, ...]
```

**矛盾点**：台账说 `online`、心跳持续推进；作业说 `offline`。

### 定位过程

| 步骤 | 观察 | 推论 |
| --- | --- | --- |
| 1 | `err = agent offline` 来自 `ledger.mark_lost_by_agent`（`ledger.rs:1090`）| 看谁调用它 |
| 2 | 调用方有三处：`dispatch_job` 的早退分支 + `run_liveness` 的 `Closed/Offline` 分支 | 作业路径的早退是 `registry.get()` 未拿到 Online 会话 |
| 3 | `dispatch_job` 用 `SessionRegistry`（内存会话），而台账 `status` 用**心跳时间戳** | 两者是不同数据源 → 解释了矛盾 |
| 4 | `handle_heartbeat` 调 `registry.touch()`，`touch` **只更新时间戳、不建会话** | 心跳能让台账显示 online，却不保证会话存在 |
| 5 | server 日志：`job_exec ... rpc failed: multiplexer closed` | 会话**存在**但底层连接已死（排除「无会话」） |
| 6 | `ss -tnp` 看 Agent 侧：`ESTAB` 但 `Send-Q = 122` 字节积压 | 连接**半死**：TCP 状态还在，数据发不出去 |
| 7 | `tcp_keepalive_time = 600` | 半死连接要 10 分钟才可能被 OS 发现 |

### 根本原因

`server.rs:170`：

```rust
tokio::spawn(handle_conn(end, registry, cfg, ledger));
```

`handle_conn` 的职责只是**注册 RPC handler**，注册完就返回。**没有任何代码等待连接结束、也没有清理会话**。因此：

1. 连接断开 → 会话**永久留在** `SessionRegistry`
2. `run_liveness` 只按 `last_seen` 推进状态（`Online → Checking → Offline`），而心跳会**不断** `touch()` 刷新 `last_seen`
3. **状态永远停在 `Online`** —— 因为心跳确实在到达（走的是新建 stream，能成功）
4. `dispatch_job` 拿到这条 `Online` 会话，`call()` 在死连接上失败 → `mark_lost_by_agent` → 作业 `lost`

**为什么重启 server 能"修好"**：重启清空内存注册表，Agent 重连时 `handle_auth` 重建会话 —— 但连接一旦再死，同样的问题立即复现。

### 为什么心跳能通而作业不能

geminio 的 multiplexer 支持多条逻辑流。心跳是**每次新建 stream**，而作业是对**已有会话对象**的 `call()`。连接半死时新建 stream 可能仍成功（或静默排队），而已有的调用无法完成。这也解释了 `Send-Q` 积压 —— 心跳数据在缓冲区里排队。

## 架构决策

### 决策 1：连接身份直接用 geminio 的 `client_id`（**0.1 已确认**）

**问题**：若只按 `agent_id` 移除，会有竞态 ——

```
时刻 T1: Agent 断线，server 开始清理
时刻 T2: Agent 重连，handle_auth 插入新会话
时刻 T3: T1 的清理逻辑执行 registry.remove(agent_id)
         → 把 T2 刚建立的新会话摘掉了
```

**方案**：用 geminio 自带的 **`End::client_id() -> u64`**，无需自造。

`geminio-rs` 的源码注释（`crates/app/src/end_tcp.rs:134`）：

> `/// Stable peer identity assigned during the conn-layer handshake.`

且 `EndListener` 用 `next_client_id: AtomicU64`（`first_client_id = 1`）**单调分配**，保证同一 listener 下不同连接拿到的 `client_id` 不同。**这正是注册表内可区分的要求。**

```rust
pub struct Session {
    pub agent_id: String,
    pub end: End,
    pub client_id: u64,        // ← geminio 握手分配，非自造
    ...
}

impl SessionRegistry {
    /// 仅当注册表中该 agent 的会话仍属于 client_id 时移除，返回是否移除。
    pub async fn remove_if_client(&self, agent_id: &str, client_id: u64) -> bool;
}
```

### 决策 2：连接结束通知用 `EndDrivers.hub_driver`（**0.1 已确认，无需退化**）

**结论：geminio 没有 `End::closed()` / `on_close()`，但提供了等价能力。**

`EndListener::accept()` 返回 `(End, EndDrivers)`：

```rust
pub struct EndDrivers {
    /// Handle for the `mux::DialogueHub` router.
    pub hub_driver: JoinHandle<Result<(), mux::Error>>,
}
```

源码注释（`end_tcp.rs:159`）明确：

> `/// `DialogueHub`'s router will follow once the conn driver exits.`

即 **连接结束（含超时、RST、正常关闭）时 `hub_driver` 会 resolve** —— 这就是连接关闭通知。

**当前代码把它丢弃了**（`server.rs:167`）：

```rust
Ok((end, _drivers)) => {          // ← _drivers 被丢弃
    tokio::spawn(handle_conn(end, registry, cfg, ledger));
}
```

**修复**：把 `_drivers` 交给 `handle_conn`，在它内部 await `hub_driver`，resolve 后执行清理。

**为什么这解释了 3.5 小时不恢复**：geminio 的连接层心跳**工作正常**（默认 `Heartbeat::Seconds5`，closewait = 5s × 6 = **30 秒**就判定对端死亡并拆连接），`hub_driver` 也确实 resolve 了 —— **只是没人监听它**。连接在 30 秒内就被拆了，而会话在注册表里留了 3.5 小时。

### 决策 3：心跳重建会话

`handle_heartbeat` 当前签名拿不到 `End`：

```rust
async fn handle_heartbeat(req: &Bytes, registry: &SessionRegistry, ledger: &Ledger)
```

需要改为接收连接身份（`End` + `conn_id`）：

```rust
async fn handle_heartbeat(
    req: &Bytes,
    end: &End,
    conn_id: u64,
    registry: &SessionRegistry,
    ledger: &Ledger,
)
```

行为：

1. `registry.get(agent_id)` 存在且 `state != Closed` → `touch`
2. 不存在 → `insert(Session::new(agent_id, end.clone(), conn_id, now))` + 日志
3. 存在但 `Closed` → **不复活**（`Closed` 是终止态）
4. **未认证不作为**：连接未认证时心跳不得建会话

### 决策 4：API 暴露会话状态

`GET /api/gse/agents` 的响应元素增加字段：

```json
{
  "agent_id": "testbkee",
  "status": "online",              // 心跳口径（既有）
  "session_state": "online",       // 会话口径（新增）
  "job_channel_available": true    // 两者结合的可判定结论（新增）
}
```

取值：
- `session_state`: `"online" | "checking" | "offline" | "closed" | "absent"`
- `job_channel_available`: 仅当 `session_state == "online"` 时为 `true`

**关键**：`status = online` 但 `session_state = absent/closed` 时，运维一眼能看出差异 —— 这正是本次事故的形态。

### 决策 5：错误措辞区分

`ledger.rs:1090` 的 `mark_lost_by_agent` 硬编码 `'agent offline'`。需要区分：

- **Agent 确实离线**（心跳窗口外）→ `'agent offline'`
- **会话不可用**（心跳在窗口内但会话不存在/非 Online）→ `'session unavailable'`

`dispatch_job` 的两个失败分支（早退 / RPC 失败 / 超时）也应给不同措辞，便于排查。

### 决策 6：不引入 TCP keepalive；依赖 geminio 连接层心跳（**0.2 已确认**）

**0.2 结论：`DialOptions` 不暴露底层 socket**（字段只有 `client_id` / `heartbeat` / `meta` / `timeout`），`End` 也不暴露。因此**无法**在 dial 后设置 `TCP_KEEPIDLE` 等 socket 选项。

**但这不重要，因为 geminio 自带更好的机制。** 源码 `crates/conn/src/heartbeat.rs`：

```rust
pub const CLOSEWAIT_MULTIPLIER: u32 = 6;
pub enum Tick { Idle, SendPing, Timeout }
// "any inbound packet — not just HeartbeatAck — counts as liveness"
```

`Heartbeater::from_negotiated(heartbeat, now)` 取 `closewait = interval × 6`，默认 `Heartbeat::Seconds5` ⇒ **30 秒无包即 `Tick::Timeout` → `Event::Error` → 拆连接**。

**对比**：

| 方案 | 检测时间 | 是否可改 |
| --- | --- | --- |
| 系统 TCP keepalive（现状） | **600s** | 需 root 改 sysctl，且 agent 侧连接半死时依然生效但很慢 |
| geminio 连接层心跳（已有） | **30s** | 通过 `DialOptions.heartbeat` 协商，可在 agent 侧调 |
| 自加 TCP keepalive socket 选项 | 目标 ~2 分钟 | ❌ `DialOptions` 不暴露 socket，**做不到** |

**决策**：**不做 TCP keepalive**（Requirement 4 的第 1 条据此修订）。已有的 30 秒连接层心跳比自加 keepalive 快得多，问题只在于**服务端没监听它的结果**（决策 2）。

Agent 侧真正要改的是：

1. **退避重置**（Requirement 4.2）—— 现状 `backoff` 在 `run()` 里跨 `connect_once` 持续增长，一次成功后若再断要等 60s
2. **心跳 RPC 失败立即重连**（已有，加测试钉住）

`DialOptions.heartbeat` 可作为**可调旋钮**保留（若现场发现 30 秒太敏感导致误判，可调到 `Seconds20` ⇒ 120 秒窗口）。

## 数据模型变更

### Session

```rust
pub struct Session {
    pub agent_id: String,
    pub end: End,
    pub conn_id: u64,          // 新增：连接身份，用于安全清理
    pub state: SessionState,
    pub last_seen_micros: i64,
    pub connected_at_micros: i64,
}
```

### 台账读取（不改存储）

`session_state` 与 `job_channel_available` **不落库** —— 它们是内存注册表的投影，在 HTTP 层实时计算。理由：会话是进程内状态，重启即失效，落库无意义。

## 接口变更

| 接口 | 变更 |
| --- | --- |
| `GET /api/gse/agents` | 响应元素新增 `session_state`、`job_channel_available` |
| 作业 `error` 字段 | 新增取值 `session unavailable` |
| `GET /api/gse/agents`（前端消费） | Agent 列表显示会话状态 |

## 风险

| 风险 | 缓解 |
| --- | --- |
| 连接结束**没有**通知机制 → 清理依赖超时 | 实现前先读 geminio API；若只能超时清理，记录并在文档中放宽验收标准 |
| keepalive 无法在 dial 后设置 | 退化为心跳 RPC 超时；两种方案都满足「尽早发现」的目标 |
| 心跳重建会话被滥用（伪造心跳建会话） | 仅**已认证连接**的心跳可建会话；未认证连接的心跳不作为 |
| 前端新增字段导致旧前端解析失败 | 新增字段向后兼容（旧前端忽略未知字段）；前端改动与本 feature 同一 PR |
| 清理逻辑竞态（旧连接摘掉新会话） | `conn_id` 校验（决策 1） |

## 与既有规格的关系

- `observability-hardening/tasklist.md` 记录了本次事故的**现象与待办**（1.18 的排查发现 + 1.19 的鉴权待办）。本 feature 是**修复**，两者应互相指向。
- `gse-server-agent/` 是 Agent/Server 通道的原始规格，本 feature 修正其中「会话生命周期」的实现缺失 —— 实现后需在该规格中补注指向。
