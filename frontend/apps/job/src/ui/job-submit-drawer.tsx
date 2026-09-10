import { Button, Drawer, Form, Input, InputNumber, Select, Space } from "antd";
import { useEffect, useState } from "react";
import type { JobSubmit, JobTemplate, TemplateSubmitRequest } from "@vectorman/adapters";
import { extractVariables } from "../features/jobs/use-job-templates";
import { TemplateVarsForm } from "./template-vars-form";

export function parseArgs(input?: string): string[] {
  return (input ?? "").split(/\s+/).filter(Boolean);
}

export const JOB_INTERPRETERS = ["bash", "sh", "python3"];

export type JobSubmitFormValues = {
  agent_id: string;
  interpreter: string;
  script: string;
  argsText?: string;
  timeout_secs?: number;
  working_dir?: string;
  vars?: Record<string, string>;
};

type FormValues = JobSubmitFormValues;

export function buildJobSubmit(v: FormValues): JobSubmit {
  const workingDir = v.working_dir?.trim();
  return {
    agent_id: v.agent_id,
    interpreter: v.interpreter,
    script: v.script,
    args: parseArgs(v.argsText),
    timeout_secs: v.timeout_secs,
    working_dir: workingDir ? workingDir : undefined,
  };
}

export function JobSubmitDrawer({
  open,
  agents,
  templates = [],
  onClose,
  onSubmit,
  onSubmitTemplate,
}: {
  open: boolean;
  agents: string[];
  templates?: JobTemplate[];
  onClose: () => void;
  onSubmit: (req: JobSubmit) => void;
  onSubmitTemplate?: (templateId: string, req: TemplateSubmitRequest) => void;
}) {
  const [form] = Form.useForm<FormValues>();
  const [templateId, setTemplateId] = useState<string | undefined>();
  const selected = templates.find((t) => t.template_id === templateId) ?? null;
  const vars = selected ? extractVariables(selected) : [];

  useEffect(() => {
    if (open) {
      form.resetFields();
      setTemplateId(undefined);
    }
  }, [open, form]);

  const onPickTemplate = (id?: string) => {
    setTemplateId(id);
    const t = templates.find((item) => item.template_id === id);
    if (!t) {
      return;
    }
    form.setFieldsValue({
      interpreter: t.interpreter,
      script: t.script,
      argsText: (t.args ?? []).join(" "),
      timeout_secs: t.timeout_secs,
      working_dir: t.working_dir ?? undefined,
      vars: {},
    });
  };

  const finish = (v: FormValues) => {
    if (selected && onSubmitTemplate) {
      onSubmitTemplate(selected.template_id, { agent_id: v.agent_id, vars: v.vars ?? {} });
      return;
    }
    onSubmit(buildJobSubmit(v));
  };

  return (
    <Drawer
      open={open}
      title="提交作业"
      width={560}
      onClose={onClose}
      destroyOnClose
      extra={
        <Space>
          <Button onClick={onClose}>取消</Button>
          <Button type="primary" onClick={() => form.submit()}>
            提交
          </Button>
        </Space>
      }
    >
      <Form
        form={form}
        layout="vertical"
        initialValues={{ interpreter: "bash" }}
        onFinish={finish}
      >
        <Form.Item name="template_id" label="模板">
          <Select
            allowClear
            placeholder="可选：选择模板"
            value={templateId}
            onChange={onPickTemplate}
            options={templates.map((t) => ({ value: t.template_id, label: t.name }))}
          />
        </Form.Item>
        <Form.Item name="agent_id" label="agent_id" rules={[{ required: true, message: "请选择 Agent" }]}>
          <Select
            placeholder="选择在线 Agent"
            options={agents.map((a) => ({ value: a, label: a }))}
          />
        </Form.Item>
        <Form.Item name="interpreter" label="interpreter" rules={[{ required: true }]}>
          <Select
            disabled={!!selected}
            options={JOB_INTERPRETERS.map((i) => ({ value: i, label: i }))}
          />
        </Form.Item>
        <Form.Item name="script" label="script" rules={[{ required: true, message: "请输入脚本" }]}>
          <Input.TextArea rows={6} placeholder="echo hello" disabled={!!selected} />
        </Form.Item>
        <TemplateVarsForm vars={vars} />
        <Form.Item name="argsText" label="args" tooltip="以空格分隔">
          <Input placeholder="arg1 arg2" disabled={!!selected} />
        </Form.Item>
        <Form.Item name="timeout_secs" label="timeout_secs">
          <InputNumber min={1} style={{ width: "100%" }} placeholder="默认 300" disabled={!!selected} />
        </Form.Item>
        <Form.Item name="working_dir" label="working_dir">
          <Input placeholder="可选" disabled={!!selected} />
        </Form.Item>
      </Form>
    </Drawer>
  );
}
