import { Button, Card, Collapse, DatePicker, Input, Space, Typography } from "antd";
import dayjs, { type Dayjs } from "dayjs";
import { useCallback, useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { promSeries, rangeParams } from "../features/prom";
import { useMetrics } from "../features/use-metrics";
import { LineChart } from "../ui/line-chart";

function expr(measurement: string, agentId: string): string {
  const id = agentId.trim();
  return id ? `${measurement}{agent_id="${id}"}` : measurement;
}

export function MetricsPage() {
  const [params] = useSearchParams();
  const dataId = params.get("data_id") ?? "";
  const [agentId, setAgentId] = useState(params.get("agent_id") ?? "");
  const [range, setRange] = useState<[Dayjs, Dayjs]>([
    dayjs().subtract(1, "hour"),
    dayjs(),
  ]);
  const [custom, setCustom] = useState("");
  const cpu = useMetrics("metrics.cpu");
  const mem = useMetrics("metrics.mem");
  const customQuery = useMetrics("metrics.custom");
  const cpuRun = cpu.run;
  const memRun = mem.run;

  const refresh = useCallback(() => {
    const { start, end, step } = rangeParams(range[0].valueOf(), range[1].valueOf());
    void cpuRun(expr("cpu_usage", agentId), start, end, step);
    void memRun(expr("mem_usage", agentId), start, end, step);
  }, [cpuRun, memRun, agentId, range]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const runCustom = () => {
    if (!custom.trim()) {
      return;
    }
    const { start, end, step } = rangeParams(range[0].valueOf(), range[1].valueOf());
    void customQuery.run(custom.trim(), start, end, step);
  };

  return (
    <Space direction="vertical" style={{ width: "100%" }} size="middle">
      <Space wrap>
        <Input
          placeholder="agent_id（可选）"
          value={agentId}
          onChange={(e) => setAgentId(e.target.value)}
          style={{ width: 220 }}
        />
        <DatePicker.RangePicker
          showTime
          value={range}
          onChange={(value) => {
            if (value?.[0] && value[1]) {
              setRange([value[0], value[1]]);
            }
          }}
        />
        <Button type="primary" onClick={refresh}>
          查询
        </Button>
        {dataId ? <Typography.Text type="secondary">data_id={dataId}</Typography.Text> : null}
      </Space>

      <Card size="small" title="cpu_usage">
        <LineChart series={promSeries(cpu.result.data?.data)} />
      </Card>
      <Card size="small" title="mem_usage">
        <LineChart series={promSeries(mem.result.data?.data)} />
      </Card>

      <Collapse
        items={[
          {
            key: "promql",
            label: "自定义 PromQL",
            children: (
              <Space.Compact style={{ width: "100%" }}>
                <Input
                  placeholder="例如 rate(cpu_usage[5m])"
                  value={custom}
                  onChange={(e) => setCustom(e.target.value)}
                />
                <Button type="primary" onClick={runCustom}>
                  查询
                </Button>
              </Space.Compact>
            ),
          },
        ]}
      />
      {customQuery.result.status !== "idle" && (
        <Card size="small" title="自定义表达式">
          <LineChart series={promSeries(customQuery.result.data?.data)} />
        </Card>
      )}
    </Space>
  );
}
