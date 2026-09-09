import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { FetchHttpClient, GseAdminAdapter, PromQueryAdapter, SqlHttpAdapter } from "@vectorman/adapters";
import { JsonErrorMapper, MemoryAuthSession, MemoryNotifier, MemoryQueryStore } from "@vectorman/primitives";
import { App } from "./app/App";

const session = new MemoryAuthSession();
const mapper = new JsonErrorMapper();
const http = new FetchHttpClient(session, mapper);
void new MemoryQueryStore();
void new MemoryNotifier();
void new GseAdminAdapter(http);
void new SqlHttpAdapter(http);
void new PromQueryAdapter(http, mapper);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
