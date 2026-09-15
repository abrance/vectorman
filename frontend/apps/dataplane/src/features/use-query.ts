import { useCallback, useSyncExternalStore } from "react";
import type { QueryRecord, QueryStore } from "@vectorman/primitives";

export function useQueryRecord<T>(store: QueryStore, key: string): QueryRecord<T> {
  const subscribe = useCallback((onStoreChange: () => void) => store.subscribe(key, onStoreChange), [store, key]);
  const getSnapshot = useCallback(() => store.get<T>(key), [store, key]);
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
