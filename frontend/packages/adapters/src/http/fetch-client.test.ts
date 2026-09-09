import { describe, expect, it, vi } from "vitest";
import { JsonErrorMapper, MemoryAuthSession } from "@vectorman/primitives";
import { FetchHttpClient } from "./fetch-client";

describe("FetchHttpClient", () => {
  it("attaches Bearer and trace header", async () => {
    const session = new MemoryAuthSession();
    session.set({ token: "abc" });
    const fetchImpl = vi.fn(async () => new Response(JSON.stringify({ ok: true }), { status: 200 }));
    const client = new FetchHttpClient(session, new JsonErrorMapper(), fetchImpl as unknown as typeof fetch);
    const res = await client.request({ method: "GET", url: "/x", context: { timeoutMs: 1000, traceId: "tid-1" } });
    expect(res.body).toEqual({ ok: true });
    const init = fetchImpl.mock.calls[0][1] as RequestInit;
    const headers = init.headers as Record<string, string>;
    expect(headers.Authorization).toBe("Bearer abc");
    expect(headers["X-Trace-Id"]).toBe("tid-1");
  });

  it("omits Authorization when session empty", async () => {
    const session = new MemoryAuthSession();
    const fetchImpl = vi.fn(async () => new Response("{}", { status: 200 }));
    const client = new FetchHttpClient(session, new JsonErrorMapper(), fetchImpl as unknown as typeof fetch);
    await client.request({ method: "GET", url: "/x" });
    const headers = (fetchImpl.mock.calls[0][1] as RequestInit).headers as Record<string, string>;
    expect(headers.Authorization).toBeUndefined();
  });

  it("calls global fetch with Window as this", async () => {
    const session = new MemoryAuthSession();
    const inner = vi.fn(async () => new Response("{}", { status: 200 }));
    const previous = globalThis.fetch;
    globalThis.fetch = function (this: unknown, input: RequestInfo | URL, init?: RequestInit) {
      if (this !== globalThis) {
        throw new TypeError("Failed to execute 'fetch' on 'Window': Illegal invocation");
      }
      return inner(input, init);
    } as typeof fetch;
    try {
      const client = new FetchHttpClient(session, new JsonErrorMapper());
      await client.request({ method: "GET", url: "/x" });
      expect(inner).toHaveBeenCalled();
    } finally {
      globalThis.fetch = previous;
    }
  });

  it("rejects mapped backend error", async () => {
    const session = new MemoryAuthSession();
    const fetchImpl = vi.fn(
      async () => new Response(JSON.stringify({ code: "not_found", error: "gone" }), { status: 404 }),
    );
    const client = new FetchHttpClient(session, new JsonErrorMapper(), fetchImpl as unknown as typeof fetch);
    await expect(client.request({ method: "GET", url: "/x" })).rejects.toEqual({
      code: "not_found",
      message: "gone",
    });
  });
});
