import { useCallback } from "react";
import type {
  EdgeSearchPage,
  EdgeSearchRequest,
  ServiceRow,
  TraceDetail,
  TraceSearchPage,
  TraceSearchRequest,
} from "@vectorman/adapters";
import { useRuntime } from "../../app/runtime";
import { toAppError } from "../errors";
import { useQueryRecord } from "../use-query";

const TRACE_LIST = "apm.traces";
const TRACE_DETAIL = "apm.trace";
const EDGES = "apm.edges";
const SERVICES = "apm.services";

/// trace 列表：手动触发 `POST /v1/traces/search`。
export function useTraces() {
  const { apm, query, notifier } = useRuntime();
  const result = useQueryRecord<TraceSearchPage>(query, TRACE_LIST);

  const run = useCallback(
    async (req: TraceSearchRequest) => {
      query.setLoading(TRACE_LIST);
      try {
        query.setSuccess(TRACE_LIST, await apm.searchTraces(req));
      } catch (e) {
        const err = toAppError(e);
        query.setError(TRACE_LIST, err);
        notifier.error(err);
      }
    },
    [apm, query, notifier],
  );

  return { result, run };
}

/// 服务拓扑边：`POST /v1/edges/search`。
export function useEdges() {
  const { apm, query, notifier } = useRuntime();
  const result = useQueryRecord<EdgeSearchPage>(query, EDGES);

  const run = useCallback(
    async (req: EdgeSearchRequest) => {
      query.setLoading(EDGES);
      try {
        query.setSuccess(EDGES, await apm.searchEdges(req));
      } catch (e) {
        const err = toAppError(e);
        query.setError(EDGES, err);
        notifier.error(err);
      }
    },
    [apm, query, notifier],
  );

  return { result, run };
}

/// 服务清单（含端点实例）：`GET /v1/apm/services`。
export function useServices() {
  const { apm, query, notifier } = useRuntime();
  const result = useQueryRecord<ServiceRow[]>(query, SERVICES);

  const run = useCallback(async () => {
    query.setLoading(SERVICES);
    try {
      query.setSuccess(SERVICES, await apm.listServices());
    } catch (e) {
      const err = toAppError(e);
      query.setError(SERVICES, err);
      notifier.error(err);
    }
  }, [apm, query, notifier]);

  return { result, run };
}

/// 单个 trace 的摘要与 span：`GET /v1/traces/{trace_id}`。
export function useTraceDetail() {
  const { apm, query, notifier } = useRuntime();
  const result = useQueryRecord<TraceDetail>(query, TRACE_DETAIL);

  const run = useCallback(
    async (traceId: string) => {
      query.setLoading(TRACE_DETAIL);
      try {
        query.setSuccess(TRACE_DETAIL, await apm.getTrace(traceId));
      } catch (e) {
        const err = toAppError(e);
        query.setError(TRACE_DETAIL, err);
        notifier.error(err);
      }
    },
    [apm, query, notifier],
  );

  return { result, run };
}
