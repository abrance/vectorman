import type { EdgeRow } from "@vectorman/adapters";

/// 边聚合：把按分钟桶返回的边合并成「逻辑边」（同 src/dst/kind）。
export type EdgeSummary = {
  src: string;
  dst: string;
  spanKind: string;
  calls: number;
  errors: number;
  /// 平均耗时（微秒）：桶内 `duration_sum` 之和 / 调用数之和。
  avgDurationMicros: number;
  maxDurationMicros: number;
  /// 错误率（0..1）。
  errorRate: number;
  bucketCount: number;
  sources: string[];
  agentIds: string[];
};

/// 合并边桶并计算派生指标；按调用量降序，等量时按 src/dst 字典序（结果确定）。
export function aggregateEdges(rows: EdgeRow[]): EdgeSummary[] {
  const groups = new Map<string, EdgeSummary & { durationSum: number }>();
  for (const row of rows) {
    const key = `${row.src_service}\u0000${row.dst_service}\u0000${row.span_kind}`;
    const current = groups.get(key) ?? {
      src: row.src_service,
      dst: row.dst_service,
      spanKind: row.span_kind,
      calls: 0,
      errors: 0,
      avgDurationMicros: 0,
      maxDurationMicros: 0,
      errorRate: 0,
      bucketCount: 0,
      sources: [],
      agentIds: [],
      durationSum: 0,
    };
    current.calls += row.calls;
    current.errors += row.errors;
    current.durationSum += row.duration_sum;
    current.maxDurationMicros = Math.max(current.maxDurationMicros, row.duration_max);
    current.bucketCount += 1;
    if (!current.sources.includes(row.source)) {
      current.sources.push(row.source);
    }
    if (row.agent_id && !current.agentIds.includes(row.agent_id)) {
      current.agentIds.push(row.agent_id);
    }
    groups.set(key, current);
  }

  return [...groups.values()]
    .map((group) => ({
      src: group.src,
      dst: group.dst,
      spanKind: group.spanKind,
      calls: group.calls,
      errors: group.errors,
      avgDurationMicros: group.calls > 0 ? Math.round(group.durationSum / group.calls) : 0,
      maxDurationMicros: group.maxDurationMicros,
      errorRate: group.calls > 0 ? group.errors / group.calls : 0,
      bucketCount: group.bucketCount,
      sources: [...group.sources].sort(),
      agentIds: [...group.agentIds].sort(),
    }))
    .sort((a, b) => {
      if (b.calls !== a.calls) {
        return b.calls - a.calls;
      }
      return `${a.src}→${a.dst}`.localeCompare(`${b.src}→${b.dst}`);
    });
}

/// 拓扑输入：节点为出现过的服务，边权重为调用量。
export function topologyInput(summaries: EdgeSummary[]): {
  services: string[];
  edges: { src: string; dst: string; value: number }[];
} {
  const services = new Set<string>();
  for (const summary of summaries) {
    services.add(summary.src);
    services.add(summary.dst);
  }
  return {
    services: [...services],
    edges: summaries.map((summary) => ({
      src: summary.src,
      dst: summary.dst,
      value: summary.calls,
    })),
  };
}

/// 边的视觉映射：线宽按调用量（相对最大值，最小 1.5px）、颜色按错误率（绿→红）。
export function edgeStyle(
  summary: EdgeSummary,
  maxCalls: number,
): { width: number; color: string } {
  const ratio = maxCalls > 0 ? summary.calls / maxCalls : 0;
  const width = 1.5 + ratio * 5;
  const errorRate = Math.min(Math.max(summary.errorRate, 0), 1);
  // 0% 绿色 (#0f9d8e) → 100% 红色 (#c4396e)，中间经过琥珀色，便于肉眼分档。
  const hue = 170 - 170 * errorRate;
  const color = `hsl(${hue}, 65%, 42%)`;
  return { width, color };
}
