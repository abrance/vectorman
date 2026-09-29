import { useCallback } from "react";
import type { Agent, AgentSpecView, SpecItem, StreamEntry } from "@vectorman/adapters";
import { useRuntime } from "../app/runtime";
import { toAppError } from "./errors";
import { useQueryRecord } from "./use-query";

const SPECS = "spec.catalog";
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

/// 某台 Agent 的一条采集项。
export type AgentItem = { agentId: string; item: SpecItem };

/// 跨 Agent 展平采集项（采集链路总览用）。
///
/// 采集项现在属于**各台 Agent 的 spec**：同一条 `item_id` 可能出现在多台 Agent 里
/// （迁移会把旧的全局采集项展开成多份拷贝，之后各自可改），所以这里保留 agent 维度。
export function flattenByAgent(views: AgentSpecView[]): AgentItem[] {
  const out: AgentItem[] = [];
  for (const view of views) {
    for (const item of view.desired?.spec.items ?? []) {
      out.push({ agentId: view.agent_id, item });
    }
  }
  return out.sort(
    (a, b) =>
      a.agentId.localeCompare(b.agentId) || a.item.item_id.localeCompare(b.item.item_id),
  );
}

/// 按 `item_id` 去重（指标页的选择器只需要「有哪些采集项」）。
export function dedupeItems(views: AgentSpecView[]): SpecItem[] {
  const byId = new Map<string, SpecItem>();
  for (const { item } of flattenByAgent(views)) {
    if (!byId.has(item.item_id)) {
      byId.set(item.item_id, item);
    }
  }
  return [...byId.values()].sort((a, b) => a.item_id.localeCompare(b.item_id));
}

/// `item_id` → 使用它的 Agent 列表（「这条采集项被几台用了」要看得见）。
export function itemUsage(rows: AgentItem[]): Map<string, string[]> {
  const out = new Map<string, string[]>();
  for (const { agentId, item } of rows) {
    const list = out.get(item.item_id) ?? [];
    if (!list.includes(agentId)) {
      list.push(agentId);
    }
    out.set(item.item_id, list);
  }
  return out;
}

/// 采集链路总览的取数：**只读**。
///
/// 配置编辑在 console 的「Agent 配置」页（直连 gse-server）；数据面只读透传
/// `GET /v1/agent-specs`，避免出现两个写入口。
export function useSpecCatalog() {
  const { dataplane, query, notifier } = useRuntime();
  const specs = useQueryRecord<AgentSpecView[]>(query, SPECS);
  const agents = useQueryRecord<Agent[]>(query, AGENTS);
  const streams = useQueryRecord<StreamEntry[]>(query, STREAMS);

  const refresh = useCallback(async () => {
    query.setLoading(SPECS);
    try {
      query.setSuccess(SPECS, await dataplane.listAgentSpecs());
    } catch (e) {
      const err = toAppError(e);
      query.setError(SPECS, err);
      notifier.error(err);
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

  const loadStreams = useCallback(async () => {
    query.setLoading(STREAMS);
    try {
      query.setSuccess(STREAMS, await dataplane.listStreams());
    } catch (e) {
      const err = toAppError(e);
      query.setError(STREAMS, err);
      notifier.error(err);
    }
  }, [dataplane, query, notifier]);

  const views = specs.data ?? [];
  const rows = flattenByAgent(views);
  return {
    specs,
    agents,
    streams,
    /// 去重后的采集项列表（指标页的 `data_id` 选择器用）。
    list: { ...specs, data: dedupeItems(views) },
    rows,
    usage: itemUsage(rows),
    refresh,
    loadAgents,
    loadStreams,
  };
}
