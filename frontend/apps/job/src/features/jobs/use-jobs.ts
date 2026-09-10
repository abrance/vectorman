import { useCallback, useEffect } from "react";
import type { Job, JobListQuery, JobRerunRequest, JobSubmit } from "@vectorman/adapters";
import { useRuntime } from "../../app/runtime";
import { toAppError } from "./errors";
import { isTerminalStatus } from "./status";
import { useQueryRecord } from "./use-query";

const POLL_MS = 5000;

function keyOf(filter: JobListQuery): string {
  return `jobs.list.${filter.agent_id ?? ""}.${filter.status ?? ""}`;
}

export function useJobs(filter: JobListQuery) {
  const { jobs, query, notifier } = useRuntime();
  const key = keyOf(filter);
  const list = useQueryRecord<Job[]>(query, key);
  const agentId = filter.agent_id;
  const status = filter.status;

  const refresh = useCallback(
    async (silent: boolean) => {
      if (!silent) {
        query.setLoading(key);
      }
      try {
        const data = await jobs.listJobs({ agent_id: agentId, status });
        query.setSuccess(key, data);
      } catch (e) {
        const err = toAppError(e);
        if (silent) {
          notifier.warning(err.message);
        } else {
          query.setError(key, err);
          notifier.error(err);
        }
      }
    },
    [jobs, query, notifier, key, agentId, status],
  );

  useEffect(() => {
    void refresh(false);
  }, [refresh]);

  // 仅在存在非终态作业时轮询，全部到达终态后自动清除定时器。
  const hasActive = (list.data ?? []).some((j) => !isTerminalStatus(j.status));
  useEffect(() => {
    if (!hasActive) {
      return;
    }
    const timer = setInterval(() => {
      void refresh(true);
    }, POLL_MS);
    return () => {
      clearInterval(timer);
    };
  }, [hasActive, refresh]);

  const submit = useCallback(
    async (req: JobSubmit): Promise<Job> => {
      const job = await jobs.submitJob(req);
      await refresh(false);
      return job;
    },
    [jobs, refresh],
  );

  const rerun = useCallback(
    async (jobId: string, req: JobRerunRequest): Promise<Job> => {
      const job = await jobs.rerunJob(jobId, req);
      await refresh(false);
      return job;
    },
    [jobs, refresh],
  );

  return { list, refresh, submit, rerun };
}
