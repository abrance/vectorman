import { Button, Drawer, Form, Input, InputNumber, Select, Space, Typography } from "antd";
import { useEffect } from "react";
import type { Job, JobRerunRequest } from "@vectorman/adapters";
import { JOB_INTERPRETERS, parseArgs } from "./job-submit-drawer";

export type JobRerunFormValues = {
  agent_id: string;
  interpreter: string;
  script: string;
  argsText?: string;
  envText?: string;
  timeout_secs?: number;
  working_dir?: string;
  /// `file_transfer` 专有：目标路径。表单里已渲染、`buildRerunRequest` 也读它，
  /// 但类型里漏了 —— 补上后 `tsc --noEmit` 才干净（见 frontend-ci 作业）。
  dest_path?: string;
};

export function formatEnv(env?: Record<string, string> | null): string {
  return Object.entries(env ?? {})
    .map(([key, value]) => `${key}=${value}`)
    .join("\n");
}

export function parseEnv(text?: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of (text ?? "").split("\n")) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) {
      continue;
    }
    const eq = trimmed.indexOf("=");
    if (eq <= 0) {
      continue;
    }
    const key = trimmed.slice(0, eq).trim();
    const value = trimmed.slice(eq + 1).trim();
    if (key) {
      out[key] = value;
    }
  }
  return out;
}

export function buildRerunRequest(v: JobRerunFormValues, kind?: string): JobRerunRequest {
  if (kind === "file_transfer") {
    return {
      agent_id: v.agent_id,
      dest_path: v.dest_path,
      timeout_secs: v.timeout_secs,
    };
  }
  return {
    agent_id: v.agent_id,
    interpreter: v.interpreter,
    script: v.script,
    args: parseArgs(v.argsText),
    env: parseEnv(v.envText),
    working_dir: v.working_dir ?? "",
    timeout_secs: v.timeout_secs,
  };
}

export function JobRerunDrawer({
  job,
  agents,
  onClose,
  onConfirm,
}: {
  job: Job | null;
  agents: string[];
  onClose: () => void;
  onConfirm: (req: JobRerunRequest) => void;
}) {
  const [form] = Form.useForm<JobRerunFormValues>();

  useEffect(() => {
    if (!job) {
      return;
    }
    form.setFieldsValue({
      agent_id:
        job.destination?.type === "agent" ? job.destination.agent_id : job.agent_id,
      interpreter: job.interpreter,
      script: job.script,
      argsText: (job.args ?? []).join(" "),
      envText: formatEnv(job.env),
      timeout_secs: job.timeout_secs,
      working_dir: job.working_dir ?? undefined,
      dest_path: job.destination?.type === "agent" ? job.destination.path : undefined,
    });
  }, [job, form]);

  return (
    <Drawer
      open={job !== null}
      title={job ? `重做作业（来源 ${job.job_id}）` : "重做作业"}
      width={560}
      onClose={onClose}
      destroyOnClose
      extra={
        <Space>
          <Button onClick={onClose}>取消</Button>
          <Button type="primary" onClick={() => form.submit()}>
            重做
          </Button>
        </Space>
      }
    >
      <Typography.Paragraph type="secondary">
        表单已按来源作业预填，可修改后重做。
      </Typography.Paragraph>
      <Form
        form={form}
        layout="vertical"
        onFinish={(v) => onConfirm(buildRerunRequest(v, job?.kind))}
      >
        <Form.Item
          name="agent_id"
          label={job?.kind === "file_transfer" ? "目标 Agent" : "agent_id"}
          rules={[{ required: true, message: "请选择 Agent" }]}
        >
          <Select
            placeholder="选择在线 Agent"
            options={agents.map((a) => ({ value: a, label: a }))}
          />
        </Form.Item>
        {job?.kind === "file_transfer" ? (
          <>
            <Form.Item name="dest_path" label="目标路径">
              <Input placeholder="覆盖目标路径，可选" />
            </Form.Item>
            <Form.Item name="timeout_secs" label="timeout_secs">
              <InputNumber min={1} style={{ width: "100%" }} placeholder="默认 300" />
            </Form.Item>
          </>
        ) : (
          <>
            <Form.Item name="interpreter" label="interpreter" rules={[{ required: true }]}>
              <Select options={JOB_INTERPRETERS.map((i) => ({ value: i, label: i }))} />
            </Form.Item>
            <Form.Item name="script" label="script" rules={[{ required: true, message: "请输入脚本" }]}>
              <Input.TextArea rows={6} />
            </Form.Item>
            <Form.Item name="argsText" label="args" tooltip="以空格分隔">
              <Input placeholder="arg1 arg2" />
            </Form.Item>
            <Form.Item name="envText" label="env" tooltip="每行 KEY=VALUE">
              <Input.TextArea rows={3} placeholder="KEY=VALUE" />
            </Form.Item>
            <Form.Item name="timeout_secs" label="timeout_secs">
              <InputNumber min={1} style={{ width: "100%" }} placeholder="默认 300" />
            </Form.Item>
            <Form.Item name="working_dir" label="working_dir">
              <Input placeholder="可选" />
            </Form.Item>
          </>
        )}
      </Form>
    </Drawer>
  );
}
