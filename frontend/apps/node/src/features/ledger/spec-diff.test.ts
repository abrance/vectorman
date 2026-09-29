import { describe, expect, it } from "vitest";
import type { AgentSpecView, SpecDiff } from "@vectorman/adapters";
import {
  MASK,
  defaultParams,
  itemChanges,
  notEnforcedLabel,
  paramDiffRows,
  syncStatusMeta,
  hasUnappliedChanges,
  isSecretSet,
} from "./spec-diff";

describe("syncStatusMeta", () => {
  it("区分「没配」与「配了没回音」", () => {
    // unspecified = 没保存过期望配置；unknown = 配了但从未上报。运维该做的事完全不同。
    const none = syncStatusMeta("unspecified");
    const unknown = syncStatusMeta("unknown");
    expect(none.label).toBe("无期望");
    expect(unknown.label).toBe("未知");
    expect(none.color).not.toBe(unknown.color);
  });

  it("未同步与拒绝都有明确颜色与提示", () => {
    expect(syncStatusMeta("stale").color).toBe("orange");
    expect(syncStatusMeta("stale").hint).toContain("下发");
    expect(syncStatusMeta("rejected").color).toBe("red");
    expect(syncStatusMeta("synced").color).toBe("green");
  });
});

describe("paramDiffRows", () => {
  it("无差异时为空", () => {
    expect(paramDiffRows(null)).toEqual([]);
    expect(paramDiffRows({ params: {}, items: { added: [], removed: [], changed: [] } })).toEqual([]);
  });

  it("按字段名排序并格式化值", () => {
    const diff: SpecDiff = {
      params: {
        max_concurrent_jobs: { desired: 2, applied: 1 },
        allowed_interpreters: { desired: ["bash"], applied: [] },
        job_work_dir: { desired: null, applied: "/tmp" },
      },
      items: { added: [], removed: [], changed: [] },
    };
    const rows = paramDiffRows(diff);
    expect(rows.map((r) => r.field)).toEqual([
      "allowed_interpreters",
      "job_work_dir",
      "max_concurrent_jobs",
    ]);
    expect(rows[0]).toEqual({ field: "allowed_interpreters", desired: "bash", applied: "（空）" });
    expect(rows[1]).toEqual({ field: "job_work_dir", desired: "—", applied: "/tmp" });
    expect(rows[2]).toEqual({ field: "max_concurrent_jobs", desired: "2", applied: "1" });
  });

  it("布尔与对象都能读出来", () => {
    const rows = paramDiffRows({
      params: {
        otlp_enabled: { desired: true, applied: false },
        job_work_dir: { desired: { a: 1 }, applied: null },
      },
      items: { added: [], removed: [], changed: [] },
    });
    // 按字段名排序：job_work_dir 在前。
    expect(rows[0]?.field).toBe("job_work_dir");
    expect(rows[0]?.desired).toBe('{"a":1}');
    expect(rows[1]?.field).toBe("otlp_enabled");
    expect(rows[1]?.desired).toBe("true");
    expect(rows[1]?.applied).toBe("false");
  });
});

describe("itemChanges", () => {
  it("把增/删/改三类合并成一张按 item_id 排序的表", () => {
    const changes = itemChanges({
      params: {},
      items: { added: ["b"], removed: ["c"], changed: ["a"] },
    });
    expect(changes).toEqual([
      { itemId: "a", change: "changed" },
      { itemId: "b", change: "added" },
      { itemId: "c", change: "removed" },
    ]);
  });

  it("缺字段时返回空数组（旧服务端可能不带 items）", () => {
    expect(itemChanges(null)).toEqual([]);
    expect(itemChanges({ params: {} } as SpecDiff)).toEqual([]);
  });
});

describe("notEnforcedLabel", () => {
  it("未实现字段必须带「未实现」字样，不能看起来像真的生效了", () => {
    for (const field of ["cpu_limit_percent", "mem_limit_percent", "log_level"]) {
      expect(notEnforcedLabel(field)).toContain("未实现");
    }
    // 未知名的字段也要标注，而不是原样显示。
    expect(notEnforcedLabel("something_new")).toContain("未实现");
  });
});

describe("hasUnappliedChanges", () => {
  const view = (status: AgentSpecView["sync_status"], desired: boolean): AgentSpecView => ({
    agent_id: "a-1",
    host_id: "h-1",
    session_state: "online",
    sync_status: status,
    desired: desired ? ({ revision: "r", spec: { params: defaultParams(), items: [] } } as never) : null,
  });

  it("没有期望配置时不算「有未下发改动」", () => {
    expect(hasUnappliedChanges(view("unspecified", false))).toBe(false);
  });

  it("有期望但未同步时算", () => {
    expect(hasUnappliedChanges(view("stale", true))).toBe(true);
    expect(hasUnappliedChanges(view("synced", true))).toBe(false);
  });
});

describe("isSecretSet", () => {
  it("哨兵与空串都表示「没有明文可看」", () => {
    expect(isSecretSet(MASK)).toBe(false);
    expect(isSecretSet("")).toBe(false);
    expect(isSecretSet(null)).toBe(false);
    expect(isSecretSet("s3cret")).toBe(true);
  });
});

describe("defaultParams", () => {
  it("与服务端缺省对齐（打开页面直接保存不该推出一份意外配置）", () => {
    const p = defaultParams();
    expect(p.heartbeat_interval_secs).toBe(30);
    expect(p.allowed_interpreters).toEqual(["bash", "sh", "python3"]);
    expect(p.job_default_interpreter).toBe("bash");
    expect(p.max_concurrent_jobs).toBe(1);
    expect(p.otlp_listen).toBe("0.0.0.0:4318");
    expect(p.otlp_max_body_bytes).toBe(8 * 1024 * 1024);
    expect(p.log_level).toBe("info");
    // 敏感字段为 null = 「不下发/不修改」。
    expect(p.token).toBeNull();
    expect(p.otlp_token).toBeNull();
  });
});
