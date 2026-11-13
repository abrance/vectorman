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
    // 认不出的英文原文回落错误码通用句（见 error-text），不直出 message。
    expect(got).toEqual(["success:ok", "warning:w", "error:操作失败，请稍后重试"]);
  });

  it("translates the server's raw message before it reaches the toast", () => {
    const n = new MemoryNotifier();
    const got: string[] = [];
    n.subscribe((notice) => got.push(notice.message));
    n.error({
      code: "invalid_argument",
      message: "unknown field `bogus_console_probe`, expected one of `log_level` at line 1 column 417",
    });
    n.error({ code: "unavailable", message: "gse_admin_url is not configured" });
    expect(got[0]).toBe("配置里有无法识别的字段：bogus_console_probe（可选字段见「参数」页说明）");
    expect(got[1]).toContain("数据面未配置");
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
