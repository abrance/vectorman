import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { GseJobTemplateAdapter, JobTemplate } from "@vectorman/adapters";
import { MemoryNotifier, MemoryQueryStore } from "@vectorman/primitives";
import { RuntimeProvider, type Runtime } from "../../app/runtime";
import { extractVariables, useJobTemplates } from "./use-job-templates";

const tpl: JobTemplate = {
  template_id: "tpl-1",
  name: "collect",
  interpreter: "bash",
  script: "echo ${svc}",
  args: ["--tag=${svc}"],
  env: { SVC: "${svc}" },
  working_dir: "/var/log/${svc}",
  timeout_secs: 60,
  created_at: "1",
  updated_at: "1",
};

function makeRuntime(templates: Partial<GseJobTemplateAdapter>) {
  const notifier = new MemoryNotifier();
  const query = new MemoryQueryStore();
  const runtime = {
    jobs: {},
    templates,
    gse: {},
    query,
    notifier,
  } as unknown as Runtime;
  return { runtime, notifier };
}

function wrap(runtime: Runtime) {
  return function Wrapper({ children }: { children: React.ReactNode }) {
    return <RuntimeProvider value={runtime}>{children}</RuntimeProvider>;
  };
}

describe("extractVariables", () => {
  it("extracts from script only", () => {
    expect(extractVariables({ script: "echo ${a} ${b}" })).toEqual(["a", "b"]);
  });

  it("extracts from args, env and working_dir", () => {
    expect(extractVariables(tpl)).toEqual(["svc"]);
  });

  it("returns empty when no placeholders", () => {
    expect(extractVariables({ script: "echo hello" })).toEqual([]);
  });
});

describe("useJobTemplates", () => {
  it("loads templates", async () => {
    const listTemplates = vi.fn(async () => [tpl]);
    const { runtime } = makeRuntime({ listTemplates });
    const { result } = renderHook(() => useJobTemplates(), { wrapper: wrap(runtime) });
    await waitFor(() => expect(result.current.list.data).toEqual([tpl]));
  });

  it("refreshes after create", async () => {
    const listTemplates = vi.fn(async () => [tpl]);
    const createTemplate = vi.fn(async () => tpl);
    const { runtime } = makeRuntime({ listTemplates, createTemplate });
    const { result } = renderHook(() => useJobTemplates(), { wrapper: wrap(runtime) });
    await waitFor(() => expect(result.current.list.data).toEqual([tpl]));

    await act(async () => {
      await result.current.create({ name: "x", script: "echo" });
    });
    expect(createTemplate).toHaveBeenCalledWith({ name: "x", script: "echo" });
    expect(listTemplates).toHaveBeenCalledTimes(2);
  });

  it("refreshes after remove", async () => {
    const listTemplates = vi.fn(async () => [tpl]);
    const deleteTemplate = vi.fn(async () => undefined);
    const { runtime } = makeRuntime({ listTemplates, deleteTemplate });
    const { result } = renderHook(() => useJobTemplates(), { wrapper: wrap(runtime) });
    await waitFor(() => expect(result.current.list.data).toEqual([tpl]));

    await act(async () => {
      await result.current.remove("tpl-1");
    });
    expect(deleteTemplate).toHaveBeenCalledWith("tpl-1");
    expect(listTemplates).toHaveBeenCalledTimes(2);
  });

  it("notifies on load failure", async () => {
    const listTemplates = vi.fn(async () => {
      throw new Error("boom");
    });
    const { runtime, notifier } = makeRuntime({ listTemplates });
    const errorSpy = vi.spyOn(notifier, "error");
    renderHook(() => useJobTemplates(), { wrapper: wrap(runtime) });
    await waitFor(() => expect(errorSpy).toHaveBeenCalled());
  });
});
