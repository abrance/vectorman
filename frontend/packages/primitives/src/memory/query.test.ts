import { describe, expect, it } from "vitest";
import { MemoryQueryStore } from "./query";

describe("MemoryQueryStore", () => {
  it("unknown key is idle", () => {
    const store = new MemoryQueryStore();
    expect(store.get("k")).toEqual({ status: "idle" });
  });

  it("migrates loading success error and notifies", () => {
    const store = new MemoryQueryStore();
    let n = 0;
    const unsub = store.subscribe("k", () => {
      n += 1;
    });
    store.setLoading("k");
    expect(store.get("k").status).toBe("loading");
    store.setSuccess("k", [1]);
    expect(store.get("k")).toEqual({ status: "success", data: [1] });
    store.setError("k", { code: "query_failed", message: "x" });
    expect(store.get("k").status).toBe("error");
    expect(store.get("k").data).toEqual([1]);
    expect(n).toBe(3);
    unsub();
    store.setLoading("k");
    expect(n).toBe(3);
  });
});
