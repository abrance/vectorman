import { Alert, Button, Card, DatePicker, Empty, Form, Input, InputNumber, Segmented, Select, Space, Table, Tabs, Tag } from "antd";
import type { Dayjs } from "dayjs";
import dayjs from "dayjs";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import type { EdgeRow, LogRecord } from "@vectorman/adapters";
import { formatTimestamp } from "@vectorman/primitives";
import {
  aliasLink,
  endpointLabel,
  formatBytes,
  sourceLabel,
  summarizeCapability,
  unknownServiceIp,
} from "../features/ebpf/format";
import { useEbpfCapability, useEbpfEdges, useEbpfEvents } from "../features/ebpf/use-ebpf";

const PRESETS = [
  { key: "1h", label: "1 小时", amount: 1, unit: "hour" as const },
  { key: "6h", label: "6 小时", amount: 6, unit: "hour" as const },
  { key: "24h", label: "24 小时", amount: 24, unit: "hour" as const },
] as const;

type PresetKey = (typeof PRESETS)[number]["key"];

/// 事件类型展示名（其余类型原样显示，便于新内核加类型时排障）。
const EVENT_TYPE_LABELS: Record<string, string> = {
  process_exec: "进程启动",
  process_exit: "进程退出",
  process_fork: "fork",
  connect: "连接建立",
  accept: "接受连接",
  close: "连接关闭",
};

/// eBPF 页：能力状态 + 边/事件两个视图。
///
/// 能力状态放在页首而不是单独页签：它回答的是「为什么没有 eBPF 数据」，
/// 与两个视图都相关；没有上报与上报了不可用要分开提示（否则会被误读成「没数据」）。
export function EbpfPage() {
  const navigate = useNavigate();
  // 注意：hook 返回的是新对象，effect 依赖必须落在稳定的 `run` 上，
  // 否则会变成「每次渲染都重新拉一次能力状态」的死循环（React 会报 update depth exceeded）。
  const { result: capabilityResult, run: runCapability } = useEbpfCapability();
  const { result: edgesResult, run: runEdges } = useEbpfEdges();
  const { result: eventsResult, run: runEvents } = useEbpfEvents();
  const [preset, setPreset] = useState<PresetKey>("1h");
  const [range, setRange] = useState<[Dayjs, Dayjs]>(() => [dayjs().subtract(1, "hour"), dayjs()]);
  const [source, setSource] = useState<"ebpf" | "all" | "otlp">("ebpf");
  const [edgeForm] = Form.useForm<{ src?: string; dst?: string; protocol?: string; port?: number }>();
  const [eventForm] = Form.useForm<{ event_type?: string; process_name?: string; query?: string }>();

  const window = useCallback((): [number, number] => {
    const [from, to] = range;
    return [from.valueOf() * 1000, to.valueOf() * 1000];
  }, [range]);

  const searchEdges = useCallback(async () => {
    const values = await edgeForm.validateFields();
    const [from_ts, to_ts] = window();
    await runEdges({
      from_ts,
      to_ts,
      src_service: values.src?.trim() || undefined,
      dst_service: values.dst?.trim() || undefined,
      protocol: (values.protocol as "tcp" | "udp" | undefined) || undefined,
      dst_port: values.port,
      source: source === "all" ? undefined : source,
      limit: 200,
    });
  }, [edgeForm, runEdges, source, window]);

  const searchEvents = useCallback(async () => {
    const values = await eventForm.validateFields();
    const [from_ts, to_ts] = window();
    await runEvents({
      from_ts,
      to_ts,
      event_type: values.event_type || undefined,
      labels: values.process_name?.trim() ? { process_name: values.process_name.trim() } : undefined,
      message_query: values.query?.trim() || undefined,
      limit: 200,
    });
  }, [eventForm, runEvents, window]);

  useEffect(() => {
    void runCapability();
  }, [runCapability]);

  // 进页面先拉一次边（与拓扑页一致）；事件视图不自动拉：默认关闭时拉也是空，白跑一次查询。
  const bootstrapped = useRef(false);
  useEffect(() => {
    if (bootstrapped.current) {
      return;
    }
    bootstrapped.current = true;
    void searchEdges();
  }, [searchEdges]);

  const summary = useMemo(
    () => summarizeCapability(capabilityResult.data),
    [capabilityResult.data],
  );

  return (
    <Space direction="vertical" size="middle" style={{ width: "100%" }}>
      <Card title="eBPF 能力状态" size="small">
        {capabilityResult.status === "loading" ? (
          <Empty description="加载中" />
        ) : summary.reported === 0 ? (
          <Alert
            type="warning"
            showIcon
            message="没有 Agent 上报 eBPF 能力状态"
            description="确认已下发 ebpf_network / ebpf_tcp / ebpf_process 采集项；Agent 只在采集项启用时上报一次。"
          />
        ) : (
          <Space direction="vertical" style={{ width: "100%" }}>
            {summary.unavailable > 0 && (
              <Alert
                type="warning"
                showIcon
                message={`${summary.unavailable} 个采集项不可用（其余 ${summary.available} 个可用）`}
                description="内核需 ≥ 5.8、存在 /sys/kernel/btf/vmlinux，并以 root 或带 CAP_BPF+CAP_PERFMON 运行。"
              />
            )}
            <Table
              size="small"
              rowKey={(row) => `${row.agentId}:${row.itemId}`}
              dataSource={summary.lines}
              pagination={false}
              columns={[
                { title: "Agent", dataIndex: "agentId", width: 160 },
                { title: "采集项", dataIndex: "itemId", width: 200 },
                {
                  title: "状态",
                  dataIndex: "available",
                  width: 100,
                  render: (ok: boolean) => <Tag color={ok ? "green" : "red"}>{ok ? "可用" : "不可用"}</Tag>,
                },
                { title: "诊断", dataIndex: "detail" },
              ]}
            />
          </Space>
        )}
      </Card>

      <Card size="small">
        <Space wrap style={{ marginBottom: 12 }}>
          <Segmented
            options={PRESETS.map((p) => ({ label: p.label, value: p.key }))}
            value={preset}
            onChange={(value) => {
              const next = value as PresetKey;
              setPreset(next);
              const item = PRESETS.find((p) => p.key === next)!;
              setRange([dayjs().subtract(item.amount, item.unit), dayjs()]);
            }}
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
        </Space>

        <Tabs
          items={[
            {
              key: "edges",
              label: "边",
              children: (
                <Space direction="vertical" style={{ width: "100%" }}>
                  <Form form={edgeForm} layout="inline" onFinish={() => void searchEdges()}>
                    <Form.Item name="src" label="源服务">
                      <Input allowClear placeholder="order-api" style={{ width: 160 }} />
                    </Form.Item>
                    <Form.Item name="dst" label="目标服务">
                      <Input allowClear placeholder="pay-api" style={{ width: 160 }} />
                    </Form.Item>
                    <Form.Item name="protocol" label="协议">
                      <Select
                        allowClear
                        placeholder="全部"
                        style={{ width: 100 }}
                        options={[
                          { value: "tcp", label: "tcp" },
                          { value: "udp", label: "udp" },
                        ]}
                      />
                    </Form.Item>
                    <Form.Item name="port" label="目标端口">
                      <InputNumber min={0} max={65535} style={{ width: 110 }} />
                    </Form.Item>
                    <Form.Item label="来源">
                      <Segmented
                        options={[
                          { label: "仅 eBPF", value: "ebpf" },
                          { label: "两路合并", value: "all" },
                          { label: "仅 OTLP", value: "otlp" },
                        ]}
                        value={source}
                        onChange={(value) => setSource(value as typeof source)}
                      />
                    </Form.Item>
                    <Form.Item>
                      <Button type="primary" htmlType="submit" loading={edgesResult.status === "loading"}>
                        查询
                      </Button>
                    </Form.Item>
                  </Form>
                  <Table<EdgeRow>
                    size="small"
                    rowKey={(row) =>
                      `${row.bucket_ts}:${row.src_service}:${row.dst_service}:${row.src_ip}:${row.dst_port}:${row.protocol}`
                    }
                    loading={edgesResult.status === "loading"}
                    dataSource={edgesResult.data?.edges ?? []}
                    pagination={false}
                    locale={{ emptyText: "该时间窗没有 eBPF 边（检查采集项与内核权限）" }}
                    columns={[
                      {
                        title: "时间",
                        dataIndex: "bucket_ts",
                        width: 170,
                        render: (value: number) => formatTimestamp(String(value)),
                      },
                      {
                        title: "源",
                        dataIndex: "src_service",
                        render: (value: string) => renderService(value, navigate),
                      },
                      {
                        title: "目标",
                        dataIndex: "dst_service",
                        render: (value: string) => renderService(value, navigate),
                      },
                      { title: "目标地址", dataIndex: "dst_ip", width: 160, render: (_: string, row) => endpointLabel(row) },
                      { title: "协议", dataIndex: "protocol", width: 70, render: (value: string) => value || "-" },
                      { title: "连接", dataIndex: "connections", width: 70 },
                      {
                        title: "失败",
                        dataIndex: "failures",
                        width: 70,
                        render: (value: number) => <Tag color={value > 0 ? "red" : "default"}>{value}</Tag>,
                      },
                      { title: "发出", dataIndex: "bytes_sent", width: 90, render: (value: number) => formatBytes(value) },
                      { title: "接收", dataIndex: "bytes_recv", width: 90, render: (value: number) => formatBytes(value) },
                      {
                        title: "平均",
                        dataIndex: "duration_avg_micros",
                        width: 90,
                        render: (value: number) => `${value} µs`,
                      },
                      { title: "重传", dataIndex: "tcp_retrans", width: 70 },
                      { title: "来源", dataIndex: "source", width: 80, render: (value: string) => sourceLabel(value) },
                    ]}
                  />
                </Space>
              ),
            },
            {
              key: "events",
              label: "事件",
              children: (
                <Space direction="vertical" style={{ width: "100%" }}>
                  <Alert
                    type="info"
                    showIcon
                    message="原始事件默认关闭"
                    description="需要采集项开启 raw_events_enabled，并按 raw_events_sample_ratio 抽样上行；这里为空是正常现象。"
                    style={{ marginBottom: 8 }}
                  />
                  <Form form={eventForm} layout="inline" onFinish={() => void searchEvents()}>
                    <Form.Item name="event_type" label="类型">
                      <Select
                        allowClear
                        placeholder="全部"
                        style={{ width: 150 }}
                        options={Object.entries(EVENT_TYPE_LABELS).map(([value, label]) => ({ value, label }))}
                      />
                    </Form.Item>
                    <Form.Item name="process_name" label="进程">
                      <Input allowClear placeholder="java" style={{ width: 140 }} />
                    </Form.Item>
                    <Form.Item name="query" label="关键词">
                      <Input allowClear placeholder="exec / connect" style={{ width: 160 }} />
                    </Form.Item>
                    <Form.Item>
                      <Button type="primary" htmlType="submit" loading={eventsResult.status === "loading"}>
                        查询
                      </Button>
                    </Form.Item>
                  </Form>
                  <Table<LogRecord>
                    size="small"
                    rowKey={(row) => row.id}
                    loading={eventsResult.status === "loading"}
                    dataSource={eventsResult.data ?? []}
                    pagination={false}
                    locale={{ emptyText: "没有原始事件（需要采集项开启 raw_events_enabled）" }}
                    columns={[
                      {
                        title: "时间",
                        dataIndex: "timestamp",
                        width: 170,
                        render: (value: number) => formatTimestamp(String(value)),
                      },
                      {
                        title: "类型",
                        width: 90,
                        render: (_: unknown, row) =>
                          EVENT_TYPE_LABELS[row.labels.event_type ?? ""] ?? row.labels.event_type ?? "-",
                      },
                      { title: "进程", width: 120, render: (_: unknown, row) => row.labels.process_name || "-" },
                      { title: "PID", width: 80, render: (_: unknown, row) => row.labels.pid || "-" },
                      { title: "消息", dataIndex: "message" },
                    ]}
                  />
                </Space>
              ),
            },
          ]}
        />
      </Card>
    </Space>
  );
}

/// 未识别服务（`unknown-<ip>`）给一个「建立映射」入口，其余原样展示。
function renderService(service: string, navigate: ReturnType<typeof useNavigate>) {
  const ip = unknownServiceIp(service);
  if (!ip) {
    return service || "-";
  }
  return (
    <Space size={4}>
      <Tag>{service}</Tag>
      <Button type="link" size="small" onClick={() => navigate(aliasLink(ip))}>
        建立映射
      </Button>
    </Space>
  );
}
