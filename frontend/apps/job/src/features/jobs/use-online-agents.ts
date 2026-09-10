import { useCallback, useEffect } from "react";
import type { Agent } from "@vectorman/adapters";
import { useRuntime } from "../../app/runtime";
import { toAppError } from "./errors";
import { useQueryRecord } from "./use-query";

const KEY = "jobs.agents.online";
const POLL_MS = 15000;

export function useOnlineAgents() {
  const { gse, query } = useRuntime();
  const list = useQueryRecord<Agent[]>(query, KEY);

  const refresh = useCallback(async () => {
    try {
      const agents = await gse.listAgents();
      query.setSuccess(
        KEY,
        agents.filter((a) => a.status === "online"),
      );
    } catch (e) {
      query.setError(KEY, toAppError(e));
    }
  }, [gse, query]);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => {
      void refresh();
    }, POLL_MS);
    return () => {
      clearInterval(timer);
    };
  }, [refresh]);

  return list;
}
