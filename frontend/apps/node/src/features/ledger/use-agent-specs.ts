import { useCallback } from "react";
import type { AgentSpecPutBody, AgentSpecView } from "@vectorman/adapters";
import { toAppError } from "./errors";
import { useQueryRecord } from "./use-query";
import { useRuntime } from "../../app/runtime";

const LIST = "agentSpecs.list";

/// Agent 配置中心的取数：列表、详情、保存期望、下发。
///
/// 保存与下发是**两个独立动作**：`put` 只写期望（不推送），`apply` 才推送单台 Agent。
export function useAgentSpecs() {
  const { gse, query, notifier } = useRuntime();
  const list = useQueryRecord<AgentSpecView[]>(query, LIST);

  const refresh = useCallback(async () => {
    query.setLoading(LIST);
    try {
      query.setSuccess(LIST, await gse.listAgentSpecs());
    } catch (e) {
      const err = toAppError(e);
      query.setError(LIST, err);
      notifier.error(err);
    }
  }, [gse, query, notifier]);

  const getOne = useCallback(
    async (id: string): Promise<AgentSpecView> => {
      const key = `agentSpecs.one.${id}`;
      query.setLoading(key);
      try {
        const data = await gse.getAgentSpec(id);
        query.setSuccess(key, data);
        return data;
      } catch (e) {
        const err = toAppError(e);
        query.setError(key, err);
        throw err;
      }
    },
    [gse, query],
  );

  const put = useCallback(
    async (id: string, body: AgentSpecPutBody): Promise<AgentSpecView> => {
      const saved = await gse.putAgentSpec(id, body);
      notifier.success("期望配置已保存（未下发）");
      await refresh();
      return saved;
    },
    [gse, notifier, refresh],
  );

  const apply = useCallback(
    async (id: string) => {
      const result = await gse.applyAgentSpec(id);
      const outcome = result?.ack?.outcome ?? "";
      if (outcome === "rejected") {
        notifier.warning(`Agent 拒绝了配置：${result.ack.detail || "含不可下发字段"}`);
      } else if (outcome === "partial") {
        notifier.warning(
          `已下发，但有未实现字段：${(result.ack.not_enforced ?? []).join("、")}`,
        );
      } else {
        notifier.success(`已下发（${outcome || "ok"}）`);
      }
      await refresh();
      return result;
    },
    [gse, notifier, refresh],
  );

  return { list, refresh, getOne, put, apply };
}
