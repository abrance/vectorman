# Requirements Document

> **已归档**（2026-09-26 文档整理）：本 feature 已实现并合入 main，本文件压缩为需求索引——完整 EARS 条款见 git 历史（本文件重写前的最后一个版本），design.md 全文保留为实施细节档案。

## Introduction

`vmctl` 是随安装包分发的一次性 HTTP 客户端二进制，运维人员用命令行调用 gse-server 的节点只读接口与作业接口。v1 不对接 apiserver，不并入 `dpc`，`ctl.sh` 不管理该二进制。标准输出为服务端 JSON 响应正文。

## Glossary

- **vmctl**: 本特性交付的命令行客户端二进制。
- **Script File**: 用户通过 `--script-file` 指定的本地脚本文件，内容作为作业 `script` 字段提交。
- **gse-server HTTP**: gse-server 进程对外的 HTTP 服务，默认本机 `http://127.0.0.1:7101`，台账与作业 API 前缀为 `/api/gse`。
- **Base URL**: 用户通过 `--url` 指定的 gse-server HTTP 根地址，不含路径后缀。
- **Host**: 主机台账记录，对应 `GET /api/gse/hosts`。
- **Agent**: Agent 台账记录，对应 `GET /api/gse/agents`。
- **Job**: 一次脚本执行记录，对应 `/api/gse/jobs`。
- **Terminal Job Status**: 作业终态，取值为 `succeeded`、`failed`、`timeout`、`rejected`、`lost` 之一。
- **Wait**: 用户在 submit 或 rerun 上传入 `--wait` 后，按 1 秒间隔轮询作业详情，直到终态或等待时限 300 秒。

## Requirements

### Requirement 1

- AS 运维人员, I want 一个独立的 vmctl 二进制随安装包提供, so that 无需 curl 即可在目标机或运维机调用 gse-server HTTP。
- 验收：THE vmctl SHALL 以独立二进制形式存在于仓库 `bins/` 与安装包组件目录中；WHEN 打包安装包，THE 打包脚本 SHALL 将 vmctl 二进制放入安装包 `vmctl/bin/vmctl`。
### Requirement 2

- AS 运维人员, I want 用 `--url` 指定 gse-server HTTP 地址, so that 同一客户端可打本机 7101 或生产 HTTPS 入口。
- 验收：WHEN 用户省略 `--url`，THE vmctl SHALL 使用 Base URL `http://127.0.0.1:7101`；WHEN 用户传入 `--url <Base URL>`，THE vmctl SHALL 把后续请求发往该 Base URL。
### Requirement 3

- AS 运维人员, I want 只读查询 Host 与 Agent, so that 能确认节点是否登记以及 Agent 是否 online。
- 验收：WHEN 用户执行 hosts list，THE vmctl SHALL 请求 `GET /api/gse/hosts` 并把 JSON 响应正文写到标准输出；WHEN 用户执行 hosts get 并提供 host_id，THE vmctl SHALL 请求 `GET /api/gse/hosts/{host_id}` 并把 JSON 响应正文写到标准输出。
### Requirement 4

- AS 运维人员, I want 提交、查询、等待、重做作业, so that 能在命令行完成一次脚本执行闭环。
- 验收：WHEN 用户执行 jobs list，THE vmctl SHALL 请求 `GET /api/gse/jobs`，并在用户提供时附加查询参数 `agent_id`、`status`、`limit`，再把 JSON 响应正文写到标准输出；WHEN 用户执行 jobs get 并提供 job_id，THE vmctl SHALL 请求 `GET /api/gse/jobs/{job_id}` 并把 JSON 响应正文写到标准输出。
### Requirement 5

- AS 运维人员, I want 探测 gse-server HTTP 是否可达, so that 先排除地址与端口问题。
- 验收：WHEN 用户执行 health，THE vmctl SHALL 请求 `GET /health` 并把 JSON 响应正文写到标准输出。
### Requirement 6

- AS 运维人员, I want HTTP 失败时有明确退出码, so that 脚本能判断成功、业务失败还是等待超时。
- 验收：WHEN HTTP 状态码为 2xx 且未启用 Wait，THE vmctl SHALL 以退出码 0 结束；WHEN Wait 轮询得到的作业终态为 `succeeded`，THE vmctl SHALL 以退出码 0 结束并把最后一次作业 JSON 写到标准输出。
### Requirement 7

- AS 开发人员, I want vmctl 与现有 musl 发布包一致, so that Debian 12 与 Kylin V10 可直接运行。
- 验收：WHEN 执行 `packaging/build-package.sh` 且未使用 `--bin-dir`，THE 打包脚本 SHALL 把 vmctl 编进 `x86_64-unknown-linux-musl` 发布包；WHEN 校验 musl 静态链接，THE 打包脚本 SHALL 对 vmctl 执行与其他组件相同的 `ldd` 校验。
