import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Job } from "@vectorman/adapters";
import { buildRerunRequest, formatEnv, JobRerunDrawer, parseEnv } from "./job-rerun-drawer";

afterEach(cleanup);

const job: Job = {
  job_id: "job-1",
  agent_id: "a1",
  interpreter: "bash",
  script: "echo hi",
  args: ["-e"],
  env: { LANG: "C" },
  working_dir: "/tmp",
  timeout_secs: 30,
  status: "succeeded",
  created_at: "1",
  updated_at: "1",
};

describe("parseEnv", () => {
  it("parses KEY=VALUE lines and ignores blanks and comments", () => {
    expect(parseEnv("LANG=C\n\n# note\nMODE=prod")).toEqual({ LANG: "C", MODE: "prod" });
    expect(parseEnv("")).toEqual({});
    expect(parseEnv(undefined)).toEqual({});
  });

  it("round-trips through formatEnv", () => {
    expect(parseEnv(formatEnv({ LANG: "C", MODE: "prod" }))).toEqual({
      LANG: "C",
      MODE: "prod",
    });
  });
});

describe("buildRerunRequest", () => {
  it("maps form values into a rerun payload", () => {
    const req = buildRerunRequest({
      agent_id: "a2",
      interpreter: "python3",
      script: "print(1)",
      argsText: "x y",
      envText: "MODE=prod",
      timeout_secs: 60,
      working_dir: "/srv",
    });
    expect(req).toEqual({
      agent_id: "a2",
      interpreter: "python3",
      script: "print(1)",
      args: ["x", "y"],
      env: { MODE: "prod" },
      working_dir: "/srv",
      timeout_secs: 60,
    });
  });

  it("sends an empty working_dir to clear it", () => {
    const req = buildRerunRequest({ agent_id: "a1", interpreter: "bash", script: "echo" });
    expect(req.working_dir).toBe("");
    expect(req.args).toEqual([]);
    expect(req.env).toEqual({});
  });
});

describe("JobRerunDrawer", () => {
  it("prefills every field from the source job and confirms", async () => {
    const onConfirm = vi.fn();
    render(
      <JobRerunDrawer job={job} agents={["a1", "a2"]} onClose={() => {}} onConfirm={onConfirm} />,
    );

    expect(screen.getByDisplayValue("echo hi")).toBeTruthy();
    expect(screen.getByDisplayValue("LANG=C")).toBeTruthy();
    expect(screen.getByDisplayValue("-e")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /重\s*做/ }));
    await waitFor(() => expect(onConfirm).toHaveBeenCalled());
    expect(onConfirm.mock.calls[0][0]).toMatchObject({
      agent_id: "a1",
      interpreter: "bash",
      script: "echo hi",
      args: ["-e"],
      env: { LANG: "C" },
      working_dir: "/tmp",
      timeout_secs: 30,
    });
  });
});
