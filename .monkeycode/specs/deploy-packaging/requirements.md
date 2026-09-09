# Requirements Document

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

**User Story:** AS 开发者或 CI, I want 执行一个脚本即可产出安装包, so that 本地与 CI 的产物一致、打包逻辑只有一份。

#### Acceptance Criteria

1. WHEN 在仓库根执行 `packaging/build-package.sh --version <版本>`，THE 打包脚本 SHALL 产出 `vectorman-<版本>-linux-x86_64.tar.gz`，内部布局符合 Requirement 2。
2. WHEN `--version` 缺省，THE 打包脚本 SHALL 以 `git describe --tags --always` 推导版本号。
3. IF Rust 构建或前端构建任一步骤失败，THE 打包脚本 SHALL 以非零退出码终止并在 stderr 输出失败步骤名。
4. WHEN 打包完成，THE 打包脚本 SHALL 在 stdout 打印产物绝对路径与文件大小。
5. THE 打包脚本 SHALL 通过 Node 工具链构建 `@vectorman/node` 的 dist 作为 web 目录来源。

### Requirement 2: 安装包布局与内容

**User Story:** AS 运维人员, I want 安装包内目录结构可预期, so that 解压后每个组件可以直接按文档启动。

#### Acceptance Criteria

1. WHEN 打包完成，THE 安装包 SHALL 在各组件目录含 `bin/` 与对应的二进制文件（apiserver、dpc、gse-server、gse-agent），且二进制具备可执行权限。
2. WHEN 打包完成，THE 安装包 SHALL 含 `gse-server/conf/gse-server.toml.example`、`gse-agent/conf/gse-agent.toml.example`、`apiserver/conf/config.toml.example` 与根目录 README.md。
3. WHEN 打包完成，THE 安装包 SHALL 含 `gse-server/web/index.html` 与 `gse-server/web/assets/`。
4. WHEN 打包完成，`gse-server/conf/gse-server.toml.example` SHALL 含 `http_web_dir = "web"` 的注释示例，与包内目录布局一致。
5. WHEN 打包完成，THE 安装包 SHALL 含 `deploy/` 目录：install.sh、ctl.sh 与 systemd unit 模板。
6. IF web 构建产物缺少 index.html，THE 打包脚本 SHALL 终止打包。

### Requirement 3: 目标机安装脚本

**User Story:** AS 运维人员, I want 在目标机一条命令完成安装, so that 组件立即进入可启动状态。

#### Acceptance Criteria

1. WHEN 执行 `install.sh <组件|all> --dest <安装根>`，THE 安装脚本 SHALL 将指定组件（`all` 为全部四个组件）的 bin、conf、web 复制到安装根对应子目录。
2. WHEN `--dest` 缺省，THE 安装脚本 SHALL 使用默认安装根 `/opt/vectorman`。
3. WHEN 安装根内实例配置不存在，THE 安装脚本 SHALL 由 `.example` 生成同名实例配置（去除 `.example` 后缀）。
4. IF 安装根内实例配置已存在，THE 安装脚本 SHALL 保留现有配置并打印提示。
5. WHEN 执行 `install.sh <组件|all> --dest <安装根> --with-systemd`，THE 安装脚本 SHALL 将 unit 模板中的安装根占位符替换为实际路径后写入 `/etc/systemd/system/` 并执行 `systemctl daemon-reload`。
6. WHEN 安装完成，THE 安装脚本 SHALL 打印该组件的启动与状态查看命令。

### Requirement 4: 统一启停与状态管理

**User Story:** AS 运维人员, I want 用同一条命令格式管理各组件进程, so that 启停操作在组件间一致。

#### Acceptance Criteria

1. WHEN 执行 `ctl.sh <组件> start`，THE 管理脚本 SHALL 通过 `systemctl start` 启动对应 unit。
2. IF 目标机 systemd 不可用（无 systemctl 或 `/run/systemd/system` 不存在），THE 管理脚本 SHALL 以非零退出码终止并提示 systemd 为唯一进程管理方式。
3. WHEN 执行 `ctl.sh <组件> stop`，THE 管理脚本 SHALL 通过 `systemctl stop` 停止对应 unit。
4. WHEN 执行 `ctl.sh <组件> status`，THE 管理脚本 SHALL 通过 `systemctl status` 输出运行状态。
5. WHEN 执行 `ctl.sh <组件> restart`，THE 管理脚本 SHALL 依次执行 stop 与 start。
6. IF 组件为 dpc，THE 管理脚本 SHALL 拒绝 start/stop/restart/status 并提示 dpc 为一次性 CLI 工具。

### Requirement 5: systemd unit 模板

**User Story:** AS 运维人员, I want 每个守护进程自带 unit 模板, so that 无需手写 systemd 配置。

#### Acceptance Criteria

1. WHEN 打包完成，THE 安装包 SHALL 含 apiserver、gse-server、gse-agent 三个 unit 模板，ExecStart 指向组件 `bin/` 内二进制。
2. THE unit 模板 SHALL 设置 `Restart=on-failure` 与 `RestartSec=5`。
3. THE unit 模板 SHALL 通过 `Environment=` 注入 `GSE_*` 配置路径环境变量，指向安装根内实例配置。
4. WHEN 打包完成，THE 安装包 SHALL 只包含守护进程 unit 模板，dpc 无 unit。

### Requirement 6: CI 发布集成

**User Story:** AS 维护者, I want tag 推送自动产出并发布安装包, so that 发布物始终来自统一打包入口。

#### Acceptance Criteria

1. WHEN 推送 `v*` tag，THE release workflow SHALL 安装 Rust 与 Node 工具链后执行 `packaging/build-package.sh`。
2. WHEN 打包脚本成功，THE release workflow SHALL 将 tar.gz 上传到对应 GitHub Release（已存在则覆盖上传）。
3. IF 打包任一步骤失败，THE release workflow SHALL 以失败结束并保留完整日志。
