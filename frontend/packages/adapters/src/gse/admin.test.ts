import { describe, expect, it } from "vitest";
import type { HttpClient, HttpRequest, HttpResponse } from "@vectorman/primitives";
import { GseAdminAdapter } from "./admin";

class FakeHttp implements HttpClient {
  last?: HttpRequest;
  constructor(private readonly body: unknown = {}) {}
  async request<T>(req: HttpRequest): Promise<HttpResponse<T>> {
    this.last = req;
    return { status: 200, body: this.body as T };
  }
}

describe("GseAdminAdapter", () => {
  it("lists hosts with GET /api/gse/hosts", async () => {
    const http = new FakeHttp([]);
    const a = new GseAdminAdapter(http);
    await a.listHosts();
    expect(http.last).toMatchObject({ method: "GET", url: "/api/gse/hosts" });
  });

  it("upserts agent with POST body", async () => {
    const http = new FakeHttp({ agent_id: "a1" });
    const a = new GseAdminAdapter(http);
    const body = { agent_id: "a1", host_id: "h1", token: "t" };
    await a.upsertAgent(body);
    expect(http.last).toMatchObject({ method: "POST", url: "/api/gse/agents", body });
  });

  it("encodes path ids", async () => {
    const http = new FakeHttp({});
    const a = new GseAdminAdapter(http);
    await a.getAgent("a/b");
    expect(http.last?.url).toBe("/api/gse/agents/a%2Fb");
  });

  it("deletes host", async () => {
    const http = new FakeHttp({ deleted: "h1" });
    const a = new GseAdminAdapter(http);
    await a.deleteHost("h1");
    expect(http.last).toMatchObject({ method: "DELETE", url: "/api/gse/hosts/h1" });
  });
});
