import type { CollectItem, CollectItemInput, CollectItemKind, ExtractRule } from "@vectorman/adapters";

/// 采集项表单的扁平值；按 `kind` 只提交相关字段。
export type CollectFormValues = {
  name: string;
  agent_ids: string[];
  kind: CollectItemKind;
  enabled: boolean;
  retention_days: number;
  interval_secs?: number;
  path_patterns?: string[];
  namespace?: string;
  pod_name_pattern?: string;
  container?: string;
  kubeconfig?: string;
  start_mode?: "head" | "tail";
  start_n?: number;
  batch_max_records?: number;
  flush_interval_secs?: number;
  include_regex?: string;
  exclude_regex?: string;
  extract?: ExtractRule[];
};

export const COLLECT_KINDS: { value: CollectItemKind; label: string }[] = [
  { value: "metrics_host", label: "主机指标" },
  { value: "log_file", label: "文件日志" },
  { value: "log_k8s_stdout", label: "K8s 标准输出" },
];

const DEFAULT_INTERVAL = 15;
const DEFAULT_BATCH_MAX = 100;
const DEFAULT_FLUSH = 5;

function positive(value: number | undefined, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : fallback;
}

function nonNegative(value: number | undefined): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : 0;
}

function trimmed(value: string | undefined): string | undefined {
  const text = value?.trim();
  return text ? text : undefined;
}

function cleanJson(values: CollectFormValues): Record<string, unknown> {
  const clean: Record<string, unknown> = {};
  const include = trimmed(values.include_regex);
  if (include) {
    clean.include_regex = include;
  }
  const exclude = trimmed(values.exclude_regex);
  if (exclude) {
    clean.exclude_regex = exclude;
  }
  const extract = (values.extract ?? []).filter((r) => trimmed(r.label) && trimmed(r.expr));
  if (extract.length > 0) {
    clean.extract = extract;
  }
  return clean;
}

function logCommon(values: CollectFormValues): Record<string, unknown> {
  return {
    start_mode: values.start_mode === "head" ? "head" : "tail",
    start_n: nonNegative(values.start_n),
    batch_max_records: positive(values.batch_max_records, DEFAULT_BATCH_MAX),
    flush_interval_secs: positive(values.flush_interval_secs, DEFAULT_FLUSH),
    clean: cleanJson(values),
  };
}

/// 表单值 → 采集项入参；按类型裁剪 `collector`，`retention_days` 缺省 1。
export function toCollectItemInput(values: CollectFormValues): CollectItemInput {
  let collector: Record<string, unknown>;
  if (values.kind === "metrics_host") {
    collector = { interval_secs: positive(values.interval_secs, DEFAULT_INTERVAL) };
  } else if (values.kind === "log_file") {
    collector = {
      path_patterns: (values.path_patterns ?? []).map((p) => p.trim()).filter(Boolean),
      ...logCommon(values),
    };
  } else {
    collector = {
      namespace: trimmed(values.namespace) ?? "",
      pod_name_pattern: trimmed(values.pod_name_pattern) ?? "",
      container: trimmed(values.container) ?? "",
      kubeconfig: trimmed(values.kubeconfig) ?? "",
      ...logCommon(values),
    };
  }
  return {
    name: values.name.trim(),
    agent_ids: values.agent_ids,
    kind: values.kind,
    enabled: values.enabled,
    collector,
    storage: { retention_days: positive(values.retention_days, 1) },
  };
}

function str(collector: Record<string, unknown>, key: string): string | undefined {
  const v = collector[key];
  return typeof v === "string" ? v : undefined;
}

function num(collector: Record<string, unknown>, key: string): number | undefined {
  const v = collector[key];
  return typeof v === "number" ? v : undefined;
}

/// 采集项 → 表单值；缺失字段回落到默认值，供编辑/详情回填。
export function toCollectFormValues(item: CollectItem): CollectFormValues {
  const collector = item.collector ?? {};
  const clean = (collector.clean ?? {}) as Record<string, unknown>;
  return {
    name: item.name,
    agent_ids: item.agent_ids,
    kind: item.kind as CollectItemKind,
    enabled: item.enabled,
    retention_days: item.storage?.retention_days ?? 1,
    interval_secs: num(collector, "interval_secs") ?? DEFAULT_INTERVAL,
    path_patterns: Array.isArray(collector.path_patterns)
      ? (collector.path_patterns as unknown[]).map(String)
      : [],
    namespace: str(collector, "namespace") ?? "",
    pod_name_pattern: str(collector, "pod_name_pattern") ?? "",
    container: str(collector, "container") ?? "",
    kubeconfig: str(collector, "kubeconfig") ?? "",
    start_mode: str(collector, "start_mode") === "head" ? "head" : "tail",
    start_n: num(collector, "start_n") ?? 0,
    batch_max_records: num(collector, "batch_max_records") ?? DEFAULT_BATCH_MAX,
    flush_interval_secs: num(collector, "flush_interval_secs") ?? DEFAULT_FLUSH,
    include_regex: str(clean, "include_regex") ?? "",
    exclude_regex: str(clean, "exclude_regex") ?? "",
    extract: Array.isArray(clean.extract) ? (clean.extract as ExtractRule[]) : [],
  };
}
