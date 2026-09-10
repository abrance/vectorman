import { Button, Descriptions, Drawer, Input, Modal, Space, Spin, Typography } from "antd";
import { useState } from "react";
import type { Job } from "@vectorman/adapters";
import { formatTimestamp } from "@vectorman/primitives";
import { useRuntime } from "../app/runtime";
import { toAppError } from "../features/jobs/errors";
import { useJobDetail } from "../features/jobs/use-job-detail";
import { JobOutput } from "./job-output";
import { JobStatusTag } from "./job-status-tag";

export function JobDetailDrawer({
  jobId,
  onClose,
  onRerun,
}: {
  jobId: string | null;
  onClose: () => void;
  onRerun?: (job: Job) => void;
}) {
  const { templates, notifier } = useRuntime();
  const detail = useJobDetail(jobId);
  const [saveOpen, setSaveOpen] = useState(false);
  const [name, setName] = useState("");
  const job = detail.data;
  const loading = detail.status === "loading" && !job;

  const onSave = async () => {
    if (!jobId || !name.trim()) {
      return;
    }
    try {
      const t = await templates.saveJobAsTemplate(jobId, name.trim());
      notifier.success(`已另存为模板：${t.name}`);
      setSaveOpen(false);
      setName("");
    } catch (e) {
      notifier.error(toAppError(e));
    }
  };

  return (
    <Drawer
      open={jobId !== null}
      title={jobId ? `作业 ${jobId}` : "作业详情"}
      width={680}
      onClose={onClose}
      destroyOnClose
      extra={
        job ? (
          <Space>
            {onRerun ? <Button onClick={() => onRerun(job)}>重做</Button> : null}
            <Button onClick={() => setSaveOpen(true)}>另存为模板</Button>
          </Space>
        ) : null
      }
    >
      {loading ? <Spin /> : null}
      {job ? (
        <Space direction="vertical" style={{ width: "100%" }} size="middle">
          <Descriptions column={1} size="small" bordered>
            <Descriptions.Item label="状态">
              <JobStatusTag status={job.status} />
            </Descriptions.Item>
            <Descriptions.Item label="Agent">{job.agent_id}</Descriptions.Item>
            <Descriptions.Item label="解释器">{job.interpreter}</Descriptions.Item>
            <Descriptions.Item label="退出码">{job.exit_code ?? "-"}</Descriptions.Item>
            <Descriptions.Item label="信号">{job.signal ?? "-"}</Descriptions.Item>
            <Descriptions.Item label="超时(秒)">{job.timeout_secs}</Descriptions.Item>
            <Descriptions.Item label="来源模板">{job.template_id ?? "-"}</Descriptions.Item>
            <Descriptions.Item label="来源作业">{job.rerun_of ?? "-"}</Descriptions.Item>
            <Descriptions.Item label="开始时间">{formatTimestamp(job.started_at)}</Descriptions.Item>
            <Descriptions.Item label="结束时间">{formatTimestamp(job.finished_at)}</Descriptions.Item>
            {job.error ? <Descriptions.Item label="错误">{job.error}</Descriptions.Item> : null}
          </Descriptions>
          <JobOutput title="stdout" text={job.stdout} truncated={job.stdout_truncated} />
          <JobOutput title="stderr" text={job.stderr} truncated={job.stderr_truncated} />
          <div>
            <Typography.Text strong>脚本</Typography.Text>
            <pre
              style={{
                background: "#fafafa",
                border: "1px solid #f0f0f0",
                borderRadius: 4,
                padding: 8,
                margin: "4px 0 0",
                whiteSpace: "pre-wrap",
                wordBreak: "break-all",
              }}
            >
              {job.script}
            </pre>
          </div>
        </Space>
      ) : null}

      <Modal
        open={saveOpen}
        title="另存为模板"
        okText="保存"
        cancelText="取消"
        onOk={() => void onSave()}
        onCancel={() => setSaveOpen(false)}
        okButtonProps={{ disabled: !name.trim() }}
      >
        <Input
          placeholder="模板名称"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
      </Modal>
    </Drawer>
  );
}
