import { Tag } from "antd";
import type { JobStatus } from "@vectorman/adapters";

const COLORS: Record<JobStatus, string> = {
  pending: "default",
  dispatched: "geekblue",
  running: "blue",
  succeeded: "green",
  failed: "red",
  timeout: "orange",
  rejected: "volcano",
  lost: "default",
};

const LABELS: Record<JobStatus, string> = {
  pending: "等待中",
  dispatched: "已下发",
  running: "运行中",
  succeeded: "成功",
  failed: "失败",
  timeout: "超时",
  rejected: "被拒绝",
  lost: "丢失",
};

export function JobStatusTag({ status }: { status: JobStatus }) {
  return <Tag color={COLORS[status] ?? "default"}>{LABELS[status] ?? status}</Tag>;
}
