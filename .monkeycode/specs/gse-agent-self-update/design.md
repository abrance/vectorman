# Design Document

## 排查记录（先记结论，因为它决定方案）

升级 agent 必须「停旧进程 → 换二进制 → 起新进程」。尝试用**普通作业脚本**实现时反复失败：

| 尝试 | 现象 | 原因 |
| --- | --- | --- |
| `ctl.sh stop` 直接停 | 作业卡 `running`，升级未完成 | 作业进程是 agent 的子进程，agent 停机时被连带终止 |
| `setsid nohup` 脱离后停 | 同样卡住；日志停在 `systemctl stop` 那一行 | `setsid` 只脱离 **session/进程组**；systemd 停 unit 时按 **cgroup** 杀该 unit 内全部进程，脚本仍在 agent 的 cgroup 里 |
| `systemd-run --unit` + setsid | 机制本身有效，但外层作业仍卡 `running` | 内层确实跑在独立 cgroup（实测 `/system.slice/exp-683551.service`），但**外层**脚本仍在 agent 的 cgroup 里等内层，被一起杀掉 |

**实测证据（systemd-run 的 cgroup 隔离确实成立）**：

```
$ systemd-run --unit=exp-$$ --collect /bin/sh /tmp/exp-inner.sh
$ cat /tmp/exp-out.log
inner started at 2026年 09月 26日 18:17:59
my cgroup: 13:pids:/system.slice/exp-683551.service      ← 独立于 agent 的 unit
```

**结论**：问题不在「怎么脱离」，而在**「Agent 停自己」这个模式本身** ——
执行升级的进程无论怎么 fork，其起因总在 agent 进程树内，总有一条链路被牵连
（作业结果的回收、外层脚本的等待）。**必须让升级动作由 Agent 之外的调度器发起。**

## 架构决策

### 决策 1：独立进程用 cron 一次性任务（已实测）

**为什么不是 systemd-run**：它确实能给独立 cgroup，但**外层作业仍会被杀**
（外层得等内层，而外层属于 agent 的 cgroup）。要绕开就得让外层也脱离，
但那样作业结果就永远回不来（`running` 卡死），仍不干净。

**为什么是 cron**：目标机 `cron.service` 常驻且与 agent unit 无关。实测：

```
$ crontab 里写一次性任务 → 到点执行
$ cat /tmp/cron-out.log
cgroup: 13:pids:/system.slice/cron.service     ← 与 agent 的 unit 完全无关
ppid: 790649                                    ← 父进程是 cron，不是 agent
$ crontab -l | grep -c cron-inner
0                                               ← 一次性任务自清理成功
```

**流程**：

```
Agent 收到升级作业
  → 校验 sha256
  → 写 /tmp/gse-agent-upgrade-inner.sh（含目标路径、备份路径、部署形式）
  → 写 crontab 一次性任务（下一分钟执行）
  → **立即返回作业受理结果**（作业通道收尾）
  ↓（一分钟后，cron 以独立 cgroup 拉起）
独立进程：校验 → 备份 → 替换 → 重启 agent → 写结果文件
  ↓
新 Agent 启动 → 读结果文件 → 心跳带上 → 标记已上报
```

**cron 的调度粒度是分钟级** —— 对升级场景可接受（原本就含重启）。
代价是要多等最多 60 秒。

### 决策 2：结果靠「重启后补报」而非作业结果

升级过程中作业通道必然断开（agent 被重启），**结果不可能通过原作业返回**。

方案：升级结果落本机文件（`/tmp/gse-agent-upgrade-result.json` 或安装目录下），
新 Agent 启动时读取，在**心跳**中带上，然后标记已上报（避免重复）。

结果记录内容：

```json
{
  "started_at": "2026-09-27T18:12:03+08:00",
  "finished_at": "2026-09-27T18:12:41+08:00",
  "from_version": "1.1.0",
  "to_version": "1.2.0",
  "from_sha256": "56ca0df6...",
  "to_sha256": "abc123...",
  "outcome": "succeeded | rolled_back | failed",
  "detail": "回滚原因等",
  "reported": false
}
```

`reported` 由 Agent 上报后置 true（或直接删文件 —— 保留文件便于事后查证，故选前者）。

### 决策 3：部署形式自动识别

```
if systemctl list-unit-files vectorman-gse-agent.service 存在
   → systemd：sudo -n systemctl stop/start vectorman-gse-agent
else if 存在 deploy/ctl.sh
   → ctl.sh：<ctl> gse-agent stop/start
else
   → 失败，记录原因
```

**二进制与 ctl.sh 的定位**沿既有布局探测：

| 部署形式 | 二进制 | ctl.sh |
| --- | --- | --- |
| `ctl.sh` direct | `/home/test/dtx/vectorman/gse-agent/bin/gse-agent` | `…/deploy/ctl.sh` |
| systemd | `/opt/vectorman/gse-agent/bin/gse-agent` | `…/deploy/ctl.sh` |

探测顺序按「存在即用」，两者都覆盖（见需求 3）。

### 决策 4：回滚判定

替换后启动新二进制，**等一个「启动成功窗口」**（如 5 秒）后检查：

- 进程存在 → 成功
- 进程不存在 → 判定启动失败 → 用备份回滚 → 再启 → 结果记 `rolled_back`

判定「进程是否存在」按部署形式取：
- systemd：`systemctl is-active vectorman-gse-agent`
- ctl.sh：`ctl.sh gse-agent status`（它按 PID 文件判断）

⚠️ 若**回滚也失败**，结果记 `failed` 并保留现场（二进制与备份都留着），日志给出手工恢复命令。

### 决策 5：接口形态

新增作业 kind `agent_upgrade`（与 `script` / `file_transfer` 并列），字段：

```
agent_id      目标 agent
binary_path   已由 file_transfer 落到目标机的新二进制路径
sha256        期望校验值
```

**不用 script 作业承载**的理由：script 的语义是「执行一段脚本」，
而升级是**结构化的、需要 Agent 内建支持**的动作（校验、备份、回滚、重启、
结果补报都不是一段 shell 能可靠表达的）。独立 kind 也让 server 侧能区分对待。

## 组件与接口

### Agent 侧（`crates/gse-agent-core`）

| 组件 | 职责 |
| --- | --- |
| `upgrade::handle_request` | 校验请求 → 生成 crontab 任务 → 立即返回受理结果 |
| `upgrade::detect_deploy` | 探测部署形式与路径（纯函数，可测） |
| `upgrade::plan_backup` | 生成备份路径（带时间戳，不覆盖历史；纯函数，可测） |
| `upgrade::verify_sha256` | 校验（纯函数，可测） |
| `upgrade::write_result / read_result` | 结果文件读写（可测） |
| 生成的 shell 脚本 | 实际执行：校验 → 备份 → 替换 → 重启 → 写结果 |
| 心跳扩展 | 带上未上报的升级结果 |

### Server 侧（`crates/gse-server-core`）

| 组件 | 职责 |
| --- | --- |
| 作业 kind `agent_upgrade` 的校验与落库 | 参数校验（sha256 格式、路径非空） |
| 作业下发 | 复用既有 `dispatch_job` 通道 |

**server 侧改动很小**：只多一个 kind 的参数校验。执行与结果都在 agent 侧。

### 心跳扩展

`Heartbeat` 消息增加可选字段：

```rust
pub struct Heartbeat {
    pub agent_id: String,
    pub ts_micros: i64,
    /// 未上报的升级结果（有则带，无则 None）。
    pub upgrade_result: Option<UpgradeResult>,
}
```

server 收到后更新台账（或仅打日志 —— **取决于是否要落库**，见待决问题）。

## 待决问题（2026-09-27 已确认）

1. **升级结果落库：是。** 台账已有成熟的加列迁移模式
   （`PRAGMA table_info` + `ALTER TABLE`，见 `ledger.rs` 的 `migrate_jobs_file_columns`），
   `agents` 表加一列 `upgrade_result_json TEXT` 即可，改动面小。
   不落库的话运维只能翻 server 日志，与「不必登机」的目标相悖。

2. **crontab 用户：跟随 agent 的运行用户**（实测确认）：
   · systemd 部署 → agent 以 **root** 跑（unit 无 `User=`），写 root 的 crontab（`sudo -n crontab`）
   · `ctl.sh` direct → agent 以 **test** 跑，写自己的 crontab（**无需 sudo**）
   真机验证（testbkee）：`cgroup=0::/user.slice/user-1001.slice/session-38435.scope`、
   `user=test`、一次性任务执行后 crontab 自清理成功。

3. **cron 未运行时拒绝受理**：受理前探测 `systemctl is-active cron`；
   不在运行则立刻返回失败并说明「目标机 cron 未运行，升级无法调度」，
   不让运维干等一个永远不会到来的结果。

## 风险

| 风险 | 缓解 |
| --- | --- |
| cron 未运行 → 升级静默不执行 | 写 crontab 后探测 `systemctl is-active cron`，不在则拒绝并说明（决策 3 的待决问题 3） |
| 升级后 agent 起不来 → 失去控制 | 回滚（决策 4）+ 保留现场 + 日志给出手工恢复命令 |
| 升级请求被伪造 | 只接受已认证连接（需求 1.3）+ sha256 校验（需求 1.4） |
| crontab 与用户权限不匹配 | 按部署形式选 crontab 用户（待决问题 2） |
| 结果重复上报 | 结果文件带 `reported` 标记（决策 2） |
| 一分钟后才执行，运维以为失败 | 受理结果里明确写「升级将在 1 分钟内由 cron 执行」 |
