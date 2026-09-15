import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";

type FakeApp = {
  app_id: string;
  name: string;
  url: string;
  tags: string[];
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
        const body = JSON.parse(String(init?.body || "{}")) as {
          name: string;
          url: string;
          tags?: string[];
        };
        const app: FakeApp = {
          app_id: `app-${store.length + 1}`,
          name: body.name,
          url: body.url,
          tags: body.tags ?? [],
          created_at: "1",
          updated_at: "1",
        };
        store = [...store, app];
        return json(app, 201);
      }
      if (url.includes("/api/console/apps/") && method === "PUT") {
        const id = url.split("/").pop() || "";
        const body = JSON.parse(String(init?.body || "{}")) as {
          name: string;
          url: string;
          tags?: string[];
        };
        store = store.map((a) =>
          a.app_id === id
            ? { ...a, name: body.name, url: body.url, tags: body.tags ?? [], updated_at: "2" }
            : a,
        );
        const updated = store.find((a) => a.app_id === id);
        return updated ? json(updated) : json({ error: "not_found", message: "no" }, 404);
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
        tags: [],
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

  it("renders records whose backend omits tags", async () => {
    // 旧版本后端不返回 tags 字段；目录应正常渲染而不是崩溃。
    store = [
      {
        app_id: "app-1",
        name: "GSE",
        url: "http://127.0.0.1:7101",
        created_at: "1",
        updated_at: "1",
      } as unknown as FakeApp,
    ];
    render(<App />);
    expect(await screen.findByText("GSE")).toBeTruthy();
    expect(screen.queryByText("全部")).toBeTruthy();
  });

  it("deletes after confirm", async () => {
    store = [
      {
        app_id: "app-1",
        name: "GSE",
        url: "http://127.0.0.1:7101",
        tags: [],
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

  it("adds tags in the create form and posts them", async () => {
    render(<App />);
    fireEvent.click(await screen.findByText("添加 App"));
    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "GSE" } });
    fireEvent.change(screen.getByLabelText("URL"), { target: { value: "http://127.0.0.1:7101" } });
    fireEvent.change(screen.getByLabelText("Tag"), { target: { value: "prod" } });
    fireEvent.keyDown(screen.getByLabelText("Tag"), { key: "Enter" });
    expect(screen.getByText("prod")).toBeTruthy();
    fireEvent.click(screen.getByText("保存"));
    await waitFor(() => {
      const post = vi
        .mocked(fetch)
        .mock.calls.find(
          ([input, init]) =>
            String(input).endsWith("/api/console/apps") && (init?.method || "GET").toUpperCase() === "POST",
        );
      expect(post).toBeTruthy();
      expect(JSON.parse(String(post?.[1]?.body))).toMatchObject({
        name: "GSE",
        url: "http://127.0.0.1:7101",
        tags: ["prod"],
      });
    });
  });

  it("shows existing tags on edit and can remove them", async () => {
    store = [
      {
        app_id: "app-1",
        name: "GSE",
        url: "http://127.0.0.1:7101",
        tags: ["prod"],
        created_at: "1",
        updated_at: "1",
      },
    ];
    render(<App />);
    fireEvent.click(await screen.findByText("编辑"));
    expect(screen.getByLabelText("移除 prod")).toBeTruthy();
    fireEvent.click(screen.getByLabelText("移除 prod"));
    fireEvent.click(screen.getByText("保存"));
    await waitFor(() => {
      const put = vi
        .mocked(fetch)
        .mock.calls.find(
          ([input, init]) =>
            String(input).includes("/api/console/apps/") && (init?.method || "").toUpperCase() === "PUT",
        );
      expect(JSON.parse(String(put?.[1]?.body)).tags).toEqual([]);
    });
  });

  it("shows 全部 when catalog has no tags", async () => {
    render(<App />);
    expect(await screen.findByRole("button", { name: "全部" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "prod" })).toBeNull();
  });

  it("renders tags under icons", async () => {
    store = [
      {
        app_id: "app-1",
        name: "GSE",
        url: "http://127.0.0.1:7101",
        tags: ["prod"],
        created_at: "1",
        updated_at: "1",
      },
    ];
    render(<App />);
    expect(await screen.findByText("GSE")).toBeTruthy();
    expect(screen.getByText("prod", { selector: ".tile-tags .chip" })).toBeTruthy();
  });

  it("filters icons with AND and restores on 全部", async () => {
    store = [
      {
        app_id: "app-1",
        name: "GSE",
        url: "http://127.0.0.1:7101",
        tags: ["prod", "gse"],
        created_at: "1",
        updated_at: "1",
      },
      {
        app_id: "app-2",
        name: "Job",
        url: "http://127.0.0.1:7101/jobs",
        tags: ["prod"],
        created_at: "2",
        updated_at: "2",
      },
    ];
    render(<App />);
    expect(await screen.findByText("GSE")).toBeTruthy();
    expect(screen.getByText("Job")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "prod" }));
    expect(screen.getByText("GSE")).toBeTruthy();
    expect(screen.getByText("Job")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "gse" }));
    expect(screen.getByText("GSE")).toBeTruthy();
    expect(screen.queryByText("Job")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "gse" }));
    expect(screen.getByText("Job")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "全部" }));
    expect(screen.getByText("GSE")).toBeTruthy();
    expect(screen.getByText("Job")).toBeTruthy();
  });

  it("does not open the app when clicking a tile tag", async () => {
    store = [
      {
        app_id: "app-1",
        name: "GSE",
        url: "http://127.0.0.1:7101",
        tags: ["prod"],
        created_at: "1",
        updated_at: "1",
      },
    ];
    const open = vi.fn();
    vi.stubGlobal("open", open);
    render(<App />);
    fireEvent.click(await screen.findByText("prod", { selector: ".tile-tags .chip" }));
    expect(open).not.toHaveBeenCalled();
  });
});
