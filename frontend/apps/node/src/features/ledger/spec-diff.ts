import type { AgentSpecView, SpecDiff, SpecParams, SyncStatus } from "@vectorman/adapters";

/// 敏感字段的读写占位值（与服务端约定一致：读出是它，写回它也表示「保持原值」）。
export const MASK = "***";

/// 同步状态的展示口径。
///
/// `unspecified`（没有期望 spec）与 `unknown`（从未上报）必须分开显示：
/// 前者是「你还没配」，后者是「配了没回音」，运维该做的事完全不同。
export function syncStatusMeta(status: SyncStatus): {
  label: string;
  color: string;
  hint: string;
} {
  switch (status) {
    case "synced":
      return { label: "已同步", color: "green", hint: "期望值与 Agent 生效值一致" };
    case "stale":
      return {
        label: "未同步",
        color: "orange",
        hint: "有改动未下发，或 Agent 未按期望生效 —— 点「下发」",
      };
    case "rejected":
      return { label: "被拒绝", color: "red", hint: "Agent 拒绝了这份配置（含不可下发字段）" };
    case "unspecified":
      return { label: "无期望", color: "blue", hint: "未保存过期望配置，Agent 跑本地文件基线" };
    default:
      return { label: "未知", color: "default", hint: "Agent 从未上报生效值" };
  }
}

/// 值 → 可读文本（差异表用）。
export function formatValue(value: unknown): string {
  if (value === null || value === undefined) {
    return "—";
  }
  if (Array.isArray(value)) {
    return value.length === 0 ? "（空）" : value.map((v) => formatValue(v)).join(", ");
  }
  if (typeof value === "boolean") {
    return value ? "true" : "false";
  }
  if (typeof value === "object") {
    return JSON.stringify(value);
  }
  const text = String(value);
  return text === "" ? "（空）" : text;
}

export type ParamDiffRow = { field: string; desired: string; applied: string };

/// `params` 的逐字段差异行（按字段名排序，保证渲染稳定）。
export function paramDiffRows(diff?: SpecDiff | null): ParamDiffRow[] {
  const params = diff?.params ?? {};
  return Object.keys(params)
    .sort()
    .map((field) => ({
      field,
      desired: formatValue(params[field]?.desired),
      applied: formatValue(params[field]?.applied),
    }));
}

export type ItemChange = { itemId: string; change: "added" | "removed" | "changed" };

/// 采集项差异，按 `item_id` 排序。
export function itemChanges(diff?: SpecDiff | null): ItemChange[] {
  const items = diff?.items ?? { added: [], removed: [], changed: [] };
  return [
    ...items.added.map((itemId) => ({ itemId, change: "added" as const })),
    ...items.changed.map((itemId) => ({ itemId, change: "changed" as const })),
    ...items.removed.map((itemId) => ({ itemId, change: "removed" as const })),
  ].sort((a, b) => a.itemId.localeCompare(b.itemId));
}

/// `not_enforced` 的字段名 → 中文标注。
///
/// 「未实现（仅记录）」必须显式可见：这些字段存得下、传得到，但 Agent 侧没有真实实现，
/// 不标出来就等于骗运维。
export function notEnforcedLabel(field: string): string {
  switch (field) {
    case "cpu_limit_percent":
      return "CPU 上限（未实现，仅记录）";
    case "mem_limit_percent":
      return "内存上限（未实现，仅记录）";
    case "log_level":
      return "日志级别（未实现，仅记录）";
    default:
      return `${field}（未实现，仅记录）`;
  }
}

/// 是否有「改了但没下发」的改动。
export function hasUnappliedChanges(view?: AgentSpecView | null): boolean {
  if (!view?.desired) {
    return false;
  }
  return view.sync_status !== "synced";
}

/// 敏感字段的写回值：从服务端读到的 `"***"` / `""` 原样回传都表示「保持原值」。
///
/// 这里不做转换，只是把「什么算已设置」的判断收在一处，避免页面各写一遍。
export function isSecretSet(value?: string | null): boolean {
  return typeof value === "string" && value.length > 0 && value !== MASK;
}

/// 新建 spec 时表单的缺省参数。
///
/// 与服务端 `SpecParams::default()` 对齐（心跳 30s、作业白名单 bash/sh/python3、
/// OTLP 关、日志级别 info）—— 不对齐的话「打开页面直接保存」会推出一份和缺省不同的配置。
export function defaultParams(): SpecParams {
  return {
    heartbeat_interval_secs: 30,
    allowed_interpreters: ["bash", "sh", "python3"],
    job_default_interpreter: "bash",
    max_concurrent_jobs: 1,
    job_work_dir: null,
    otlp_enabled: false,
    otlp_listen: "0.0.0.0:4318",
    otlp_max_body_bytes: 8 * 1024 * 1024,
    otlp_token: null,
    otlp_allowed_cidrs: [],
    token: null,
    cpu_limit_percent: null,
    mem_limit_percent: null,
    log_level: "info",
  };
}
