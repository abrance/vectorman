import { useCallback, useEffect } from "react";
import type { Agent } from "@vectorman/adapters";
import { toAppError } from "./errors";
import { useQueryRecord } from "./use-query";
import { useRuntime } from "../../app/runtime";

const LIST = "agents.list";
const POLL_MS = 30_000;

export function useAgents(opts: { poll: boolean }) {
  const { gse, query, notifier } = useRuntime();
  const list = useQueryRecord<Agent[]>(query, LIST);

  const refresh = useCallback(
    async (silent: boolean) => {
      if (!silent) {
        query.setLoading(LIST);
      }
      try {
        const data = await gse.listAgents();
        query.setSuccess(LIST, data);
      } catch (e) {
        const err = toAppError(e);
        if (silent) {
          notifier.warning(err.message);
        } else {
          query.setError(LIST, err);
          notifier.error(err);
        }
      }
    },
    [gse, query, notifier],
  );

  useEffect(() => {
    if (!opts.poll) {
      return;
    }
    void refresh(false);
    const timer = setInterval(() => {
      void refresh(true);
    }, POLL_MS);
    return () => {
      clearInterval(timer);
    };
  }, [opts.poll, refresh]);

  const getOne = useCallback(
    async (id: string): Promise<Agent> => {
      const key = `agents.one.${id}`;
      query.setLoading(key);
      try {
        const data = await gse.getAgent(id);
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
    async (agent: Agent) => {
      await gse.upsertAgent(agent);
      notifier.success("Agent 已保存");
      await refresh(false);
    },
    [gse, notifier, refresh],
  );

  const remove = useCallback(
    async (id: string) => {
      await gse.deleteAgent(id);
      notifier.success("Agent 已删除");
      await refresh(false);
    },
    [gse, notifier, refresh],
  );

  return { list, refresh, getOne, save, remove };
}
