import type { JobStatus } from "@vectorman/adapters";

export const JOB_STATUSES: JobStatus[] = [
  "pending",
  "dispatched",
  "running",
  "succeeded",
  "failed",
  "timeout",
  "rejected",
  "lost",
];

const TERMINAL: ReadonlySet<JobStatus> = new Set([
  "succeeded",
  "failed",
  "timeout",
  "rejected",
  "lost",
]);

export function isTerminalStatus(status: JobStatus): boolean {
  return TERMINAL.has(status);
}
