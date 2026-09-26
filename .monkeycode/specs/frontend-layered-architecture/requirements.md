# Requirements Document

> **已归档**（2026-09-26 文档整理）：本 feature 已实现并合入 main，本文件压缩为需求索引——完整 EARS 条款见 git 历史（本文件重写前的最后一个版本），design.md 全文保留为实施细节档案。

## Introduction

vectorman 前端采用分层架构，并以 npm workspaces 拆成可独立编译的包。v1 交付：原子能力包、适配器包、控制台应用与作业应用的装配入口。本期不交付业务页面。后续 CMDB、节点、作业等页面在对应应用内增加业务模块，并复用同一套原子能力。

> **架构修订（2026-09-10）**：前端收敛为单一构建产物 `@vectorman/console`；`@vectorman/node` 与 `@vectorman/job` 改为 UI 包，仅导出页面与运行时，由 console 组合。下文 Requirement 8 中三个应用各自装配/独立产物的表述为 v1 原始需求，现以实现单一入口为准。

## Glossary

- **工作区（Workspace）**：根目录 `frontend/` 下的 npm workspaces，包含库包与应用包。
- **库包（Library package）**：被应用依赖的 TypeScript 包，自身不作为浏览器入口。
- **应用包（App package）**：带 Vite 入口、可独立 `dev`/`build` 的前端应用。
- **控制台（Console）**：应用包 `@vectorman/console`；v1 仅含装配入口，不含业务页面。
- **作业应用（Job app）**：应用包 `@vectorman/job`；v1 仅含装配入口，不含作业编排页面。
- **页面层（Page）**：路由对应的视图；v1 在各应用内保留分层位置，不实现业务路由页。
- **业务模块（Feature）**：面向领域的功能单元；v1 保留分层位置，不实现领域模块。
- **原子能力（Primitive）**：跨应用复用的底层能力接口，不包含具体业务语义。
- **适配器（Adapter）**：把原子能力接到具体实现（HTTP 库、后端协议、浏览器 API）。
- **装配入口（Composition Root）**：每个应用包内把原子能力的具体实现注入给上层的唯一组装点。
- **GSE 管理 HTTP**：gse-server 独立管理端口，默认 `127.0.0.1:7101`，提供 hosts / access_points / agents / agent_configs 增删改查。
- **SQL HTTP**：apiserver `POST /v1/sql`。
- **Prom 查询 HTTP**：apiserver `GET /api/v1/query` 与 `GET /api/v1/query_range`。
- **错误契约（Error Contract）**：前端统一的错误对象，含机器可读 `code` 与给人看的 `message`。
- **请求上下文（Request Context）**：一次调用携带的超时、取消、追踪标识与鉴权信息。
- **查询状态（Query State）**：一次远程查询的 idle、loading、success、error 四种状态之一。

## Requirements

### Requirement 1: 分层与依赖方向

- AS 前端开发者, I want 页面、业务、原子能力、适配器分层且依赖只向下, so that 后续业务代码不绑死具体实现。
- 验收：THE 每个应用包 SHALL 将代码划分为页面层、业务模块层，并依赖原子能力层与适配器层；THE 页面层 SHALL 只依赖业务模块与布局，不直接调用适配器实现。
### Requirement 2: 原子能力清单

- AS 前端开发者, I want v1 先沉淀网络与状态相关的原子能力, so that 多个应用可以复用同一底座。
- 验收：THE 原子能力层 SHALL 提供以下五个接口：`HttpClient`、`ErrorMapper`、`AuthSession`、`QueryStore`、`Notifier`；THE 每个原子能力接口 SHALL 在模块文档中使用本文件 Glossary 或本节给出的名称。
### Requirement 3: HTTP 原子能力

- AS 业务模块, I want 统一的异步 HTTP 调用, so that 超时、取消、错误码与追踪标识处理方式一致。
- 验收：THE `HttpClient` SHALL 提供 `request` 方法，入参包含 method、url、headers、body 与 Request Context；WHEN 调用成功，THE `HttpClient` SHALL 返回状态码与已解析的响应体。
### Requirement 4: 会话原子能力

- AS 业务模块, I want 登录态走统一入口, so that 各模块不必各自解析凭据。
- 验收：THE `AuthSession` SHALL 提供读取当前会话、写入会话、清除会话三个方法；THE `AuthSession` 的 v1 实现 SHALL 将会话保存在进程内存中；页面刷新后会话为空。
### Requirement 5: 查询状态与通知

- AS 业务模块, I want 查询状态与错误提示有统一接口, so that 后续页面不必各自维护 loading 与报错。
- 验收：THE `QueryStore` SHALL 为一次远程查询保存 idle、loading、success、error 四种状态中的一种；WHEN 查询进入 success，THE `QueryStore` SHALL 保存该次查询的结果数据。
### Requirement 6: 后端适配器边界

- AS 前端开发者, I want 每个后端协议有独立适配器, so that GSE 与 dataplane 的 URL、载荷形状变化不影响后续业务模块。
- 验收：THE 适配器层 SHALL 提供 `GseAdminAdapter`、`SqlHttpAdapter`、`PromQueryAdapter` 三个适配器；THE `GseAdminAdapter` SHALL 覆盖 GSE 管理 HTTP 的 hosts、access_points、agents、agent_configs 四类资源的列表、读取、写入与删除。
### Requirement 7: 开发期反向代理与单入口

- AS 前端开发者, I want 每个应用只暴露一个浏览器入口, so that 预览环境单端口即可打到 GSE 与 dataplane。
- 验收：THE 每个应用包的开发服务器 SHALL 将 `/api/gse` 前缀转发到 GSE 管理 HTTP；THE 每个应用包的开发服务器 SHALL 将 `/api/sql` 前缀转发到 SQL HTTP。
### Requirement 8: v1 交付范围

- AS 前端开发者, I want v1 只落地骨架, so that 分层与原子能力可以先被测试和被多个应用复用。
- 验收：THE `@vectorman/console` SHALL 提供 React 装配入口，将五个原子能力的具体实现与三个后端适配器注入到运行时；THE `@vectorman/job` SHALL 提供独立的 React 装配入口，将同一套原子能力与适配器注入到该应用运行时。
### Requirement 9: 技术栈与工作区目录

- AS 前端开发者, I want React 与 Vite 的多包工作区, so that 原子能力可被多个应用独立编译。
- 验收：THE 仓库 SHALL 将前端工作区放在根目录 `frontend/`，与 Rust crates 分离；THE 工作区与各应用包 SHALL 使用 React、Vite 与 TypeScript。
### Requirement 10: 可测试性

- AS 前端开发者, I want 原子能力与适配器可脱离页面测试, so that 骨架在没有业务页时也能验证。
- 验收：WHEN 运行前端单元测试，THE 测试 SHALL 在不启动 gse-server 与 apiserver 的前提下覆盖五个原子能力接口的成功与失败路径；THE 适配器测试 SHALL 使用可注入的 `HttpClient` 假实现验证 URL、方法与 JSON 体，不发起真实网络请求。
### Requirement 11: 多包编译

- AS 前端开发者, I want 用同一套原子能力分别构建多个应用, so that 控制台与作业前端可以独立发版。
- 验收：THE `frontend/` SHALL 使用 npm workspaces，成员包含 `packages/*` 与 `apps/*`；THE workspace SHALL 提供库包 `@vectorman/primitives` 与 `@vectorman/adapters`。
