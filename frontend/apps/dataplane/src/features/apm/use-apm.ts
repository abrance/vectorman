import { useCallback } from "react";
import type {
  AliasInput,
  AliasRecord,
  EdgeSearchPage,
  EdgeSearchRequest,
  ServiceRow,
  TraceDetail,
  TraceSearchPage,
  TraceSearchRequest,
  TsStorageStats,
} from "@vectorman/adapters";
import { useRuntime } from "../../app/runtime";
import { toAppError } from "../errors";
import { latestValue, promSeries } from "../prom";
import { useQueryRecord } from "../use-query";

const TRACE_LIST = "apm.traces";
const TRACE_DETAIL = "apm.trace";
const EDGES = "apm.edges";
const SERVICES = "apm.services";
const ALIASES = "apm.aliases";
const STORAGE = "apm.storage";
const SELF_METRICS = "apm.self_metrics";

/// 存储状态卡片要看的自监控指标（`measurement` 即指标名，`field_name` 恒为 value）。
export const SELF_METRIC_EXPRS = {
  apm_data_bytes: "dataserver_apm_data_bytes",
  apm_retention_runs: "dataserver_apm_retention_runs_total",
  apm_details_deleted: "dataserver_apm_details_deleted_total",
  apm_summaries_deleted: "dataserver_apm_summaries_deleted_total",
  apm_edges_deleted: "dataserver_apm_edges_deleted_total",
  apm_ingest_throttled_batches: "dataserver_apm_ingest_throttled_batches_total",
  apm_paired_edges: "dataserver_apm_paired_edges",
  apm_pending_spans: "dataserver_apm_pending_spans",
  ts_series_count: "dataserver_ts_series_count",
  ts_degraded: "dataserver_ts_degraded",
} as const;

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

/// 静态服务名映射：列表 + 增删改（写操作后自动刷新列表）。
export function useAliases() {
  const { apm, query, notifier } = useRuntime();
  const result = useQueryRecord<AliasRecord[]>(query, ALIASES);

  const reload = useCallback(
    async (filter?: { match_kind?: string; enabled?: boolean }) => {
      query.setLoading(ALIASES);
      try {
        query.setSuccess(ALIASES, await apm.listAliases(filter ?? {}));
      } catch (e) {
        const err = toAppError(e);
        query.setError(ALIASES, err);
        notifier.error(err);
      }
    },
    [apm, query, notifier],
  );

  const mutate = useCallback(
    async (action: () => Promise<unknown>) => {
      try {
        await action();
        query.setSuccess(ALIASES, await apm.listAliases({}));
        return true;
      } catch (e) {
        notifier.error(toAppError(e));
        return false;
      }
    },
    [apm, query, notifier],
  );

  const create = useCallback(
    (input: AliasInput) => mutate(() => apm.createAlias(input)),
    [apm, mutate],
  );
  const update = useCallback(
    (aliasId: string, input: AliasInput) => mutate(() => apm.updateAlias(aliasId, input)),
    [apm, mutate],
  );
  const remove = useCallback(
    (aliasId: string) => mutate(() => apm.deleteAlias(aliasId)),
    [apm, mutate],
  );

  return { result, reload, create, update, remove };
}

/// 时序存储运行状态：`GET /v1/ts/stats`。
export function useStorageStats() {
  const { apm, query, notifier } = useRuntime();
  const result = useQueryRecord<TsStorageStats>(query, STORAGE);

  const run = useCallback(async () => {
    query.setLoading(STORAGE);
    try {
      query.setSuccess(STORAGE, await apm.storageStats());
    } catch (e) {
      const err = toAppError(e);
      query.setError(STORAGE, err);
      notifier.error(err);
    }
  }, [apm, query, notifier]);

  return { result, run };
}

/// 自监控指标读数（instant 查询）：用于把保留/淘汰/限流结果可视化。
export function useSelfMetrics() {
  const { dataplane, query, notifier } = useRuntime();
  const result = useQueryRecord<Record<string, number | undefined>>(query, SELF_METRICS);

  const run = useCallback(async () => {
    query.setLoading(SELF_METRICS);
    const readings: Record<string, number | undefined> = {};
    try {
      for (const [key, expr] of Object.entries(SELF_METRIC_EXPRS)) {
        const envelope = await dataplane.queryInstant(expr);
        readings[key] = latestValue(promSeries(envelope.data));
      }
      query.setSuccess(SELF_METRICS, readings);
    } catch (e) {
      const err = toAppError(e);
      query.setError(SELF_METRICS, err);
      notifier.error(err);
    }
  }, [dataplane, query, notifier]);

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
