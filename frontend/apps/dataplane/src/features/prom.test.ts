import { describe, expect, it } from "vitest";
import {
  axisTimeLabel,
  discoveryExpr,
  latestValue,
  metricExpr,
  metricNames,
  promSeries,
  seriesLabel,
} from "./prom";

describe("metricExpr", () => {
  it("returns the measurement when filters are empty", () => {
    expect(metricExpr("cpu_usage", {})).toBe("cpu_usage");
    expect(metricExpr("cpu_usage", { agentId: "  ", itemId: "" })).toBe("cpu_usage");
  });

  it("puts agent_id and item_id into the selector", () => {
    expect(metricExpr("cpu_usage", { agentId: "a-1", itemId: "item-1" })).toBe(
      'cpu_usage{agent_id="a-1",item_id="item-1"}',
    );
  });

  it("escapes quotes in matcher values", () => {
    expect(metricExpr("cpu_usage", { agentId: 'a"1' })).toBe('cpu_usage{agent_id="a\\"1"}');
  });
});

describe("discoveryExpr", () => {
  it("builds a count-by-name selector from filters", () => {
    expect(discoveryExpr({ agentId: "a-1", itemId: "item-1" })).toBe(
      'count by (__name__) ({agent_id="a-1",item_id="item-1"})',
    );
  });

  it("returns empty string with no filters to avoid a full scan", () => {
    expect(discoveryExpr({})).toBe("");
    expect(discoveryExpr({ agentId: "  ", itemId: "" })).toBe("");
  });
});

describe("metricNames", () => {
  it("pulls distinct sorted __name__ from a vector result", () => {
    const data = {
      resultType: "vector",
      result: [
        { metric: { __name__: "mem_usage" }, value: [1, 1] },
        { metric: { __name__: "cpu_usage" }, value: [1, 1] },
        { metric: { __name__: "cpu_usage" }, value: [2, 1] },
      ],
    };
    expect(metricNames(data)).toEqual(["cpu_usage", "mem_usage"]);
  });

  it("accepts the envelope and tolerates junk", () => {
    const inner = { resultType: "vector", result: [{ metric: { __name__: "x" }, value: [1, 1] }] };
    expect(metricNames({ status: "success", data: inner })).toEqual(["x"]);
    expect(metricNames(undefined)).toEqual([]);
    expect(metricNames({ status: "success" })).toEqual([]);
  });
});

describe("seriesLabel", () => {
  it("prefers agent and host and drops item_id", () => {
    expect(
      seriesLabel({ __name__: "cpu_usage", agent_id: "gs", host_id: "h-2", item_id: "item-1" }, 0),
    ).toBe("gs · h-2");
  });

  it("falls back to series-n", () => {
    expect(seriesLabel({}, 3)).toBe("series-3");
  });
});

describe("promSeries", () => {
  it("coerces numeric strings and ignores junk", () => {
    const series = promSeries({
      resultType: "matrix",
      result: [
        {
          metric: { agent_id: "gs" },
          values: [
            [1, "12.5"],
            [2, "not-a-number"],
          ],
        },
      ],
    });
    expect(series).toEqual([{ label: "gs", points: [[1, 12.5]] }]);
  });

  it("accepts the envelope as well as the inner data段", () => {
    const inner = {
      resultType: "matrix",
      result: [{ metric: { status: "ok" }, values: [[1_710_000_000_000, 3]] }],
    };
    // 适配器返回的是信封，调用方也可能直接传内层 data；两种都要能出序列，
    // 否则曲线会静默为空（看起来像「没有采集数据」）。
    expect(promSeries({ status: "success", data: inner })).toHaveLength(1);
    expect(promSeries(inner)).toHaveLength(1);
    expect(promSeries(undefined)).toEqual([]);
    expect(promSeries({ status: "success" })).toEqual([]);
  });
});

describe("latestValue", () => {
  it("picks the newest sample across series", () => {
    expect(
      latestValue([
        { label: "a", points: [[1, 10], [3, 30]] },
        { label: "b", points: [[4, 40]] },
      ]),
    ).toBe(40);
  });
});

describe("axisTimeLabel", () => {
  it("shows clock time for short windows", () => {
    const label = axisTimeLabel(Date.UTC(2026, 8, 17, 8, 5) / 1000, 3600);
    expect(label).toMatch(/^\d{2}:\d{2}$/);
  });
});
