import { Button, Form, Input, Modal, Space, Table, Tag } from "antd";
import { useState } from "react";
import type { Agent } from "@vectorman/adapters";
import { useRuntime } from "../app/runtime";
import { toAppError } from "../features/ledger/errors";
import { useAgents } from "../features/ledger/use-agents";
import { LedgerDrawer, type DrawerMode } from "../ui/ledger-drawer";
import { MaskedToken } from "../ui/masked-token";

function statusTag(status?: string) {
  if (status === "online") {
    return <Tag color="green">online</Tag>;
  }
  if (status === "offline") {
    return <Tag>offline</Tag>;
  }
  return <Tag>{status || "unknown"}</Tag>;
}

export function AgentsPage() {
  const { notifier } = useRuntime();
  const { list, refresh, getOne, save, remove } = useAgents({ poll: true });
  const [form] = Form.useForm<Agent>();
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<DrawerMode>("create");
  const [submitting, setSubmitting] = useState(false);
  const [missing, setMissing] = useState<string | null>(null);
  const [originalToken, setOriginalToken] = useState("");

  const openCreate = () => {
    setMode("create");
    setMissing(null);
    setOriginalToken("");
    form.resetFields();
    setOpen(true);
  };

  const load = async (id: string, next: DrawerMode) => {
    setMode(next);
    setMissing(null);
    form.resetFields();
    setOpen(true);
    try {
      const agent = await getOne(id);
      setOriginalToken(agent.token);
      form.setFieldsValue(agent);
    } catch (e) {
      setMissing(toAppError(e).message);
    }
  };

  const submit = async () => {
    const values = form.getFieldsValue();
    if (mode === "create") {
      if (!values.agent_id?.trim() || !values.host_id?.trim() || !values.token?.trim()) {
        notifier.warning("缺少必填字段：agent_id、host_id、token");
        return;
      }
    } else if (!values.agent_id?.trim() || !values.host_id?.trim()) {
      notifier.warning("缺少必填字段：agent_id、host_id");
      return;
    }
    const payload: Agent = {
      ...values,
      token: mode === "edit" ? originalToken : values.token,
    };
    setSubmitting(true);
    try {
      await save(payload);
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
          预登记
        </Button>
        <Button onClick={() => void refresh(false)}>刷新</Button>
      </Space>
      <Table
        rowKey="agent_id"
        loading={list.status === "loading"}
        dataSource={list.data ?? []}
        locale={{ emptyText: list.error?.message ?? "暂无数据" }}
        columns={[
          { title: "agent_id", dataIndex: "agent_id" },
          { title: "host_id", dataIndex: "host_id" },
          { title: "status", dataIndex: "status", render: statusTag },
          { title: "last_heartbeat_at", dataIndex: "last_heartbeat_at" },
          { title: "version", dataIndex: "version" },
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
                <Button
                  type="link"
                  danger
                  onClick={() =>
                    Modal.confirm({
                      title: "删除 Agent",
                      content: `将删除 Agent ${row.agent_id}，并同时清理该 Agent 的运行时配置与活跃会话。`,
                      okText: "删除",
                      cancelText: "取消",
                      onOk: () => remove(row.agent_id),
                    })
                  }
                >
                  删除
                </Button>
              </Space>
            ),
          },
        ]}
      />
      <LedgerDrawer
        open={open}
        title={mode === "create" ? "预登记 Agent" : mode === "edit" ? "编辑 Agent" : "查看 Agent"}
        mode={mode}
        loading={submitting}
        missing={missing}
        onClose={() => setOpen(false)}
        onSubmit={() => void submit()}
      >
        <Form form={form} layout="vertical">
          <Form.Item name="agent_id" label="agent_id" rules={[{ required: true }]}>
            <Input disabled={mode !== "create"} />
          </Form.Item>
          <Form.Item name="host_id" label="host_id" rules={[{ required: true }]}>
            <Input disabled={mode === "view"} />
          </Form.Item>
          {mode === "create" ? (
            <Form.Item name="token" label="token" rules={[{ required: true }]}>
              <Input />
            </Form.Item>
          ) : (
            <Form.Item label="token">
              <MaskedToken value={originalToken} />
            </Form.Item>
          )}
          <Form.Item name="access_point_id" label="access_point_id">
            <Input disabled={mode === "view"} />
          </Form.Item>
          <Form.Item name="version" label="version">
            <Input disabled={mode === "view"} />
          </Form.Item>
          <Form.Item name="install_path" label="install_path">
            <Input disabled={mode === "view"} />
          </Form.Item>
          <Form.Item name="status" label="status">
            <Input disabled />
          </Form.Item>
          <Form.Item name="last_heartbeat_at" label="last_heartbeat_at">
            <Input disabled />
          </Form.Item>
        </Form>
      </LedgerDrawer>
    </Space>
  );
}
