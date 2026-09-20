import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { JobTemplate } from "@vectorman/adapters";
import { buildFileJobSubmit, buildJobSubmit, JobSubmitDrawer, parseArgs } from "./job-submit-drawer";

afterEach(cleanup);

describe("parseArgs", () => {
  it("splits on whitespace and drops empties", () => {
    expect(parseArgs("a  b\tc")).toEqual(["a", "b", "c"]);
    expect(parseArgs("")).toEqual([]);
    expect(parseArgs(undefined)).toEqual([]);
  });
});

describe("buildJobSubmit", () => {
  it("maps form values into a job submit payload", () => {
    const req = buildJobSubmit({
      agent_id: "a1",
      interpreter: "python3",
      script: "print(1)",
      argsText: "x y",
      timeout_secs: 30,
      working_dir: "  /tmp  ",
    });
    expect(req).toEqual({
      agent_id: "a1",
      interpreter: "python3",
      script: "print(1)",
      args: ["x", "y"],
      timeout_secs: 30,
      working_dir: "/tmp",
    });
  });

  it("omits empty working_dir and defaults args to empty", () => {
    const req = buildJobSubmit({ agent_id: "a1", interpreter: "bash", script: "echo" });
    expect(req.working_dir).toBeUndefined();
    expect(req.args).toEqual([]);
  });
});

describe("buildFileJobSubmit", () => {
  it("builds agent to agent endpoints", () => {
    expect(
      buildFileJobSubmit({
        kind: "file_transfer",
        sourceType: "agent",
        source_agent_id: "a1",
        source_path: "/tmp/a",
        destType: "agent",
        dest_agent_id: "a2",
        dest_path: "/tmp/b",
        timeout_secs: 60,
      }),
    ).toEqual({
      kind: "file_transfer",
      source: { type: "agent", agent_id: "a1", path: "/tmp/a" },
      destination: { type: "agent", agent_id: "a2", path: "/tmp/b" },
      timeout_secs: 60,
    });
  });

  it("builds uploaded file to server temp", () => {
    expect(
      buildFileJobSubmit({
        kind: "file_transfer",
        sourceType: "uploaded",
        source_file_id: "file-1",
        destType: "server_temp",
      }),
    ).toEqual({
      kind: "file_transfer",
      source: { type: "server_temp", file_id: "file-1" },
      destination: { type: "server_temp" },
      timeout_secs: undefined,
    });
  });
});

describe("JobSubmitDrawer", () => {
  it("does not submit when required fields are missing", () => {
    const onSubmit = vi.fn();
    render(<JobSubmitDrawer open agents={["a1"]} onClose={() => {}} onSubmit={onSubmit} />);
    fireEvent.click(screen.getByRole("button", { name: /提\s*交/ }));
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("submits via template with vars when a template is selected", async () => {
    const template: JobTemplate = {
      template_id: "tpl-1",
      name: "collect",
      interpreter: "bash",
      script: "echo ${svc}",
      args: ["--tag=${svc}"],
      env: {},
      working_dir: null,
      timeout_secs: 60,
      created_at: "1",
      updated_at: "1",
    };
    const onSubmit = vi.fn();
    const onSubmitTemplate = vi.fn();
    render(
      <JobSubmitDrawer
        open
        agents={["a1"]}
        templates={[template]}
        onClose={() => {}}
        onSubmit={onSubmit}
        onSubmitTemplate={onSubmitTemplate}
      />,
    );

    fireEvent.mouseDown(screen.getAllByRole("combobox")[0]);
    fireEvent.click(await screen.findByTitle("collect"));

    fireEvent.mouseDown(screen.getAllByRole("combobox")[1]);
    fireEvent.click(await screen.findByTitle("a1"));

    fireEvent.change(screen.getByPlaceholderText("${svc}"), {
      target: { value: "nginx" },
    });

    fireEvent.click(screen.getByRole("button", { name: /提\s*交/ }));
    await waitFor(() =>
      expect(onSubmitTemplate).toHaveBeenCalledWith("tpl-1", {
        agent_id: "a1",
        vars: { svc: "nginx" },
      }),
    );
    expect(onSubmit).not.toHaveBeenCalled();
  });
});
