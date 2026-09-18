import { useMemo, useState } from "react";
import { axisTimeLabel, type MetricSeries } from "../features/prom";
import "./line-chart.css";

const COLORS = ["#2f6bff", "#0f9d8e", "#c45c14", "#7a3ee6", "#c4396e"];
const WIDTH = 720;
const PAD = { l: 46, r: 14, t: 14, b: 30 };

type Props = {
  series: MetricSeries[];
  height?: number;
  unit?: string;
  emptyText?: string;
};

/// 多序列折线图：时间横轴、数值纵轴、面积填充、悬停读数。
export function LineChart({ series, height = 220, unit = "", emptyText = "该时间窗没有样本" }: Props) {
  const [hoverX, setHoverX] = useState<number | null>(null);

  const layout = useMemo(() => layoutChart(series, height), [series, height]);

  if (!layout) {
    return (
      <div className="line-chart line-chart--empty" data-testid="line-chart">
        {emptyText}
      </div>
    );
  }

  const { minX, maxX, spanX, plotW, plotH, scaleX, scaleY, yTicks, xTicks } = layout;
  const hoverT =
    hoverX === null ? null : minX + ((clamp(hoverX, PAD.l, PAD.l + plotW) - PAD.l) / plotW) * spanX;
  const hoverPoints =
    hoverT === null
      ? []
      : series.map((s, i) => ({
          ...nearest(s.points, hoverT),
          label: s.label,
          color: COLORS[i % COLORS.length],
        }));

  return (
    <div className="line-chart" data-testid="line-chart">
      <svg
        width="100%"
        viewBox={`0 0 ${WIDTH} ${height}`}
        role="img"
        onMouseMove={(e) => {
          const rect = e.currentTarget.getBoundingClientRect();
          setHoverX(((e.clientX - rect.left) / rect.width) * WIDTH);
        }}
        onMouseLeave={() => setHoverX(null)}
      >
        {yTicks.map((y) => (
          <g key={y}>
            <line
              x1={PAD.l}
              x2={PAD.l + plotW}
              y1={scaleY(y)}
              y2={scaleY(y)}
              stroke="#eef0f3"
            />
            <text x={PAD.l - 8} y={scaleY(y) + 4} textAnchor="end" className="line-chart__tick">
              {formatTick(y)}
              {unit}
            </text>
          </g>
        ))}
        {xTicks.map((x) => (
          <text
            key={x}
            x={scaleX(x)}
            y={height - 8}
            textAnchor="middle"
            className="line-chart__tick"
          >
            {axisTimeLabel(x, maxX - minX)}
          </text>
        ))}
        <line
          x1={PAD.l}
          y1={PAD.t + plotH}
          x2={PAD.l + plotW}
          y2={PAD.t + plotH}
          stroke="#d7dbe3"
        />
        {series.map((s, i) => {
          const color = COLORS[i % COLORS.length];
          const pts = s.points.map(([x, y]) => `${scaleX(x)},${scaleY(y)}`).join(" ");
          const first = s.points[0];
          const last = s.points[s.points.length - 1];
          const area =
            first && last
              ? `${scaleX(first[0])},${PAD.t + plotH} ${pts} ${scaleX(last[0])},${PAD.t + plotH}`
              : "";
          return (
            <g key={s.label}>
              {area ? <polygon points={area} fill={color} opacity={0.12} /> : null}
              <polyline fill="none" stroke={color} strokeWidth={2} strokeLinejoin="round" points={pts} />
              {s.points.length === 1 ? (
                <circle cx={scaleX(s.points[0][0])} cy={scaleY(s.points[0][1])} r={3.5} fill={color} />
              ) : null}
            </g>
          );
        })}
        {hoverT !== null ? (
          <line
            x1={scaleX(hoverT)}
            x2={scaleX(hoverT)}
            y1={PAD.t}
            y2={PAD.t + plotH}
            stroke="#8b93a2"
            strokeDasharray="3 3"
          />
        ) : null}
        {hoverPoints.map((p) =>
          p.point ? (
            <circle key={p.label} cx={scaleX(p.point[0])} cy={scaleY(p.point[1])} r={3.5} fill={p.color} />
          ) : null,
        )}
      </svg>
      <div className="line-chart__legend">
        {hoverPoints.length > 0
          ? hoverPoints.map((p) => (
              <span key={p.label} style={{ color: p.color }}>
                {p.label} {p.point ? `${formatTick(p.point[1])}${unit}` : "—"}
                {p.point ? ` · ${axisTimeLabel(p.point[0], maxX - minX)}` : ""}
              </span>
            ))
          : series.map((s, i) => {
              const last = s.points[s.points.length - 1];
              return (
                <span key={s.label} style={{ color: COLORS[i % COLORS.length] }}>
                  {s.label}
                  {last ? `  ${formatTick(last[1])}${unit}` : ""}
                </span>
              );
            })}
      </div>
    </div>
  );
}

function layoutChart(series: MetricSeries[], height: number) {
  const points = series.flatMap((s) => s.points);
  if (points.length === 0) {
    return null;
  }
  const xs = points.map((p) => p[0]);
  const ys = points.map((p) => p[1]);
  const minX = Math.min(...xs);
  const maxX = Math.max(...xs);
  const rawMinY = Math.min(...ys);
  const rawMaxY = Math.max(...ys);
  const padY = (rawMaxY - rawMinY) * 0.12 || Math.max(Math.abs(rawMaxY) * 0.08, 1);
  const minY = rawMinY - padY;
  const maxY = rawMaxY + padY;
  const spanX = maxX - minX || 1;
  const spanY = maxY - minY || 1;
  const plotW = WIDTH - PAD.l - PAD.r;
  const plotH = height - PAD.t - PAD.b;
  const scaleX = (x: number) => PAD.l + ((x - minX) / spanX) * plotW;
  const scaleY = (y: number) => PAD.t + plotH - ((y - minY) / spanY) * plotH;
  const yTicks = ticks(minY, maxY, 4);
  const xTicks = ticks(minX, maxX, 4);
  return { minX, maxX, minY, maxY, spanX, plotW, plotH, scaleX, scaleY, yTicks, xTicks };
}

function ticks(min: number, max: number, count: number): number[] {
  if (count <= 1 || max === min) {
    return [min];
  }
  const step = (max - min) / (count - 1);
  return Array.from({ length: count }, (_, i) => min + step * i);
}

function nearest(points: [number, number][], t: number): { point: [number, number] | null } {
  if (points.length === 0) {
    return { point: null };
  }
  let best = points[0];
  let bestD = Math.abs(points[0][0] - t);
  for (const p of points) {
    const d = Math.abs(p[0] - t);
    if (d < bestD) {
      best = p;
      bestD = d;
    }
  }
  return { point: best };
}

function formatTick(value: number): string {
  const abs = Math.abs(value);
  if (abs >= 100) {
    return value.toFixed(0);
  }
  if (abs >= 10) {
    return value.toFixed(1);
  }
  return value.toFixed(2);
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}
