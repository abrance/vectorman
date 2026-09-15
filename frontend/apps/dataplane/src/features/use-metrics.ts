import { useCallback } from "react";
import type { PromEnvelope } from "@vectorman/adapters";
import { useRuntime } from "../app/runtime";
import { toAppError } from "./errors";
import { useQueryRecord } from "./use-query";

/// 指标查询：手动触发 `query_range`，结果写入查询存储（按 `key` 区分多路查询）。
export function useMetrics(key = "metrics.result") {
  const { dataplane, query, notifier } = useRuntime();
  const result = useQueryRecord<PromEnvelope>(query, key);

  const run = useCallback(
    async (expr: string, start: string, end: string, step: string) => {
      query.setLoading(key);
      try {
        query.setSuccess(key, await dataplane.queryRange(expr, start, end, step));
      } catch (e) {
        const err = toAppError(e);
        query.setError(key, err);
        notifier.error(err);
      }
    },
    [dataplane, query, notifier, key],
  );

  return { result, run };
}
