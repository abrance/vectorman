import { describe, expect, it } from "vitest";
import { MemoryAuthSession } from "./session";

describe("MemoryAuthSession", () => {
  it("starts empty, set then get, clear returns null", () => {
    const s = new MemoryAuthSession();
    expect(s.get()).toBeNull();
    s.set({ token: "t1", subject: "a" });
    expect(s.get()).toEqual({ token: "t1", subject: "a" });
    s.clear();
    expect(s.get()).toBeNull();
  });

  it("isolates instances", () => {
    const a = new MemoryAuthSession();
    const b = new MemoryAuthSession();
    a.set({ token: "a" });
    expect(b.get()).toBeNull();
  });
});
