import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import {
  MemoryNotifier,
  MemoryQueryStore,
  type HttpClient,
} from "@vectorman/primitives";
import { GseAdminAdapter } from "@vectorman/adapters";
import { RuntimeProvider } from "../app/runtime";
import { AgentsPage } from "./agents-page";

afterEach(() => {
  cleanup();
});

/// 起页面，`/api/gse/agents` 返回给定的一份台账。
function renderPage(agents: unknown[]) {
  const notifier = new MemoryNotifier();
  const query = new MemoryQueryStore();
  const gse = new GseAdminAdapter({
    request: async (req: { path?: string; url?: string }) => {
      const url = String(req.path ?? req.url ?? "");
      if (url.includes("agents")) {
        return { status: 200, body: agents };
      }
      return { status: 200, body: [] };
    },
  } as unknown as HttpClient);
  render(
    <RuntimeProvider value={{ gse, query, notifier }}>
      <AgentsPage />
    </RuntimeProvider>,
  );
}

describe("AgentsPage 会话状态", () => {
  it("心跳在线但会话缺失时，页面必须显示作业通道不可用", async () => {
    // 这是本次事故的形态：心跳一直刷新（status=online），连接却已死（session absent）。
    renderPage([
      {
        agent_id: "testbkee",
        host_id: "bkee5",
        token: "vm-x",
        status: "online",
        session_state: "absent",
        job_channel_available: false,
      },
    ]);

    await waitFor(() => {
      expect(screen.getByText("testbkee")).toBeTruthy();
    });
    // 心跳口径与作业通道口径都要可见
    expect(screen.getByText("心跳在线但会话不可用")).toBeTruthy();
    expect(screen.getByText("no session")).toBeTruthy();
  });

  it("会话在线时显示 available", async () => {
    renderPage([
      {
        agent_id: "debian12-agent",
        host_id: "debian12",
        token: "vm-y",
        status: "online",
        session_state: "online",
        job_channel_available: true,
      },
    ]);

    await waitFor(() => {
      expect(screen.getByText("debian12-agent")).toBeTruthy();
    });
    expect(screen.getByText("available")).toBeTruthy();
    expect(screen.queryByText("心跳在线但会话不可用")).toBeNull();
  });
});
