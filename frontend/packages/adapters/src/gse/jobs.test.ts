import { describe, expect, it } from "vitest";
import type { HttpClient, HttpRequest, HttpResponse } from "@vectorman/primitives";
import { GseJobAdapter } from "./jobs";

class FakeHttp implements HttpClient {
  last?: HttpRequest;
  constructor(private readonly body: unknown = {}) {}
  async request<T>(req: HttpRequest): Promise<HttpResponse<T>> {
    this.last = req;
    return { status: 200, body: this.body as T };
  }
}

describe("GseJobAdapter", () => {
  it("submits a job with POST body", async () => {
    const http = new FakeHttp({ job_id: "job-1" });
    const a = new GseJobAdapter(http);
    const body = { agent_id: "a1", script: "echo hi" };
    await a.submitJob(body);
    expect(http.last).toMatchObject({ method: "POST", url: "/api/gse/jobs", body });
  });

  it("lists jobs without query when no filters", async () => {
    const http = new FakeHttp([]);
    const a = new GseJobAdapter(http);
    await a.listJobs();
    expect(http.last?.url).toBe("/api/gse/jobs");
  });

  it("lists jobs with agent, status and limit filters", async () => {
    const http = new FakeHttp([]);
    const a = new GseJobAdapter(http);
    await a.listJobs({ agent_id: "a1", status: "running", limit: 50 });
    expect(http.last).toMatchObject({ method: "GET" });
    expect(http.last?.url).toBe("/api/gse/jobs?agent_id=a1&status=running&limit=50");
  });

  it("encodes job id in path", async () => {
    const http = new FakeHttp({ job_id: "x" });
    const a = new GseJobAdapter(http);
    await a.getJob("job/1");
    expect(http.last?.url).toBe("/api/gse/jobs/job%2F1");
  });

  it("reruns a job with overrides", async () => {
    const http = new FakeHttp({ job_id: "job-2" });
    const a = new GseJobAdapter(http);
    const body = { script: "echo edited" };
    await a.rerunJob("job/1", body);
    expect(http.last).toMatchObject({
      method: "POST",
      url: "/api/gse/jobs/job%2F1/rerun",
      body,
    });
  });

  it("reruns a job with an empty body by default", async () => {
    const http = new FakeHttp({ job_id: "job-2" });
    const a = new GseJobAdapter(http);
    await a.rerunJob("job-1");
    expect(http.last).toMatchObject({
      method: "POST",
      url: "/api/gse/jobs/job-1/rerun",
      body: {},
    });
  });
});
