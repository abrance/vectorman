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
  /// apm_otlp：服务名单与属性白名单（逗号/换行分隔）。
  service_allowlist?: string;
  service_denylist?: string;
  attribute_allowlist?: string;
  /// ebpf_*：采集回环（本机自测用，生产一般关）；原始事件默认关。
  include_loopback?: boolean;
  raw_events_enabled?: boolean;
  /// ebpf_syscall：慢调用阈值（微秒），超过才上报慢调用原始事件。
  slow_threshold_micros?: number;
};

export const COLLECT_KINDS: { value: CollectItemKind; label: string }[] = [
  { value: "metrics_host", label: "主机指标" },
  { value: "log_file", label: "文件日志" },
  { value: "log_k8s_stdout", label: "K8s 标准输出" },
  { value: "apm_otlp", label: "APM（OTLP trace）" },
  { value: "ebpf_network", label: "eBPF 网络连接" },
  { value: "ebpf_tcp", label: "eBPF TCP 异常" },
  { value: "ebpf_process", label: "eBPF 进程生命周期" },
  { value: "ebpf_syscall", label: "eBPF 文件与 syscall" },
];

const DEFAULT_INTERVAL = 15;
const DEFAULT_BATCH_MAX = 100;
const DEFAULT_FLUSH = 5;
/// eBPF 上报间隔缺省 10 秒（与 Agent 侧 `EbpfConfig` 的缺省一致）。
const DEFAULT_EBPF_FLUSH = 10;
/// syscall 慢调用阈值缺省 100ms（Agent 侧同样缺省 100_000 微秒）。
const DEFAULT_SLOW_THRESHOLD_MICROS = 100_000;

/// 是否 eBPF 采集项（决定 `collector` 里带哪些参数）。
export function isEbpfKind(kind: CollectItemKind | string): boolean {
  return kind.startsWith("ebpf_");
}

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

/// 逗号/换行分隔的名单 → 字符串数组（去空去重，保持顺序）。
export function splitList(value: string | undefined): string[] {
  const items = (value ?? "")
    .split(/[,\n]/)
    .map((item) => item.trim())
    .filter(Boolean);
  return items.filter((item, index) => items.indexOf(item) === index);
}

/// 表单值 → 采集项入参；按类型裁剪 `collector`，`retention_days` 缺省 1。
export function toCollectItemInput(values: CollectFormValues): CollectItemInput {
  let collector: Record<string, unknown>;
  if (values.kind === "metrics_host") {
    collector = { interval_secs: positive(values.interval_secs, DEFAULT_INTERVAL) };
  } else if (isEbpfKind(values.kind)) {
    // eBPF：内核态程序由 Agent 按采集项加载；这里只暴露最常用的几个开关，
    // 其余（map 容量/限流/包含排除名单）用 `collector` JSON 精调（Agent 侧会夹取越界值）。
    collector = {
      flush_interval_secs: positive(values.flush_interval_secs, DEFAULT_EBPF_FLUSH),
      include_loopback: values.include_loopback === true,
      raw_events_enabled: values.raw_events_enabled === true,
    };
    if (values.kind === "ebpf_syscall") {
      // 慢调用阈值：非负数（0 会被 Agent 夹到下限 1ms）。
      collector.slow_threshold_micros = nonNegative(
        values.slow_threshold_micros ?? DEFAULT_SLOW_THRESHOLD_MICROS,
      );
    }
  } else if (values.kind === "apm_otlp") {
    collector = {
      service_allowlist: splitList(values.service_allowlist),
      service_denylist: splitList(values.service_denylist),
      attribute_allowlist: splitList(values.attribute_allowlist),
      batch_max_records: positive(values.batch_max_records, DEFAULT_BATCH_MAX),
      flush_interval_secs: positive(values.flush_interval_secs, DEFAULT_FLUSH),
    };
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

/// 字符串数组 → 逗号分隔文本（编辑表单回填）。
function listStr(collector: Record<string, unknown>, key: string): string {
  const value = collector[key];
  return Array.isArray(value) ? value.filter((v) => typeof v === "string").join(",") : "";
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
    service_allowlist: listStr(collector, "service_allowlist"),
    service_denylist: listStr(collector, "service_denylist"),
    attribute_allowlist: listStr(collector, "attribute_allowlist"),
    include_loopback: collector.include_loopback === true,
    raw_events_enabled: collector.raw_events_enabled === true,
    slow_threshold_micros:
      num(collector, "slow_threshold_micros") ?? DEFAULT_SLOW_THRESHOLD_MICROS,
  };
}
