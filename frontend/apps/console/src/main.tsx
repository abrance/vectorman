import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { FetchHttpClient, GseAdminAdapter, PromQueryAdapter, SqlHttpAdapter } from "@vectorman/adapters";
import { JsonErrorMapper, MemoryAuthSession, MemoryNotifier, MemoryQueryStore } from "@vectorman/primitives";
import { App } from "./app/App";

const session = new MemoryAuthSession();
const mapper = new JsonErrorMapper();
const queryStore = new MemoryQueryStore();
const notifier = new MemoryNotifier();
const http = new FetchHttpClient(session, mapper);
const gse = new GseAdminAdapter(http);
const sql = new SqlHttpAdapter(http);
const prom = new PromQueryAdapter(http, mapper);
void queryStore;
void notifier;
void gse;
void sql;
void prom;

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
