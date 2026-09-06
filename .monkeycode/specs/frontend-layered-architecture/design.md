# 前端分层架构

Feature Name: frontend-layered-architecture
Updated: 2026-09-06

## Description

vectorman 控制台 v1 只交付分层骨架：五个原子能力接口与内存实现、三个后端协议适配器、React 装配入口。业务页面列为后续范围。依赖方向只允许向下：页面 -> 业务模块 -> 原子能力 / 领域适配器接口 <- 适配器实现。

技术栈：React + Vite + TypeScript，源码目录 `frontend/`。

## Architecture

分层自上而下：装配入口 -> 页面 -> 业务模块 -> 原子能力接口 -> 适配器实现 -> 开发期反代 -> 后端 HTTP。

```mermaid
graph TD
    ROOT["composition root frontend/src/main.tsx"]
    subgraph pages ["页面层 v1 空目录"]
        PAGE["pages/"]
    end
    subgraph features ["业务模块层 v1 空目录"]
        FEAT["features/"]
    end
    subgraph primitives ["原子能力接口"]
        HTTP["HttpClient"]
        MAP["ErrorMapper"]
        AUTH["AuthSession"]
        QS["QueryStore"]
        NOTE["Notifier"]
    end
    subgraph adapters ["适配器"]
        FETCH["FetchHttpClient"]
        GSE["GseAdminAdapter"]
        SQLA["SqlHttpAdapter"]
        PROMA["PromQueryAdapter"]
        MEM["内存实现 Auth Query Notifier Mapper"]
    end
    subgraph proxy ["Vite 反代"]
        PGSE["/api/gse"]
        PSQL["/api/sql"]
        PPROM["/api/prom"]
    end
    subgraph backends ["已有后端"]
        GSEH["gse-server 127.0.0.1:7101"]
        SQLH["apiserver SQL 127.0.0.1:8081"]
        PROMH["apiserver Prom 127.0.0.1:9090"]
    end
    ROOT --> PAGE
    ROOT --> FEAT
    ROOT --> HTTP
    ROOT --> AUTH
    ROOT --> QS
    ROOT --> NOTE
    PAGE --> FEAT
    FEAT --> HTTP
    FEAT --> AUTH
    FEAT --> QS
    FEAT --> NOTE
    FEAT --> GSE
    FEAT --> SQLA
    FEAT --> PROMA
    HTTP --> FETCH
    MAP --> MEM
    AUTH --> MEM
    QS --> MEM
    NOTE --> MEM
    GSE --> HTTP
    SQLA --> HTTP
    PROMA --> HTTP
    FETCH --> PGSE
    FETCH --> PSQL
    FETCH --> PPROM
    PGSE --> GSEH
    PSQL --> SQLH
    PPROM --> PROMH
```

依赖规则：

- `primitives/` 零运行时依赖，不从 `react` 导入类型。
- `adapters/` 依赖 `primitives/`；`FetchHttpClient` 是唯一调用浏览器 `fetch` 的模块。
- `features/` 与 `pages/` 只依赖接口与适配器对外类型，不 import `fetch`。
- 装配入口是唯一 `new` 具体实现的地方。

## Components and Interfaces

### 目录

```text
frontend/
  package.json
  vite.config.ts
  tsconfig.json
  index.html
  src/
    main.tsx                 # 装配入口
    app/App.tsx              # 装配成功确认页
    primitives/
      error.ts               # AppError
      http.ts                # HttpClient, HttpRequest, HttpResponse, RequestContext
      mapper.ts              # ErrorMapper
      session.ts             # AuthSession, Session
      query.ts               # QueryStore, QueryState
      notifier.ts            # Notifier, Notice
      index.ts
    adapters/
      memory/
        session.ts
        query.ts
        notifier.ts
        mapper.ts
      http/
        fetch-client.ts
      gse/
        admin.ts             # GseAdminAdapter + DTO
      dataplane/
        sql.ts
        prom.ts
    features/                # v1 空，保留 .gitkeep
    pages/                   # v1 空，保留 .gitkeep
  src/primitives/*.test.ts
  src/adapters/**/*.test.ts
```

### 错误契约

与后端 `GseError` / `DataplaneError` 对齐。稳定 `code`：

| code | 含义 |
| --- | --- |
| `not_found` | 资源不存在 |
| `invalid_argument` | JSON/参数无法解析或缺字段 |
| `query_failed` | 后端执行失败 |
| `unimplemented` | 未实现的协议或函数 |
| `unavailable` | 网络不可达或超时 |
| `unauthorized` | 会话无效（v1 预留） |

```ts
type AppError = { code: string; message: string }
```

`ErrorMapper.map(input)`：

- HTTP JSON 含 `code`：使用该 `code`；`message` 取 `error` 或 `message` 字段。
- Prom 形状 `{status:"error"}`：`code` 取 `errorType`，缺省 `query_failed`；`message` 取 `error`。
- 无 body 的 4xx/5xx：按状态码映射（404 -> `not_found`，400 -> `invalid_argument`，401/403 -> `unauthorized`，其余 `query_failed`）。
- `TypeError` / `AbortError`：`unavailable`。
- JSON 解析失败：`invalid_argument`。

### HttpClient

```ts
type HttpMethod = "GET" | "POST" | "PUT" | "PATCH" | "DELETE"

type RequestContext = {
  timeoutMs: number
  signal?: AbortSignal
  traceId?: string
}

type HttpRequest = {
  method: HttpMethod
  url: string
  headers?: Record<string, string>
  body?: unknown
  context?: RequestContext
}

type HttpResponse<T = unknown> = {
  status: number
  body: T
}

interface HttpClient {
  request<T>(req: HttpRequest): Promise<HttpResponse<T>>
}
```

默认 `timeoutMs = 15000`。超时用 `AbortController`；调用方传入的 `signal` 与超时控制器合并。

`FetchHttpClient` 构造注入 `AuthSession` 与 `ErrorMapper`：

- 成功：`response.ok` 且 body 可按 JSON 解析（空 body 视为 `null`）。
- 失败：抛出 `AppError`（Promise reject），不返回 `{ok:false}` 联合类型。业务侧用 try/catch 或 `QueryStore`。
- 会话非空且含 `token` 时附加 `Authorization: Bearer <token>`。
- 每次请求附加 `X-Trace-Id`：使用 `context.traceId`，缺省由客户端生成。

### AuthSession

```ts
type Session = {
  token?: string
  subject?: string
}

interface AuthSession {
  get(): Session | null
  set(session: Session): void
  clear(): void
}
```

v1：`MemoryAuthSession`，模块内变量保存；刷新页面后为 `null`。不写 `localStorage`。

### QueryStore

按查询键保存一条状态。接口与 React 解耦；后续页面可用 `useSyncExternalStore` 订阅。

```ts
type QueryStatus = "idle" | "loading" | "success" | "error"

type QueryRecord<T> = {
  status: QueryStatus
  data?: T
  error?: AppError
}

interface QueryStore {
  get<T>(key: string): QueryRecord<T>
  setLoading(key: string): void
  setSuccess<T>(key: string, data: T): void
  setError(key: string, error: AppError): void
  subscribe(key: string, listener: () => void): () => void
}
```

`MemoryQueryStore`：`Map<string, QueryRecord>` + 按 key 的 listener 集合。`get` 对未知 key 返回 `{status:"idle"}`。

### Notifier

```ts
type NoticeLevel = "success" | "warning" | "error"

type Notice = {
  id: string
  level: NoticeLevel
  message: string
}

interface Notifier {
  success(message: string): void
  warning(message: string): void
  error(error: AppError): void
  subscribe(listener: (notice: Notice) => void): () => void
}
```

`MemoryNotifier`：内存队列，上限 50 条，超出丢弃最早一条。`error` 使用 `error.message`。v1 装配入口不渲染 Toast。

### 后端适配器

三个适配器均注入 `HttpClient`。URL 只用相对前缀。

`GseAdminAdapter` 前缀 `/api/gse`：

| 方法 | HTTP | 路径 |
| --- | --- | --- |
| `listHosts` | GET | `/api/gse/hosts` |
| `upsertHost` | POST | `/api/gse/hosts` |
| `getHost` | GET | `/api/gse/hosts/{host_id}` |
| `deleteHost` | DELETE | `/api/gse/hosts/{host_id}` |
| `listAccessPoints` | GET | `/api/gse/access-points` |
| `upsertAccessPoint` | POST | `/api/gse/access-points` |
| `getAccessPoint` | GET | `/api/gse/access-points/{id}` |
| `deleteAccessPoint` | DELETE | `/api/gse/access-points/{id}` |
| `listAgents` | GET | `/api/gse/agents` |
| `upsertAgent` | POST | `/api/gse/agents` |
| `getAgent` | GET | `/api/gse/agents/{agent_id}` |
| `deleteAgent` | DELETE | `/api/gse/agents/{agent_id}` |
| `listAgentConfigs` | GET | `/api/gse/agent-configs` |
| `upsertAgentConfig` | POST | `/api/gse/agent-configs` |
| `getAgentConfig` | GET | `/api/gse/agent-configs/{agent_id}` |

DTO 与 `crates/gse-server-core/src/ledger.rs` 字段同名：`Host`、`AccessPoint`、`Agent`、`AgentConfig`。路径参数做 `encodeURIComponent`。

`SqlHttpAdapter` 前缀 `/api/sql`：

- `execute(sql: string, params: unknown[]): Promise<{columns: string[]; rows: unknown[][]}>`
- 请求：`POST /api/sql/v1/sql`，body `{"sql","params"}`

`PromQueryAdapter` 前缀 `/api/prom`：

- `queryInstant(expr: string, time?: string): Promise<PromEnvelope>`
- `queryRange(expr: string, start: string, end: string, step: string): Promise<PromEnvelope>`
- `PromEnvelope = { status: string; data?: unknown; error?: string; errorType?: string }`
- 路径：`GET /api/prom/api/v1/query`、`GET /api/prom/api/v1/query_range`
- 当 `status === "error"`，适配器抛出经 `ErrorMapper` 映射的 `AppError`

Vite 反代剥掉前端前缀，后端收到原路径：

| 浏览器路径 | target | rewrite |
| --- | --- | --- |
| `/api/gse/hosts` | `http://127.0.0.1:7101` | `/hosts` |
| `/api/sql/v1/sql` | `http://127.0.0.1:8081` | `/v1/sql` |
| `/api/prom/api/v1/query` | `http://127.0.0.1:9090` | `/api/v1/query` |

`vite.config.ts`：`server.allowedHosts` 含 `.monkeycode-ai.online`。

### 装配入口

`main.tsx` 一次性构造：

1. `MemoryAuthSession`、`JsonErrorMapper`、`MemoryQueryStore`、`MemoryNotifier`
2. `FetchHttpClient({ session, mapper })`
3. 三个后端适配器
4. 通过 React Context 把上述实例交给 `App`

`App.tsx` 渲染一行静态确认文案（例如“console composition ready”），证明装配成功。不调用后端。

## Data Models

见上节 TypeScript 类型。GSE DTO 与 ledger 对齐：

| 类型 | 必填字段 |
| --- | --- |
| Host | `host_id`, `inner_ip` |
| AccessPoint | `id`, `name`, `server_ip`, `rpc_port` |
| Agent | `agent_id`, `host_id`, `token` |
| AgentConfig | `agent_id`, `host_id` |

其余字段可选，缺省为空字符串或 `null`，与 serde `default` 一致。

## Correctness Properties

- 依赖单向：`primitives` 不 import `adapters`、`react`、`features`、`pages`。
- `FetchHttpClient` 是唯一 `fetch` 调用点。
- 适配器 URL 以 `/api/gse`、`/api/sql`、`/api/prom` 开头。
- `AuthSession.get()` 在 `clear()` 之后返回 `null`。
- `QueryStore` 同一 key 的状态为 idle/loading/success/error 四者之一。
- `Notifier` 订阅者收到的 `error` 级 `message` 等于传入 `AppError.message`。
- 适配器测试零真实网络。

## Error Handling

| 场景 | 行为 |
| --- | --- |
| 网络失败 / 超时 | `HttpClient` reject，`code=unavailable` |
| 请求/响应 JSON 非法 | `code=invalid_argument` |
| GSE 4xx/5xx JSON `{code,error}` | 原样映射 |
| Prom `status=error` | 适配器 reject，映射 `errorType`/`error` |
| 空会话 | 仍发请求，无 `Authorization` |
| Notifier 无订阅者 | 消息进队列，不抛错 |

## Test Strategy

测试框架：Vitest。不启动 gse-server / apiserver。

1. `JsonErrorMapper`：后端 `code`、Prom `errorType`、AbortError、非法 JSON。
2. `MemoryAuthSession`：set/get/clear。
3. `MemoryQueryStore`：状态迁移与 subscribe 通知。
4. `MemoryNotifier`：三级提示、error 用 `message`、subscribe、队列上限。
5. `FetchHttpClient`：用 stub `fetch` 断言 timeout abort、Bearer、trace 头、reject 形状。
6. 三个适配器：注入假 `HttpClient`，断言 method、url、body。

## References

[^1]: (Filename) - [GSE HTTP 管理口](crates/gse-server-core/src/http.rs)
[^2]: (Filename) - [GSE ledger DTO](crates/gse-server-core/src/ledger.rs)
[^3]: (Filename) - [dataplane SQL/Prom HTTP](.monkeycode/specs/dataplane-layered-storage/design.md)
[^4]: (Filename) - [本 feature 需求](.monkeycode/specs/frontend-layered-architecture/requirements.md)
