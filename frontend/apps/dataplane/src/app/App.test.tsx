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
import { DataplaneAdapter, FetchHttpClient, type CollectItem } from "@vectorman/adapters";
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
  return {
    http,
    ...render(
      <RuntimeProvider value={{ dataplane: adapter, query: new MemoryQueryStore(), notifier: new MemoryNotifier() }}>
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

describe("dataplane shell", () => {
  it("routes to collect, metrics and logs pages", async () => {
    const collect = renderAt("/");
    expect(await screen.findByText("新建采集项")).toBeTruthy();
    collect.unmount();

    const metrics = renderAt("/metrics");
    expect(await screen.findByText("cpu_usage")).toBeTruthy();
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
    renderAt("/");
    await screen.findByRole("switch", { name: "启用 cpu" });
    fireEvent.click(screen.getByRole("button", { name: "数据检索 cpu" }));
    expect(await screen.findByText("data_id=item-1")).toBeTruthy();
    expect((screen.getByPlaceholderText("agent_id（可选）") as HTMLInputElement).value).toBe("a-1");
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
