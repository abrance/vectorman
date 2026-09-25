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
import { ApmAdapter, DataplaneAdapter, EbpfAdapter, FetchHttpClient, type CollectItem } from "@vectorman/adapters";
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
    if (url.startsWith("/api/v1/query") && decodeURIComponent(url).includes("dataserver_")) {
      const expr = decodeURIComponent(url).match(/query=([^&]+)/)?.[1] ?? "";
      const value = expr.includes("apm_data_bytes")
        ? 1_048_576
        : expr.includes("ts_series_count")
          ? 42
          : expr.includes("details_deleted")
            ? 7
            : expr.includes("ingest_throttled")
              ? 2
              : 0;
      return {
        status: 200,
        body: {
          status: "success",
          data: { resultType: "vector", result: [{ metric: {}, value: [1_710_000_000_000, value] }] },
        } as T,
      };
    }
    if (url.startsWith("/api/v1/query_range") || url.startsWith("/api/v1/query")) {
      // APM 指标查询返回一小段矩阵，供 `/apm` 页渲染曲线与读数。
      if (url.includes("apm_service_")) {
        const isDuration = url.includes("duration_micros");
        const metric = isDuration
          ? { field: "p95" }
          : url.includes("errors_total")
            ? {}
            : { status: "ok" };
        return {
          status: 200,
          body: {
            status: "success",
            data: {
              resultType: "matrix",
              result: [
                {
                  metric,
                  values: [
                    [1_710_000_000_000, isDuration ? 12_000 : 3],
                    [1_710_000_060_000, isDuration ? 15_000 : 4],
                  ],
                },
              ],
            },
          } as T,
        };
      }
      return { status: 200, body: { status: "success", data: { resultType: "matrix", result: [] } } as T };
    }
    if (url === "/v1/logs/search") {
      return {
        status: 200,
        body: {
          records: [
            {
              id: "log-1",
              timestamp: 1_710_000_000_000_000,
              level: "error",
              message: "upstream timeout",
              labels: { trace_id: "b".repeat(32), service: "payment" },
            },
          ],
        } as T,
      };
    }
    if (url === "/v1/ebpf/capability") {
      return {
        status: 200,
        body: {
          reported: 2,
          agents: [
            {
              agent_id: "a-1",
              item_id: "item-ebpf",
              available: true,
              kernel_ok: true,
              btf_ok: true,
              capability_ok: true,
              kernel_release: "6.1.0",
              reason: "",
            },
            {
              agent_id: "a-2",
              item_id: "item-ebpf-2",
              available: false,
              kernel_ok: false,
              btf_ok: true,
              capability_ok: false,
              kernel_release: "5.4.0",
              reason: "eBPF preflight failed",
            },
          ],
        } as T,
      };
    }
    if (url === "/v1/ebpf/events/search") {
      return {
        status: 200,
        body: {
          records: [
            {
              id: "ebpf-1",
              timestamp: 1_710_000_000_000_000,
              level: "info",
              message: "java exec /usr/bin/java",
              labels: { event_type: "process_exec", process_name: "java", pid: "42" },
            },
          ],
        } as T,
      };
    }
    if (url === "/v1/edges/search") {
      const body = req.body as Record<string, unknown> | undefined;
      const onlyEbpf = body?.source === "ebpf";
      return {
        status: 200,
        body: (onlyEbpf
          ? {
              total: 1,
              edges: [
                {
                  bucket_ts: 1_710_000_000_000_000,
                  src_service: "order-api",
                  dst_service: "unknown-10.0.0.9",
                  span_kind: "",
                  calls: 6,
                  errors: 1,
                  duration_sum: 500,
                  duration_max: 300,
                  source: "ebpf",
                  agent_id: "a-1",
                  src_ip: "10.0.0.5",
                  dst_ip: "10.0.0.9",
                  dst_port: 8080,
                  protocol: "tcp",
                  connections: 6,
                  failures: 1,
                  bytes_sent: 1_048_576,
                  bytes_recv: 2_048,
                  duration_avg_micros: 83,
                  tcp_retrans: 2,
                },
              ],
            }
          : {
              total: 2,
              edges: [
            {
              bucket_ts: 1_710_000_000_000_000,
              src_service: "gateway",
              dst_service: "order-api",
              span_kind: "server",
              calls: 6,
              errors: 1,
              duration_sum: 6_000,
              duration_max: 3_000,
              source: "otlp",
              agent_id: "a-1",
              src_ip: "",
              dst_ip: "",
              dst_port: 0,
              protocol: "",
              connections: 6,
              failures: 1,
              bytes_sent: 0,
              bytes_recv: 0,
              duration_avg_micros: 1_000,
              tcp_retrans: 0,
            },
            {
              bucket_ts: 1_710_000_000_000_000,
              src_service: "order-api",
              dst_service: "db",
              span_kind: "server",
              calls: 2,
              errors: 0,
              duration_sum: 400,
              duration_max: 250,
              source: "otlp",
              agent_id: "a-1",
              src_ip: "",
              dst_ip: "",
              dst_port: 0,
              protocol: "",
              connections: 2,
              failures: 0,
              bytes_sent: 0,
              bytes_recv: 0,
              duration_avg_micros: 200,
              tcp_retrans: 0,
            },
          ],
            }) as T,
      };
    }
    if (url === "/v1/ts/stats") {
      return {
        status: 200,
        body: {
          series_count: 42,
          memory_used_bytes: 2_048,
          memory_budget_bytes: 4_096,
          wal_size_bytes: 1_024,
          retention_days: 30,
          retention_enforced: true,
          expired_segments_total: 3,
          future_skew_points_total: 0,
          background_errors_total: 0,
          degraded: false,
          last_background_error: null,
          sampled_at_ts: 1_710_000_000_000_000,
        } as T,
      };
    }
    if (url.startsWith("/v1/apm/service-aliases")) {
      if (req.method === "GET") {
        return {
          status: 200,
          body: {
            aliases: [
              {
                alias_id: "pod_prefix:order-api",
                match_kind: "pod_prefix",
                match_value: "order-api",
                service: "order-api",
                enabled: true,
                note: "订单服务",
                updated_ts: 1_710_000_000_000_000,
              },
            ],
          } as T,
        };
      }
      return { status: 200, body: { alias_id: "cidr:10.0.0.0/8" } as T };
    }
    if (url === "/v1/apm/services") {
      return {
        status: 200,
        body: {
          services: [
            {
              service: "order-api",
              instance_count: 1,
              last_seen_ts: 1_710_000_000_000_000,
              instances: [
                {
                  instance_id: "order-api-1",
                  pod_name: "order-api-7c9f",
                  node_name: "node-1",
                  host_ip: "10.0.0.9",
                  listen_port: 8080,
                  collector: "otlp",
                  first_seen_ts: 1_710_000_000_000_000,
                  last_seen_ts: 1_710_000_000_000_000,
                },
              ],
            },
          ],
        } as T,
      };
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
  const ebpf = new EbpfAdapter(http);
  const query = new MemoryQueryStore();
  return {
    http,
    query,
    ...render(
      <RuntimeProvider value={{ dataplane: adapter, apm, ebpf, query, notifier: new MemoryNotifier() }}>
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

describe("topology and apm pages", () => {
  it("renders the service topology, edge list and RED metrics", async () => {
    const topology = renderAt("/topology");
    // 拓扑图：两个服务节点进入 SVG，边上有调用次数。
    expect(await screen.findByRole("img", { name: "服务拓扑图" })).toBeTruthy();
    expect(screen.getByText("gateway")).toBeTruthy();
    expect(screen.getByText("order-api")).toBeTruthy();
    // 边列表页签展示聚合后的调用/错误/平均耗时。
    fireEvent.click(screen.getByText("边列表（2）"));
    expect(await screen.findByText(/16\.7%/)).toBeTruthy();
    expect(screen.getByText("1ms")).toBeTruthy();
    topology.unmount();

    const apm = renderAt("/apm");
    expect(await screen.findByText("请求量（每分钟，按状态）")).toBeTruthy();
    expect(screen.getByText("延迟（平均值与分位）")).toBeTruthy();
    expect(screen.getByText("错误数（每分钟）")).toBeTruthy();
    // 读数：错误率由两份查询相除得到（4/4 → 100%），分位标签取自 label field
    // （`field=p95` 由服务端命名规范给出）；查询是异步的，用 findBy 等待落定。
    expect(await screen.findByText(/错误率 100\.00%/)).toBeTruthy();
    expect(await screen.findByText(/p95 15ms/)).toBeTruthy();
    apm.unmount();
  });
});

describe("trace and log correlation", () => {
  it("jumps from a log record to its trace and back to service logs", async () => {
    const logs = renderAt("/logs");
    expect(await screen.findByText("upstream timeout")).toBeTruthy();
    const link = screen.getByRole("link", { name: "查看链路" });
    expect(link.getAttribute("href")).toBe(`/traces/${"b".repeat(32)}`);
    logs.unmount();

    // 详情页反向跳转：带上根服务与 trace 的时间窗。
    const detail = renderAt(`/traces/${"b".repeat(32)}`);
    const back = await screen.findByRole("link", { name: "查看该服务日志" });
    const href = back.getAttribute("href") ?? "";
    expect(href).toContain("/logs?service=payment");
    expect(href).toContain("from_ts=1710000000000000");
    detail.unmount();
  });
});

describe("ebpf page", () => {
  it("shows capability status, edge rows and unknown-service mapping entry", async () => {
    const ebpfView = renderAt("/ebpf");
    await waitFor(() => {
      expect(screen.getByText("eBPF 能力状态")).toBeTruthy();
    });
    await waitFor(() => {
      expect(screen.getByText(/1 个采集项不可用/)).toBeTruthy();
    });
    // 诊断文案里同时含失败项与原因（中间用 `·` 连接，因此按正则匹配整段）。
    expect(screen.getByText(/内核版本不足（5.4.0，需 ≥ 5.8）/)).toBeTruthy();
    expect(screen.getByText(/eBPF preflight failed/)).toBeTruthy();
    await waitFor(() => {
      expect(screen.getByText("unknown-10.0.0.9")).toBeTruthy();
    });
    // 边表里的 eBPF 独有字段：目标地址、字节、重传。
    expect(screen.getByText("10.0.0.9:8080")).toBeTruthy();
    expect(screen.getByText("1.0 MiB")).toBeTruthy();
    // 未识别服务可以一键跳到映射配置（带 CIDR 预填）。
    fireEvent.click(screen.getByRole("button", { name: "建立映射" }));
    await waitFor(() => {
      expect(screen.getByText("服务名映射")).toBeTruthy();
    });
    cleanup();
    ebpfView.unmount();
  });

  it("shows the raw-event hint and queries the events view", async () => {
    const eventsView = renderAt("/ebpf");
    await waitFor(() => {
      expect(screen.getByText("事件")).toBeTruthy();
    });
    fireEvent.click(screen.getByText("事件"));
    await waitFor(() => {
      expect(screen.getByText(/原始事件默认关闭/)).toBeTruthy();
    });
    cleanup();
    eventsView.unmount();
  });
});

describe("settings page", () => {
  it("shows storage stats, alias list and bulk import preview", async () => {
    const view = renderAt("/settings");
    // 存储卡片：时序统计 + 自监控读数（目录占用 1 MiB、序列数取自 self-metric）。
    expect(await screen.findByText("时序存储（聚合指标）")).toBeTruthy();
    expect(screen.getByText("APM 保留与淘汰（自监控读数）")).toBeTruthy();
    expect(await screen.findByText("1.0 MiB")).toBeTruthy();
    expect(screen.getByText("30 天")).toBeTruthy();

    // 映射页签：列表 + 批量导入预览（新增/覆盖计数）。
    fireEvent.click(screen.getByText("服务名映射"));
    expect(await screen.findByText("pod_prefix")).toBeTruthy();
    fireEvent.click(screen.getByText("批量导入"));
    const textarea = await screen.findByPlaceholderText(/pod_prefix,order-api,order-api/);
    fireEvent.change(textarea, {
      target: {
        value: [
          "pod_prefix,order-api,order-api",
          "cidr,10.0.0.0/8,legacy",
          "nope,x,y",
        ].join("\n"),
      },
    });
    expect(await screen.findByText("将新增 1 条")).toBeTruthy();
    expect(screen.getByText("将覆盖 1 条")).toBeTruthy();
    expect(screen.getByText("非法行 1")).toBeTruthy();
    view.unmount();
  });

  it("prefills the alias form from the topology unknown-node entry", async () => {
    const view = renderAt("/settings/service-aliases?match_kind=cidr&match_value=10.0.0.9");
    const input = await screen.findByDisplayValue("10.0.0.9/32");
    expect(input).toBeTruthy();
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
