import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it } from "vitest";
import type { HttpClient, HttpRequest, HttpResponse } from "@vectorman/primitives";
import {
  JsonErrorMapper,
  MemoryAuthSession,
  MemoryNotifier,
  MemoryQueryStore,
} from "@vectorman/primitives";
import { ApmAdapter, DataplaneAdapter, FetchHttpClient, type CollectItem } from "@vectorman/adapters";
import { App } from "./App";
import { RuntimeProvider } from "./runtime";

class FakeHttp implements HttpClient {
  readonly calls: HttpRequest[] = [];

  constructor(private readonly items: CollectItem[]) {}

  async request<T>(req: HttpRequest): Promise<HttpResponse<T>> {
    this.calls.push(req);
    const url = req.url;
    if (req.method === "GET" && url.startsWith("/v1/collect-items")) {
      return { status: 200, body: this.items as T };
    }
    if (url === "/v1/streams") {
      return {
        status: 200,
        body: {
          streams: [
            { agent_id: "a-1", data_type: "metrics", data_id: "item-1", last_seen_micros: 1710000000000000, accepted: 4 },
          ],
        } as T,
      };
    }
    if (url === "/v1/agents") {
      return {
        status: 200,
        body: [
          { agent_id: "a-1", host_id: "h-1", token: "t" },
          { agent_id: "a-2", host_id: "h-2", token: "t" },
        ] as T,
      };
    }
    if (url.startsWith("/api/v1/query_range") || url.startsWith("/api/v1/query")) {
      return { status: 200, body: { status: "success", data: { resultType: "matrix", result: [] } } as T };
    }
    if (url === "/v1/logs/search") {
      return { status: 200, body: { records: [] } as T };
    }
    if (url === "/v1/traces/search") {
      return {
        status: 200,
        body: {
          total: 2,
          traces: [
            {
              trace_id: "b".repeat(32),
              start_ts: 1_710_000_000_000_000,
              duration_micros: 250_000,
              root_service: "payment",
              root_operation: "POST /pay",
              span_count: 2,
              error_count: 1,
              status: "error",
              services: ["payment", "db"],
              collector: "otlp",
              agent_id: "a-1",
              host_id: "h-1",
              data_id: "item-1",
            },
            {
              trace_id: "a".repeat(32),
              start_ts: 1_710_000_000_100_000,
              duration_micros: 12_000,
              root_service: "order-api",
              root_operation: "GET /orders",
              span_count: 1,
              error_count: 0,
              status: "ok",
              services: ["order-api"],
              collector: "otlp",
              agent_id: "a-1",
              host_id: "h-1",
              data_id: "item-1",
            },
          ],
        } as T,
      };
    }
    if (req.method === "GET" && url.startsWith("/v1/traces/")) {
      const traceId = url.split("/").pop()!;
      return {
        status: 200,
        body: {
          partial: false,
          reason: null,
          expected_span_count: 2,
          summary: {
            trace_id: traceId,
            start_ts: 1_710_000_000_000_000,
            duration_micros: 250_000,
            root_service: "payment",
            root_operation: "POST /pay",
            span_count: 2,
            error_count: 1,
            status: "error",
            services: ["payment", "db"],
            collector: "otlp",
            agent_id: "a-1",
            host_id: "h-1",
            data_id: "item-1",
          },
          spans: [
            {
              record_id: `${traceId}:root`,
              trace_id: traceId,
              span_id: "root",
              parent_span_id: "",
              name: "POST /pay",
              kind: "server",
              service: "payment",
              status_code: "error",
              status_message: "card declined",
              start_unix_nano: 1_710_000_000_000_000_000,
              end_unix_nano: 1_710_000_000_250_000_000,
              duration_micros: 250_000,
              attributes: { "http.request.method": "POST", "http.response.status_code": "500" },
              resource: { "k8s.pod.name": "payment-1" },
              events: [{ name: "exception", time_unix_nano: 1, attributes: { "exception.type": "CardDeclined" } }],
              links: [{ trace_id: "c".repeat(32), span_id: "d".repeat(16), attributes: {} }],
              dropped_events: 0,
              dropped_attributes: 0,
              dropped_links: 0,
              collector: "otlp",
            },
            {
              record_id: `${traceId}:child`,
              trace_id: traceId,
              span_id: "child",
              parent_span_id: "root",
              name: "INSERT payments",
              kind: "client",
              service: "db",
              status_code: "ok",
              status_message: "",
              start_unix_nano: 1_710_000_000_050_000_000,
              end_unix_nano: 1_710_000_000_120_000_000,
              duration_micros: 70_000,
              attributes: {},
              resource: {},
              events: [],
              links: [],
              collector: "otlp",
            },
          ],
        } as T,
      };
    }
    return { status: 200, body: (this.items[0] ?? null) as T };
  }
}

const items: CollectItem[] = [
  {
    item_id: "item-1",
    agent_ids: ["a-1"],
    name: "cpu",
    kind: "metrics_host",
    enabled: true,
    collector: { interval_secs: 15 },
    storage: { retention_days: 1 },
  },
  {
    item_id: "item-2",
    agent_ids: ["a-2"],
    name: "nginx logs",
    kind: "log_file",
    enabled: true,
    collector: { path_patterns: ["/var/log/*.log"], start_mode: "tail", start_n: 0 },
    storage: { retention_days: 2 },
  },
];

function renderAt(path: string) {
  const http = new FakeHttp(items);
  const adapter = new DataplaneAdapter(http);
  const apm = new ApmAdapter(http);
  return {
    http,
    ...render(
      <RuntimeProvider
        value={{ dataplane: adapter, apm, query: new MemoryQueryStore(), notifier: new MemoryNotifier() }}
      >
        <MemoryRouter initialEntries={[path]}>
          <App />
        </MemoryRouter>
      </RuntimeProvider>,
    ),
  };
}

afterEach(() => {
  cleanup();
});

describe("trace pages", () => {
  it("lists traces and opens the waterfall detail", async () => {
    const view = renderAt("/traces");
    // 列表：展示根服务与来源。
    expect(await screen.findByText("payment")).toBeTruthy();
    expect(screen.getByText("order-api")).toBeTruthy();
    expect(screen.getAllByText("250ms").length).toBeGreaterThan(0);
    expect(screen.getByText("trace 列表（共 2 条）")).toBeTruthy();

    // 点击行进入详情：瀑布图渲染两个 span，摘要展示根服务与采集项。
    fireEvent.click(screen.getByText("payment"));
    expect(await screen.findByRole("img", { name: "trace span 瀑布图" })).toBeTruthy();
    await waitFor(() => expect(screen.getByText("trace 摘要")).toBeTruthy());
    expect(screen.getByText("payment · POST /pay")).toBeTruthy();
    expect(screen.getByText("db · INSERT payments")).toBeTruthy();
    expect(screen.getAllByText("250ms").length).toBeGreaterThan(0);

    // 点击某个 span 打开抽屉：概览里有 status_message，切页签看属性/事件/链接。
    fireEvent.click(screen.getByText("payment · POST /pay"));
    expect(await screen.findByText("card declined")).toBeTruthy();

    fireEvent.click(screen.getByText("属性（2）"));
    expect(await screen.findByText("http.request.method")).toBeTruthy();

    fireEvent.click(screen.getByText("事件（1）"));
    expect(await screen.findByText("exception")).toBeTruthy();
    expect(await screen.findByText("exception.type")).toBeTruthy();

    fireEvent.click(screen.getByText("链接（1）"));
    expect(await screen.findByText("c".repeat(32))).toBeTruthy();
    view.unmount();
  });
});

describe("dataplane shell", () => {
  it("routes to collect, metrics and logs pages", async () => {
    const collect = renderAt("/");
    expect(await screen.findByText("新建采集项")).toBeTruthy();
    collect.unmount();

    const metrics = renderAt("/metrics");
    expect(await screen.findByRole("heading", { name: "指标检索" })).toBeTruthy();
    expect(screen.getByText("CPU 使用率")).toBeTruthy();
    metrics.unmount();

    const logs = renderAt("/logs");
    expect(await screen.findByRole("button", { name: /检\s*索/ })).toBeTruthy();
  });
});

describe("collect page", () => {
  it("toggles enabled through the row switch", async () => {
    const { http } = renderAt("/");
    const toggle = await screen.findByRole("switch", { name: "启用 cpu" });
    fireEvent.click(toggle);
    await waitFor(() => {
      expect(http.calls.some((c) => c.method === "PUT" && c.url === "/v1/collect-items/item-1")).toBe(true);
    });
    const put = http.calls.find((c) => c.method === "PUT" && c.url === "/v1/collect-items/item-1");
    expect((put?.body as { enabled: boolean }).enabled).toBe(false);
  });

  it("deletes an item after confirmation", async () => {
    const { http } = renderAt("/");
    await screen.findByRole("switch", { name: "启用 cpu" });
    fireEvent.click(screen.getByRole("button", { name: "删除 cpu" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: /删\s*除/ }));
    await waitFor(() => {
      expect(http.calls.some((c) => c.method === "DELETE" && c.url === "/v1/collect-items/item-1")).toBe(true);
    });
  });

  it("jumps to metrics with agent_id and data_id", async () => {
    const { http } = renderAt("/");
    await screen.findByRole("switch", { name: "启用 cpu" });
    fireEvent.click(screen.getByRole("button", { name: "数据检索 cpu" }));
    expect(await screen.findByRole("heading", { name: "指标检索" })).toBeTruthy();
    await waitFor(() => {
      const call = http.calls.find((c) => c.url.startsWith("/api/v1/query_range"));
      expect(call).toBeTruthy();
      const decoded = decodeURIComponent(call!.url);
      expect(decoded).toContain('agent_id="a-1"');
      expect(decoded).toContain('item_id="item-1"');
    });
  });

  it("opens a create form with multi-select agents", async () => {
    const { http } = renderAt("/");
    fireEvent.click(await screen.findByRole("button", { name: "新建采集项" }));
    expect((await screen.findAllByText("目标 Agents")).length).toBeGreaterThan(0);
    expect(document.querySelector(".ant-select-multiple")).toBeTruthy();
    await waitFor(() => {
      expect(http.calls.some((c) => c.url === "/v1/agents")).toBe(true);
    });
  });
});

describe("logs page", () => {
  it("prefills filters from the query string", async () => {
    const { http } = renderAt("/logs?agent_id=a-1&data_id=item-2&data_type=logs");
    expect(await screen.findByDisplayValue("a-1")).toBeTruthy();
    expect(screen.getByDisplayValue("item-2")).toBeTruthy();
    await waitFor(() => {
      const call = http.calls.find((c) => c.url === "/v1/logs/search");
      expect(call?.body).toMatchObject({ data_type: "logs", agent_id: "a-1", data_id: "item-2", limit: 100 });
    });
  });
});

describe("metrics page", () => {
  it("filters query_range by agent_id and data_id from the URL", async () => {
    const { http } = renderAt("/metrics?agent_id=a-1&data_id=item-1");
    await screen.findByRole("heading", { name: "指标检索" });
    await waitFor(() => {
      const urls = http.calls
        .filter((c) => c.url.startsWith("/api/v1/query_range"))
        .map((c) => decodeURIComponent(c.url));
      expect(urls.some((u) => u.includes("query=cpu_usage{agent_id=\"a-1\",item_id=\"item-1\"}"))).toBe(true);
      expect(urls.some((u) => u.includes("query=mem_usage{agent_id=\"a-1\",item_id=\"item-1\"}"))).toBe(true);
    });
  });
});
