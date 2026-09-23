import { Alert, Button, Card, Col, DatePicker, Form, Input, Row, Segmented, Select, Space, Spin } from "antd";
import type { Dayjs } from "dayjs";
import dayjs from "dayjs";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useSearchParams } from "react-router-dom";
import type { QueryRecord } from "@vectorman/primitives";
import type { PromEnvelope } from "@vectorman/adapters";
import { latestValue, promSeries, rangeParams } from "../features/prom";
import { formatDuration } from "../features/apm/layout";
import { useServices } from "../features/apm/use-apm";
import { useMetrics } from "../features/use-metrics";
import { LineChart } from "../ui/line-chart";

const PRESETS = [
  { key: "15m", label: "15 分钟", amount: 15, unit: "minute" as const },
  { key: "1h", label: "1 小时", amount: 1, unit: "hour" as const },
  { key: "6h", label: "6 小时", amount: 6, unit: "hour" as const },
] as const;

type PresetKey = (typeof PRESETS)[number]["key"];

/// APM 指标页：服务的请求量/错误数/延迟分位（数据来自聚合任务写入的 `apm_service_*`）。
///
/// 指标口径（`observability-data-model` 命名表）：`apm_service_requests_total` 按
/// `status` 分组、`apm_service_duration_micros` 用 label `field` 区分
/// `avg/p50/p95/p99/max`。服务端不做除法，错误率在前端由两份查询的读数相除得到。
export function ApmPage() {
  const [params] = useSearchParams();
  const { result: services, run: loadServices } = useServices();
  // 只取稳定的 `run` 与 `result`：`useMetrics` 每次渲染都会返回新对象，
  // 直接把它放进 useCallback 依赖会导致「每渲染都触发查询」的无限循环。
  const { result: requestsResult, run: runRequests } = useMetrics("apm.requests");
  const { result: errorsResult, run: runErrors } = useMetrics("apm.errors");
  const { result: durationResult, run: runDuration } = useMetrics("apm.duration");
  const [service, setService] = useState(params.get("service") ?? "");
  const [operation, setOperation] = useState(params.get("operation") ?? "");
  const [preset, setPreset] = useState<PresetKey>("1h");
  const [range, setRange] = useState<[Dayjs, Dayjs]>(() => [dayjs().subtract(1, "hour"), dayjs()]);

  useEffect(() => {
    void loadServices();
  }, [loadServices]);

  useEffect(() => {
    const first = services.data?.[0]?.service;
    if (!service && first) {
      setService(first);
    }
  }, [services.data, service]);

  const matcher = useMemo(() => {
    const parts: string[] = [];
    if (service) {
      parts.push(`service="${service}"`);
    }
    if (operation.trim()) {
      parts.push(`operation="${operation.trim()}"`);
    }
    return parts.join(",");
  }, [service, operation]);

  const run = useCallback(() => {
    const window: [Dayjs, Dayjs] = range;
    const { start, end, step } = rangeParams(window[0].valueOf(), window[1].valueOf());
    const selector = matcher ? `{${matcher}}` : "";
    void runRequests(`sum by (status) (apm_service_requests_total${selector})`, start, end, step);
    void runErrors(`sum(apm_service_errors_total${selector})`, start, end, step);
    void runDuration(`apm_service_duration_micros${selector}`, start, end, step);
  }, [range, matcher, runRequests, runErrors, runDuration]);

  useEffect(run, [run]);

  // 注意：`promSeries` 吃的是 envelope 内层的 `data`（与指标页一致），
  // 传外层会让序列恒为空——曲线看着「没样本」，很容易误判成采集没数据。
  const latestRequests = latestValue(promSeries(requestsResult.data?.data));
  const latestErrors = latestValue(promSeries(errorsResult.data?.data));
  const errorRate =
    latestRequests && latestRequests > 0 && latestErrors !== undefined
      ? (latestErrors / latestRequests) * 100
      : undefined;
  const latency = promSeries(durationResult.data?.data)
    // 分位由 label `field` 区分（服务端命名规范），从展示标签里取回该维度。
    .map((series) => ({
      field: /field=([^\s,]+)/.exec(series.label)?.[1] ?? series.label,
      value: latestValue([series]),
    }))
    .filter((item) => item.value !== undefined)
    .sort((a, b) => (a.field > b.field ? 1 : -1));

  return (
    <Space direction="vertical" style={{ width: "100%" }} size="middle">
      <Card size="small">
        <Form layout="inline">
          <Form.Item label="服务">
            <Select
              showSearch
              style={{ width: 220 }}
              value={service || undefined}
              placeholder={services.status === "loading" ? "加载中…" : "选择服务"}
              onChange={setService}
              options={(services.data ?? []).map((row) => ({
                value: row.service,
                label: `${row.service}（${row.instance_count} 实例）`,
              }))}
            />
          </Form.Item>
          <Form.Item label="操作">
            <Input
              allowClear
              style={{ width: 200 }}
              value={operation}
              onChange={(e) => setOperation(e.target.value)}
              placeholder="span 名称，可留空"
            />
          </Form.Item>
          <Form.Item label="时间">
            <Segmented
              value={preset}
              onChange={(value) => {
                const key = value as PresetKey;
                setPreset(key);
                const preset = PRESETS.find((p) => p.key === key)!;
                setRange([dayjs().subtract(preset.amount, preset.unit), dayjs()]);
              }}
              options={PRESETS.map((p) => ({ value: p.key, label: p.label }))}
            />
          </Form.Item>
          <Form.Item>
            <Button type="primary" onClick={run}>
              查询
            </Button>
          </Form.Item>
        </Form>
      </Card>

      {!service && services.status === "success" && (services.data?.length ?? 0) === 0 && (
        <Alert
          type="info"
          showIcon
          message="还没有服务数据"
          description="服务清单来自 span 的 resource（service.name）。确认应用已上报 trace，且采集项启用、明细阈值未把 span 全部过滤。"
        />
      )}

      <Row gutter={[16, 16]}>
        <Col span={12}>
          <MetricCard title="请求量（每分钟，按状态）" unit="次" record={requestsResult} empty="该窗口没有请求样本" />
        </Col>
        <Col span={12}>
          <MetricCard title="错误数（每分钟）" unit="次" record={errorsResult} empty="该窗口没有错误样本" />
        </Col>
      </Row>

      <Row gutter={[16, 16]}>
        <Col span={24}>
          <Card
            size="small"
            title="延迟（平均值与分位）"
            extra={
              <Space size="large">
                <span>错误率 {errorRate === undefined ? "-" : `${errorRate.toFixed(2)}%`}</span>
                {latency.map((item) => (
                  <span key={item.field}>
                    {item.field} {formatDuration(item.value ?? 0)}
                  </span>
                ))}
              </Space>
            }
          >
            {durationResult.status === "loading" ? (
              <Spin />
            ) : (
              <LineChart
                series={promSeries(durationResult.data?.data)}
                unit="µs"
                emptyText="该窗口没有延迟样本"
              />
            )}
          </Card>
        </Col>
      </Row>
    </Space>
  );
}

function MetricCard({
  title,
  unit,
  record,
  empty,
}: {
  title: string;
  unit: string;
  record: QueryRecord<PromEnvelope>;
  empty: string;
}) {
  return (
    <Card size="small" title={title}>
      {record.status === "loading" ? (
        <Spin />
      ) : (
        <LineChart series={promSeries(record.data?.data)} unit={unit} emptyText={empty} />
      )}
    </Card>
  );
}
