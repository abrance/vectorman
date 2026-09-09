import { Button, Form, Input, InputNumber, Space, Table } from "antd";
import { useEffect, useState } from "react";
import type { AgentConfig } from "@vectorman/adapters";
import { useRuntime } from "../app/runtime";
import { toAppError } from "../features/ledger/errors";
import { useAgentConfigs } from "../features/ledger/use-agent-configs";
import { LedgerDrawer, type DrawerMode } from "../ui/ledger-drawer";

export function AgentConfigsPage() {
  const { notifier } = useRuntime();
  const { list, refresh, getOne, save } = useAgentConfigs();
  const [form] = Form.useForm<AgentConfig>();
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<DrawerMode>("create");
  const [submitting, setSubmitting] = useState(false);
  const [missing, setMissing] = useState<string | null>(null);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const openCreate = () => {
    setMode("create");
    setMissing(null);
    form.resetFields();
    setOpen(true);
  };

  const load = async (id: string, next: DrawerMode) => {
    setMode(next);
    setMissing(null);
    form.resetFields();
    setOpen(true);
    try {
      form.setFieldsValue(await getOne(id));
    } catch (e) {
      setMissing(toAppError(e).message);
    }
  };

  const submit = async () => {
    const values = form.getFieldsValue();
    if (!values.agent_id?.trim() || !values.host_id?.trim()) {
      notifier.warning("缺少必填字段：agent_id、host_id");
      return;
    }
    setSubmitting(true);
    try {
      await save({
        ...values,
        log_level: values.log_level?.trim() ? values.log_level : "info",
      });
      setOpen(false);
    } catch (e) {
      notifier.error(toAppError(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Space direction="vertical" style={{ width: "100%" }} size="middle">
      <Space>
        <Button type="primary" onClick={openCreate}>
          保存
        </Button>
        <Button onClick={() => void refresh()}>刷新</Button>
      </Space>
      <Table
        rowKey="agent_id"
        loading={list.status === "loading"}
        dataSource={list.data ?? []}
        locale={{ emptyText: list.error?.message ?? "暂无数据" }}
        columns={[
          { title: "agent_id", dataIndex: "agent_id" },
          { title: "host_id", dataIndex: "host_id" },
          { title: "cpu_limit_percent", dataIndex: "cpu_limit_percent" },
          { title: "mem_limit_percent", dataIndex: "mem_limit_percent" },
          { title: "log_level", dataIndex: "log_level" },
          {
            title: "操作",
            render: (_, row) => (
              <Space>
                <Button type="link" onClick={() => void load(row.agent_id, "view")}>
                  查看
                </Button>
                <Button type="link" onClick={() => void load(row.agent_id, "edit")}>
                  编辑
                </Button>
              </Space>
            ),
          },
        ]}
      />
      <LedgerDrawer
        open={open}
        title={mode === "create" ? "保存配置" : mode === "edit" ? "编辑配置" : "查看配置"}
        mode={mode}
        loading={submitting}
        missing={missing}
        onClose={() => setOpen(false)}
        onSubmit={() => void submit()}
      >
        <Form form={form} layout="vertical" disabled={mode === "view"}>
          <Form.Item name="agent_id" label="agent_id" rules={[{ required: true }]}>
            <Input disabled={mode === "edit"} />
          </Form.Item>
          <Form.Item name="host_id" label="host_id" rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item name="cpu_limit_percent" label="cpu_limit_percent">
            <InputNumber style={{ width: "100%" }} />
          </Form.Item>
          <Form.Item name="mem_limit_percent" label="mem_limit_percent">
            <InputNumber style={{ width: "100%" }} />
          </Form.Item>
          <Form.Item name="log_level" label="log_level">
            <Input />
          </Form.Item>
        </Form>
      </LedgerDrawer>
    </Space>
  );
}
