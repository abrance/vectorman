import { Button, DatePicker, Input, Segmented, Select, Space, Spin } from "antd";
import type { Dayjs } from "dayjs";
import dayjs from "dayjs";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useSearchParams } from "react-router-dom";
import type { QueryRecord } from "@vectorman/primitives";
import type { PromEnvelope } from "@vectorman/adapters";
import { latestValue, metricExpr, promSeries, rangeParams } from "../features/prom";
import { useCollectItems } from "../features/use-collect";
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
  const { list, agents, refresh: loadCatalog, loadAgents } = useCollectItems();
  const cpu = useMetrics("metrics.cpu");
  const mem = useMetrics("metrics.mem");
  const customQuery = useMetrics("metrics.custom");
  const cpuRun = cpu.run;
  const memRun = mem.run;
  const customRun = customQuery.run;

  const filters = useMemo(() => ({ agentId, itemId }), [agentId, itemId]);
  const cpuExpr = metricExpr("cpu_usage", filters);
  const memExpr = metricExpr("mem_usage", filters);

  const runRange = useCallback(() => {
    const window = preset === "custom" ? range : presetRange(preset);
    const { start, end, step } = rangeParams(window[0].valueOf(), window[1].valueOf());
    void cpuRun(cpuExpr, start, end, step);
    void memRun(memExpr, start, end, step);
  }, [cpuRun, memRun, cpuExpr, memExpr, preset, range]);

  const runPreset = useCallback(() => {
    if (preset === "custom") {
      return;
    }
    const window = presetRange(preset);
    const { start, end, step } = rangeParams(window[0].valueOf(), window[1].valueOf());
    void cpuRun(cpuExpr, start, end, step);
    void memRun(memExpr, start, end, step);
  }, [cpuRun, memRun, cpuExpr, memExpr, preset]);

  useEffect(() => {
    void loadCatalog();
    void loadAgents();
  }, [loadCatalog, loadAgents]);

  useEffect(() => {
    runPreset();
  }, [runPreset]);

  useEffect(() => {
    if (preset === "custom") {
      return;
    }
    const timer = globalThis.setInterval(runPreset, 30_000);
    return () => globalThis.clearInterval(timer);
  }, [preset, runPreset]);

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

  const runCustom = () => {
    if (!custom.trim()) {
      return;
    }
    const window = preset === "custom" ? range : presetRange(preset);
    const { start, end, step } = rangeParams(window[0].valueOf(), window[1].valueOf());
    void customRun(custom.trim(), start, end, step);
  };

  return (
    <div className="metrics-page">
      <div className="metrics-page__head">
        <div>
          <h1>指标检索</h1>
          <p>按采集项与时间窗查看主机 CPU / 内存，采集链路里的「数据检索」会带上过滤条件。</p>
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
        <div className="metrics-toolbar__actions">
          <Button type="primary" onClick={runRange}>
            查询
          </Button>
        </div>
      </div>

      <div className="metrics-expr" title="当前查询">
        {cpuExpr}
        <span style={{ opacity: 0.45 }}> · </span>
        {memExpr}
      </div>

      <div className="metrics-grid">
        <MetricPanel
          title="CPU 使用率"
          measurement="cpu_usage"
          unit="%"
          accent="#2f6bff"
          result={cpu.result}
        />
        <MetricPanel
          title="内存使用率"
          measurement="mem_usage"
          unit="%"
          accent="#0f9d8e"
          result={mem.result}
        />
      </div>

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

function MetricPanel({
  title,
  measurement,
  unit,
  accent,
  result,
}: {
  title: string;
  measurement: string;
  unit: string;
  accent: string;
  result: QueryRecord<PromEnvelope>;
}) {
  const series = promSeries(result.data?.data);
  const reading = latestValue(series);
  return (
    <section className="metric-panel" style={{ ["--accent" as string]: accent }}>
      <div className="metric-panel__top">
        <div>
          <h2>{title}</h2>
          <code>{measurement}</code>
        </div>
        <div className="metric-panel__reading">
          {formatReading(reading)}
          <small>{unit}</small>
        </div>
      </div>
      <div className="metric-panel__body">
        <MetricChart result={result} unit={unit} />
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
    return <div className="metric-panel__error">{result.error?.message ?? "查询失败"}</div>;
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
