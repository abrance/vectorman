# Requirements Document

> **已归档**（2026-09-26 文档整理）：本 feature 已实现并合入 main，本文件压缩为需求索引——完整 EARS 条款见 git 历史（本文件重写前的最后一个版本），design.md 全文保留为实施细节档案。

## Introduction

为 vectorman 建立统一的部署脚本与打包执行方案：打包逻辑收敛为仓库内脚本（本地与 CI 复用同一入口），安装包纳入前端 dist 与部署脚本；目标机用 install.sh 完成安装，用统一的 ctl 脚本或 systemd unit 管理守护进程的启停与状态。本期覆盖 linux-x86_64 单平台，产出可复现的 tar.gz 安装包。

## Glossary

- **组件（Component）**：仓库产出的可部署单元，本期为 apiserver、dpc、gse-server、gse-agent 四个二进制。
- **安装包（Package）**：`vectorman-<版本>-linux-x86_64.tar.gz`，内部为按组件分目录的目录树。
- **打包脚本（build-package.sh）**：`packaging/build-package.sh`，本地与 CI 共用的唯一打包入口。
- **部署脚本（Deploy Scripts）**：随安装包分发的 install.sh 与 ctl.sh。
- **安装根（Install Root）**：目标机上的部署根目录，组件各自落在其下同名子目录。
- **web 目录（web/）**：`gse-server/web/`，存放 `@vectorman/node` 的 vite 构建产物，由 gse-server `http_web_dir` 托管。
- **守护进程（Daemon）**：apiserver、gse-server、gse-agent 三个常驻组件；dpc 为一次性 CLI 工具。

## Requirements

### Requirement 1: 统一打包脚本（本地与 CI 复用）

- AS 开发者或 CI, I want 执行一个脚本即可产出安装包, so that 本地与 CI 的产物一致、打包逻辑只有一份。
- 验收：WHEN 在仓库根执行 `packaging/build-package.sh --version <版本>`，THE 打包脚本 SHALL 产出 `vectorman-<版本>-linux-x86_64.tar.gz`，内部布局符合 Requirement 2；WHEN `--version` 缺省，THE 打包脚本 SHALL 以 `git describe --tags --always` 推导版本号。
### Requirement 2: 安装包布局与内容

- AS 运维人员, I want 安装包内目录结构可预期, so that 解压后每个组件可以直接按文档启动。
- 验收：WHEN 打包完成，THE 安装包 SHALL 在各组件目录含 `bin/` 与对应的二进制文件（apiserver、dpc、gse-server、gse-agent），且二进制具备可执行权限；WHEN 打包完成，THE 安装包 SHALL 含 `gse-server/conf/gse-server.toml.example`、`gse-agent/conf/gse-agent.toml.example`、`apiserver/conf/config.toml.example` 与根目录 README.md。
### Requirement 3: 目标机安装脚本

- AS 运维人员, I want 在目标机一条命令完成安装, so that 组件立即进入可启动状态。
- 验收：WHEN 执行 `install.sh <组件|all> --dest <安装根>`，THE 安装脚本 SHALL 将指定组件（`all` 为全部四个组件）的 bin、conf、web 复制到安装根对应子目录；WHEN `--dest` 缺省，THE 安装脚本 SHALL 使用默认安装根 `/opt/vectorman`。
### Requirement 4: 统一启停与状态管理

- AS 运维人员, I want 用同一条命令格式管理各组件进程, so that 启停操作在组件间一致。
- 验收：WHEN 执行 `ctl.sh <组件> start`，THE 管理脚本 SHALL 通过 `systemctl start` 启动对应 unit；IF 目标机 systemd 不可用（无 systemctl 或 `/run/systemd/system` 不存在），THE 管理脚本 SHALL 以非零退出码终止并提示 systemd 为唯一进程管理方式。
### Requirement 5: systemd unit 模板

- AS 运维人员, I want 每个守护进程自带 unit 模板, so that 无需手写 systemd 配置。
- 验收：WHEN 打包完成，THE 安装包 SHALL 含 apiserver、gse-server、gse-agent 三个 unit 模板，ExecStart 指向组件 `bin/` 内二进制；THE unit 模板 SHALL 设置 `Restart=on-failure` 与 `RestartSec=5`。
### Requirement 6: CI 发布集成

- AS 维护者, I want tag 推送自动产出并发布安装包, so that 发布物始终来自统一打包入口。
- 验收：WHEN 推送 `v*` tag，THE release workflow SHALL 安装 Rust 与 Node 工具链后执行 `packaging/build-package.sh`；WHEN 打包脚本成功，THE release workflow SHALL 将 tar.gz 上传到对应 GitHub Release（已存在则覆盖上传）。
