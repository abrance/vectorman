import { describe, expect, it } from "vitest";
import type { HttpClient, HttpRequest, HttpResponse } from "@vectorman/primitives";
import { SqlHttpAdapter } from "./sql";

class FakeHttp implements HttpClient {
  last?: HttpRequest;
  async request<T>(req: HttpRequest): Promise<HttpResponse<T>> {
    this.last = req;
    return { status: 200, body: { columns: [], rows: [] } as T };
  }
}

describe("SqlHttpAdapter", () => {
  it("posts sql body to /api/sql/v1/sql", async () => {
    const http = new FakeHttp();
    const a = new SqlHttpAdapter(http);
    await a.execute("SELECT 1", [1]);
    expect(http.last).toMatchObject({
      method: "POST",
      url: "/api/sql/v1/sql",
      body: { sql: "SELECT 1", params: [1] },
    });
  });
});
