import { describe, expect, it } from "vitest";
import type { HttpClient, HttpRequest, HttpResponse } from "@vectorman/primitives";
import { DataplaneAdapter, type CollectItemInput } from "./ingest";

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

const input: CollectItemInput = {
  agent_ids: ["a-1"],
  name: "cpu",
  kind: "metrics_host",
  enabled: true,
  collector: { interval_secs: 15 },
  storage: { retention_days: 1 },
};

describe("DataplaneAdapter", () => {
  it("reads collect items with optional agent filter", async () => {
    const http = new FakeHttp([]);
    const a = new DataplaneAdapter(http);
    await a.listCollectItems();
    await a.listCollectItems("a 1");
    expect(http.calls[0]).toMatchObject({ method: "GET", url: "/v1/collect-items" });
    expect(http.calls[1]).toMatchObject({ method: "GET", url: "/v1/collect-items?agent_id=a%201" });
  });

  it("creates, updates and deletes collect items", async () => {
    const http = new FakeHttp({}, {}, null);
    const a = new DataplaneAdapter(http);
    await a.createCollectItem(input);
    await a.updateCollectItem("item a", { ...input, enabled: false });
    await a.deleteCollectItem("item a");
    expect(http.calls[0]).toMatchObject({ method: "POST", url: "/v1/collect-items", body: input });
    expect(http.calls[1]).toMatchObject({
      method: "PUT",
      url: "/v1/collect-items/item%20a",
      body: { ...input, enabled: false },
    });
    expect(http.calls[2]).toMatchObject({
      method: "DELETE",
      url: "/v1/collect-items/item%20a",
    });
  });

  it("unwraps streams, agents and log records", async () => {
    const http = new FakeHttp(
      { streams: [{ agent_id: "a-1", data_type: "logs", data_id: "i-1", last_seen_micros: 1, accepted: 2 }] },
      [{ agent_id: "a-1", host_id: "h-1", token: "t" }],
      { records: [{ id: "r-1", timestamp: 1, level: "info", message: "hi", labels: {} }] },
    );
    const a = new DataplaneAdapter(http);
    expect((await a.listStreams())[0].accepted).toBe(2);
    expect((await a.listAgents())[0].agent_id).toBe("a-1");
    expect((await a.searchLogs({ data_type: "logs", limit: 50 }))[0].message).toBe("hi");
    expect(http.calls[2]).toMatchObject({
      method: "POST",
      url: "/v1/logs/search",
      body: { data_type: "logs", limit: 50 },
    });
  });

  it("builds prom query urls", async () => {
    const http = new FakeHttp({ status: "success" }, { status: "success" });
    const a = new DataplaneAdapter(http);
    await a.queryRange("cpu_usage{agent_id=\"a-1\"}", "1", "2", "15");
    await a.queryInstant("up", "3");
    expect(http.calls[0].url).toBe(
      "/api/v1/query_range?query=cpu_usage%7Bagent_id%3D%22a-1%22%7D&start=1&end=2&step=15",
    );
    expect(http.calls[1].url).toBe("/api/v1/query?query=up&time=3");
  });
});
