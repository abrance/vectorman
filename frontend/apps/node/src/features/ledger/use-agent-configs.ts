import { useCallback } from "react";
import type { AgentConfig } from "@vectorman/adapters";
import { toAppError } from "./errors";
import { useQueryRecord } from "./use-query";
import { useRuntime } from "../../app/runtime";

const LIST = "agentConfigs.list";

export function useAgentConfigs() {
  const { gse, query, notifier } = useRuntime();
  const list = useQueryRecord<AgentConfig[]>(query, LIST);

  const refresh = useCallback(async () => {
    query.setLoading(LIST);
    try {
      const data = await gse.listAgentConfigs();
      query.setSuccess(LIST, data);
    } catch (e) {
      const err = toAppError(e);
      query.setError(LIST, err);
      notifier.error(err);
    }
  }, [gse, query, notifier]);

  const getOne = useCallback(
    async (id: string): Promise<AgentConfig> => {
      const key = `agentConfigs.one.${id}`;
      query.setLoading(key);
      try {
        const data = await gse.getAgentConfig(id);
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

  const save = useCallback(
    async (cfg: AgentConfig) => {
      await gse.upsertAgentConfig(cfg);
      notifier.success("配置已保存");
      await refresh();
    },
    [gse, notifier, refresh],
  );

  return { list, refresh, getOne, save };
}
