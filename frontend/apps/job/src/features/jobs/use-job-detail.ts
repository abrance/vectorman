import { useEffect } from "react";
import type { Job } from "@vectorman/adapters";
import { useRuntime } from "../../app/runtime";
import { toAppError } from "./errors";
import { isTerminalStatus } from "./status";
import { useQueryRecord } from "./use-query";

const POLL_MS = 3000;

export function useJobDetail(jobId: string | null) {
  const { jobs, query } = useRuntime();
  const key = jobId ? `jobs.one.${jobId}` : "jobs.one.none";
  const record = useQueryRecord<Job>(query, key);

  useEffect(() => {
    if (!jobId) {
      return;
    }
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const tick = async () => {
      try {
        const job = await jobs.getJob(jobId);
        if (cancelled) {
          return;
        }
        query.setSuccess(key, job);
        if (!isTerminalStatus(job.status)) {
          timer = setTimeout(() => void tick(), POLL_MS);
        }
      } catch (e) {
        if (!cancelled) {
          query.setError(key, toAppError(e));
        }
      }
    };
    void tick();
    return () => {
      cancelled = true;
      if (timer !== null) {
        clearTimeout(timer);
      }
    };
  }, [jobId, key, jobs, query]);

  return record;
}
