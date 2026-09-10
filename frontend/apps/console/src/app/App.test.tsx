import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
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
import { App } from "./App";

function renderAt(path: string) {
  const session = new MemoryAuthSession();
  const mapper = new JsonErrorMapper();
  const query = new MemoryQueryStore();
  const notifier = new MemoryNotifier();
  const http = new FetchHttpClient(session, mapper);
  const gse = new GseAdminAdapter(http);
  const jobs = new GseJobAdapter(http);
  const templates = new GseJobTemplateAdapter(http);
  return render(
    <NodeRuntimeProvider value={{ gse, query, notifier }}>
      <JobRuntimeProvider value={{ jobs, templates, gse, query, notifier }}>
        <MemoryRouter initialEntries={[path]}>
          <App />
        </MemoryRouter>
      </JobRuntimeProvider>
    </NodeRuntimeProvider>,
  );
}

beforeEach(() => {
  vi.stubGlobal(
    "fetch",
    vi.fn(
      async () =>
        new Response("[]", {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
    ),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("console shell", () => {
  it("renders both module groups and the node section", async () => {
    renderAt("/hosts");
    expect(await screen.findByText("节点管理")).toBeTruthy();
    expect(screen.getByText("作业平台")).toBeTruthy();
    expect(screen.getByText("主机")).toBeTruthy();
  });

  it("renders the job section on the jobs route", async () => {
    renderAt("/jobs");
    expect(await screen.findByText("提交作业")).toBeTruthy();
    expect(screen.getByText("模板")).toBeTruthy();
  });
});
