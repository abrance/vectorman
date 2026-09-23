import { Button, Card, DatePicker, Empty, Form, InputNumber, Input, Segmented, Space, Table, Tabs, Tag } from "antd";
import type { Dayjs } from "dayjs";
import dayjs from "dayjs";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import type { EdgeSearchRequest, EdgeRow } from "@vectorman/adapters";
import { formatTimestamp } from "@vectorman/primitives";
import { aggregateEdges, edgeStyle, topologyInput } from "../features/apm/aggregate";
import { formatDuration } from "../features/apm/layout";
import { layeredTopology } from "../features/apm/layout";
import { useEdges } from "../features/apm/use-apm";

const PRESETS = [
  { key: "1h", label: "1 小时", amount: 1, unit: "hour" as const },
  { key: "6h", label: "6 小时", amount: 6, unit: "hour" as const },
  { key: "24h", label: "24 小时", amount: 24, unit: "hour" as const },
] as const;

type PresetKey = (typeof PRESETS)[number]["key"];

const NODE_W = 150;
const NODE_H = 40;

/// 服务拓扑：`/v1/edges/search` 的边聚合成图，并提供边列表明细。
///
/// 布局用 `layeredTopology` 纯函数（确定性分层，不引入图库）；线条宽度表示调用量、
/// 颜色表示错误率；点击边跳转 trace 列表并按该边过滤。
export function TopologyPage() {
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const { result, run } = useEdges();
  const [preset, setPreset] = useState<PresetKey>("1h");
  const [range, setRange] = useState<[Dayjs, Dayjs]>(() => [dayjs().subtract(1, "hour"), dayjs()]);
  const [form] = Form.useForm<{ src?: string; dst?: string; min_requests?: number }>();
  const [source, setSource] = useState<"all" | "otlp" | "ebpf">("all");

  const search = useCallback(async () => {
    const values = await form.validateFields();
    const window: [Dayjs, Dayjs] = range;
    const req: EdgeSearchRequest = {
      from_ts: window[0].valueOf() * 1000,
      to_ts: window[1].valueOf() * 1000,
      src_service: values.src?.trim() || undefined,
      dst_service: values.dst?.trim() || undefined,
      min_requests: values.min_requests,
      source: source === "all" ? undefined : source,
      limit: 500,
    };
    await run(req);
  }, [form, preset, range, run, source]);

  useEffect(() => {
    form.setFieldsValue({
      src: params.get("src_service") ?? "",
      dst: params.get("dst_service") ?? "",
    });
    void search();
  }, [form, params, search]);

  const summary = useMemo(() => aggregateEdges(result.data?.edges ?? []), [result.data]);
  const maxCalls = summary.reduce((acc, item) => Math.max(acc, item.calls), 0);
  const topology = useMemo(() => {
    const input = topologyInput(summary);
    return layeredTopology(input, { columnGap: 230, rowGap: 76, padding: 40 });
  }, [summary]);
  const styleByEdge = useMemo(
    () => new Map(summary.map((item) => [`${item.src}→${item.dst}`, edgeStyle(item, maxCalls)])),
    [summary, maxCalls],
  );

  const jumpToTraces = (_src: string, dst: string) => {
    const window = range;
    navigate(
      `/traces?service=${encodeURIComponent(dst)}&from_ts=${window[0].valueOf() * 1000}&to_ts=${window[1].valueOf() * 1000}`,
    );
  };

  return (
    <Space direction="vertical" style={{ width: "100%" }} size="middle">
      <Card size="small">
        <Form form={form} layout="inline" onFinish={() => void search()}>
          <Form.Item name="src" label="源服务">
            <Input allowClear style={{ width: 150 }} />
          </Form.Item>
          <Form.Item name="dst" label="目标服务">
            <Input allowClear style={{ width: 150 }} />
          </Form.Item>
          <Form.Item name="min_requests" label="最小调用">
            <InputNumber min={0} style={{ width: 100 }} />
          </Form.Item>
          <Form.Item label="来源">
            <Segmented
              value={source}
              onChange={(value) => setSource(value as typeof source)}
              options={[
                { value: "all", label: "全部" },
                { value: "otlp", label: "otlp" },
                { value: "ebpf", label: "ebpf" },
              ]}
            />
          </Form.Item>
          <Form.Item label="时间">
            <Segmented
              value={preset}
              onChange={(value) => {
                setPreset(value as PresetKey);
                setRange([
                  dayjs().subtract(PRESETS.find((p) => p.key === (value as PresetKey))!.amount, "hour"),
                  dayjs(),
                ]);
              }}
              options={PRESETS.map((p) => ({ value: p.key, label: p.label }))}
            />
          </Form.Item>
          <Form.Item>
            <Space>
              <Button type="primary" htmlType="submit" loading={result.status === "loading"}>
                查询
              </Button>
              <Button onClick={() => void search()}>刷新</Button>
            </Space>
          </Form.Item>
        </Form>
      </Card>

      <Card size="small">
        <Tabs
          items={[
            {
              key: "graph",
              label: "拓扑图",
              children: (
                <TopologyGraph
                  layout={topology}
                  styleByEdge={styleByEdge}
                  onEdgeClick={jumpToTraces}
                  loading={result.status === "loading"}
                />
              ),
            },
            {
              key: "edges",
              label: `边列表（${summary.length}）`,
              children: (
                <Table
                  size="small"
                  rowKey={(row: (typeof summary)[number]) => `${row.src}→${row.dst}`}
                  loading={result.status === "loading"}
                  dataSource={summary}
                  pagination={false}
                  locale={{ emptyText: "该时间窗没有调用边（检查是否有 trace 上报）" }}
                  columns={[
                    { title: "源服务", dataIndex: "src", width: 150 },
                    {
                      title: "目标服务",
                      dataIndex: "dst",
                      width: 190,
                      render: (value: string, row: (typeof summary)[number]) =>
                        value.startsWith("unknown") ? (
                          <Space size="small">
                            <Tag color="orange">{value}</Tag>
                            <Button
                              size="small"
                              type="link"
                              onClick={() =>
                                navigate(
                                  `/settings/service-aliases?match_kind=cidr&match_value=${encodeURIComponent(
                                    value.replace(/^unknown[-:]/, ""),
                                  )}`,
                                )
                              }
                            >
                              建映射
                            </Button>
                          </Space>
                        ) : (
                          value
                        ),
                    },
                    { title: "kind", dataIndex: "spanKind", width: 80 },
                    { title: "调用", dataIndex: "calls", width: 80 },
                    {
                      title: "错误",
                      dataIndex: "errors",
                      width: 80,
                      render: (value: number, row: (typeof summary)[number]) => (
                        <Tag color={value > 0 ? "red" : "default"}>
                          {value}（{(row.errorRate * 100).toFixed(1)}%）
                        </Tag>
                      ),
                    },
                    {
                      title: "平均",
                      dataIndex: "avgDurationMicros",
                      width: 100,
                      render: (value: number) => formatDuration(value),
                    },
                    {
                      title: "最大",
                      dataIndex: "maxDurationMicros",
                      width: 100,
                      render: (value: number) => formatDuration(value),
                    },
                    { title: "分钟桶", dataIndex: "bucketCount", width: 90 },
                    {
                      title: "来源",
                      dataIndex: "sources",
                      width: 110,
                      render: (value: string[]) => value.join("+"),
                    },
                    {
                      title: "Agent",
                      dataIndex: "agentIds",
                      render: (value: string[]) => value.join(", "),
                    },
                  ]}
                />
              ),
            },
          ]}
        />
      </Card>
    </Space>
  );
}

function TopologyGraph({
  layout,
  styleByEdge,
  onEdgeClick,
  loading,
}: {
  layout: ReturnType<typeof layeredTopology>;
  styleByEdge: Map<string, { width: number; color: string }>;
  onEdgeClick: (src: string, dst: string) => void;
  loading: boolean;
}) {
  if (loading && layout.nodes.length === 0) {
    return <div>加载中…</div>;
  }
  if (layout.nodes.length === 0) {
    return <Empty description="该时间窗没有服务调用边" />;
  }
  const height = Math.max(layout.height, 160);
  return (
    <div>
      <div style={{ marginBottom: 8, color: "rgba(0,0,0,0.45)", fontSize: 12 }}>
        线宽 = 调用量，颜色 = 错误率（绿→红）；点击边跳转该目标的 trace 列表
      </div>
      <svg
        width="100%"
        height={height}
        viewBox={`0 0 ${Math.max(layout.width, 600)} ${height}`}
        role="img"
        aria-label="服务拓扑图"
      >
        <defs>
          <marker id="arrow" markerWidth="8" markerHeight="8" refX="6" refY="3" orient="auto">
            <path d="M0,0 L6,3 L0,6 Z" fill="rgba(0,0,0,0.45)" />
          </marker>
        </defs>
        {layout.edges.map((edge) => {
          const style = styleByEdge.get(`${edge.src}→${edge.dst}`) ?? { width: 1.5, color: "#2f6bff" };
          return (
            <g
              key={`${edge.src}→${edge.dst}`}
              onClick={() => onEdgeClick(edge.src, edge.dst)}
              style={{ cursor: "pointer" }}
            >
              <line
                x1={edge.x1 + NODE_W}
                y1={edge.y1 + NODE_H / 2}
                x2={edge.x2}
                y2={edge.y2 + NODE_H / 2}
                stroke={style.color}
                strokeWidth={style.width}
                markerEnd="url(#arrow)"
              />
              <text
                x={(edge.x1 + NODE_W + edge.x2) / 2}
                y={(edge.y1 + edge.y2) / 2 + NODE_H / 2 - 6}
                fontSize={11}
                fill="rgba(0,0,0,0.6)"
                textAnchor="middle"
              >
                {edge.value}
              </text>
            </g>
          );
        })}
        {layout.nodes.map((node) => {
          const unknown = node.service.startsWith("unknown");
          return (
            <g key={node.service}>
              <rect
                x={node.x}
                y={node.y}
                width={NODE_W}
                height={NODE_H}
                rx={6}
                fill={unknown ? "#fafafa" : "#f0f5ff"}
                stroke={unknown ? "#d9d9d9" : "#adc6ff"}
                strokeDasharray={unknown ? "4 3" : undefined}
              />
              <text x={node.x + 10} y={node.y + 17} fontSize={13} fill="rgba(0,0,0,0.85)">
                {node.service.length > 18 ? `${node.service.slice(0, 17)}…` : node.service}
              </text>
              <text x={node.x + 10} y={node.y + 32} fontSize={11} fill="rgba(0,0,0,0.45)">
                入 {node.inbound} · 出 {node.outbound}
              </text>
            </g>
          );
        })}
      </svg>
    </div>
  );
}
