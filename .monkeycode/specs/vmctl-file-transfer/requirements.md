# Requirements Document

> **已归档**（2026-09-26 文档整理）：本 feature 已实现并合入 main，本文件压缩为需求索引——完整 EARS 条款见 git 历史（本文件重写前的最后一个版本），design.md 全文保留为实施细节档案。

## Introduction

本 feature 在 `gse-cli` 与 `gse-job-file-transfer` 之上，扩展 `vmctl jobs submit`，使运维人员用同一条子命令提交脚本作业或文件传输作业。文件传输 v1 覆盖 Agent 互传，以及把运维机本地文件上传后下发到 Agent。Server 临时文件的列出、上传、下载、删除挂在 `jobs files` 下。

传输执行仍由 gse-server 经已认证会话中转；`vmctl` 只做 HTTP 客户端，不建立 geminio 连接。脚本作业的 `--script-file` 语义保持不变。命令行提交不以 `--from-file-id` 或 `--to-temp` 作为作业源/目标标志。

v1 范围：

- `jobs submit --kind file_transfer`：源 Agent 绝对路径 → 目标 Agent 绝对路径。
- `jobs submit --kind file_transfer --upload`：本地文件先上传到 Server 临时目录，再作为源端下发到目标 Agent。
- `jobs files list|upload|download|delete` 管理 Server 临时文件。
- 既有 `jobs get` / `jobs list` / `jobs rerun` / `--wait` 用于文件作业。

非目标：目录递归、P2P、多目标广播、传输进度条、改动 gse-server 传输协议、命令行提交 Agent→临时目录作业。

## Glossary

- **vmctl**：随安装包分发的一次性 HTTP 客户端，沿用 gse-cli。
- **Base URL**：`--url` 指定的 gse-server HTTP 根地址，缺省 `http://127.0.0.1:7101`。
- **作业种类（Job Kind）**：`jobs submit` 的 `--kind`，取值为 `script` 或 `file_transfer`；省略时为 `script`。
- **文件传输作业（File Transfer Job）**：`kind` 为 `file_transfer` 的作业，沿用 gse-job-file-transfer。
- **源端（Source）**：文件读取位置；本 feature 的提交入口为源 Agent 绝对路径，或本地上传后得到的 Server 临时文件。
- **目标端（Destination）**：本 feature 的提交入口为目标 Agent 绝对路径。
- **本地上传（Local Upload）**：`vmctl` 读取运维机本地文件，以 multipart 字段名 `file` 上传到 gse-server。
- **Server 临时文件（Server Temp File）**：Temp Store 中以 `file_id` 标识的对象。
- **Wait**：submit / rerun 上传入 `--wait` 后，每 1 秒请求作业详情，直到 Terminal Job Status 或累计 300 秒，沿用 gse-cli。
- **Terminal Job Status**：`succeeded`、`failed`、`timeout`、`rejected`、`lost`。
- 其余术语沿用 gse-job-execution、gse-cli 与 gse-job-file-transfer。

## Requirements

### Requirement 1: 按 kind 分流提交

- AS 运维人员, I want 在 jobs submit 上用 --kind 选择脚本或文件传输, so that 同一条子命令覆盖两类作业。
- 验收：WHEN 用户执行 `jobs submit` 且省略 `--kind`，THE vmctl SHALL 按脚本作业提交，并要求 `--agent-id` 与 `--script-file`；WHEN 用户执行 `jobs submit --kind script`，THE vmctl SHALL 按脚本作业提交，并要求 `--agent-id` 与 `--script-file`。
### Requirement 2: Agent 互传

- AS 运维人员, I want 指定源 Agent 路径与目标 Agent 路径并提交, so that 无需打开作业平台即可在两台在线 Agent 之间搬运单个文件。
- 验收：WHEN 用户执行 `jobs submit --kind file_transfer` 并同时提供 `--from-agent`、`--from-path`、`--to-agent`、`--to-path`，THE vmctl SHALL 请求 `POST /api/gse/jobs`，JSON 体 `kind` 为 `file_transfer`，`source` 为 `{type: agent, agent_id, path}`，`destination` 为 `{type: agent, agent_id, path}`；WHEN 用户在该提交上传入 `--timeout-secs`，THE vmctl SHALL 把该值写入请求体 `timeout_secs`。
### Requirement 3: 本地上传后下发到 Agent

- AS 运维人员, I want 把运维机上的文件上传到 Server 再下发到 Agent, so that 无需先登录目标机。
- 验收：WHEN 用户执行 `jobs submit --kind file_transfer --upload <local-path> --to-agent <id> --to-path <path>`，THE vmctl SHALL 先请求 `POST /api/gse/job-files`（multipart 字段名 `file`，文件名为本地路径的 basename），再使用返回的 `file_id` 作为 `source.type=server_temp` 提交文件传输作业，`destination` 为目标 Agent 路径；IF `--upload` 指向的本地路径无法读取，THE vmctl SHALL 把错误信息写到标准错误并以退出码 1 结束。
### Requirement 4: Wait 与退出码

- AS 运维人员, I want 文件作业与脚本作业使用同一套等待与退出码, so that 现有脚本判断逻辑可复用。
- 验收：WHEN 用户在文件传输的 `jobs submit` 上传入 `--wait`，THE vmctl SHALL 使用返回的 `job_id` 按 Wait 规则轮询 `GET /api/gse/jobs/{job_id}`；WHEN Wait 得到的终态为 `succeeded`，THE vmctl SHALL 以退出码 0 结束并把最后一次作业 JSON 写到标准输出。
### Requirement 5: 查询与重做文件作业

- AS 运维人员, I want 用既有 jobs 命令查看和重做文件作业, so that 传输历史与脚本作业在同一套命令下管理。
- 验收：WHEN 用户执行 `jobs get` 或 `jobs list`，THE vmctl SHALL 继续把服务端 JSON 正文写到标准输出，正文中的 `kind`、`source`、`destination`、`file_name`、`file_bytes`、`file_sha256`、`file_id` 保持原样；WHEN 用户执行 `jobs rerun` 并提供 `--agent-id`，THE vmctl SHALL 把该值作为重做请求体的 `agent_id` 发出。
### Requirement 6: jobs files 管理临时文件

- AS 运维人员, I want 在 jobs files 下列出、上传、下载、删除 Server 临时文件, so that 可以管理暂存文件。
- 验收：WHEN 用户执行 `jobs files list`，THE vmctl SHALL 请求 `GET /api/gse/job-files` 并把 JSON 响应正文写到标准输出；WHEN 用户执行 `jobs files upload --file <local-path>`，THE vmctl SHALL 请求 `POST /api/gse/job-files`（multipart 字段名 `file`）并把 JSON 响应正文写到标准输出。
### Requirement 7: 脚本作业入口保持

- AS 运维人员, I want 继续用 jobs submit --script-file 提交脚本, so that 现有自动化脚本无需修改。
- 验收：WHEN 用户执行 `jobs submit` 并提供 `--agent-id` 与 `--script-file` 且省略 `--kind`，THE vmctl SHALL 继续按 gse-cli Requirement 4 提交脚本作业；WHEN 用户按脚本作业提交时，THE vmctl SHALL 把 `--script-file` 作为必填参数。
### Requirement 8: 连接与鉴权沿用

- AS 运维人员, I want 文件命令使用同一 --url 与无鉴权头约定, so that 与现有 vmctl 调用方式一致。
- 验收：WHEN 用户省略 `--url`，THE vmctl SHALL 把文件相关请求发往 `http://127.0.0.1:7101`；THE vmctl SHALL 在文件相关请求中省略鉴权头。
