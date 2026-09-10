import { Button, Drawer, Form, Input, InputNumber, Select, Space } from "antd";
import { useEffect } from "react";
import type { JobTemplate, TemplateInput } from "@vectorman/adapters";
import { JOB_INTERPRETERS, parseArgs } from "./job-submit-drawer";

export type TemplateFormValues = {
  name: string;
  description?: string;
  interpreter: string;
  script: string;
  argsText?: string;
  timeout_secs?: number;
  working_dir?: string;
};

export function buildTemplateInput(v: TemplateFormValues): TemplateInput {
  const description = v.description?.trim();
  const workingDir = v.working_dir?.trim();
  return {
    name: v.name.trim(),
    description: description ? description : undefined,
    interpreter: v.interpreter,
    script: v.script,
    args: parseArgs(v.argsText),
    timeout_secs: v.timeout_secs,
    working_dir: workingDir ? workingDir : undefined,
  };
}

export function TemplateFormDrawer({
  open,
  initial,
  onClose,
  onSubmit,
}: {
  open: boolean;
  initial: JobTemplate | null;
  onClose: () => void;
  onSubmit: (input: TemplateInput) => void;
}) {
  const [form] = Form.useForm<TemplateFormValues>();

  useEffect(() => {
    if (!open) {
      return;
    }
    if (initial) {
      form.setFieldsValue({
        name: initial.name,
        description: initial.description ?? undefined,
        interpreter: initial.interpreter,
        script: initial.script,
        argsText: (initial.args ?? []).join(" "),
        timeout_secs: initial.timeout_secs,
        working_dir: initial.working_dir ?? undefined,
      });
    } else {
      form.resetFields();
    }
  }, [open, initial, form]);

  return (
    <Drawer
      open={open}
      title={initial ? "编辑模板" : "新建模板"}
      width={560}
      onClose={onClose}
      destroyOnClose
      extra={
        <Space>
          <Button onClick={onClose}>取消</Button>
          <Button type="primary" onClick={() => form.submit()}>
            保存
          </Button>
        </Space>
      }
    >
      <Form
        form={form}
        layout="vertical"
        initialValues={{ interpreter: "bash", timeout_secs: 300 }}
        onFinish={(v) => onSubmit(buildTemplateInput(v))}
      >
        <Form.Item name="name" label="name" rules={[{ required: true, message: "请输入模板名称" }]}>
          <Input placeholder="collect-logs" />
        </Form.Item>
        <Form.Item name="description" label="description">
          <Input placeholder="可选" />
        </Form.Item>
        <Form.Item name="interpreter" label="interpreter" rules={[{ required: true }]}>
          <Select options={JOB_INTERPRETERS.map((i) => ({ value: i, label: i }))} />
        </Form.Item>
        <Form.Item name="script" label="script" rules={[{ required: true, message: "请输入脚本" }]}>
          <Input.TextArea rows={6} placeholder="echo ${name}" />
        </Form.Item>
        <Form.Item name="argsText" label="args" tooltip="以空格分隔">
          <Input placeholder="arg1 arg2" />
        </Form.Item>
        <Form.Item
          name="timeout_secs"
          label="timeout_secs"
          rules={[{ type: "number", min: 1, max: 3600, message: "超时需在 1..3600 秒" }]}
        >
          <InputNumber min={1} max={3600} style={{ width: "100%" }} placeholder="默认 300" />
        </Form.Item>
        <Form.Item name="working_dir" label="working_dir">
          <Input placeholder="可选" />
        </Form.Item>
      </Form>
    </Drawer>
  );
}
