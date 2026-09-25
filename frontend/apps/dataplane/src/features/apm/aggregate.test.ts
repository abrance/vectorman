import { describe, expect, it } from "vitest";
import type { EdgeRow } from "@vectorman/adapters";
import { aggregateEdges, edgeStyle, topologyInput } from "./aggregate";

function edge(partial: Partial<EdgeRow> & { src_service: string; dst_service: string }): EdgeRow {
  return {
    bucket_ts: partial.bucket_ts ?? 60_000_000,
    src_service: partial.src_service,
    dst_service: partial.dst_service,
    span_kind: partial.span_kind ?? "server",
    calls: partial.calls ?? 1,
    errors: partial.errors ?? 0,
    duration_sum: partial.duration_sum ?? 100,
    duration_max: partial.duration_max ?? 100,
    source: partial.source ?? "otlp",
    agent_id: partial.agent_id ?? "agent-1",
    src_ip: partial.src_ip ?? "",
    dst_ip: partial.dst_ip ?? "",
    dst_port: partial.dst_port ?? 0,
    protocol: partial.protocol ?? "",
    connections: partial.connections ?? 0,
    failures: partial.failures ?? 0,
    bytes_sent: partial.bytes_sent ?? 0,
    bytes_recv: partial.bytes_recv ?? 0,
    duration_avg_micros: partial.duration_avg_micros ?? 0,
    tcp_retrans: partial.tcp_retrans ?? 0,
  };
}

describe("aggregateEdges", () => {
  it("merges buckets and derives avg/max/error rate", () => {
    const summaries = aggregateEdges([
      edge({ src_service: "gateway", dst_service: "order-api", calls: 2, errors: 0, duration_sum: 200, duration_max: 150 }),
      edge({ src_service: "gateway", dst_service: "order-api", bucket_ts: 120_000_000, calls: 2, errors: 1, duration_sum: 400, duration_max: 300 }),
      edge({ src_service: "order-api", dst_service: "db", calls: 5, errors: 0 }),
    ]);
    const gateway = summaries.find((s) => s.src === "gateway")!;
    expect(gateway.calls).toBe(4);
    expect(gateway.errors).toBe(1);
    expect(gateway.avgDurationMicros).toBe(150);
    expect(gateway.maxDurationMicros).toBe(300);
    expect(gateway.errorRate).toBeCloseTo(0.25);
    expect(gateway.bucketCount).toBe(2);
    // 调用量降序：db 边 5 次排最前。
    expect(summaries[0].dst).toBe("db");
  });

  it("keeps multiple sources and agents", () => {
    const summaries = aggregateEdges([
      edge({ src_service: "a", dst_service: "b", source: "otlp", agent_id: "agent-2" }),
      edge({ src_service: "a", dst_service: "b", source: "ebpf", agent_id: "agent-1" }),
    ]);
    expect(summaries[0].sources).toEqual(["ebpf", "otlp"]);
    expect(summaries[0].agentIds).toEqual(["agent-1", "agent-2"]);
  });

  it("returns nothing for no rows", () => {
    expect(aggregateEdges([])).toEqual([]);
  });
});

describe("topologyInput", () => {
  it("collects unique services and weighted edges", () => {
    const input = topologyInput(
      aggregateEdges([
        edge({ src_service: "gateway", dst_service: "order-api", calls: 3 }),
        edge({ src_service: "order-api", dst_service: "db", calls: 1 }),
      ]),
    );
    expect([...input.services].sort()).toEqual(["db", "gateway", "order-api"]);
    expect(input.edges).toHaveLength(2);
    expect(input.edges[0].value).toBe(3);
  });
});

describe("edgeStyle", () => {
  it("scales width by calls and colors by error rate", () => {
    const healthy = { calls: 10, errorRate: 0 } as never;
    const broken = { calls: 5, errorRate: 1 } as never;
    const wide = edgeStyle(healthy, 10);
    const narrow = edgeStyle(broken, 10);
    expect(wide.width).toBeGreaterThan(narrow.width);
    expect(wide.color).toBe("hsl(170, 65%, 42%)");
    expect(narrow.color).toBe("hsl(0, 65%, 42%)");
  });
});
