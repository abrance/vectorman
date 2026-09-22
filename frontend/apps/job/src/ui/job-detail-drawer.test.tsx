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
    jobs: { downloadJobFileUrl: (id: string) => `/api/gse/job-files/${id}` },
  }),
}));

vi.mock("../features/jobs/use-job-detail", () => ({
  useJobDetail: () => ({ data: mocks.job, status: "success" }),
}));

import { formatFileEndpoint, JobDetailDrawer } from "./job-detail-drawer";

afterEach(cleanup);

describe("JobDetailDrawer", () => {
  it("shows the rerun source and invokes onRerun", () => {
    const onRerun = vi.fn();
    render(<JobDetailDrawer jobId="job-2" onClose={() => {}} onRerun={onRerun} />);

    expect(screen.getByText("job-src")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /重\s*做/ }));
    expect(onRerun).toHaveBeenCalledWith(mocks.job);
  });

  it("formats file endpoints", () => {
    expect(formatFileEndpoint({ type: "agent", agent_id: "a1", path: "/tmp/a" })).toBe("a1:/tmp/a");
    expect(formatFileEndpoint({ type: "server_temp", file_id: "file-1" })).toBe("临时文件 file-1");
    expect(formatFileEndpoint({ type: "server_temp" })).toBe("Server 临时目录");
  });
});
