//! `/ebpf` 页面针对**真实 dataserver** 的验证（默认跳过，需要显式给出地址）。
//!
//! ## 为什么需要它
//!
//! 其它前端用例都跑在 `FakeHttp` 上，响应形状是**手写**的 —— 后端字段改名时它们照过。
//! 本仓库已经因此踩过两次坑（原始事件 `kind` vs `event_type`、时间戳恒为 0），所以这里补一条
//! 「真实 HTTP → 适配器 → React → DOM」的链路检查。
//!
//! ## 怎么跑
//!
//! ```bash
//! # 1. 起 dataserver 并采集一点数据（见 .monkeycode/specs/ebpf-observability/todo.md 附录）
//! # 2. 指向它的 SQL/Prom 基址跑本用例（Vite 只把 `VITE_` 前缀的变量暴露给客户端）
//! VITE_E2E_URL=http://127.0.0.1:18081 npm test -w @vectorman/dataplane
//! ```
//!
//! 不设 `VITE_E2E_URL` 时整个套件跳过（CI 里没有真实服务，也能保持绿）。
//!
//! **边界**：这是 jsdom 渲染 + 真实接口，不是真实浏览器 —— 覆盖不到 CSS/布局与真实事件循环。
//! 真浏览器验证（Playwright）仍是 `todo.md` 里的 TODO-2 剩余部分。

/// Vite 的 `import.meta.env` 类型来自这里；应用 tsconfig 没配 `types`，所以按需引用。
/// <reference types="vite/client" />

import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { MemoryRouter } from "react-router-dom";
import type { HttpClient, HttpRequest, HttpResponse } from "@vectorman/primitives";
import {
  JsonErrorMapper,
  MemoryAuthSession,
  MemoryNotifier,
  MemoryQueryStore,
} from "@vectorman/primitives";
import { ApmAdapter, DataplaneAdapter, EbpfAdapter, FetchHttpClient } from "@vectorman/adapters";
import { App } from "../app/App";
import { RuntimeProvider } from "../app/runtime";

// 用 Vite 的 `import.meta.env` 而不是 `process.env`：应用侧没有 Node 类型声明（引 @types/node
// 只为读一个环境变量不划算），而 `VITE_` 前缀本来就是 Vite 暴露变量的约定。
const baseUrl = (import.meta.env.VITE_E2E_URL as string | undefined)?.trim();

/// 真实 fetch 包装：**去掉 `signal`**。
///
/// jsdom 提供自己的 `AbortController`/`AbortSignal`，而 Node 的 `fetch`（undici）只认 Node 的
/// `AbortSignal`，直接传会报 `Expected signal ("AbortSignal {}") to be an instance of AbortSignal`。
/// 这是**测试环境**的差异（真实浏览器里两者同源，生产路径不受影响），因此这里只丢 signal；
/// 请求挂死由 vitest 的用例超时兜住。
const realFetch: typeof fetch = (input, init) => {
  const { signal: _signal, ...rest } = (init ?? {}) as RequestInit;
  return globalThis.fetch(input as never, rest as never);
};

/// 把相对路径拼到真实基址上（`FetchHttpClient` 只接受完整 URL）。
class PrefixHttp implements HttpClient {
  constructor(
    private readonly inner: HttpClient,
    private readonly base: string,
  ) {}

  async request<T>(req: HttpRequest): Promise<HttpResponse<T>> {
    return this.inner.request<T>({ ...req, url: `${this.base}${req.url}` });
  }
}

function renderLive() {
  const http = new PrefixHttp(
    new FetchHttpClient(new MemoryAuthSession(), new JsonErrorMapper(), realFetch),
    baseUrl!,
  );
  return render(
    <RuntimeProvider
      value={{
        dataplane: new DataplaneAdapter(http),
        apm: new ApmAdapter(http),
        ebpf: new EbpfAdapter(http),
        query: new MemoryQueryStore(),
        notifier: new MemoryNotifier(),
      }}
    >
      <MemoryRouter initialEntries={["/ebpf"]}>
        <App />
      </MemoryRouter>
    </RuntimeProvider>,
  );
}

afterEach(() => {
  cleanup();
});

// 没有真实服务时跳过：这类用例的价值全在「打到真接口」，用假响应跑没有意义。
const liveDescribe = baseUrl ? describe : describe.skip;

liveDescribe(`/ebpf 页面（真实 dataserver：${baseUrl ?? "未设置 VITE_E2E_URL"}）`, () => {
  it("能力状态卡片显示真实 Agent 与可用状态", async () => {
    renderLive();
    expect(await screen.findByText("eBPF 能力状态")).toBeTruthy();
    // 加载中会先渲染「加载中」占位，所以必须等**正条件**（可用状态）而不是等「没有告警」——
    // 后者在首帧就成立，会在数据到达前提前通过（踩过）。
    expect(await screen.findByText("可用", {}, { timeout: 10_000 })).toBeTruthy();
    // 检查点工具缺省用 `--agent-id agent-checkpoint` / `--item-id item-ebpf`。
    expect(await screen.findByText("agent-checkpoint")).toBeTruthy();
    expect(screen.getByText("item-ebpf")).toBeTruthy();
    // 能力卡片里不该出现「没有 Agent 上报」的告警。
    expect(screen.queryByText("没有 Agent 上报 eBPF 能力状态")).toBeNull();
  });

  it("边表渲染真实数据，事件视图能检索到原始事件", async () => {
    renderLive();
    // 边表进页面自动拉一次（POST /v1/edges/search，source=ebpf）；未识别服务显示 `unknown-<ip>`。
    const unknown = await screen.findAllByText(/unknown-\d+\.\d+\.\d+\.\d+/, {}, { timeout: 10_000 });
    expect(unknown.length).toBeGreaterThan(0);

    // 切到事件视图：`POST /v1/ebpf/events/search` 只在点查询时发。
    fireEvent.click(screen.getByRole("tab", { name: "事件" }));
    // 用 placeholder 定位事件表单：两个页签都挂着查询按钮，按文本取会歧义。
    const keyword = await screen.findByPlaceholderText("exec / connect");
    const eventForm = keyword.closest("form");
    expect(eventForm).toBeTruthy();
    // antd 会在两个汉字之间插一个空格，按钮文案实际是「查 询」。
    fireEvent.click(within(eventForm!).getByRole("button", { name: /查\s*询/ }));

    // 事件类型列渲染中文标签（`process_exec` → 进程启动），说明 labels.event_type 被解析出来了。
    const kinds = await screen.findAllByText(
      /进程启动|进程退出|fork|连接建立|接受连接|连接关闭/,
      {},
      { timeout: 10_000 },
    );
    expect(kinds.length).toBeGreaterThan(0);
  });
});
