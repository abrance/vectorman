import { describe, expect, it } from "vitest";
import { MemoryNotifier } from "./notifier";

describe("MemoryNotifier", () => {
  it("emits three levels and uses error.message", () => {
    const n = new MemoryNotifier();
    const got: string[] = [];
    n.subscribe((notice) => {
      got.push(`${notice.level}:${notice.message}`);
    });
    n.success("ok");
    n.warning("w");
    n.error({ code: "x", message: "boom" });
    expect(got).toEqual(["success:ok", "warning:w", "error:boom"]);
  });

  it("does not throw without subscribers", () => {
    const n = new MemoryNotifier();
    n.success("ok");
  });

  it("drops oldest when over 50", () => {
    const n = new MemoryNotifier();
    const last: string[] = [];
    n.subscribe((notice) => {
      last.push(notice.id);
    });
    for (let i = 0; i < 51; i += 1) {
      n.success(String(i));
    }
    expect(last).toHaveLength(51);
  });
});
