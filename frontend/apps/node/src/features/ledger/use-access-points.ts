import { useCallback } from "react";
import type { AccessPoint } from "@vectorman/adapters";
import { toAppError } from "./errors";
import { useQueryRecord } from "./use-query";
import { useRuntime } from "../../app/runtime";

const LIST = "accessPoints.list";

export function useAccessPoints() {
  const { gse, query, notifier } = useRuntime();
  const list = useQueryRecord<AccessPoint[]>(query, LIST);

  const refresh = useCallback(async () => {
    query.setLoading(LIST);
    try {
      const data = await gse.listAccessPoints();
      query.setSuccess(LIST, data);
    } catch (e) {
      const err = toAppError(e);
      query.setError(LIST, err);
      notifier.error(err);
    }
  }, [gse, query, notifier]);

  const getOne = useCallback(
    async (id: string): Promise<AccessPoint> => {
      const key = `accessPoints.one.${id}`;
      query.setLoading(key);
      try {
        const data = await gse.getAccessPoint(id);
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
    async (ap: AccessPoint) => {
      await gse.upsertAccessPoint(ap);
      notifier.success("接入点已保存");
      await refresh();
    },
    [gse, notifier, refresh],
  );

  const remove = useCallback(
    async (id: string) => {
      await gse.deleteAccessPoint(id);
      notifier.success("接入点已删除");
      await refresh();
    },
    [gse, notifier, refresh],
  );

  return { list, refresh, getOne, save, remove };
}
