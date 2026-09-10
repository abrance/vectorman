import { Button, Popconfirm, Space, Table } from "antd";
import { useState } from "react";
import type { JobTemplate, TemplateInput } from "@vectorman/adapters";
import { formatTimestamp } from "@vectorman/primitives";
import { useRuntime } from "../app/runtime";
import { toAppError } from "../features/jobs/errors";
import { useJobTemplates } from "../features/jobs/use-job-templates";
import { TemplateFormDrawer } from "../ui/template-form-drawer";

export function TemplatesPage() {
  const { notifier } = useRuntime();
  const { list, refresh, create, update, remove } = useJobTemplates();
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<JobTemplate | null>(null);

  const openCreate = () => {
    setEditing(null);
    setFormOpen(true);
  };

  const openEdit = (t: JobTemplate) => {
    setEditing(t);
    setFormOpen(true);
  };

  const doSubmit = async (input: TemplateInput) => {
    try {
      if (editing) {
        await update(editing.template_id, input);
      } else {
        await create(input);
      }
      setFormOpen(false);
      setEditing(null);
    } catch (e) {
      notifier.error(toAppError(e));
    }
  };

  const doDelete = async (templateId: string) => {
    try {
      await remove(templateId);
    } catch (e) {
      notifier.error(toAppError(e));
    }
  };

  return (
    <Space direction="vertical" style={{ width: "100%" }} size="middle">
      <Space wrap>
        <Button onClick={() => void refresh(false)}>刷新</Button>
        <Button type="primary" onClick={openCreate}>
          新建模板
        </Button>
      </Space>

      <Table
        rowKey="template_id"
        loading={list.status === "loading"}
        dataSource={list.data ?? []}
        locale={{ emptyText: list.error?.message ?? "暂无模板" }}
        columns={[
          { title: "name", dataIndex: "name" },
          { title: "interpreter", dataIndex: "interpreter" },
          { title: "timeout_secs", dataIndex: "timeout_secs" },
          { title: "updated_at", dataIndex: "updated_at", render: (v: string) => formatTimestamp(v) },
          {
            title: "操作",
            render: (_, row) => (
              <Space>
                <Button type="link" onClick={() => openEdit(row)}>
                  编辑
                </Button>
                <Popconfirm
                  title="确认删除该模板？"
                  onConfirm={() => void doDelete(row.template_id)}
                >
                  <Button type="link" danger>
                    删除
                  </Button>
                </Popconfirm>
              </Space>
            ),
          },
        ]}
      />

      <TemplateFormDrawer
        open={formOpen}
        initial={editing}
        onClose={() => {
          setFormOpen(false);
          setEditing(null);
        }}
        onSubmit={(input) => void doSubmit(input)}
      />
    </Space>
  );
}
