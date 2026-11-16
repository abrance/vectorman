import { Button, DatePicker, Input, Segmented, Select, Space, Spin } from "antd";
import type { Dayjs } from "dayjs";
import dayjs from "dayjs";
import { useCallback, useEffect, useMemo, useReducer, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { toUserMessage, type QueryRecord } from "@vectorman/primitives";
import type { PromEnvelope } from "@vectorman/adapters";
import {
  discoveryExpr,
  latestValue,
  metricExpr,
  metricNames,
  promSeries,
  rangeParams,
} from "../features/prom";
import { useSpecCatalog } from "../features/use-collect";
import { useMetrics } from "../features/use-metrics";
import { LineChart } from "../ui/line-chart";
import "./metrics-page.css";

const PRESETS = [
  { key: "15m", label: "15 分钟", amount: 15, unit: "minute" as const },
  { key: "1h", label: "1 小时", amount: 1, unit: "hour" as const },
  { key: "6h", label: "6 小时", amount: 6, unit: "hour" as const },
  { key: "24h", label: "24 小时", amount: 24, unit: "hour" as const },
] as const;

type PresetKey = (typeof PRESETS)[number]["key"] | "custom";

function presetRange(key: Exclude<PresetKey, "custom">): [Dayjs, Dayjs] {
  const preset = PRESETS.find((p) => p.key === key)!;
  return [dayjs().subtract(preset.amount, preset.unit), dayjs()];
}

function formatReading(value: number | undefined): string {
  if (value === undefined) {
    return "-";
  }
  return Math.abs(value) >= 10 ? value.toFixed(1) : value.toFixed(2);
}

export function MetricsPage() {
  const [params, setParams] = useSearchParams();
  const [agentId, setAgentId] = useState(params.get("agent_id") ?? "");
  const [itemId, setItemId] = useState(params.get("data_id") ?? "");
  const [preset, setPreset] = useState<PresetKey>("1h");
  const [range, setRange] = useState<[Dayjs, Dayjs]>(() => presetRange("1h"));
  const [custom, setCustom] = useState("");
  // 用户显式选择的指标；`undefined` = 还没选过（发现到指标后自动全选）。
  const [selected, setSelected] = useState<string[] | undefined>(undefined);
  // 手动/定时刷新计数：自增让各面板重查。
  const [nonce, bump] = useReducer((n: number) => n + 1, 0);
  const { list, agents, refresh: loadCatalog, loadAgents } = useSpecCatalog();
  const discovery = useMetrics("metrics.discovery");
  const customQuery = useMetrics("metrics.custom");
  const discoveryRun = discovery.run;
  const customRun = customQuery.run;

  const filters = useMemo(() => ({ agentId, itemId }), [agentId, itemId]);
  const discoverExpr = discoveryExpr(filters);

  // 时间窗 → query_range 参数（秒）；preset 与自定义都落到 start/end/step。
  const rangeWindow = useMemo(() => {
    const [from, to] = preset === "custom" ? range : presetRange(preset);
    return rangeParams(from.valueOf(), to.valueOf());
  }, [preset, range]);

  const runDiscovery = useCallback(() => {
    if (!discoverExpr) {
      return;
    }
    // 发现用当前时间窗的范围查询（`count by (__name__)`），只统计窗内出现过的指标。
    void discoveryRun(discoverExpr, rangeWindow.start, rangeWindow.end, rangeWindow.step);
  }, [discoveryRun, discoverExpr, rangeWindow]);

  useEffect(() => {
    void loadCatalog();
    void loadAgents();
  }, [loadCatalog, loadAgents]);

  useEffect(() => {
    runDiscovery();
  }, [runDiscovery]);

  // 筛选条件变化 → 回到「自动全选」，让新采集项的指标自动出现。
  useEffect(() => {
    setSelected(undefined);
  }, [agentId, itemId]);

  const discovered = useMemo(() => metricNames(discovery.result.data), [discovery.result.data]);
  const activeMetrics = selected ?? discovered;

  // 定时刷新（自定义时间窗不自动跑）。
  useEffect(() => {
    if (preset === "custom") {
      return;
    }
    const timer = globalThis.setInterval(() => {
      runDiscovery();
      bump();
    }, 30_000);
    return () => globalThis.clearInterval(timer);
  }, [preset, runDiscovery]);

  const writeFilters = (nextAgent: string, nextItem: string) => {
    setAgentId(nextAgent);
    setItemId(nextItem);
    const next = new URLSearchParams();
    if (nextAgent) {
      next.set("agent_id", nextAgent);
    }
    if (nextItem) {
      next.set("data_id", nextItem);
    }
    setParams(next, { replace: true });
  };

  const metricItems = (list.data ?? []).filter((item) => item.kind === "metrics_host");
  const agentOptions = (agents.data ?? []).map((agent) => ({
    value: agent.agent_id,
    label: agent.host_id ? `${agent.agent_id} · ${agent.host_id}` : agent.agent_id,
  }));
  const itemOptions = metricItems.map((item) => ({
    value: item.item_id,
    label: item.name,
  }));
  if (itemId && !itemOptions.some((o) => o.value === itemId)) {
    itemOptions.push({ value: itemId, label: itemId });
  }
  if (agentId && !agentOptions.some((o) => o.value === agentId)) {
    agentOptions.push({ value: agentId, label: agentId });
  }
  // 指标候选 = 发现的指标 ∪ 已选（保留已选避免被过滤掉）。
  const metricOptions = [...new Set([...discovered, ...activeMetrics])]
    .sort()
    .map((name) => ({ value: name, label: name }));

  const runCustom = () => {
    if (!custom.trim()) {
      return;
    }
    const [from, to] = preset === "custom" ? range : presetRange(preset);
    const { start, end, step } = rangeParams(from.valueOf(), to.valueOf());
    void customRun(custom.trim(), start, end, step);
  };

  return (
    <div className="metrics-page">
      <div className="metrics-page__head">
        <div>
          <h1>指标检索</h1>
          <p>按采集项与时间窗查看该采集项上报的指标，采集链路里的「数据检索」会带上过滤条件。</p>
        </div>
      </div>

      <div className="metrics-toolbar">
        <label className="metrics-field">
          <span>时间窗</span>
          <Segmented
            value={preset}
            options={[
              ...PRESETS.map((p) => ({ value: p.key, label: p.label })),
              { value: "custom", label: "自定义" },
            ]}
            onChange={(value) => {
              const key = String(value) as PresetKey;
              setPreset(key);
              if (key !== "custom") {
                setRange(presetRange(key));
              }
            }}
          />
        </label>
        <label className="metrics-field">
          <span>自定义范围</span>
          <DatePicker.RangePicker
            showTime
            value={range}
            onChange={(value) => {
              if (value?.[0] && value[1]) {
                setPreset("custom");
                setRange([value[0], value[1]]);
              }
            }}
          />
        </label>
        <label className="metrics-field">
          <span>Agent</span>
          <Select
            allowClear
            showSearch
            placeholder="全部"
            value={agentId || undefined}
            options={agentOptions}
            style={{ width: 200 }}
            onChange={(value) => writeFilters(value ?? "", itemId)}
          />
        </label>
        <label className="metrics-field">
          <span>采集项</span>
          <Select
            allowClear
            showSearch
            placeholder="全部主机指标"
            value={itemId || undefined}
            options={itemOptions}
            style={{ width: 220 }}
            onChange={(value) => writeFilters(agentId, value ?? "")}
          />
        </label>
        <label className="metrics-field">
          <span>指标</span>
          <Select
            mode="multiple"
            allowClear
            showSearch
            placeholder={discovered.length ? "选择要展示的指标" : "先选采集项"}
            value={activeMetrics}
            options={metricOptions}
            style={{ minWidth: 260, maxWidth: 420 }}
            onChange={(value) => setSelected(value)}
          />
        </label>
        <div className="metrics-toolbar__actions">
          <Button type="primary" onClick={bump}>
            查询
          </Button>
        </div>
      </div>

      <div className="metrics-expr" title="发现表达式">
        {discoverExpr || "（未选 Agent / 采集项，无法发现指标）"}
      </div>

      {discovery.result.status === "error" ? (
        <div className="metrics-empty">
          指标发现失败：{discovery.result.error ? toUserMessage(discovery.result.error) : "查询失败"}
        </div>
      ) : discovery.result.status === "loading" && discovered.length === 0 ? (
        <div style={{ display: "flex", justifyContent: "center", padding: 48 }}>
          <Spin />
        </div>
      ) : activeMetrics.length === 0 ? (
        <div className="metrics-empty">该采集项/时间窗下没有发现可展示的指标。</div>
      ) : (
        <div className="metrics-grid">
          {activeMetrics.map((name) => (
            <MetricPanel key={name} name={name} filters={filters} window={rangeWindow} nonce={nonce} />
          ))}
        </div>
      )}

      <div className="metrics-custom">
        <h2>自定义 PromQL</h2>
        <Space.Compact style={{ width: "100%" }}>
          <Input
            placeholder='例如 cpu_usage{agent_id="gs"}'
            value={custom}
            onChange={(e) => setCustom(e.target.value)}
            onPressEnter={runCustom}
          />
          <Button type="primary" onClick={runCustom}>
            运行
          </Button>
        </Space.Compact>
        {customQuery.result.status !== "idle" ? (
          <div style={{ marginTop: 12 }}>
            <MetricChart result={customQuery.result} unit="" emptyText="表达式没有返回序列" />
          </div>
        ) : null}
      </div>
    </div>
  );
}

/// 单个指标的折线面板：自己订阅一条 `useMetrics` 通道（按指标名区分 key），
/// 表达式或时间窗变化时重查——不能所有面板共用一条通道，否则互相覆盖只剩最后一条。
function MetricPanel({
  name,
  filters,
  window,
  nonce,
}: {
  name: string;
  filters: { agentId: string; itemId: string };
  window: { start: string; end: string; step: string };
  nonce: number;
}) {
  const { result, run } = useMetrics(`metrics.metric.${name}`);
  const expr = metricExpr(name, filters);
  const { start, end, step } = window;
  const runRef = run;

  useEffect(() => {
    void runRef(expr, start, end, step);
  }, [runRef, expr, start, end, step, nonce]);

  const reading = latestValue(promSeries(result.data?.data));
  return (
    <section className="metric-panel">
      <div className="metric-panel__top">
        <div>
          <h2>{name}</h2>
          <code>{expr}</code>
        </div>
        <div className="metric-panel__reading">{formatReading(reading)}</div>
      </div>
      <div className="metric-panel__body">
        <MetricChart result={result} unit="" />
      </div>
    </section>
  );
}

function MetricChart({
  result,
  unit,
  emptyText,
}: {
  result: QueryRecord<PromEnvelope>;
  unit: string;
  emptyText?: string;
}) {
  if (result.status === "error") {
    return <div className="metric-panel__error">{result.error ? toUserMessage(result.error) : "查询失败"}</div>;
  }
  if (result.status === "loading" && !result.data) {
    return (
      <div style={{ display: "flex", justifyContent: "center", padding: 48 }}>
        <Spin />
      </div>
    );
  }
  return <LineChart series={promSeries(result.data?.data)} unit={unit} emptyText={emptyText} />;
}
