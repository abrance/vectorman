import { describe, expect, it } from "vitest";
import type { HttpClient, HttpRequest, HttpResponse } from "@vectorman/primitives";
import { ApmAdapter, type AliasInput } from "./apm";

class FakeHttp implements HttpClient {
  readonly calls: HttpRequest[] = [];
  private readonly replies: unknown[];

  constructor(...replies: unknown[]) {
    this.replies = replies;
  }

  async request<T>(req: HttpRequest): Promise<HttpResponse<T>> {
    this.calls.push(req);
    const body = this.replies.length > 0 ? this.replies.shift() : null;
    return { status: 200, body: body as T };
  }
}

const aliasInput: AliasInput = {
  match_kind: "pod_prefix",
  match_value: "order-api",
  service: "order-api",
};

describe("ApmAdapter", () => {
  it("posts trace search filters as-is", async () => {
    const http = new FakeHttp({ total: 1, traces: [] });
    const a = new ApmAdapter(http);
    const page = await a.searchTraces({
      service: "order-api",
      min_duration_micros: 500_000,
      sort: "duration_micros",
      order: "desc",
      limit: 20,
    });
    expect(page.total).toBe(1);
    expect(http.calls[0]).toMatchObject({
      method: "POST",
      url: "/v1/traces/search",
      body: { service: "order-api", min_duration_micros: 500_000, sort: "duration_micros", order: "desc", limit: 20 },
    });
  });

  it("gets a trace detail and unwraps services", async () => {
    const http = new FakeHttp(
      { summary: { trace_id: "t" }, spans: [], partial: true },
      { services: [{ service: "order-api", instance_count: 1, last_seen_ts: 1, instances: [] }] },
    );
    const a = new ApmAdapter(http);
    const detail = await a.getTrace("4bf92f3577b34da6a3ce929d0e0e4736");
    expect(detail.partial).toBe(true);
    expect(http.calls[0]).toMatchObject({
      method: "GET",
      url: "/v1/traces/4bf92f3577b34da6a3ce929d0e0e4736",
    });
    expect((await a.listServices())[0].service).toBe("order-api");
    expect(http.calls[1]).toMatchObject({ method: "GET", url: "/v1/apm/services" });
  });

  it("builds alias urls with encoded ids and filters", async () => {
    const http = new FakeHttp(
      { aliases: [] },
      { alias_id: "cidr:10.0.0.0/8" },
      { alias_id: "cidr:10.0.0.0/8" },
      null,
    );
    const a = new ApmAdapter(http);
    await a.listAliases({ match_kind: "cidr", enabled: true });
    await a.createAlias({ match_kind: "cidr", match_value: "10.0.0.0/8", service: "legacy" });
    await a.updateAlias("cidr:10.0.0.0/8", { ...aliasInput, enabled: false });
    await a.deleteAlias("cidr:10.0.0.0/8");

    expect(http.calls[0]).toMatchObject({
      method: "GET",
      url: "/v1/apm/service-aliases?match_kind=cidr&enabled=true",
    });
    expect(http.calls[1]).toMatchObject({ method: "POST", url: "/v1/apm/service-aliases" });
    // `:` 与 `/` 必须转义，否则路由会把 alias_id 切断。
    expect(http.calls[2]).toMatchObject({
      method: "PUT",
      url: "/v1/apm/service-aliases/cidr%3A10.0.0.0%2F8",
    });
    expect(http.calls[3]).toMatchObject({
      method: "DELETE",
      url: "/v1/apm/service-aliases/cidr%3A10.0.0.0%2F8",
    });
  });

  it("posts edge filters and reads storage stats", async () => {
    const http = new FakeHttp({ total: 0, edges: [] }, { series_count: 3 });
    const a = new ApmAdapter(http);
    await a.searchEdges({ src_service: "gateway", source: "otlp", min_requests: 10 });
    const stats = await a.storageStats();
    expect(stats.series_count).toBe(3);
    expect(http.calls[0]).toMatchObject({
      method: "POST",
      url: "/v1/edges/search",
      body: { src_service: "gateway", source: "otlp", min_requests: 10 },
    });
    expect(http.calls[1]).toMatchObject({ method: "GET", url: "/v1/ts/stats" });
  });
});
