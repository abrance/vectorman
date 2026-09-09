import { createContext, useContext } from "react";
import type { GseAdminAdapter } from "@vectorman/adapters";
import type { Notifier, QueryStore } from "@vectorman/primitives";

export type Runtime = {
  gse: GseAdminAdapter;
  query: QueryStore;
  notifier: Notifier;
};

const RuntimeContext = createContext<Runtime | null>(null);

export function RuntimeProvider({
  value,
  children,
}: {
  value: Runtime;
  children: React.ReactNode;
}) {
  return <RuntimeContext.Provider value={value}>{children}</RuntimeContext.Provider>;
}

export function useRuntime(): Runtime {
  const ctx = useContext(RuntimeContext);
  if (!ctx) {
    throw new Error("RuntimeProvider missing");
  }
  return ctx;
}
