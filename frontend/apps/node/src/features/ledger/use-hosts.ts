import { useCallback } from "react";
import type { Host } from "@vectorman/adapters";
import { toAppError } from "./errors";
import { useQueryRecord } from "./use-query";
import { useRuntime } from "../../app/runtime";

const LIST = "hosts.list";

export function useHosts() {
  const { gse, query, notifier } = useRuntime();
  const list = useQueryRecord<Host[]>(query, LIST);

  const refresh = useCallback(async () => {
    query.setLoading(LIST);
    try {
      const data = await gse.listHosts();
      query.setSuccess(LIST, data);
    } catch (e) {
      const err = toAppError(e);
      query.setError(LIST, err);
      notifier.error(err);
    }
  }, [gse, query, notifier]);

  const getOne = useCallback(
    async (id: string): Promise<Host> => {
      const key = `hosts.one.${id}`;
      query.setLoading(key);
      try {
        const data = await gse.getHost(id);
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
    async (host: Host) => {
      await gse.upsertHost(host);
      notifier.success("主机已保存");
      await refresh();
    },
    [gse, notifier, refresh],
  );

  const remove = useCallback(
    async (id: string) => {
      await gse.deleteHost(id);
      notifier.success("主机已删除");
      await refresh();
    },
    [gse, notifier, refresh],
  );

  return { list, refresh, getOne, save, remove };
}
