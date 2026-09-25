import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import { ConfigProvider } from "antd";
import zhCN from "antd/locale/zh_CN";
import { ApmAdapter, DataplaneAdapter, EbpfAdapter, FetchHttpClient } from "@vectorman/adapters";
import {
  JsonErrorMapper,
  MemoryAuthSession,
  MemoryNotifier,
  MemoryQueryStore,
} from "@vectorman/primitives";
import { App } from "./app/App";
import { RuntimeProvider } from "./app/runtime";

const session = new MemoryAuthSession();
const mapper = new JsonErrorMapper();
const query = new MemoryQueryStore();
const notifier = new MemoryNotifier();
const http = new FetchHttpClient(session, mapper);
const dataplane = new DataplaneAdapter(http);
const apm = new ApmAdapter(http);
const ebpf = new EbpfAdapter(http);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ConfigProvider
      locale={zhCN}
      theme={{ token: { motion: false } }}
      autoInsertSpaceInButton={false}
    >
      <RuntimeProvider value={{ dataplane, apm, ebpf, query, notifier }}>
        <BrowserRouter>
          <App />
        </BrowserRouter>
      </RuntimeProvider>
    </ConfigProvider>
  </StrictMode>,
);
