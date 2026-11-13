import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, describe, expect, it } from "vitest";
import { MemoryNotifier, MemoryQueryStore, type HttpClient } from "@vectorman/primitives";
import { GseAdminAdapter } from "@vectorman/adapters";
import { RuntimeProvider } from "../app/runtime";
import { AgentConfigPage } from "./agent-config-page";

afterEach(() => {
  cleanup();
});

/// 起页面，`/api/gse/agents/<id>/spec` 返回给定的 view。
function renderPage(view: unknown) {
  const gse = new GseAdminAdapter({
    request: async () => ({ status: 200, body: view }),
  } as unknown as HttpClient);
  render(
    <RuntimeProvider
      value={{ gse, query: new MemoryQueryStore(), notifier: new MemoryNotifier() }}
    >
      <MemoryRouter initialEntries={["/agents/a1/config"]}>
        <Routes>
          <Route path="/agents/:agentId/config" element={<AgentConfigPage />} />
        </Routes>
      </MemoryRouter>
    </RuntimeProvider>,
  );
}

const baseView = {
  agent_id: "a1",
  host_id: "h1",
  session_state: "online",
  sync_status: "synced",
  desired: {
    revision: "r1",
    spec: { params: { heartbeat_interval_secs: 15 }, items: [] },
  },
  applied: null,
};

describe("AgentConfigPage 心跳上限", () => {
  it("上限读服务端下发的 limits，不硬编码 30", async () => {
    // 服务端判活窗口被调小后，页面必须跟着变，否则会拦下合法值或放过非法值。
    renderPage({ ...baseView, limits: { heartbeat_interval_secs_max: 10 } });

    await waitFor(() => {
      expect(screen.getByDisplayValue("15")).toBeTruthy();
    });
    expect(screen.getByText(/不得超过 10 秒/)).toBeTruthy();
    // 输入框的 max 也必须是服务端那个数。
    const input = screen.getByDisplayValue("15") as HTMLInputElement;
    expect(input.getAttribute("aria-valuemax")).toBe("10");
  });

  it("拿不到 limits 时文案标明是缺省值，不谎称已确认", async () => {
    renderPage({ ...baseView, limits: null });

    await waitFor(() => {
      expect(screen.getByDisplayValue("15")).toBeTruthy();
    });
    expect(screen.getByText(/当前未知，缺省 30 秒/)).toBeTruthy();
  });
});

describe("AgentConfigPage 同步状态接线", () => {
  it("sync_status=stale 时渲染「未同步」标签与下发提示", async () => {
    // 接线层：spec-diff 的单测只证明 syncStatusMeta 返回什么，
    // 这里证明页面真的把它印在标签和提示条上（改错 label 必须变红）。
    renderPage({ ...baseView, sync_status: "stale" });

    await waitFor(() => {
      expect(screen.getByText("未同步")).toBeTruthy();
    });
    expect(
      screen.getByText("有改动未下发，或 Agent 未按期望生效 —— 点「下发」"),
    ).toBeTruthy();
    // 反向：stale 绝不能印成「已同步」，否则运维会以为配置已生效。
    expect(screen.queryByText("已同步")).toBeNull();
  });

  it("sync_status=synced 时渲染「已同步」，与 stale 不是同一句", async () => {
    renderPage({ ...baseView, sync_status: "synced" });

    await waitFor(() => {
      expect(screen.getByText("已同步")).toBeTruthy();
    });
    expect(screen.getByText("期望值与 Agent 生效值一致")).toBeTruthy();
    expect(screen.queryByText("未同步")).toBeNull();
  });
});
