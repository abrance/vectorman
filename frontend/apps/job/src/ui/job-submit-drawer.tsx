import { Button, Drawer, Form, Input, InputNumber, Radio, Select, Space, Upload } from "antd";
import { useEffect, useState } from "react";
import type {
  FileEndpoint,
  JobFileMeta,
  JobSubmit,
  JobTemplate,
  TemplateSubmitRequest,
} from "@vectorman/adapters";
import { extractVariables } from "../features/jobs/use-job-templates";
import { TemplateVarsForm } from "./template-vars-form";

export function parseArgs(input?: string): string[] {
  return (input ?? "").split(/\s+/).filter(Boolean);
}

export const JOB_INTERPRETERS = ["bash", "sh", "python3"];

export type JobSubmitFormValues = {
  kind?: "script" | "file_transfer";
  agent_id?: string;
  interpreter?: string;
  script?: string;
  argsText?: string;
  timeout_secs?: number;
  working_dir?: string;
  vars?: Record<string, string>;
  sourceType?: "agent" | "uploaded" | "upload";
  source_agent_id?: string;
  source_path?: string;
  source_file_id?: string;
  destType?: "agent" | "server_temp";
  dest_agent_id?: string;
  dest_path?: string;
};

type FormValues = JobSubmitFormValues;

export function buildJobSubmit(v: FormValues): JobSubmit {
  const workingDir = v.working_dir?.trim();
  return {
    agent_id: v.agent_id ?? "",
    interpreter: v.interpreter,
    script: v.script ?? "",
    args: parseArgs(v.argsText),
    timeout_secs: v.timeout_secs,
    working_dir: workingDir ? workingDir : undefined,
  };
}

export function buildFileJobSubmit(v: FormValues): JobSubmit {
  const source: FileEndpoint =
    v.sourceType === "agent"
      ? { type: "agent", agent_id: v.source_agent_id ?? "", path: v.source_path ?? "" }
      : { type: "server_temp", file_id: v.source_file_id };
  const destination: FileEndpoint =
    v.destType === "server_temp"
      ? { type: "server_temp" }
      : { type: "agent", agent_id: v.dest_agent_id ?? "", path: v.dest_path ?? "" };
  return {
    kind: "file_transfer",
    source,
    destination,
    timeout_secs: v.timeout_secs,
  };
}

export function JobSubmitDrawer({
  open,
  agents,
  templates = [],
  onClose,
  onSubmit,
  onSubmitTemplate,
  onUploadFile,
  listJobFiles,
}: {
  open: boolean;
  agents: string[];
  templates?: JobTemplate[];
  onClose: () => void;
  onSubmit: (req: JobSubmit) => void;
  onSubmitTemplate?: (templateId: string, req: TemplateSubmitRequest) => void;
  onUploadFile?: (file: File) => Promise<JobFileMeta>;
  listJobFiles?: () => Promise<JobFileMeta[]>;
}) {
  const [form] = Form.useForm<FormValues>();
  const [templateId, setTemplateId] = useState<string | undefined>();
  const [tempFiles, setTempFiles] = useState<JobFileMeta[]>([]);
  const selected = templates.find((t) => t.template_id === templateId) ?? null;
  const vars = selected ? extractVariables(selected) : [];
  const kind = Form.useWatch("kind", form) ?? "script";
  const sourceType = Form.useWatch("sourceType", form) ?? "agent";
  const destType = Form.useWatch("destType", form) ?? "agent";

  useEffect(() => {
    if (open) {
      form.resetFields();
      setTemplateId(undefined);
    }
  }, [open, form]);

  useEffect(() => {
    if (!open || !listJobFiles) {
      return;
    }
    void listJobFiles()
      .then(setTempFiles)
      .catch(() => setTempFiles([]));
  }, [open, listJobFiles]);

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
    if (v.kind === "file_transfer") {
      onSubmit(buildFileJobSubmit(v));
      return;
    }
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
        initialValues={{ interpreter: "bash", kind: "script", sourceType: "agent", destType: "agent" }}
        onFinish={finish}
      >
        <Form.Item name="kind" label="作业种类">
          <Radio.Group
            options={[
              { value: "script", label: "脚本" },
              { value: "file_transfer", label: "文件传输" },
            ]}
          />
        </Form.Item>
        {kind === "file_transfer" ? (
          <>
            <Form.Item name="sourceType" label="源类型" rules={[{ required: true }]}>
              <Select
                options={[
                  { value: "agent", label: "Agent 路径" },
                  { value: "uploaded", label: "已上传文件" },
                  { value: "upload", label: "立即上传" },
                ]}
              />
            </Form.Item>
            {sourceType === "agent" ? (
              <>
                <Form.Item
                  name="source_agent_id"
                  label="源 Agent"
                  rules={[{ required: true, message: "请选择源 Agent" }]}
                >
                  <Select options={agents.map((a) => ({ value: a, label: a }))} placeholder="选择在线 Agent" />
                </Form.Item>
                <Form.Item
                  name="source_path"
                  label="源路径"
                  rules={[{ required: true, message: "请输入绝对路径" }]}
                >
                  <Input placeholder="/var/log/app.log" />
                </Form.Item>
              </>
            ) : null}
            {sourceType === "uploaded" ? (
              <Form.Item
                name="source_file_id"
                label="临时文件"
                rules={[{ required: true, message: "请选择临时文件" }]}
              >
                <Select
                  placeholder="选择已上传文件"
                  options={tempFiles.map((f) => ({
                    value: f.file_id,
                    label: `${f.file_name} (${f.file_id})`,
                  }))}
                />
              </Form.Item>
            ) : null}
            {sourceType === "upload" ? (
              <Form.Item label="上传文件" required>
                <Upload
                  maxCount={1}
                  beforeUpload={(file) => {
                    if (!onUploadFile) {
                      return false;
                    }
                    void onUploadFile(file as File).then((meta) => {
                      form.setFieldsValue({
                        sourceType: "uploaded",
                        source_file_id: meta.file_id,
                      });
                      setTempFiles((prev) => [meta, ...prev.filter((p) => p.file_id !== meta.file_id)]);
                    });
                    return false;
                  }}
                >
                  <Button>选择文件</Button>
                </Upload>
              </Form.Item>
            ) : null}
            <Form.Item name="destType" label="目标类型" rules={[{ required: true }]}>
              <Select
                options={[
                  { value: "agent", label: "Agent 路径" },
                  { value: "server_temp", label: "Server 临时目录" },
                ]}
              />
            </Form.Item>
            {destType === "agent" ? (
              <>
                <Form.Item
                  name="dest_agent_id"
                  label="目标 Agent"
                  rules={[{ required: true, message: "请选择目标 Agent" }]}
                >
                  <Select options={agents.map((a) => ({ value: a, label: a }))} placeholder="选择在线 Agent" />
                </Form.Item>
                <Form.Item
                  name="dest_path"
                  label="目标路径"
                  rules={[{ required: true, message: "请输入绝对路径" }]}
                >
                  <Input placeholder="/tmp/app.log" />
                </Form.Item>
              </>
            ) : null}
            <Form.Item name="timeout_secs" label="timeout_secs">
              <InputNumber min={1} style={{ width: "100%" }} placeholder="默认 300" />
            </Form.Item>
          </>
        ) : (
          <>
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
          </>
        )}
      </Form>
    </Drawer>
  );
}
