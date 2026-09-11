import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";

type FakeApp = {
  app_id: string;
  name: string;
  url: string;
  created_at: string;
  updated_at: string;
};

let store: FakeApp[] = [];

function json(data: unknown, status = 200) {
  return new Response(status === 204 ? null : JSON.stringify(data), {
    status,
    headers: { "content-type": "application/json" },
  });
}

beforeEach(() => {
  store = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const method = (init?.method || "GET").toUpperCase();
      if (url.endsWith("/api/console/apps") && method === "GET") {
        return json(store);
      }
      if (url.endsWith("/api/console/apps") && method === "POST") {
        const body = JSON.parse(String(init?.body || "{}")) as { name: string; url: string };
        const app: FakeApp = {
          app_id: `app-${store.length + 1}`,
          name: body.name,
          url: body.url,
          created_at: "1",
          updated_at: "1",
        };
        store = [...store, app];
        return json(app, 201);
      }
      if (url.includes("/api/console/apps/") && method === "DELETE") {
        const id = url.split("/").pop() || "";
        store = store.filter((a) => a.app_id !== id);
        return json(null, 204);
      }
      return json({ error: "not_found", message: "no" }, 404);
    }),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("desktop", () => {
  it("shows empty catalog and add entry", async () => {
    render(<App />);
    expect(await screen.findByText("目录是空的。把 GSE 或其他页面加进来。")).toBeTruthy();
    expect(screen.getByText("添加 App")).toBeTruthy();
  });

  it("opens target url in a new tab", async () => {
    store = [
      {
        app_id: "app-1",
        name: "GSE",
        url: "http://127.0.0.1:7101",
        created_at: "1",
        updated_at: "1",
      },
    ];
    const open = vi.fn();
    vi.stubGlobal("open", open);
    render(<App />);
    fireEvent.click(await screen.findByText("GSE"));
    expect(open).toHaveBeenCalledWith("http://127.0.0.1:7101", "_blank", "noopener,noreferrer");
  });

  it("deletes after confirm", async () => {
    store = [
      {
        app_id: "app-1",
        name: "GSE",
        url: "http://127.0.0.1:7101",
        created_at: "1",
        updated_at: "1",
      },
    ];
    vi.stubGlobal("confirm", () => true);
    render(<App />);
    fireEvent.click(await screen.findByText("删除"));
    await waitFor(() => {
      expect(screen.queryByText("GSE")).toBeNull();
    });
  });
});
