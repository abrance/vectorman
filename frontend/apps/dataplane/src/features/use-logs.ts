import { useCallback } from "react";
import type { LogRecord, LogSearchRequest } from "@vectorman/adapters";
import { useRuntime } from "../app/runtime";
import { toAppError } from "./errors";
import { useQueryRecord } from "./use-query";

const RESULT = "logs.result";

/// 日志检索：手动触发 `POST /v1/logs/search`。
export function useLogs() {
  const { dataplane, query, notifier } = useRuntime();
  const result = useQueryRecord<LogRecord[]>(query, RESULT);

  const run = useCallback(
    async (req: LogSearchRequest) => {
      query.setLoading(RESULT);
      try {
        query.setSuccess(RESULT, await dataplane.searchLogs(req));
      } catch (e) {
        const err = toAppError(e);
        query.setError(RESULT, err);
        notifier.error(err);
      }
    },
    [dataplane, query, notifier],
  );

  return { result, run };
}
