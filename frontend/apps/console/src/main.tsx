import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import { ConfigProvider } from "antd";
import zhCN from "antd/locale/zh_CN";
import {
  FetchHttpClient,
  GseAdminAdapter,
  GseJobAdapter,
  GseJobTemplateAdapter,
} from "@vectorman/adapters";
import {
  JsonErrorMapper,
  MemoryAuthSession,
  MemoryNotifier,
  MemoryQueryStore,
} from "@vectorman/primitives";
import { RuntimeProvider as NodeRuntimeProvider } from "@vectorman/node";
import { RuntimeProvider as JobRuntimeProvider } from "@vectorman/job";
import { App } from "./app/App";

const session = new MemoryAuthSession();
const mapper = new JsonErrorMapper();
const query = new MemoryQueryStore();
const notifier = new MemoryNotifier();
const http = new FetchHttpClient(session, mapper);
const gse = new GseAdminAdapter(http);
const jobs = new GseJobAdapter(http);
const templates = new GseJobTemplateAdapter(http);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ConfigProvider
      locale={zhCN}
      theme={{ token: { motion: false } }}
      autoInsertSpaceInButton={false}
    >
      <NodeRuntimeProvider value={{ gse, query, notifier }}>
        <JobRuntimeProvider value={{ jobs, templates, gse, query, notifier }}>
          <BrowserRouter>
            <App />
          </BrowserRouter>
        </JobRuntimeProvider>
      </NodeRuntimeProvider>
    </ConfigProvider>
  </StrictMode>,
);
