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

### 决策 1：会话清理用「连接身份」而非 `agent_id`

**问题**：若只按 `agent_id` 移除，会有竞态 ——

```
时刻 T1: Agent 断线，server 开始清理
时刻 T2: Agent 重连，handle_auth 插入新会话
时刻 T3: T1 的清理逻辑执行 registry.remove(agent_id)
         → 把 T2 刚建立的新会话摘掉了
```

**方案**：给 `Session` 加一个**连接唯一标识**（`conn_id`），清理时校验「当前会话是否仍是我这条连接建立的」。

```rust
impl SessionRegistry {
    /// 仅当注册表中该 agent 的会话仍属于 conn_id 时移除，返回是否移除。
    pub async fn remove_if_conn(&self, agent_id: &str, conn_id: u64) -> bool;
}
```

`conn_id` 用进程内单调递增的 `AtomicU64`（无需全局唯一，只需注册表内可区分）。

### 决策 2：连接结束的等待点

`handle_conn` 注册完 handler 后需要**等待连接结束**。geminio 的 `End` 提供的能力需确认：

- 若有 `end.closed()` / `end.on_close()` 之类的 future → 直接 await
- 若没有 → 用**心跳超时**兜底：连接结束后心跳自然停止，`run_liveness` 在 `heartbeat_timeout_secs` 后把会话推进到 `Offline`（**已有逻辑**），但需要保证存档路径也会 `remove`

⚠️ **这一条在实现前必须读 geminio 的 API 确认**（见 tasklist 第 1 步）。若没有关闭通知，则退化为「超时清理」，本 feature 的 Requirement 1 验收标准需相应放宽并记录原因。

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

### 决策 6：TCP keepalive

在 `gse-agent` 建立连接后设置 socket 选项。keepalive 是 OS 层能力：

- Linux: `TCP_KEEPIDLE`（起始等待）、`TCP_KEEPINTVL`（探测间隔）、`TCP_KEEPCNT`（探测次数）
- 目标：**60s 起始 + 15s 间隔 × 4 次** ⇒ 死连接约 2 分钟内被发现（vs 现在的 10 分钟）

⚠️ **实现约束**：`geminio::dial()` 返回的 `End` 是否暴露底层 socket？若不暴露，则需要在 dial 之前设置（可能不可行）。**实现前需确认**（tasklist 第 2 步）；若不可行，退化为「给心跳 RPC 加超时」——同样能达到「尽早发现」的目的。

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
