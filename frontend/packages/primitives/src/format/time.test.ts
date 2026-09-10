import { describe, expect, it } from "vitest";
import { formatTimestamp } from "./time";

describe("formatTimestamp", () => {
  it("formats microsecond epoch in local time", () => {
    const date = new Date("2026-09-10T08:09:10");
    const micros = date.getTime() * 1000;
    expect(formatTimestamp(String(micros))).toBe("2026-09-10 08:09:10");
  });

  it("ignores the ledger_stamp sequence suffix", () => {
    const date = new Date("2026-01-02T03:04:05");
    const micros = date.getTime() * 1000;
    expect(formatTimestamp(`${micros}-7`)).toBe("2026-01-02 03:04:05");
  });

  it("returns a placeholder for empty values", () => {
    expect(formatTimestamp(undefined)).toBe("-");
    expect(formatTimestamp(null)).toBe("-");
    expect(formatTimestamp("")).toBe("-");
  });

  it("returns the original value when it is not a timestamp", () => {
    expect(formatTimestamp("not-a-time")).toBe("not-a-time");
  });
});
