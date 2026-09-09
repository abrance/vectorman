import { Button, Form, Input, InputNumber, Modal, Space, Table } from "antd";
import { useEffect, useState } from "react";
import type { AccessPoint } from "@vectorman/adapters";
import { useRuntime } from "../app/runtime";
import { toAppError } from "../features/ledger/errors";
import { useAccessPoints } from "../features/ledger/use-access-points";
import { LedgerDrawer, type DrawerMode } from "../ui/ledger-drawer";

export function AccessPointsPage() {
  const { notifier } = useRuntime();
  const { list, refresh, getOne, save, remove } = useAccessPoints();
  const [form] = Form.useForm<AccessPoint>();
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
    if (!values.id?.trim() || !values.name?.trim() || !values.server_ip?.trim() || values.rpc_port == null) {
      notifier.warning("缺少必填字段：id、name、server_ip、rpc_port");
      return;
    }
    setSubmitting(true);
    try {
      await save({
        ...values,
        file_port: values.file_port ?? null,
        data_port: values.data_port ?? null,
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
          登记
        </Button>
        <Button onClick={() => void refresh()}>刷新</Button>
      </Space>
      <Table
        rowKey="id"
        loading={list.status === "loading"}
        dataSource={list.data ?? []}
        locale={{ emptyText: list.error?.message ?? "暂无数据" }}
        columns={[
          { title: "id", dataIndex: "id" },
          { title: "name", dataIndex: "name" },
          { title: "server_ip", dataIndex: "server_ip" },
          { title: "rpc_port", dataIndex: "rpc_port" },
          {
            title: "操作",
            render: (_, row) => (
              <Space>
                <Button type="link" onClick={() => void load(row.id, "view")}>
                  查看
                </Button>
                <Button type="link" onClick={() => void load(row.id, "edit")}>
                  编辑
                </Button>
                <Button
                  type="link"
                  danger
                  onClick={() =>
                    Modal.confirm({
                      title: "删除接入点",
                      content: `将删除接入点 ${row.id}`,
                      okText: "删除",
                      cancelText: "取消",
                      onOk: () => remove(row.id),
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
        title={mode === "create" ? "登记接入点" : mode === "edit" ? "编辑接入点" : "查看接入点"}
        mode={mode}
        loading={submitting}
        missing={missing}
        onClose={() => setOpen(false)}
        onSubmit={() => void submit()}
      >
        <Form form={form} layout="vertical" disabled={mode === "view"}>
          <Form.Item name="id" label="id" rules={[{ required: true }]}>
            <Input disabled={mode === "edit"} />
          </Form.Item>
          <Form.Item name="name" label="name" rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item name="server_ip" label="server_ip" rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item name="rpc_port" label="rpc_port" rules={[{ required: true }]}>
            <InputNumber style={{ width: "100%" }} />
          </Form.Item>
          <Form.Item name="file_port" label="file_port">
            <InputNumber style={{ width: "100%" }} />
          </Form.Item>
          <Form.Item name="data_port" label="data_port">
            <InputNumber style={{ width: "100%" }} />
          </Form.Item>
        </Form>
      </LedgerDrawer>
    </Space>
  );
}
