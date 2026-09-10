import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { buildTemplateInput, TemplateFormDrawer } from "./template-form-drawer";

afterEach(cleanup);

describe("buildTemplateInput", () => {
  it("maps and trims form values", () => {
    const input = buildTemplateInput({
      name: "  collect  ",
      description: "  logs  ",
      interpreter: "bash",
      script: "echo ${svc}",
      argsText: "a b",
      timeout_secs: 30,
      working_dir: "  /tmp  ",
    });
    expect(input).toEqual({
      name: "collect",
      description: "logs",
      interpreter: "bash",
      script: "echo ${svc}",
      args: ["a", "b"],
      timeout_secs: 30,
      working_dir: "/tmp",
    });
  });

  it("omits empty description and working_dir", () => {
    const input = buildTemplateInput({
      name: "t",
      interpreter: "bash",
      script: "echo",
      description: "   ",
      working_dir: "",
    });
    expect(input.description).toBeUndefined();
    expect(input.working_dir).toBeUndefined();
    expect(input.args).toEqual([]);
  });
});

describe("TemplateFormDrawer", () => {
  it("blocks submit when name and script are missing", () => {
    const onSubmit = vi.fn();
    render(
      <TemplateFormDrawer open initial={null} onClose={() => {}} onSubmit={onSubmit} />,
    );
    fireEvent.click(screen.getByRole("button", { name: /保\s*存/ }));
    expect(onSubmit).not.toHaveBeenCalled();
  });
});
