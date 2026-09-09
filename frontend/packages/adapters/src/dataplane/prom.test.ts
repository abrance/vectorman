import { describe, expect, it } from "vitest";
import type { HttpClient, HttpRequest, HttpResponse } from "@vectorman/primitives";
import { JsonErrorMapper } from "@vectorman/primitives";
import { PromQueryAdapter } from "./prom";

class FakeHttp implements HttpClient {
  last?: HttpRequest;
  constructor(private readonly body: unknown) {}
  async request<T>(req: HttpRequest): Promise<HttpResponse<T>> {
    this.last = req;
    return { status: 200, body: this.body as T };
  }
}

describe("PromQueryAdapter", () => {
  it("maps instant query url", async () => {
    const http = new FakeHttp({ status: "success", data: {} });
    const a = new PromQueryAdapter(http, new JsonErrorMapper());
    await a.queryInstant("up", "1");
    expect(http.last?.method).toBe("GET");
    expect(http.last?.url).toContain("/api/prom/api/v1/query?");
    expect(http.last?.url).toContain("query=up");
  });

  it("throws when status is error", async () => {
    const http = new FakeHttp({ status: "error", errorType: "unimplemented", error: "rate" });
    const a = new PromQueryAdapter(http, new JsonErrorMapper());
    await expect(a.queryInstant("rate(x)")).rejects.toEqual({
      code: "unimplemented",
      message: "rate",
    });
  });
});
