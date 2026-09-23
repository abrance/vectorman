import type { SpanDetail } from "@vectorman/adapters";

/// 瀑布图/拓扑图的纯布局计算：不依赖 React 与图表库，便于单测。
/// 设计参考 `.monkeycode/specs/apm-tracing/design.md`：拓扑必须确定性分层，
/// 不允许力导向布局（同一份数据每次渲染要一致）。

export type SpanNode = {
  span: SpanDetail;
  /// 从根开始的层级（根为 0）。
  depth: number;
  /// 相对 trace 起点的时间偏移比例（0..1）。
  offsetRatio: number;
  /// 相对 trace 总时长的宽度比例（0..1），最小可见宽度由渲染层决定。
  widthRatio: number;
  /// 行序号（按时间排序后）。
  row: number;
  /// 单位是微秒：越界或缺失时用 0。
  durationMicros: number;
};

/// 把 span 列表整理成瀑布图行：按开始时间排序、按 `parent_span_id` 计算层级。
///
/// 容错：父 span 缺失（明细被阈值过滤/过期）时该 span 视为根；父指向自己或成环时
/// 截断到最大深度，避免无限递归。
export function waterfallRows(spans: SpanDetail[]): SpanNode[] {
  if (spans.length === 0) {
    return [];
  }
  const sorted = [...spans].sort((a, b) => {
    const byStart = (a.start_unix_nano ?? 0) - (b.start_unix_nano ?? 0);
    return byStart !== 0 ? byStart : a.span_id.localeCompare(b.span_id);
  });

  const start = Math.min(...sorted.map((s) => s.start_unix_nano ?? 0));
  const end = Math.max(
    ...sorted.map((s) => s.end_unix_nano ?? (s.start_unix_nano ?? 0)),
    start + 1,
  );
  const total = Math.max(end - start, 1);

  const byId = new Map(sorted.map((s) => [s.span_id, s]));
  const depthOf = new Map<string, number>();
  const resolveDepth = (span: SpanDetail, seen: Set<string>): number => {
    const cached = depthOf.get(span.span_id);
    if (cached !== undefined) {
      return cached;
    }
    if (
      !span.parent_span_id ||
      span.parent_span_id === span.span_id ||
      !byId.has(span.parent_span_id) ||
      seen.has(span.span_id)
    ) {
      depthOf.set(span.span_id, 0);
      return 0;
    }
    seen.add(span.span_id);
    const parent = byId.get(span.parent_span_id)!;
    const depth = Math.min(resolveDepth(parent, seen) + 1, 64);
    depthOf.set(span.span_id, depth);
    return depth;
  };

  return sorted.map((span, row) => {
    const spanStart = span.start_unix_nano ?? 0;
    const spanEnd = Math.max(span.end_unix_nano ?? spanStart, spanStart);
    return {
      span,
      depth: resolveDepth(span, new Set()),
      offsetRatio: (spanStart - start) / total,
      widthRatio: Math.max((spanEnd - spanStart) / total, 0),
      row,
      durationMicros: span.duration_micros ?? Math.max((spanEnd - spanStart) / 1000, 0),
    };
  });
}

export type TopologyInput = {
  /// 节点（服务名）。
  services: string[];
  /// 边（源、目标、权重）。
  edges: { src: string; dst: string; value: number }[];
};

export type TopologyNode = {
  service: string;
  /// 层号：入度为 0 的服务在第 0 层，其余取「所有上游层号最大值 + 1」。
  layer: number;
  /// 所在层的序号（层内按服务名字典序）。
  slot: number;
  x: number;
  y: number;
  inbound: number;
  outbound: number;
};

export type TopologyEdge = {
  src: string;
  dst: string;
  value: number;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
};

export type TopologyLayout = {
  nodes: TopologyNode[];
  edges: TopologyEdge[];
  width: number;
  height: number;
};

/// 确定性分层布局：按「上游层号最大值 + 1」分层，层内按服务名字典序，坐标均分。
///
/// 不引入图库：同一份数据每次渲染的坐标完全一致（力导向布局会随机抖动，拓扑图不应如此）。
export function layeredTopology(
  input: TopologyInput,
  options: { columnGap?: number; rowGap?: number; padding?: number } = {},
): TopologyLayout {
  const columnGap = options.columnGap ?? 200;
  const rowGap = options.rowGap ?? 84;
  const padding = options.padding ?? 40;

  const services = [...new Set(input.services)].sort();
  const inbound = new Map<string, number>(services.map((s) => [s, 0]));
  const outbound = new Map<string, number>(services.map((s) => [s, 0]));
  const outgoing = new Map<string, string[]>(services.map((s) => [s, []]));
  const incoming = new Map<string, string[]>(services.map((s) => [s, []]));
  for (const edge of input.edges) {
    if (!inbound.has(edge.src) || !inbound.has(edge.dst)) {
      continue;
    }
    inbound.set(edge.dst, inbound.get(edge.dst)! + 1);
    outbound.set(edge.src, outbound.get(edge.src)! + 1);
    outgoing.get(edge.src)!.push(edge.dst);
    incoming.get(edge.dst)!.push(edge.src);
  }

  // 层号 = 最长上游链长度；用 Kahn 拓扑序迭代，环上的服务保持 0 层（不阻塞渲染）。
  const layer = new Map<string, number>(services.map((s) => [s, 0]));
  const remaining = new Map(services.map((s) => [s, inbound.get(s)!]));
  const queue = services.filter((s) => remaining.get(s) === 0);
  const visited = new Set<string>(queue);
  while (queue.length > 0) {
    const current = queue.shift()!;
    for (const next of outgoing.get(current) ?? []) {
      layer.set(next, Math.max(layer.get(next)!, layer.get(current)! + 1));
      remaining.set(next, remaining.get(next)! - 1);
      if (remaining.get(next) === 0 && !visited.has(next)) {
        visited.add(next);
        queue.push(next);
      }
    }
  }

  const byLayer = new Map<number, string[]>();
  for (const service of services) {
    const key = layer.get(service)!;
    byLayer.set(key, [...(byLayer.get(key) ?? []), service]);
  }

  const nodes: TopologyNode[] = [];
  let maxRows = 1;
  for (const [layerIndex, members] of [...byLayer.entries()].sort((a, b) => a[0] - b[0])) {
    members.sort();
    maxRows = Math.max(maxRows, members.length);
    members.forEach((service, slot) => {
      nodes.push({
        service,
        layer: layerIndex,
        slot,
        x: padding + layerIndex * columnGap,
        y: padding + slot * rowGap,
        inbound: inbound.get(service)!,
        outbound: outbound.get(service)!,
      });
    });
  }

  const position = new Map(nodes.map((n) => [n.service, n]));
  const edges: TopologyEdge[] = [];
  for (const edge of input.edges) {
    const from = position.get(edge.src);
    const to = position.get(edge.dst);
    if (!from || !to) {
      continue;
    }
    edges.push({
      src: edge.src,
      dst: edge.dst,
      value: edge.value,
      x1: from.x,
      y1: from.y,
      x2: to.x,
      y2: to.y,
    });
  }

  const maxLayer = nodes.reduce((acc, node) => Math.max(acc, node.layer), 0);
  return {
    nodes,
    edges,
    width: padding * 2 + maxLayer * columnGap,
    height: padding * 2 + Math.max(maxRows - 1, 0) * rowGap,
  };
}

/// 相对时间标签：`1.25ms` / `120µs` / `3.4s`。
export function formatDuration(micros: number): string {
  if (!Number.isFinite(micros) || micros <= 0) {
    return "0µs";
  }
  if (micros < 1_000) {
    return `${Math.round(micros)}µs`;
  }
  if (micros < 1_000_000) {
    const ms = micros / 1_000;
    return `${ms >= 100 ? Math.round(ms) : ms.toFixed(2).replace(/\.?0+$/, "")}ms`;
  }
  const seconds = micros / 1_000_000;
  return `${seconds.toFixed(2).replace(/\.?0+$/, "")}s`;
}
