import { describe, expect, it } from "vitest";
import { axisTimeLabel, latestValue, metricExpr, promSeries, seriesLabel } from "./prom";

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
