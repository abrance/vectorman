import { describe, expect, it } from "vitest";
import type { HttpClient, HttpRequest, HttpResponse } from "@vectorman/primitives";
import { GseJobTemplateAdapter } from "./templates";

class FakeHttp implements HttpClient {
  last?: HttpRequest;
  constructor(private readonly body: unknown = {}) {}
  async request<T>(req: HttpRequest): Promise<HttpResponse<T>> {
    this.last = req;
    return { status: 200, body: this.body as T };
  }
}

describe("GseJobTemplateAdapter", () => {
  it("lists templates without query when no filters", async () => {
    const http = new FakeHttp([]);
    const a = new GseJobTemplateAdapter(http);
    await a.listTemplates();
    expect(http.last).toMatchObject({ method: "GET", url: "/api/gse/job-templates" });
  });

  it("lists templates with name and limit filters", async () => {
    const http = new FakeHttp([]);
    const a = new GseJobTemplateAdapter(http);
    await a.listTemplates({ name: "deploy", limit: 20 });
    expect(http.last?.url).toBe("/api/gse/job-templates?name=deploy&limit=20");
  });

  it("gets a template with encoded id", async () => {
    const http = new FakeHttp({ template_id: "x" });
    const a = new GseJobTemplateAdapter(http);
    await a.getTemplate("tpl/1");
    expect(http.last?.url).toBe("/api/gse/job-templates/tpl%2F1");
  });

  it("creates a template", async () => {
    const http = new FakeHttp({ template_id: "tpl-1" });
    const a = new GseJobTemplateAdapter(http);
    const body = { name: "t", script: "echo hi" };
    await a.createTemplate(body);
    expect(http.last).toMatchObject({ method: "POST", url: "/api/gse/job-templates", body });
  });

  it("updates a template", async () => {
    const http = new FakeHttp({ template_id: "tpl-1" });
    const a = new GseJobTemplateAdapter(http);
    const body = { name: "t2", script: "echo hi" };
    await a.updateTemplate("tpl-1", body);
    expect(http.last).toMatchObject({
      method: "PUT",
      url: "/api/gse/job-templates/tpl-1",
      body,
    });
  });

  it("deletes a template", async () => {
    const http = new FakeHttp();
    const a = new GseJobTemplateAdapter(http);
    await a.deleteTemplate("tpl-1");
    expect(http.last).toMatchObject({ method: "DELETE", url: "/api/gse/job-templates/tpl-1" });
  });

  it("submits a template with vars", async () => {
    const http = new FakeHttp({ job_id: "job-1" });
    const a = new GseJobTemplateAdapter(http);
    const body = { agent_id: "a1", vars: { svc: "nginx" } };
    await a.submitTemplate("tpl-1", body);
    expect(http.last).toMatchObject({
      method: "POST",
      url: "/api/gse/job-templates/tpl-1/submit",
      body,
    });
  });

  it("saves a job as a template", async () => {
    const http = new FakeHttp({ template_id: "tpl-2" });
    const a = new GseJobTemplateAdapter(http);
    await a.saveJobAsTemplate("job/1", "copy");
    expect(http.last).toMatchObject({
      method: "POST",
      url: "/api/gse/jobs/job%2F1/save-as-template",
      body: { name: "copy" },
    });
  });
});
