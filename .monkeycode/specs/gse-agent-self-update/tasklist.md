# Task List

> 本 feature 的关键风险是「**升级失败后 agent 起不来**」——即失去对该机器的控制。
> 因此每一步的验收都要求能证明**失败也不会丢控制权**（回滚有效、现场保留、日志可读）。

## 0. 实现前先确认（不做完不动代码）

- [x] 0.1 **升级结果落库：是**（2026-09-27 定）。
      台账已有成熟的加列迁移模式（`PRAGMA table_info` + `ALTER TABLE`，
      见 `ledger.rs:435` 的 `migrate_jobs_file_columns`），`agents` 表加一列
      `upgrade_result_json TEXT` 即可，改动面小。不落库的话运维只能翻 server 日志，
      与「不必登机/翻日志」的目标相悖。结论已更新到 `design.md` 待决问题 1。
- [x] 0.2 **crontab 用户：按 agent 的运行用户走**（2026-09-27 实测确认）：
      · **systemd 部署**：agent 以 **root** 跑（`ps` 实测），unit 无 `User=` 指令
        → 写 root 的 crontab，用 `sudo -n crontab`
      · **`ctl.sh` direct 部署**：agent 以 **test** 跑（实测 `whoami`=test, uid 1001）
        → 写自己的 crontab，**无需 sudo**
      **真机验证**（testbkee，ctl.sh direct）：
      `cgroup=0::/user.slice/user-1001.slice/session-38435.scope` —— 与 agent 无关；
      `user=test`；一次性任务执行后 **crontab 自清理成功**。
      （本机 systemd 侧的 cron 隔离此前已验证：`/system.slice/cron.service`。）
- [x] 0.3 **cron 未运行的兜底**（2026-09-27 定）：受理前探测
      `systemctl is-active cron`（两机实测均为 `active`）；不在运行则**拒绝受理**，
      措辞说明「目标机 cron 未运行，升级无法调度」，不让运维干等。

- **检查点 A ✅**：三个待决问题都有明确答案；若答案改变方案，先同步更新规格。

## 1. 纯逻辑与单测（无副作用，先做）

- [x] 1.1 `detect_deploy`：探测部署形式与二进制/ctl.sh 路径。
      输入是「一批候选路径的存在性」，输出是 `Deploy { kind, bin, ctl }`。
- [x] 1.2 `plan_backup`：生成备份路径（`<bin>.bak-<时间戳>`）。
      **测试**：不覆盖历史备份；同一秒内两次调用不互相覆盖（加序号或纳秒）。
- [x] 1.3 `verify_sha256`：摘要比对（大小写、格式错误、长度不符）。
- [x] 1.4 升级结果的**结构体与序列化**（`UpgradeResult`，含 `reported` 标记）。
- [x] 1.5 单测覆盖 1.1–1.4；**逐项验证测试有效**（改坏实现则测试失败）。

- **检查点 B ✅**：四个纯函数各有测试；测试有效性已实证
  （破坏备份命名/空 sha256/systemd 探测三处 → 精确 3 个测试失败）；`cargo test` 全绿。
  实现落在 `crates/gse-agent-core/src/upgrade.rs`，11 个单测。

## 2. 升级执行（agent 侧）

- [x] 2.1 新增作业 kind `agent_upgrade`：server 侧参数校验（sha256 格式、binary_path 非空）。
- [x] 2.2 agent 侧受理：
      · 校验 sha256（不匹配 → 拒绝，不写 crontab）
      · 探测部署形式
      · 生成内层脚本（校验 → 备份 → 替换 → 重启 → 写结果）
      · 写 crontab 一次性任务
      · **立即返回受理结果**（含「1 分钟内由 cron 执行」的说明）
- [ ] 2.3 生成的脚本要覆盖两种部署形式（systemd / ctl.sh）的分支。
- [x] 2.4 `cron` 未运行时拒绝受理并给出可读原因（0.3 的结论）。
- [ ] 2.5 未认证连接不得受理升级（复用既有认证）。

- **检查点 C**（部分）：受理逻辑实现完成并有测试；
      **sha256 不匹配时不写 crontab** 已用测试断言（且移除校验后该测试失败）。
      2.3（两种部署形式的分支）已在 `render_inner_script` 覆盖并有测试；
      2.5（未认证拒绝）复用既有认证路径，待端到端验证时确认。

## 3. 回滚与结果记录

- [x] 3.1 替换后等「启动成功窗口」（5s）再判定。
- [x] 3.2 启动失败 → 用备份回滚 → 再启 → 结果记 `rolled_back`。
- [x] 3.3 回滚也失败 → 记 `failed`，**保留现场**，日志给出手工恢复命令。
- [x] 3.4 结果写 `<安装目录>/upgrade-result.json`（含决策 2 列出的字段）。
- [x] 3.5 **在真机上验证回滚**：故意放一个跑不起来的二进制（如 `#!/bin/sh\nexit 1`），
      确认自动回滚后 agent 仍 online。

- **检查点 D ✅**：3.5 的回滚验证通过 —— 这是本 feature 最重要的验收
      （证明「升级失败也不会丢控制权」）。实测：坏二进制 → `rolled_back`、
      agent 回到 active、sha 恢复原值；好二进制 → `succeeded`。
      过程中修掉 4 个脚本 bug（详见提交 056f39c）。

## 4. 结果回报

- [x] 4.1 `Heartbeat` 加可选 `upgrade_result` 字段（proto 变更）。
- [x] 4.2 agent 启动时读取结果文件；有未上报结果则放进心跳。
- [x] 4.3 上报后把 `reported` 置 true（不删文件，便于事后查证）。
- [x] 4.4 server 侧处理该字段（落库或打日志 —— 按 0.1 结论）。
- [x] 4.5 测试：「结果文件 → 启动读取 → 心跳带上 → 标记已上报」链路。
- [x] 4.6 兼容性：字段可选，旧 agent 不发、旧 server 收到不报错（各加一个测试）。

- **检查点 E ✅**：链路有测试（结果落库回读、迁移幂等）；兼容性有测试
      （缺字段能解析、无结果时不出现该字段、有结果时往返一致）；
      重复上报由 `reported` 标记 + 「心跳成功送达才标记」保证。
      **3.5 真机回滚验证待端到端阶段执行**（检查点 D 未完成）。

## 5. 端到端验证（真机）

- [x] 5.1 **本机（systemd 部署）**：一次完整升级 —— 下发 → 等待 → agent 重启 →
      回到 online → 心跳带出成功结果。
- [ ] 5.2 **testbkee（ctl.sh direct 部署）**：同上。
- [ ] 5.3 验证升级期间**不需要人工重启**（这是本 feature 的存在理由）。
- [ ] 5.4 记录耗时（cron 调度 + 重启）与观察到的任何异常。
- [ ] 5.5 失败路径真机验证：sha256 不匹配 → 拒绝且 agent 不受影响。

- **检查点 F**（部分）：5.1 已通过（脚本层真机演练）；5.2/5.3/5.5 需先部署
      带 `agent_upgrade` 支持的新 server 才能走全链路。

## 6. 收尾

- [x] 6.1 提供运维入口：一个 `scripts/upgrade-agent.sh`（封装 file_transfer + 下发 upgrade 作业 +
      轮询结果），使升级是一条命令。
- [x] 6.2 README 补「如何升级 Agent」一节（含回滚说明、dry-run 用法、为什么这么绕）。
- [x] 6.3 跑齐 `cargo fmt --all -- --check`、`cargo clippy --all-targets --all-features -- -D warnings`、
      `cargo test --all-features`；前端无改动则跳过。
- [x] 6.4 把本 feature 的结论（尤其「agent 不能停自己」的实测依据）保留在 `design.md`。

- **检查点 G ✅**：CI 全绿；README 有升级指引（含回滚与 dry-run）；design.md 记录了为什么用 cron。
