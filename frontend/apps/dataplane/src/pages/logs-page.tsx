import { Button, Card, DatePicker, Form, Input, InputNumber, Select, Space, Table, Tag } from "antd";
import type { Dayjs } from "dayjs";
import dayjs from "dayjs";
import { useCallback, useEffect } from "react";
import { useSearchParams } from "react-router-dom";
import type { LogSearchRequest } from "@vectorman/adapters";
import { formatTimestamp } from "@vectorman/primitives";
import { useLogs } from "../features/use-logs";

type LogFormValues = {
  data_type: string;
  agent_id?: string;
  data_id?: string;
  level?: string;
  keyword?: string;
  range?: [Dayjs, Dayjs];
  limit?: number;
};

export function LogsPage() {
  const [params] = useSearchParams();
  const { result, run } = useLogs();
  const [form] = Form.useForm<LogFormValues>();

  const search = useCallback(async () => {
    const values = await form.validateFields();
    const req: LogSearchRequest = {
      data_type: values.data_type,
      agent_id: values.agent_id?.trim() || undefined,
      data_id: values.data_id?.trim() || undefined,
      level: values.level?.trim() || undefined,
      message_query: values.keyword?.trim() || undefined,
      limit: values.limit,
    };
    if (values.range?.[0] && values.range[1]) {
      req.from_ts = values.range[0].valueOf() * 1000;
      req.to_ts = values.range[1].valueOf() * 1000;
    }
    await run(req);
  }, [form, run]);

  useEffect(() => {
    form.setFieldsValue({
      data_type: params.get("data_type") ?? "logs",
      agent_id: params.get("agent_id") ?? "",
      data_id: params.get("data_id") ?? "",
      limit: 100,
    });
    void search();
  }, [form, params, search]);

  return (
    <Space direction="vertical" style={{ width: "100%" }} size="middle">
      <Card size="small">
        <Form form={form} layout="inline" onFinish={() => void search()}>
          <Form.Item name="data_type" label="类型">
            <Select
              style={{ width: 120 }}
              options={[
                { value: "logs", label: "logs" },
                { value: "apm", label: "apm" },
                { value: "ebpf", label: "ebpf" },
              ]}
            />
          </Form.Item>
          <Form.Item name="agent_id" label="agent_id">
            <Input allowClear style={{ width: 160 }} />
          </Form.Item>
          <Form.Item name="data_id" label="data_id">
            <Input allowClear style={{ width: 180 }} />
          </Form.Item>
          <Form.Item name="level" label="level">
            <Input allowClear style={{ width: 110 }} />
          </Form.Item>
          <Form.Item name="keyword" label="关键词">
            <Input allowClear style={{ width: 160 }} />
          </Form.Item>
          <Form.Item name="range" label="时间">
            <DatePicker.RangePicker showTime />
          </Form.Item>
          <Form.Item name="limit" label="limit">
            <InputNumber min={1} max={1000} />
          </Form.Item>
          <Form.Item>
            <Button type="primary" htmlType="submit">
              检索
            </Button>
          </Form.Item>
        </Form>
      </Card>
      <Table
        rowKey="id"
        size="small"
        loading={result.status === "loading"}
        dataSource={result.data ?? []}
        locale={{ emptyText: result.error?.message ?? "暂无记录" }}
        columns={[
          {
            title: "时间",
            dataIndex: "timestamp",
            render: (value: number) => formatTimestamp(String(value)),
          },
          {
            title: "level",
            dataIndex: "level",
            render: (value: string) => (value ? <Tag>{value}</Tag> : null),
          },
          { title: "message", dataIndex: "message" },
          {
            title: "labels",
            dataIndex: "labels",
            render: (value: Record<string, string>) =>
              Object.entries(value ?? {})
                .map(([k, v]) => `${k}=${v}`)
                .join(", "),
          },
        ]}
      />
    </Space>
  );
}
