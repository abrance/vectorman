import { describe, expect, it } from "vitest";
import type { SpanDetail } from "@vectorman/adapters";
import { formatDuration, layeredTopology, waterfallRows } from "./layout";

function span(partial: Partial<SpanDetail> & { span_id: string }): SpanDetail {
  return {
    record_id: partial.record_id ?? `t:${partial.span_id}`,
    trace_id: partial.trace_id ?? "t",
    parent_span_id: partial.parent_span_id ?? "",
    name: partial.name ?? "op",
    kind: partial.kind ?? "server",
    service: partial.service ?? "svc",
    status_code: partial.status_code ?? "ok",
    start_unix_nano: partial.start_unix_nano ?? 0,
    end_unix_nano: partial.end_unix_nano ?? 1_000,
    duration_micros: partial.duration_micros ?? 1,
    span_id: partial.span_id,
  };
}

describe("waterfallRows", () => {
  it("computes depth from parent chain and orders by start time", () => {
    const rows = waterfallRows([
      span({ span_id: "b", parent_span_id: "a", start_unix_nano: 200, end_unix_nano: 600 }),
      span({ span_id: "a", start_unix_nano: 100, end_unix_nano: 800 }),
      span({ span_id: "c", parent_span_id: "b", start_unix_nano: 300, end_unix_nano: 400 }),
    ]);
    expect(rows.map((r) => r.span.span_id)).toEqual(["a", "b", "c"]);
    expect(rows.map((r) => r.depth)).toEqual([0, 1, 2]);
    expect(rows.map((r) => r.row)).toEqual([0, 1, 2]);
  });

  it("treats orphan and self-parented spans as roots without recursing forever", () => {
    const rows = waterfallRows([
      span({ span_id: "orphan", parent_span_id: "missing" }),
      span({ span_id: "self", parent_span_id: "self" }),
    ]);
    expect(rows.map((r) => r.depth)).toEqual([0, 0]);
  });

  it("maps offsets and widths to the trace window", () => {
    const rows = waterfallRows([
      span({ span_id: "root", start_unix_nano: 0, end_unix_nano: 1_000 }),
      span({ span_id: "child", parent_span_id: "root", start_unix_nano: 250, end_unix_nano: 750 }),
    ]);
    expect(rows[0].offsetRatio).toBe(0);
    expect(rows[0].widthRatio).toBe(1);
    expect(rows[1].offsetRatio).toBeCloseTo(0.25);
    expect(rows[1].widthRatio).toBeCloseTo(0.5);
  });

  it("returns nothing for an empty trace", () => {
    expect(waterfallRows([])).toEqual([]);
  });
});

describe("layeredTopology", () => {
  it("lays out layers by longest upstream chain and sorts within a layer", () => {
    const layout = layeredTopology({
      services: ["gateway", "order-api", "payment", "db"],
      edges: [
        { src: "gateway", dst: "order-api", value: 3 },
        { src: "order-api", dst: "payment", value: 2 },
        { src: "payment", dst: "db", value: 1 },
      ],
    });
    const layerOf = (s: string) => layout.nodes.find((n) => n.service === s)!.layer;
    expect(layerOf("gateway")).toBe(0);
    expect(layerOf("order-api")).toBe(1);
    expect(layerOf("payment")).toBe(2);
    expect(layerOf("db")).toBe(3);
    // 同一层内按服务名字典序排 slot。
    expect(layout.nodes.find((n) => n.service === "gateway")!.slot).toBe(0);
    expect(layout.edges).toHaveLength(3);
    expect(layout.width).toBeGreaterThan(0);
  });

  it("keeps coordinates deterministic for the same input", () => {
    const input = {
      services: ["b", "a", "c"],
      edges: [{ src: "a", dst: "c", value: 1 }],
    };
    expect(layeredTopology(input)).toEqual(layeredTopology(input));
    const nodes = layeredTopology(input).nodes;
    // 同层两个节点（a、b）坐标固定来自字典序，不受输入顺序影响。
    expect(nodes.find((n) => n.service === "a")!.y).toBeLessThan(
      nodes.find((n) => n.service === "b")!.y,
    );
  });

  it("ignores edges that reference unknown services", () => {
    const layout = layeredTopology({
      services: ["a"],
      edges: [{ src: "a", dst: "ghost", value: 1 }],
    });
    expect(layout.edges).toHaveLength(0);
    expect(layout.nodes).toHaveLength(1);
  });
});

describe("formatDuration", () => {
  it("formats micro, milli and seconds", () => {
    expect(formatDuration(0)).toBe("0µs");
    expect(formatDuration(120)).toBe("120µs");
    expect(formatDuration(1_250)).toBe("1.25ms");
    expect(formatDuration(120_000)).toBe("120ms");
    expect(formatDuration(3_400_000)).toBe("3.4s");
  });
});
