import { Button, Form, Input, Modal, Space, Table } from "antd";
import { useEffect, useState } from "react";
import type { Host } from "@vectorman/adapters";
import { useRuntime } from "../app/runtime";
import { useHosts } from "../features/ledger/use-hosts";
import { LedgerDrawer, type DrawerMode } from "../ui/ledger-drawer";
import { toAppError } from "../features/ledger/errors";

export function HostsPage() {
  const { notifier } = useRuntime();
  const { list, refresh, getOne, save, remove } = useHosts();
  const [form] = Form.useForm<Host>();
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

  const openView = async (id: string) => {
    setMode("view");
    setMissing(null);
    form.resetFields();
    setOpen(true);
    try {
      form.setFieldsValue(await getOne(id));
    } catch (e) {
      setMissing(toAppError(e).message);
    }
  };

  const openEdit = async (id: string) => {
    setMode("edit");
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
    if (!values.host_id?.trim() || !values.inner_ip?.trim()) {
      notifier.warning("缺少必填字段：host_id、inner_ip");
      return;
    }
    setSubmitting(true);
    try {
      await save(values);
      setOpen(false);
    } catch (e) {
      notifier.error(toAppError(e));
    } finally {
      setSubmitting(false);
    }
  };

  const confirmDelete = (id: string) => {
    Modal.confirm({
      title: "删除主机",
      content: `将删除主机 ${id}`,
      okText: "删除",
      cancelText: "取消",
      onOk: () => remove(id),
    });
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
        rowKey="host_id"
        loading={list.status === "loading"}
        dataSource={list.data ?? []}
        locale={{ emptyText: list.error?.message ?? "暂无数据" }}
        columns={[
          { title: "host_id", dataIndex: "host_id" },
          { title: "inner_ip", dataIndex: "inner_ip" },
          { title: "hostname", dataIndex: "hostname" },
          { title: "os_type", dataIndex: "os_type" },
          { title: "os_version", dataIndex: "os_version" },
          {
            title: "操作",
            render: (_, row) => (
              <Space>
                <Button type="link" onClick={() => void openView(row.host_id)}>
                  查看
                </Button>
                <Button type="link" onClick={() => void openEdit(row.host_id)}>
                  编辑
                </Button>
                <Button type="link" danger onClick={() => confirmDelete(row.host_id)}>
                  删除
                </Button>
              </Space>
            ),
          },
        ]}
      />
      <LedgerDrawer
        open={open}
        title={mode === "create" ? "登记主机" : mode === "edit" ? "编辑主机" : "查看主机"}
        mode={mode}
        loading={submitting}
        missing={missing}
        onClose={() => setOpen(false)}
        onSubmit={() => void submit()}
      >
        <Form form={form} layout="vertical" disabled={mode === "view"}>
          <Form.Item name="host_id" label="host_id" rules={[{ required: true }]}>
            <Input disabled={mode === "edit"} />
          </Form.Item>
          <Form.Item name="inner_ip" label="inner_ip" rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item name="hostname" label="hostname">
            <Input />
          </Form.Item>
          <Form.Item name="os_type" label="os_type">
            <Input />
          </Form.Item>
          <Form.Item name="os_version" label="os_version">
            <Input />
          </Form.Item>
          <Form.Item name="cpu_spec" label="cpu_spec">
            <Input />
          </Form.Item>
          <Form.Item name="mem_spec" label="mem_spec">
            <Input />
          </Form.Item>
        </Form>
      </LedgerDrawer>
    </Space>
  );
}
