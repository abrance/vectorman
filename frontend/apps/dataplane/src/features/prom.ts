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
  const entries = Object.entries(metric ?? {});
  return entries.length === 0
    ? `series-${index}`
    : entries.map(([k, v]) => `${k}=${v}`).join(" ");
}

/// 从 `PromEnvelope.data` 取绘图序列；形状不符时返回空数组。
export function promSeries(data: unknown): MetricSeries[] {
  const d = data as PromData | undefined;
  if (!d || !Array.isArray(d.result)) {
    return [];
  }
  return d.result.map((sample, i) => ({
    label: promLabel(sample.metric, i),
    points: sample.values ?? (sample.value ? [sample.value] : []),
  }));
}

/// 时间范围（秒）与步长拼成 `query_range` 参数。
export function rangeParams(fromMillis: number, toMillis: number): { start: string; end: string; step: string } {
  const start = Math.floor(fromMillis / 1000);
  const end = Math.floor(toMillis / 1000);
  const step = Math.max(1, Math.floor((end - start) / 60));
  return { start: String(start), end: String(end), step: String(step) };
}
