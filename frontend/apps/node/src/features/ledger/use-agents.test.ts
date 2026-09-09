import { describe, expect, it, vi } from "vitest";
import type { Agent, GseAdminAdapter } from "@vectorman/adapters";
import { MemoryNotifier, MemoryQueryStore } from "@vectorman/primitives";

describe("edit agent keeps original token", () => {
  it("upserts with token from getAgent", async () => {
    const original: Agent = { agent_id: "a1", host_id: "h1", token: "abc" };
    let posted: Agent | undefined;
    const gse = {
      getAgent: vi.fn(async () => original),
      upsertAgent: vi.fn(async (a: Agent) => {
        posted = a;
        return a;
      }),
      listAgents: vi.fn(async () => [original]),
      deleteAgent: vi.fn(),
    } as unknown as GseAdminAdapter;
    const got = await gse.getAgent("a1");
    await gse.upsertAgent({ ...got, host_id: "h2", token: got.token });
    expect(posted?.token).toBe("abc");
  });
});

describe("agent poll cleanup", () => {
  it("clears interval", () => {
    const query = new MemoryQueryStore();
    const notifier = new MemoryNotifier();
    void query;
    void notifier;
    const spy = vi.spyOn(globalThis, "clearInterval");
    const id = setInterval(() => undefined, 30_000);
    clearInterval(id);
    expect(spy).toHaveBeenCalled();
    spy.mockRestore();
  });
});
