import { describe, expect, it } from "vitest";
import type { AppError } from "./error";
import { toUserMessage } from "./error-text";

const err = (code: string, message: string): AppError => ({ code, message });

describe("toUserMessage：服务端原文不直出", () => {
  it("serde 未知字段 → 中文点名多余字段，且不带 line/column 噪声", () => {
    // 这条 message 是从 400 响应里实取的原样文本（P1-13 的复现输入）。
    const raw =
      "invalid JSON body: Failed to deserialize the JSON body into the target type: " +
      "params.bogus_console_probe: unknown field `bogus_console_probe`, expected one of " +
      "`heartbeat_interval_secs`, `max_concurrent_jobs`, `log_level` at line 1 column 417";
    const text = toUserMessage(err("invalid_argument", raw));
    expect(text).toBe("配置里有无法识别的字段：bogus_console_probe（可选字段见「参数」页说明）");
    expect(text).not.toMatch(/line \d+ column \d+/);
    expect(text).not.toMatch(/unknown field/);
  });

  it("serde 缺字段 → 中文点名缺哪个", () => {
    const raw = "invalid JSON body: missing field `items` at line 1 column 2";
    expect(toUserMessage(err("invalid_argument", raw))).toBe("请求缺少必需字段：items");
  });

  it("类型不匹配 → 中文说明期望类型", () => {
    const raw = 'invalid JSON body: invalid type: string "abc", expected u64 at line 1 column 30';
    expect(toUserMessage(err("invalid_argument", raw))).toBe("字段类型不对：服务端要求 u64");
  });

  it("P1-5 内部键名：gse_admin_url 未配置 → 人话 + 该做什么", () => {
    const text = toUserMessage(err("unavailable", "gse_admin_url is not configured"));
    expect(text).toContain("数据面未配置");
    expect(text).not.toContain("gse_admin_url");
  });

  it("心跳上限的散文带真实阈值，不硬编码 30", () => {
    const raw = "heartbeat_interval_secs 不得大于 20（判活窗口 heartbeat_timeout_secs/3 秒）";
    expect(toUserMessage(err("invalid_argument", raw))).toBe(
      "心跳周期不得超过 20 秒（由服务端判活窗口决定）",
    );
  });

  it("认不出的英文原文 → 回落错误码通用句，不直出", () => {
    const text = toUserMessage(err("invalid_argument", "something totally new happened"));
    expect(text).toBe("这份配置没通过服务端校验，请检查参数后再保存");
    expect(text).not.toMatch(/[A-Za-z]{4,} /);
  });

  it("认不出的错误码 + 英文 → 仍有兜底句", () => {
    expect(toUserMessage(err("weird_code", "boom"))).toBe("操作失败，请稍后重试");
  });

  it("已经是中文的 message 原样保留（服务端自己的中文校验句）", () => {
    const msg = "归档产物已存在同名版本，请换个版本号";
    expect(toUserMessage(err("invalid_argument", msg))).toBe(msg);
  });

  it("空 message 不返回空串", () => {
    expect(toUserMessage(err("invalid_argument", ""))).toBe(
      "这份配置没通过服务端校验，请检查参数后再保存",
    );
  });

  it("409 并发冲突按错误码给人话", () => {
    expect(toUserMessage(err("revision_conflict", "revision mismatch"))).toBe(
      "配置已被他人修改，请重新加载后再保存",
    );
  });
});
