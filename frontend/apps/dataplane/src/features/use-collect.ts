import { useCallback } from "react";
import type { Agent, CollectItem, StreamEntry } from "@vectorman/adapters";
import { useRuntime } from "../app/runtime";
import { toAppError } from "./errors";
import { toCollectFormValues, toCollectItemInput, type CollectFormValues } from "./collect-form";
import { useQueryRecord } from "./use-query";

const LIST = "collect.items";
const AGENTS = "collect.agents";
const STREAMS = "collect.streams";

/// 单个 `item_id` 的最近接入汇总。
export type StreamSummary = { lastSeenMicros: number; accepted: number };

/// 按 `data_id`（即 `item_id`）汇总流：取最近接入时间与累计 accepted。
export function summarizeStreams(streams: StreamEntry[]): Map<string, StreamSummary> {
  const out = new Map<string, StreamSummary>();
  for (const s of streams) {
    const prev = out.get(s.data_id);
    out.set(s.data_id, {
      lastSeenMicros: Math.max(prev?.lastSeenMicros ?? 0, s.last_seen_micros),
      accepted: (prev?.accepted ?? 0) + s.accepted,
    });
  }
  return out;
}

export function useCollectItems() {
  const { dataplane, query, notifier } = useRuntime();
  const list = useQueryRecord<CollectItem[]>(query, LIST);
  const agents = useQueryRecord<Agent[]>(query, AGENTS);
  const streams = useQueryRecord<StreamEntry[]>(query, STREAMS);

  const refresh = useCallback(async () => {
    query.setLoading(LIST);
    try {
      query.setSuccess(LIST, await dataplane.listCollectItems());
    } catch (e) {
      const err = toAppError(e);
      query.setError(LIST, err);
      notifier.error(err);
    }
    // 流列表只用于展示最近接入，失败不打断链路页。
    try {
      query.setSuccess(STREAMS, await dataplane.listStreams());
    } catch (e) {
      query.setError(STREAMS, toAppError(e));
    }
  }, [dataplane, query, notifier]);

  const loadAgents = useCallback(async () => {
    query.setLoading(AGENTS);
    try {
      query.setSuccess(AGENTS, await dataplane.listAgents());
    } catch (e) {
      const err = toAppError(e);
      query.setError(AGENTS, err);
      notifier.error(err);
    }
  }, [dataplane, query, notifier]);

  const getOne = useCallback(
    (itemId: string): Promise<CollectItem> => dataplane.getCollectItem(itemId),
    [dataplane],
  );

  const save = useCallback(
    async (itemId: string | null, values: CollectFormValues) => {
      const input = toCollectItemInput(values);
      if (itemId) {
        await dataplane.updateCollectItem(itemId, input);
        notifier.success("采集项已保存");
      } else {
        await dataplane.createCollectItem(input);
        notifier.success("采集项已创建");
      }
      await refresh();
    },
    [dataplane, notifier, refresh],
  );

  const setEnabled = useCallback(
    async (item: CollectItem, enabled: boolean) => {
      const input = { ...toCollectItemInput(toCollectFormValues(item)), enabled };
      await dataplane.updateCollectItem(item.item_id, input);
      notifier.success(enabled ? "采集项已启用" : "采集项已停用");
      await refresh();
    },
    [dataplane, notifier, refresh],
  );

  const remove = useCallback(
    async (itemId: string) => {
      await dataplane.deleteCollectItem(itemId);
      notifier.success("采集项已删除");
      await refresh();
    },
    [dataplane, notifier, refresh],
  );

  return { list, agents, streams, refresh, loadAgents, getOne, save, setEnabled, remove };
}
