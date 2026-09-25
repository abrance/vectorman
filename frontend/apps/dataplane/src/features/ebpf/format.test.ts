import { describe, expect, it } from "vitest";
import type { CapabilityReport, EdgeRow } from "@vectorman/adapters";
import {
  aliasLink,
  capabilityLine,
  endpointLabel,
  formatBytes,
  sourceLabel,
  summarizeCapability,
  unknownServiceIp,
} from "./format";

function edge(overrides: Partial<EdgeRow> = {}): EdgeRow {
  return {
    bucket_ts: 1_710_000_000_000_000,
    src_service: "order-api",
    dst_service: "pay-api",
    span_kind: "",
    calls: 2,
    errors: 1,
    duration_sum: 600,
    duration_max: 400,
    source: "ebpf",
    agent_id: "a-1",
    src_ip: "10.0.0.5",
    dst_ip: "10.0.0.9",
    dst_port: 8080,
    protocol: "tcp",
    connections: 2,
    failures: 1,
    bytes_sent: 1024,
    bytes_recv: 2048,
    duration_avg_micros: 300,
    tcp_retrans: 1,
    ...overrides,
  };
}

describe("ebpf 展示辅助", () => {
  it("格式化字节数", () => {
    expect(formatBytes(0)).toBe("-");
    expect(formatBytes(-1)).toBe("-");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1024)).toBe("1.0 KiB");
    expect(formatBytes(1536)).toBe("1.5 KiB");
    expect(formatBytes(5 * 1024 * 1024)).toBe("5.0 MiB");
    expect(formatBytes(Number.NaN)).toBe("-");
  });

  it("来源与端点展示", () => {
    expect(sourceLabel("ebpf")).toBe("eBPF");
    expect(sourceLabel("merged")).toBe("合并");
    expect(sourceLabel("")).toBe("-");
    expect(endpointLabel(edge())).toBe("10.0.0.9:8080");
    expect(endpointLabel(edge({ dst_port: 0 }))).toBe("10.0.0.9");
    expect(endpointLabel(edge({ dst_ip: "" }))).toBe("-");
  });

  it("只把 unknown-<ip> 当成未识别节点", () => {
    expect(unknownServiceIp("unknown-10.0.0.9")).toBe("10.0.0.9");
    expect(unknownServiceIp("unknown")).toBeNull();
    expect(unknownServiceIp("unknown-")).toBeNull();
    expect(unknownServiceIp("order-api")).toBeNull();
    expect(aliasLink("10.0.0.9")).toBe(
      "/settings/service-aliases?match_kind=cidr&match_value=10.0.0.9",
    );
  });

  it("能力诊断按 内核 → BTF → 权限 列出失败项", () => {
    const line = capabilityLine({
      agent_id: "a-1",
      item_id: "item-ebpf",
      available: false,
      kernel_ok: false,
      btf_ok: true,
      capability_ok: false,
      kernel_release: "5.4.0",
      reason: "preflight failed",
    });
    expect(line.available).toBe(false);
    expect(line.detail).toContain("内核版本不足");
    expect(line.detail).toContain("权限不足");
    expect(line.detail).not.toContain("BTF");
    expect(line.detail).toContain("preflight failed");

    const ok = capabilityLine({
      agent_id: "a-2",
      item_id: "item-ebpf",
      available: true,
      kernel_ok: true,
      btf_ok: true,
      capability_ok: true,
      kernel_release: "6.1.0",
      reason: "",
    });
    expect(ok.available).toBe(true);
    expect(ok.detail).toBe("内核 6.1.0");
  });

  it("能力汇总区分没人上报与不可用", () => {
    const empty = summarizeCapability(undefined);
    expect(empty).toEqual({ reported: 0, available: 0, unavailable: 0, lines: [] });

    const report: CapabilityReport = {
      reported: 2,
      agents: [
        {
          agent_id: "a-1",
          item_id: "i1",
          available: true,
          kernel_ok: true,
          btf_ok: true,
          capability_ok: true,
          kernel_release: "6.1.0",
          reason: "",
        },
        {
          agent_id: "a-2",
          item_id: "i2",
          available: false,
          kernel_ok: true,
          btf_ok: false,
          capability_ok: true,
          kernel_release: "5.15.0",
          reason: "",
        },
      ],
    };
    const summary = summarizeCapability(report);
    expect(summary.reported).toBe(2);
    expect(summary.available).toBe(1);
    expect(summary.unavailable).toBe(1);
    expect(summary.lines).toHaveLength(2);
  });
});
