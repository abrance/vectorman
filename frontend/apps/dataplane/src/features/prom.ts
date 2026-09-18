/// Prom `query_range` 单条样本。
export type PromSample = {
  metric: Record<string, string>;
  value?: [number, number];
  values?: [number, number][];
};

/// Prom `query_range` 的 data 段。
export type PromData = {
  resultType: string;
  result: PromSample[];
};

/// 绘图用的一条序列。
export type MetricSeries = {
  label: string;
  points: [number, number][];
};

/// `metric` 标签拼成可读名称；无标签时回落 `series-{index}`。
export function promLabel(metric: Record<string, string>, index: number): string {
  return seriesLabel(metric, index);
}

/// 从 `PromEnvelope.data` 取绘图序列；形状不符时返回空数组。
export function promSeries(data: unknown): MetricSeries[] {
  const d = data as PromData | undefined;
  if (!d || !Array.isArray(d.result)) {
    return [];
  }
  return d.result.map((sample, i) => ({
    label: promLabel(sample.metric, i),
    points: asPoints(sample),
  }));
}

/// 时间范围（秒）与步长拼成 `query_range` 参数。
export function rangeParams(fromMillis: number, toMillis: number): { start: string; end: string; step: string } {
  const start = Math.floor(fromMillis / 1000);
  const end = Math.floor(toMillis / 1000);
  const step = Math.max(1, Math.floor((end - start) / 60));
  return { start: String(start), end: String(end), step: String(step) };
}

/// 拼 PromQL 选择器；空过滤则返回裸 measurement。
export function metricExpr(
  measurement: string,
  filters: { agentId?: string; itemId?: string },
): string {
  const parts: string[] = [];
  const agentId = filters.agentId?.trim();
  const itemId = filters.itemId?.trim();
  if (agentId) {
    parts.push(`agent_id="${escapeMatcher(agentId)}"`);
  }
  if (itemId) {
    parts.push(`item_id="${escapeMatcher(itemId)}"`);
  }
  return parts.length === 0 ? measurement : `${measurement}{${parts.join(",")}}`;
}

/// 图例优先 agent / host，去掉 __name__ 与 item_id。
export function seriesLabel(metric: Record<string, string>, index: number): string {
  const tags = metric ?? {};
  const agent = tags.agent_id?.trim();
  const host = tags.host_id?.trim();
  const skip = new Set(["__name__", "agent_id", "host_id", "item_id"]);
  const rest = Object.entries(tags)
    .filter(([k, v]) => v && !skip.has(k))
    .map(([k, v]) => `${k}=${v}`);
  const head = [agent, host].filter(Boolean).join(" · ");
  const label = [head, ...rest].filter(Boolean).join("  ");
  return label || `series-${index}`;
}

/// 所有序列里时间戳最新的一个值。
export function latestValue(series: MetricSeries[]): number | undefined {
  let best: { t: number; v: number } | undefined;
  for (const s of series) {
    const point = s.points[s.points.length - 1];
    if (!point) {
      continue;
    }
    if (!best || point[0] >= best.t) {
      best = { t: point[0], v: point[1] };
    }
  }
  return best?.v;
}

/// 横轴时间；跨度超过一天时带上月日。
export function axisTimeLabel(sec: number, spanSec: number): string {
  const date = new Date(sec * 1000);
  if (Number.isNaN(date.getTime())) {
    return "";
  }
  const hh = pad2(date.getHours());
  const mm = pad2(date.getMinutes());
  if (spanSec >= 20 * 3600) {
    return `${date.getMonth() + 1}/${date.getDate()} ${hh}:${mm}`;
  }
  return `${hh}:${mm}`;
}

function escapeMatcher(value: string): string {
  return value.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

function asPoints(sample: PromSample): [number, number][] {
  const raw = sample.values ?? (sample.value ? [sample.value] : []);
  return raw
    .map((pair) => [Number(pair[0]), Number(pair[1])] as [number, number])
    .filter(([t, v]) => Number.isFinite(t) && Number.isFinite(v));
}

function pad2(value: number): string {
  return String(value).padStart(2, "0");
}
