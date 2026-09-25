import { describe, expect, it } from "vitest";
import type { HttpClient, HttpRequest, HttpResponse } from "@vectorman/primitives";
import { EbpfAdapter } from "./ebpf";

class FakeHttp implements HttpClient {
  readonly calls: HttpRequest[] = [];

  constructor(private readonly handler: (req: HttpRequest) => HttpResponse<unknown>) {}

  async request<T>(req: HttpRequest): Promise<HttpResponse<T>> {
    this.calls.push(req);
    return this.handler(req) as HttpResponse<T>;
  }
}

describe("EbpfAdapter", () => {
  it("事件检索固定走 /v1/ebpf/events/search，且不传 data_type", async () => {
    const http = new FakeHttp(() => ({
      status: 200,
      body: {
        records: [
          {
            id: "ebpf-1",
            timestamp: 1_710_000_000_000_000,
            level: "info",
            message: "java exec",
            labels: { event_type: "process_exec", process_name: "java" },
          },
        ],
      },
    }));
    const adapter = new EbpfAdapter(http);
    const records = await adapter.searchEvents({
      event_type: "process_exec",
      labels: { process_name: "java" },
      limit: 20,
    });
    expect(records).toHaveLength(1);
    expect(records[0].labels.process_name).toBe("java");
    expect(http.calls[0]).toMatchObject({
      method: "POST",
      url: "/v1/ebpf/events/search",
      body: { event_type: "process_exec", labels: { process_name: "java" }, limit: 20 },
    });
    expect((http.calls[0].body as Record<string, unknown>).data_type).toBeUndefined();
  });

  it("能力状态缺省字段归一为空数组与 0", async () => {
    const http = new FakeHttp(() => ({ status: 200, body: {} }));
    const adapter = new EbpfAdapter(http);
    expect(await adapter.capability()).toEqual({ agents: [], reported: 0 });

    const http2 = new FakeHttp(() => ({
      status: 200,
      body: {
        reported: 1,
        agents: [
          {
            agent_id: "a-1",
            item_id: "item-ebpf",
            available: false,
            kernel_ok: false,
            btf_ok: true,
            capability_ok: true,
            kernel_release: "5.4.0",
            reason: "kernel >= 5.8",
          },
        ],
      },
    }));
    const report = await new EbpfAdapter(http2).capability();
    expect(report.reported).toBe(1);
    expect(report.agents[0].agent_id).toBe("a-1");
    expect(http2.calls[0]).toMatchObject({ method: "GET", url: "/v1/ebpf/capability" });
  });
});
