import type { CapabilityEntry, CapabilityReport, EdgeRow } from "@vectorman/adapters";

/// 字节数的可读格式（二进制单位，保留一位小数；0 与负数都显示 `-`）。
export function formatBytes(value: number): string {
  if (!Number.isFinite(value) || value <= 0) {
    return "-";
  }
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let size = value;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${size} B` : `${size.toFixed(1)} ${units[unit]}`;
}

/// 边的来源展示名。`merged` 是服务端把两路相加后的行。
export function sourceLabel(source: string): string {
  switch (source) {
    case "ebpf":
      return "eBPF";
    case "otlp":
      return "OTLP";
    case "merged":
      return "合并";
    default:
      return source || "-";
  }
}

/// 边的「IP:端口」展示；eBPF 侧才有，OTLP 侧返回 `-`。
export function endpointLabel(row: EdgeRow): string {
  if (!row.dst_ip) {
    return "-";
  }
  return row.dst_port > 0 ? `${row.dst_ip}:${row.dst_port}` : row.dst_ip;
}

/// 未识别服务（`unknown-<ip>`）判定：这类节点在拓扑页要单列，并提供「建立映射」入口。
export function unknownServiceIp(service: string): string | null {
  if (!service.startsWith("unknown-")) {
    return null;
  }
  const ip = service.slice("unknown-".length);
  return ip.length > 0 && ip !== "unknown" ? ip : null;
}

/// 能力状态卡片需要的一行文案。
export type CapabilityLine = {
  agentId: string;
  itemId: string;
  available: boolean;
  /// 一句可读的诊断：可用时给内核版本，不可用时给失败项与原因。
  detail: string;
};

/// 单个 Agent 的能力诊断文案。
///
/// 检查项按「内核版本 → BTF → 权限」的顺序列出失败项：这是设计里的检查顺序，
/// 也是运维按顺序排查的顺序（权限最容易通过 sudo 解决，放最后）。
export function capabilityLine(entry: CapabilityEntry): CapabilityLine {
  if (entry.available) {
    return {
      agentId: entry.agent_id,
      itemId: entry.item_id,
      available: true,
      detail: entry.kernel_release ? `内核 ${entry.kernel_release}` : "可用",
    };
  }
  const failed: string[] = [];
  if (!entry.kernel_ok) {
    failed.push(`内核版本不足（${entry.kernel_release || "未知"}，需 ≥ 5.8）`);
  }
  if (!entry.btf_ok) {
    failed.push("缺少 BTF（/sys/kernel/btf/vmlinux）");
  }
  if (!entry.capability_ok) {
    failed.push("权限不足（需 root 或 CAP_BPF+CAP_PERFMON）");
  }
  const reason = entry.reason.trim();
  return {
    agentId: entry.agent_id,
    itemId: entry.item_id,
    available: false,
    detail: [failed.join("；"), reason].filter(Boolean).join(" · ") || "不可用",
  };
}

/// 能力报告汇总：区分「没人上报」与「上报了但不可用」。
export type CapabilitySummary = {
  reported: number;
  available: number;
  unavailable: number;
  lines: CapabilityLine[];
};

export function summarizeCapability(report: CapabilityReport | undefined): CapabilitySummary {
  const agents = report?.agents ?? [];
  const lines = agents.map(capabilityLine);
  return {
    // 以服务端计数为准：它为 0 但本地拿到行时也不会误报成「没人上报」。
    reported: report?.reported ?? agents.length,
    available: lines.filter((line) => line.available).length,
    unavailable: lines.filter((line) => !line.available).length,
    lines,
  };
}

/// 「建立映射」跳转链接：复用设置页的映射页签，按 CIDR 预填未识别 IP。
export function aliasLink(ip: string): string {
  return `/settings/service-aliases?match_kind=cidr&match_value=${encodeURIComponent(ip)}`;
}
