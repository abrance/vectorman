# Requirements Document

## Introduction

本 feature 为 GSE Agent 增加**自更新能力**：运维通过已有的作业通道下发一次「升级」指令，
Agent 自行完成「下载 → 校验 → 替换二进制 → 重启 → 回报结果」，**不需要登目标机、也不需要人工重启**。

## 为什么需要它（问题背景）

升级 agent 必须替换正在运行的二进制并重启它。此前尝试用「普通作业脚本」做这件事，
在真实环境反复失败，根因是**停 agent 会连带杀掉正在执行升级的作业**：

| 尝试 | 结果 |
| --- | --- |
| 脚本里直接 `ctl.sh stop` | 作业是 agent 的子进程，被连带杀掉，升级半途而废 |
| `setsid nohup` 脱离 | 只脱**进程组**，**不脱 cgroup** —— systemd 停 unit 时按 cgroup 杀全部进程（实测踩到） |
| `systemd-run --unit`（独立 cgroup） | 机制**验证有效**（内层 cgroup 实测为 `/system.slice/<unit>`），但外层作业仍卡在 `running`（外层在 agent 的 cgroup 里，等内层时被杀） |

**结论：「agent 停自己」这个模式不可靠** —— 无论怎么脱离，执行升级的进程总有一条链路源自 agent。

因此把升级做成 Agent 的**内建能力**：由 Agent 主动把升级动作交给一个**与自身 cgroup 无关**的调度器执行。

## 已确认的技术选型（2026-09-27 问卷定稿）

- **独立进程机制**：**cron 一次性任务**（写 crontab → 到点执行 → 自清理）。
  实测 `cron.service` 的 cgroup 与 agent 的 unit 无关（`/system.slice/cron.service`），
  父进程是 cron 而非 agent，且一次性任务能自清理。目标机 cron 常驻运行。
- **结果回报**：**重启后补报** —— 升级结果落本机记录文件，新 Agent 启动后读取，
  在心跳中带上（不改作业结果通道，因为那时通道已断）。
- **推进节奏**：先出规格评审，再实现。

## Requirements

### Requirement 1: 通过作业通道下发升级

**User Story:** AS 运维人员, I want 用一次作业下发完成 agent 升级, so that 不必登每台机器。

#### Acceptance Criteria

1. THE `gse-agent` SHALL 接受一种**升级作业**（新的作业 kind，或带特定指令的 script 作业），
   指令中至少包含：目标二进制的位置（已由 `file_transfer` 落到本机）与期望的 sha256。
2. THE 升级作业 SHALL **立即受理并返回**（不等升级完成），使作业结果通道在 agent 重启前就收尾。
3. THE 升级 SHALL **只接受来自已认证连接**的指令。
4. THE sha256 **不匹配时 SHALL 拒绝执行**升级（防止半截传输或投毒）。

### Requirement 2: 升级动作与 Agent 进程隔离

**User Story:** AS 运维人员, I want 升级动作不受 agent 重启影响, so that 升级不会半途而废。

#### Acceptance Criteria

1. THE 实际执行升级的进程 SHALL **不属于 Agent 的 cgroup**，
   使 Agent 被停止/重启时不波及它。
2. THE Agent SHALL NOT 在自身进程内执行「停止自己」—— 那将导致升级动作被连带终止。
3. THE 升级动作 SHALL 在独立进程中完成：**校验 → 备份 → 替换 → 重启**。
4. WHEN 独立进程启动失败, THE Agent SHALL 在作业受理结果中给出可读原因。

### Requirement 3: 支持两种部署形式

**User Story:** AS 运维人员, I want 同一套升级逻辑覆盖不同部署方式, so that 不必按机器分别处理。

#### Acceptance Criteria

1. THE 升级 SHALL 支持 **systemd 部署**：通过 `systemctl stop/start` 或 `restart` 重启。
2. THE 升级 SHALL 支持 **`ctl.sh` direct 部署**（无 systemd）：通过 `ctl.sh <comp> stop/start`。
3. THE 升级 SHALL **自动识别**部署形式（存在 systemd unit 则用 systemd，否则用 `ctl.sh`），
   不需要调用方指定。
4. WHEN 两种方式都不可用, THE 升级 SHALL 失败并给出明确原因（不静默）。

### Requirement 4: 可靠性与可回滚

**User Story:** AS 运维人员, I want 升级失败时能退回, so that 我不会把 agent 弄死。

#### Acceptance Criteria

1. THE 升级 SHALL 在替换前**备份**原二进制（带时间戳，不覆盖历史备份）。
2. WHEN 新二进制**无法启动**（启动后进程不在或立即退出），THE 升级 SHALL **自动回滚**到备份并重启。
3. THE 升级 SHALL NOT 删除历史备份（由运维自行清理）。
4. THE 升级结果（成功/失败/回滚）SHALL 落**本机记录文件**，含时间戳、新旧 sha256、结果。

### Requirement 5: 结果回报

**User Story:** AS 运维人员, I want 知道升级结果, so that 我不必登机查看。

#### Acceptance Criteria

1. THE Agent 启动时 SHALL 读取上次升级的记录文件（若存在且未上报）。
2. THE 记录 SHALL 在**心跳**中带上（或在首次心跳时上报），使运维能从 server 侧看到结果。
3. THE 上报后的记录 SHALL 被标记（避免每次心跳重复上报）。
4. WHEN 升级失败或回滚, THE 汇报内容 SHALL 让人一眼看出（含失败原因）。

### Requirement 6: 可观测与不受版本影响

**User Story:** AS 运维人员, I want 升级过程可见且不破坏既有能力, so that 我能判断它是否在正常工作。

#### Acceptance Criteria

1. THE 升级的每一步 SHALL 记录日志（受理、校验、备份、替换、重启、结果），
   日志位置在目标机上可通过作业命令读取。
2. THE 本 feature SHALL NOT 改变既有作业类型的语义（script / file_transfer 保持原样）。
3. THE 本 feature SHALL NOT 要求 Agent 具备额外的系统权限（除既有部署所需的 sudo/ctl.sh）。

### Requirement 7: 验证

**User Story:** AS 开发人员, I want 上述行为有可运行的测试, so that 它能经受后续改动。

#### Acceptance Criteria

1. THE 部署形式识别、sha256 校验、备份命名、回滚判定 SHALL 有单元测试。
2. THE 「结果记录 → 启动读取 → 心跳上报」链路 SHALL 有测试。
3. THE 端到端 SHALL 在真机验证：一次完整升级（含重启）后 agent 回到 online，
   且心跳带出升级结果。
4. THE 失败路径 SHALL 验证：sha256 不匹配时拒绝执行；新二进制不可用时自动回滚。

## Out of Scope

- **批量升级多台**：本 feature 只做「单台可自更新」，批量编排由运维用脚本循环调用作业接口实现。
- **灰度与自动回滚策略**：不做「观察 N 分钟无异常才确认」的编排逻辑。
- **二进制分发本身**：复用既有 `file_transfer`（已验证可行），不新建分发通道。
- **Agent 容器化场景**：DaemonSet 形态的升级方式不同（换镜像），不在本 feature。
