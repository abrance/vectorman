import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  job: {
    job_id: "job-2",
    agent_id: "a1",
    interpreter: "bash",
    script: "echo hi",
    timeout_secs: 30,
    status: "succeeded",
    template_id: null,
    rerun_of: "job-src",
    created_at: "1",
    updated_at: "1",
  },
}));

vi.mock("../app/runtime", () => ({
  useRuntime: () => ({
    templates: { saveJobAsTemplate: vi.fn() },
    notifier: { success: vi.fn(), error: vi.fn() },
  }),
}));

vi.mock("../features/jobs/use-job-detail", () => ({
  useJobDetail: () => ({ data: mocks.job, status: "success" }),
}));

import { JobDetailDrawer } from "./job-detail-drawer";

afterEach(cleanup);

describe("JobDetailDrawer", () => {
  it("shows the rerun source and invokes onRerun", () => {
    const onRerun = vi.fn();
    render(<JobDetailDrawer jobId="job-2" onClose={() => {}} onRerun={onRerun} />);

    expect(screen.getByText("job-src")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /重\s*做/ }));
    expect(onRerun).toHaveBeenCalledWith(mocks.job);
  });
});
