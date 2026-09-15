import type { MetricSeries } from "../features/prom";

const COLORS = ["#1677ff", "#52c41a", "#fa8c16", "#eb2f96", "#722ed1"];
const WIDTH = 600;
const PADDING = 24;

/// 极简多序列折线图；无第三方图表依赖。
export function LineChart({ series, height = 200 }: { series: MetricSeries[]; height?: number }) {
  if (series.length === 0) {
    return <div style={{ color: "#999" }}>暂无数据</div>;
  }
  const points = series.flatMap((s) => s.points);
  const xs = points.map((p) => p[0]);
  const ys = points.map((p) => p[1]);
  const minX = Math.min(...xs);
  const maxX = Math.max(...xs);
  const minY = Math.min(...ys);
  const maxY = Math.max(...ys);
  const spanX = maxX - minX || 1;
  const spanY = maxY - minY || 1;
  const plotW = WIDTH - PADDING * 2;
  const plotH = height - PADDING * 2;
  const scaleX = (x: number) => PADDING + ((x - minX) / spanX) * plotW;
  const scaleY = (y: number) => PADDING + plotH - ((y - minY) / spanY) * plotH;

  return (
    <div data-testid="line-chart">
      <svg width="100%" viewBox={`0 0 ${WIDTH} ${height}`} role="img">
        <line
          x1={PADDING}
          y1={PADDING + plotH}
          x2={PADDING + plotW}
          y2={PADDING + plotH}
          stroke="#d9d9d9"
        />
        {series.map((s, i) => (
          <polyline
            key={s.label}
            fill="none"
            stroke={COLORS[i % COLORS.length]}
            strokeWidth={2}
            points={s.points.map(([x, y]) => `${scaleX(x)},${scaleY(y)}`).join(" ")}
          />
        ))}
      </svg>
      <div style={{ display: "flex", flexWrap: "wrap", gap: 12, fontSize: 12 }}>
        {series.map((s, i) => (
          <span key={s.label} style={{ color: COLORS[i % COLORS.length] }}>
            {s.label} (最新 {s.points.length > 0 ? s.points[s.points.length - 1][1].toFixed(2) : "-"})
          </span>
        ))}
      </div>
    </div>
  );
}
