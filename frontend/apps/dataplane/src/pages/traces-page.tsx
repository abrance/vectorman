import { Button, Card, DatePicker, Form, Input, InputNumber, Select, Space, Table, Tag } from "antd";
import type { Dayjs } from "dayjs";
import dayjs from "dayjs";
import { useCallback, useEffect } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import type { TraceSearchRequest, TraceSummary } from "@vectorman/adapters";
import { formatTimestamp } from "@vectorman/primitives";
import { formatDuration } from "../features/apm/layout";
import { useTraces } from "../features/apm/use-apm";

type TraceFormValues = {
  service?: string;
  operation?: string;
  status?: string;
  agent_id?: string;
  data_id?: string;
  min_duration_ms?: number;
  sort?: string;
  order?: string;
  range?: [Dayjs, Dayjs];
  limit?: number;
};

const COLUMNS = [
  {
    title: "开始时间",
    dataIndex: "start_ts",
    key: "start_ts",
    width: 180,
    render: (value: number) => formatTimestamp(String(value)),
  },
  { title: "根服务", dataIndex: "root_service", key: "root_service", width: 160 },
  { title: "根操作", dataIndex: "root_operation", key: "root_operation", ellipsis: true },
  {
    title: "耗时",
    dataIndex: "duration_micros",
    key: "duration_micros",
    width: 100,
    render: (value: number) => formatDuration(value),
  },
  { title: "span", dataIndex: "span_count", key: "span_count", width: 70 },
  {
    title: "错误",
    dataIndex: "error_count",
    key: "error_count",
    width: 70,
    render: (value: number) => (value > 0 ? <Tag color="red">{value}</Tag> : value),
  },
  {
    title: "状态",
    dataIndex: "status",
    key: "status",
    width: 80,
    render: (value: string) =>
      value === "error" ? <Tag color="red">error</Tag> : <Tag color="green">ok</Tag>,
  },
  { title: "来源", dataIndex: "collector", key: "collector", width: 80 },
  {
    title: "服务数",
    dataIndex: "services",
    key: "services",
    width: 90,
    render: (value: string[]) => value.length,
  },
];

/// trace 列表：按服务/操作/状态/耗时过滤，排序后进入详情。
export function TracesPage() {
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const { result, run } = useTraces();
  const [form] = Form.useForm<TraceFormValues>();

  const search = useCallback(async () => {
    const values = await form.validateFields();
    const req: TraceSearchRequest = {
      service: values.service?.trim() || undefined,
      operation: values.operation?.trim() || undefined,
      status: values.status || undefined,
      agent_id: values.agent_id?.trim() || undefined,
      data_id: values.data_id?.trim() || undefined,
      min_duration_micros: values.min_duration_ms ? values.min_duration_ms * 1_000 : undefined,
      sort: (values.sort as TraceSearchRequest["sort"]) ?? "start_ts",
      order: (values.order as TraceSearchRequest["order"]) ?? "desc",
      limit: values.limit,
    };
    if (values.range?.[0] && values.range[1]) {
      req.from_ts = values.range[0].valueOf() * 1000;
      req.to_ts = values.range[1].valueOf() * 1000;
    }
    await run(req);
  }, [form, run]);

  useEffect(() => {
    const from = params.get("from_ts");
    const to = params.get("to_ts");
    form.setFieldsValue({
      service: params.get("service") ?? "",
      operation: params.get("operation") ?? "",
      status: params.get("status") ?? undefined,
      agent_id: params.get("agent_id") ?? "",
      data_id: params.get("data_id") ?? "",
      sort: params.get("sort") ?? "start_ts",
      order: params.get("order") ?? "desc",
      limit: Number(params.get("limit") ?? 50),
      range:
        from && to
          ? [dayjs(Number(from) / 1000), dayjs(Number(to) / 1000)]
          : [dayjs().subtract(1, "hour"), dayjs()],
    });
    void search();
  }, [form, params, search]);

  const data = result.data;

  return (
    <Space direction="vertical" style={{ width: "100%" }} size="middle">
      <Card size="small">
        <Form form={form} layout="inline" onFinish={() => void search()}>
          <Form.Item name="service" label="服务">
            <Input allowClear style={{ width: 150 }} placeholder="根服务" />
          </Form.Item>
          <Form.Item name="operation" label="操作">
            <Input allowClear style={{ width: 180 }} placeholder="根操作" />
          </Form.Item>
          <Form.Item name="status" label="状态">
            <Select
              allowClear
              style={{ width: 96 }}
              options={[
                { value: "ok", label: "ok" },
                { value: "error", label: "error" },
              ]}
            />
          </Form.Item>
          <Form.Item name="min_duration_ms" label="最慢(ms)">
            <InputNumber min={0} style={{ width: 96 }} />
          </Form.Item>
          <Form.Item name="range" label="时间">
            <DatePicker.RangePicker showTime />
          </Form.Item>
          <Form.Item name="sort" label="排序">
            <Select
              style={{ width: 130 }}
              options={[
                { value: "start_ts", label: "开始时间" },
                { value: "duration_micros", label: "耗时" },
              ]}
            />
          </Form.Item>
          <Form.Item name="order" label="方向">
            <Select
              style={{ width: 96 }}
              options={[
                { value: "desc", label: "降序" },
                { value: "asc", label: "升序" },
              ]}
            />
          </Form.Item>
          <Form.Item name="agent_id" label="Agent">
            <Input allowClear style={{ width: 130 }} />
          </Form.Item>
          <Form.Item name="data_id" label="采集项">
            <Input allowClear style={{ width: 130 }} />
          </Form.Item>
          <Form.Item name="limit" label="条数">
            <InputNumber min={1} max={500} style={{ width: 90 }} />
          </Form.Item>
          <Form.Item>
            <Space>
              <Button type="primary" htmlType="submit" loading={result.status === "loading"}>
                检索
              </Button>
              <Button onClick={() => void search()}>刷新</Button>
            </Space>
          </Form.Item>
        </Form>
      </Card>

      <Card size="small" title={`trace 列表${data ? `（共 ${data.total} 条）` : ""}`}>
        <Table<TraceSummary>
          rowKey="trace_id"
          size="small"
          loading={result.status === "loading"}
          dataSource={data?.traces ?? []}
          columns={COLUMNS}
          pagination={false}
          locale={{ emptyText: "该时间窗没有 trace（检查采集项是否启用、服务是否上报）" }}
          onRow={(record) => ({
            onClick: () => navigate(`/traces/${record.trace_id}`),
            style: { cursor: "pointer" },
          })}
        />
      </Card>
    </Space>
  );
}
