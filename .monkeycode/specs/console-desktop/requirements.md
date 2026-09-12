# Requirements Document

## Introduction

Console 服务是独立于 gse-server 的门户进程：后端提供 App 目录的持久化 CRUD，前端提供桌面式图标墙。运维在桌面上点击 App 后，浏览器新标签打开该 App 的目标 URL。现有 GSE 控制台（节点/作业页面）继续由 gse-server `http_web_dir` 托管，作为目录中的一个 App 被统一入口管理。

v1 范围：独立二进制与 HTTP 端口；桌面展示与新标签跳转；App 的新增、编辑、删除、列表；无鉴权；无桌面内 iframe 窗口。

## Glossary

- **Console 服务**: 本特性交付的独立进程，含 HTTP API 与桌面前端静态资源。
- **GSE 控制台**: 现有 `@vectorman/console` 构建产物，由 gse-server 托管，提供节点与作业页面。
- **App**: 目录中的一条记录，至少包含显示名称与目标 URL。
- **桌面**: Console 服务前端首页，以壁纸区域和图标网格展示 App。
- **目标 URL**: App 记录中的 http 或 https 地址，点击后在新浏览器标签打开。
- **App 目录**: Console 服务后端持久化保存的 App 列表，进程重启后仍可读取。
- **Console HTTP**: Console 服务对外 HTTP，默认 `http://127.0.0.1:7200`，API 前缀 `/api/console`。

## Requirements

### Requirement 1

**User Story:** AS 运维人员, I want 一个独立的 Console 服务进程, so that 各业务 UI 有统一入口，GSE 控制台继续单独部署。

#### Acceptance Criteria

1. THE Console 服务 SHALL 以独立二进制存在于仓库 `bins/` 与安装包组件目录 `console/`。
2. WHEN 用户执行 `install.sh console`，THE 安装脚本 SHALL 安装 Console 二进制、示例配置与桌面前端静态文件。
3. WHEN 用户对 console 执行 `ctl.sh` 的 start、stop、status 或 restart，THE ctl.sh SHALL 按与 gse-server 相同的进程管理语义处理 console。
4. THE GSE 控制台 SHALL 继续由 gse-server `http_web_dir` 托管。
5. WHEN 打包安装包且未使用 `--bin-dir`，THE 打包脚本 SHALL 将 console 编进 `x86_64-unknown-linux-musl` 发布包并对 console 执行与其他组件相同的 `ldd` 校验。

### Requirement 2

**User Story:** AS 运维人员, I want 打开 Console 服务看到桌面和图标, so that 用启动器方式找到各 App。

#### Acceptance Criteria

1. WHEN 浏览器请求 Console HTTP 根路径，THE Console 服务 SHALL 返回桌面前端。
2. THE 桌面 SHALL 在壁纸区域上以图标网格展示 App 目录中的全部 App。
3. WHEN App 目录为空，THE 桌面 SHALL 展示可新增 App 的入口。
4. WHEN App 目录加载失败，THE 桌面 SHALL 展示错误说明并提供重试。

### Requirement 3

**User Story:** AS 运维人员, I want 点击 App 在新标签打开目标 URL, so that 进入 gse-server 或其他业务页面。

#### Acceptance Criteria

1. WHEN 用户点击某个 App 图标，THE 桌面 SHALL 在新浏览器标签打开该 App 的目标 URL。
2. THE 桌面 SHALL 使用 `noopener` 打开目标 URL。
3. THE Console 服务 v1 SHALL 将打开方式限制为新浏览器标签。

### Requirement 4

**User Story:** AS 运维人员, I want 在桌面新增 App 的名称与 URL, so that 把分散的业务入口收进同一目录。

#### Acceptance Criteria

1. WHEN 用户提交名称为 1 至 64 字符且目标 URL 的 scheme 为 `http` 或 `https` 的新建表单，THE Console 服务 SHALL 把该 App 写入 App 目录并在桌面展示新图标。
2. IF 名称为空、名称超过 64 字符、目标 URL 无法解析、或 scheme 不是 `http` 且不是 `https`，THE Console 服务 SHALL 返回 HTTP 400 并说明校验失败原因。
3. IF App 目录中已存在相同名称的 App，THE Console 服务 SHALL 返回 HTTP 409。
4. THE App 目录容纳的 App 数量上限 SHALL 为 100。
5. IF 新建时 App 数量已达到 100，THE Console 服务 SHALL 返回 HTTP 400 并说明已达上限。

### Requirement 5

**User Story:** AS 运维人员, I want 编辑或删除已有 App, so that 目录能跟着环境地址变化。

#### Acceptance Criteria

1. WHEN 用户提交对已有 App 的名称或目标 URL 修改且通过与新建相同的校验，THE Console 服务 SHALL 更新该 App 并刷新桌面图标。
2. WHEN 用户确认删除某个 App，THE Console 服务 SHALL 从 App 目录移除该 App 并在桌面去掉对应图标。
3. IF 编辑或删除的 App 标识在目录中不存在，THE Console 服务 SHALL 返回 HTTP 404。

### Requirement 6

**User Story:** AS 运维人员, I want App 目录重启后仍在, so that 不必每次手工重填链接。

#### Acceptance Criteria

1. WHEN Console 服务进程退出后再次启动，THE Console 服务 SHALL 从持久化存储加载上次保存的 App 目录。
2. WHEN 新建、编辑或删除成功，THE Console 服务 SHALL 在响应返回前把 App 目录写入持久化存储。
3. THE Console HTTP API v1 SHALL 在无凭证的情况下处理 `/api/console` 请求。
4. WHEN 配置省略监听地址，THE Console 服务 SHALL 监听 `0.0.0.0:7200`。
5. WHEN 用户首次启动且持久化存储中没有 App 记录，THE App 目录 SHALL 为空；示例配置只提供注释示例，安装过程不写入 GSE 控制台 App。

### Requirement 7

**User Story:** AS 开发人员, I want 前端开发服务器把 `/api` 反代到 Console 后端, so that 浏览器只暴露一个预览端口。

#### Acceptance Criteria

1. WHEN 本地启动 Console 桌面前端开发服务器，THE 开发服务器 SHALL 把路径前缀 `/api` 转发到 Console HTTP。
2. THE Console 桌面前端 SHALL 使用 `/api/console` 调用 App 目录接口。

## Out of Scope (v1)

- 桌面内 iframe 窗口、拖动、最小化。
- 用户登录、鉴权、多租户。
- 从 gse-server 自动发现 URL。
- 修改 GSE 控制台的节点/作业页面信息架构。
