import { Button, Select, Space, Table } from "antd";
import { useState } from "react";
import type {
  Job,
  JobListQuery,
  JobRerunRequest,
  JobStatus,
  JobSubmit,
  TemplateSubmitRequest,
} from "@vectorman/adapters";
import { formatTimestamp } from "@vectorman/primitives";
import { useRuntime } from "../app/runtime";
import { toAppError } from "../features/jobs/errors";
import { useJobs } from "../features/jobs/use-jobs";
import { useJobTemplates } from "../features/jobs/use-job-templates";
import { useOnlineAgents } from "../features/jobs/use-online-agents";
import { JobDetailDrawer } from "../ui/job-detail-drawer";
import { JobRerunDrawer } from "../ui/job-rerun-drawer";
import { JobStatusTag } from "../ui/job-status-tag";
import { JobSubmitDrawer } from "../ui/job-submit-drawer";
import { JOB_STATUSES } from "../features/jobs/status";

export function JobsPage() {
  const { notifier, templates } = useRuntime();
  const [agentFilter, setAgentFilter] = useState<string | undefined>();
  const [statusFilter, setStatusFilter] = useState<JobStatus | undefined>();
  const [detailId, setDetailId] = useState<string | null>(null);
  const [submitOpen, setSubmitOpen] = useState(false);
  const [rerunJob, setRerunJob] = useState<Job | null>(null);

  const agents = useOnlineAgents();
  const templateList = useJobTemplates();
  const filter: JobListQuery = { agent_id: agentFilter, status: statusFilter };
  const { list, refresh, submit, rerun } = useJobs(filter);

  const doSubmit = async (values: JobSubmit) => {
    try {
      const job = await submit(values);
      setSubmitOpen(false);
      notifier.success(`作业已提交：${job.job_id}`);
      setDetailId(job.job_id);
    } catch (e) {
      notifier.error(toAppError(e));
    }
  };

  const doSubmitTemplate = async (templateId: string, req: TemplateSubmitRequest) => {
    try {
      const job = await templates.submitTemplate(templateId, req);
      await refresh(false);
      setSubmitOpen(false);
      notifier.success(`作业已提交：${job.job_id}`);
      setDetailId(job.job_id);
    } catch (e) {
      notifier.error(toAppError(e));
    }
  };

  const doRerun = async (req: JobRerunRequest) => {
    if (!rerunJob) {
      return;
    }
    try {
      const job = await rerun(rerunJob.job_id, req);
      setRerunJob(null);
      notifier.success(`作业已重做：${job.job_id}`);
      setDetailId(job.job_id);
    } catch (e) {
      notifier.error(toAppError(e));
    }
  };

  return (
    <Space direction="vertical" style={{ width: "100%" }} size="middle">
      <Space wrap>
        <Select
          allowClear
          placeholder="按 Agent 过滤"
          style={{ width: 200 }}
          value={agentFilter}
          onChange={setAgentFilter}
          options={(agents.data ?? []).map((a) => ({ value: a.agent_id, label: a.agent_id }))}
        />
        <Select
          allowClear
          placeholder="按状态过滤"
          style={{ width: 160 }}
          value={statusFilter}
          onChange={setStatusFilter}
          options={JOB_STATUSES.map((s) => ({ value: s, label: s }))}
        />
        <Button onClick={() => void refresh(false)}>刷新</Button>
        <Button type="primary" onClick={() => setSubmitOpen(true)}>
          提交作业
        </Button>
      </Space>

      <Table
        rowKey="job_id"
        loading={list.status === "loading"}
        dataSource={list.data ?? []}
        locale={{ emptyText: list.error?.message ?? "暂无作业" }}
        columns={[
          { title: "job_id", dataIndex: "job_id" },
          { title: "agent_id", dataIndex: "agent_id" },
          { title: "interpreter", dataIndex: "interpreter" },
          { title: "status", dataIndex: "status", render: (s: JobStatus) => <JobStatusTag status={s} /> },
          { title: "timeout_secs", dataIndex: "timeout_secs" },
          { title: "created_at", dataIndex: "created_at", render: (v: string) => formatTimestamp(v) },
          {
            title: "操作",
            render: (_, row) => (
              <Button type="link" onClick={() => setDetailId(row.job_id)}>
                查看
              </Button>
            ),
          },
        ]}
      />

      <JobSubmitDrawer
        open={submitOpen}
        agents={(agents.data ?? []).map((a) => a.agent_id)}
        templates={templateList.list.data ?? []}
        onClose={() => setSubmitOpen(false)}
        onSubmit={(values) => void doSubmit(values)}
        onSubmitTemplate={(id, req) => void doSubmitTemplate(id, req)}
      />

      <JobDetailDrawer
        jobId={detailId}
        onClose={() => setDetailId(null)}
        onRerun={(job) => setRerunJob(job)}
      />

      <JobRerunDrawer
        job={rerunJob}
        agents={(agents.data ?? []).map((a) => a.agent_id)}
        onClose={() => setRerunJob(null)}
        onConfirm={(req) => void doRerun(req)}
      />
    </Space>
  );
}
