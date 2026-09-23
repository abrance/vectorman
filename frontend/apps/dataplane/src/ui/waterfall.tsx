import { useMemo, useState } from "react";
import { formatTimestamp } from "@vectorman/primitives";
import type { SpanDetail } from "@vectorman/adapters";
import { formatDuration, waterfallRows } from "../features/apm/layout";
import "./waterfall.css";

type Props = {
  spans: SpanDetail[];
  onSelect?: (span: SpanDetail) => void;
  height?: number;
};

const BAR_HEIGHT = 16;
const ROW_HEIGHT = 26;
const LABEL_WIDTH = 320;
const RIGHT_PAD = 96;

/// span 瀑布图：横轴为 trace 内的相对时间，纵轴按发生顺序，缩进表示父子层级。
///
/// 纯 SVG 手绘（与仓库既有的 `ui/line-chart.tsx` 保持一致）：不引入图表库，
/// 布局由 `waterfallRows` 纯函数给出，坐标来自 `start_unix_nano`/`duration_micros`,
/// 因此不受渲染时机影响。
export function Waterfall({ spans, onSelect, height }: Props) {
  const [hover, setHover] = useState<string | null>(null);
  const rows = useMemo(() => waterfallRows(spans), [spans]);
  const totalMicros = rows.reduce((acc, row) => Math.max(acc, row.span.duration_micros), 0);
  const chartHeight = height ?? Math.max(rows.length * ROW_HEIGHT + 16, 120);

  if (rows.length === 0) {
    return <div className="waterfall-empty">该 trace 没有明细（可能已过保留期或被明细阈值过滤）</div>;
  }

  return (
    <div className="waterfall">
      <div className="waterfall-scale">
        <span>0</span>
        <span>{formatDuration(totalMicros / 2)}</span>
        <span>{formatDuration(totalMicros)}</span>
      </div>
      <svg
        width="100%"
        height={chartHeight}
        viewBox={`0 0 1000 ${chartHeight}`}
        preserveAspectRatio="none"
        role="img"
        aria-label="trace span 瀑布图"
      >
        {rows.map((row) => {
          const labelRoom = LABEL_WIDTH / 1000;
          const track = 1 - labelRoom - RIGHT_PAD / 1000;
          const x = (labelRoom + row.offsetRatio * track) * 1000;
          const width = Math.max(row.widthRatio * track * 1000, 2);
          const y = 8 + row.row * ROW_HEIGHT;
          const failed = row.span.status_code === "error";
          const active = hover === row.span.span_id;
          return (
            <g
              key={row.span.span_id}
              onMouseEnter={() => setHover(row.span.span_id)}
              onMouseLeave={() => setHover(null)}
              onClick={() => onSelect?.(row.span)}
              className="waterfall-row"
            >
              <rect x={0} y={y - 2} width={1000} height={ROW_HEIGHT} className="waterfall-hit" />
              <text
                x={8 + row.depth * 14}
                y={y + 12}
                className={failed ? "waterfall-label error" : "waterfall-label"}
              >
                {truncate(`${row.span.service} · ${row.span.name}`, 46)}
              </text>
              <rect
                x={x}
                y={y}
                width={width}
                height={BAR_HEIGHT}
                rx={3}
                className={failed ? "waterfall-bar error" : "waterfall-bar"}
                opacity={active ? 1 : 0.85}
              />
              <text x={Math.min(x + width + 6, 990)} y={y + 12} className="waterfall-duration">
                {formatDuration(row.span.duration_micros)}
              </text>
            </g>
          );
        })}
      </svg>
      <div className="waterfall-hint">
        {rows.length} 个 span，总时长 {formatDuration(totalMicros)}
        {spans[0]?.start_unix_nano
          ? `，起始 ${formatTimestamp(String(Math.round(spans[0].start_unix_nano / 1000)))}（点击查看详情）`
          : "（点击查看详情）"}
      </div>
    </div>
  );
}

function truncate(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}
