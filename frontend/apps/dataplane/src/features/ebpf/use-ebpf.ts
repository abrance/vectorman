import { useCallback } from "react";
import type {
  CapabilityReport,
  EdgeSearchPage,
  EbpfEventSearchRequest,
  LogRecord,
} from "@vectorman/adapters";
import { useRuntime } from "../../app/runtime";
import { toAppError } from "../errors";
import { useQueryRecord } from "../use-query";

const EDGES = "ebpf.edges";
const EVENTS = "ebpf.events";
const CAPABILITY = "ebpf.capability";

/// eBPF 边列表：与拓扑页共用 `POST /v1/edges/search`，这里缺省只看 eBPF。
export function useEbpfEdges() {
  const { apm, query, notifier } = useRuntime();
  const result = useQueryRecord<EdgeSearchPage>(query, EDGES);

  const run = useCallback(
    async (req: Parameters<typeof apm.searchEdges>[0]) => {
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

/// eBPF 原始事件：`POST /v1/ebpf/events/search`。
export function useEbpfEvents() {
  const { ebpf, query, notifier } = useRuntime();
  const result = useQueryRecord<LogRecord[]>(query, EVENTS);

  const run = useCallback(
    async (req: EbpfEventSearchRequest) => {
      query.setLoading(EVENTS);
      try {
        query.setSuccess(EVENTS, await ebpf.searchEvents(req));
      } catch (e) {
        const err = toAppError(e);
        query.setError(EVENTS, err);
        notifier.error(err);
      }
    },
    [ebpf, query, notifier],
  );

  return { result, run };
}

/// 能力状态：`GET /v1/ebpf/capability`。
export function useEbpfCapability() {
  const { ebpf, query, notifier } = useRuntime();
  const result = useQueryRecord<CapabilityReport>(query, CAPABILITY);

  const run = useCallback(async () => {
    query.setLoading(CAPABILITY);
    try {
      query.setSuccess(CAPABILITY, await ebpf.capability());
    } catch (e) {
      const err = toAppError(e);
      query.setError(CAPABILITY, err);
      notifier.error(err);
    }
  }, [ebpf, query, notifier]);

  return { result, run };
}
