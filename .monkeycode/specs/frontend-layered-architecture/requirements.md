# Requirements Document

## Introduction

vectorman 前端采用分层架构。v1 只交付分层骨架：原子能力接口、可替换实现、后端协议适配器，以及 React + Vite 的装配入口。本期不交付业务页面。后续 CMDB、节点、作业等控制台页面在此底座上增加业务模块。

## Glossary

- **控制台（Console）**：运维在浏览器中使用的 Web 应用；v1 仅含装配入口，不含业务页面。
- **页面层（Page）**：路由对应的视图；v1 保留分层位置，不实现业务路由页。
- **业务模块（Feature）**：面向领域的功能单元；v1 保留分层位置，不实现领域模块。
- **原子能力（Primitive）**：跨业务复用的底层能力接口，不包含具体业务语义。
- **适配器（Adapter）**：把原子能力接到具体实现（HTTP 库、后端协议、浏览器 API）。
- **装配入口（Composition Root）**：把原子能力的具体实现注入给上层的唯一组装点。
- **GSE 管理 HTTP**：gse-server 独立管理端口，默认 `127.0.0.1:7101`，提供 hosts / access_points / agents / agent_configs 增删改查。
- **SQL HTTP**：apiserver `POST /v1/sql`。
- **Prom 查询 HTTP**：apiserver `GET /api/v1/query` 与 `GET /api/v1/query_range`。
- **错误契约（Error Contract）**：前端统一的错误对象，含机器可读 `code` 与给人看的 `message`。
- **请求上下文（Request Context）**：一次调用携带的超时、取消、追踪标识与鉴权信息。
- **查询状态（Query State）**：一次远程查询的 idle、loading、success、error 四种状态之一。

## Requirements

### Requirement 1: 分层与依赖方向

**User Story:** AS 前端开发者, I want 页面、业务、原子能力、适配器分层且依赖只向下, so that 后续业务代码不绑死具体实现。

#### Acceptance Criteria

1. THE 控制台 SHALL 将代码划分为页面层、业务模块层、原子能力层、适配器层。
2. THE 页面层 SHALL 只依赖业务模块与布局，不直接调用适配器。
3. THE 业务模块 SHALL 只通过原子能力接口与后端适配器的领域接口访问远程能力。
4. THE 原子能力层 SHALL 定义接口与类型，不绑定某一 HTTP 库或某一 UI 组件库。
5. THE 适配器层 SHALL 实现原子能力接口与后端协议映射，并作为唯一接触浏览器 API 与后端协议的层。

### Requirement 2: 原子能力清单

**User Story:** AS 前端开发者, I want v1 先沉淀网络与状态相关的原子能力, so that 后续页面可以复用同一底座。

#### Acceptance Criteria

1. THE 原子能力层 SHALL 提供以下五个接口：`HttpClient`、`ErrorMapper`、`AuthSession`、`QueryStore`、`Notifier`。
2. THE 每个原子能力接口 SHALL 在模块文档中使用本文件 Glossary 或本节给出的名称。
3. THE 原子能力接口 SHALL 可被单元测试用内存实现替换。
4. WHEN 新增业务模块，THE 业务模块 SHALL 复用上述五个接口访问网络、会话、查询状态与提示，不并行引入第二套同职责底座。

### Requirement 3: HTTP 原子能力

**User Story:** AS 业务模块, I want 统一的异步 HTTP 调用, so that 超时、取消、错误码与追踪标识处理方式一致。

#### Acceptance Criteria

1. THE `HttpClient` SHALL 提供 `request` 方法，入参包含 method、url、headers、body 与 Request Context。
2. WHEN 调用成功，THE `HttpClient` SHALL 返回状态码与已解析的响应体。
3. WHEN 后端返回 JSON 且包含 `code` 字段，THE `ErrorMapper` SHALL 将该字段映射进错误契约的 `code`。
4. IF 网络不可达或超过 Request Context 中的超时，THE `HttpClient` SHALL 返回错误契约，`code` 为 `unavailable`。
5. IF 请求体无法序列化或响应体无法按约定解析，THE `HttpClient` SHALL 返回错误契约，`code` 为 `invalid_argument`。

### Requirement 4: 会话原子能力

**User Story:** AS 业务模块, I want 登录态走统一入口, so that 各模块不必各自解析凭据。

#### Acceptance Criteria

1. THE `AuthSession` SHALL 提供读取当前会话、写入会话、清除会话三个方法。
2. THE `AuthSession` 的 v1 实现 SHALL 将会话保存在进程内存中；页面刷新后会话为空。
3. WHILE 会话包含鉴权信息，THE `HttpClient` SHALL 在发出请求时附带该会话的鉴权信息。
4. IF 会话为空，THE `HttpClient` SHALL 仍发出请求，请求头不含鉴权信息。
5. WHILE v1 后端鉴权默认关闭，THE 控制台 SHALL 装配 `AuthSession`，并允许空会话访问后端适配器。

### Requirement 5: 查询状态与通知

**User Story:** AS 业务模块, I want 查询状态与错误提示有统一接口, so that 后续页面不必各自维护 loading 与报错。

#### Acceptance Criteria

1. THE `QueryStore` SHALL 为一次远程查询保存 idle、loading、success、error 四种状态中的一种。
2. WHEN 查询进入 success，THE `QueryStore` SHALL 保存该次查询的结果数据。
3. WHEN 查询进入 error，THE `QueryStore` SHALL 保存错误契约。
4. THE `Notifier` SHALL 提供成功、警告、错误三类提示方法，以及按条订阅提示的 `subscribe` 方法。
5. WHEN 调用错误提示方法，THE `Notifier` SHALL 使用错误契约的 `message` 作为提示正文，并将该条提示推入内存队列。
6. THE v1 装配入口 SHALL 装配内存订阅实现，不渲染 Toast UI。

### Requirement 6: 后端适配器边界

**User Story:** AS 前端开发者, I want 每个后端协议有独立适配器, so that GSE 与 dataplane 的 URL、载荷形状变化不影响后续业务模块。

#### Acceptance Criteria

1. THE 适配器层 SHALL 提供 `GseAdminAdapter`、`SqlHttpAdapter`、`PromQueryAdapter` 三个适配器。
2. THE `GseAdminAdapter` SHALL 覆盖 GSE 管理 HTTP 的 hosts、access_points、agents、agent_configs 四类资源的列表、读取、写入与删除。
3. THE `SqlHttpAdapter` SHALL 将一条 SQL 语句与参数映射为 `POST /v1/sql` 的 JSON 体 `{"sql":"<statement>","params":[<values>]}`。
4. THE `PromQueryAdapter` SHALL 将即时查询映射为 `GET /api/v1/query`，将区间查询映射为 `GET /api/v1/query_range`。
5. THE 三个适配器 SHALL 只通过 `HttpClient` 发请求。

### Requirement 7: 开发期反向代理与单入口

**User Story:** AS 前端开发者, I want 浏览器只访问一个前端入口, so that 后续预览环境单端口即可同时打到 GSE 与 dataplane。

#### Acceptance Criteria

1. THE 控制台开发服务器 SHALL 将 `/api/gse` 前缀转发到 GSE 管理 HTTP。
2. THE 控制台开发服务器 SHALL 将 `/api/sql` 前缀转发到 SQL HTTP。
3. THE 控制台开发服务器 SHALL 将 `/api/prom` 前缀转发到 Prom 查询 HTTP。
4. THE 适配器 SHALL 只使用上述相对前缀，不硬编码主机名或绝对后端地址。
5. THE 开发服务器 SHALL 允许通过 `*.monkeycode-ai.online` 主机名访问。

### Requirement 8: v1 交付范围

**User Story:** AS 前端开发者, I want v1 只落地骨架, so that 分层与原子能力可以先被测试和复用。

#### Acceptance Criteria

1. THE v1 控制台 SHALL 提供 React 装配入口，将五个原子能力的具体实现与三个后端适配器注入到运行时。
2. THE v1 控制台 SHALL 将业务页面与领域模块列为后续范围。
3. THE 页面层与业务模块层 SHALL 在源码目录中保留对应位置，供后续功能写入。
4. THE 装配入口 SHALL 在开发服务器启动后可被浏览器打开，用于确认分层装配成功。

### Requirement 9: 技术栈与目录约定

**User Story:** AS 前端开发者, I want React 与 Vite 的独立前端目录, so that 前端与 Rust crates 分离构建。

#### Acceptance Criteria

1. THE 仓库 SHALL 将控制台源码放在根目录 `frontend/`，与 Rust crates 分离。
2. THE 控制台 SHALL 使用 React、Vite 与 TypeScript。
3. THE 原子能力接口与其内存假实现 SHALL 位于同一能力目录树下，供测试直接引用。
4. THE 原子能力的 TypeScript 接口定义 SHALL 不从 `react` 包导入类型。

### Requirement 10: 可测试性

**User Story:** AS 前端开发者, I want 原子能力与适配器可脱离页面测试, so that 骨架在没有业务页时也能验证。

#### Acceptance Criteria

1. WHEN 运行前端单元测试，THE 测试 SHALL 在不启动 gse-server 与 apiserver 的前提下覆盖五个原子能力接口的成功与失败路径。
2. THE 适配器测试 SHALL 使用可注入的 `HttpClient` 假实现验证 URL、方法与 JSON 体，不发起真实网络请求。
3. THE 五个原子能力 SHALL 各提供一份内存实现，供测试与装配入口选用。
