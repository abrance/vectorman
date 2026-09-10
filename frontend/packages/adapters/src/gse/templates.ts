import type { HttpClient } from "@vectorman/primitives";
import type { Job } from "./jobs";

export type JobTemplate = {
  template_id: string;
  name: string;
  description?: string | null;
  interpreter: string;
  script: string;
  args?: string[];
  env?: Record<string, string>;
  working_dir?: string | null;
  timeout_secs: number;
  created_at: string;
  updated_at: string;
};

export type TemplateInput = {
  name: string;
  description?: string;
  interpreter?: string;
  script: string;
  args?: string[];
  env?: Record<string, string>;
  working_dir?: string;
  timeout_secs?: number;
};

export type TemplateSubmitRequest = {
  agent_id: string;
  vars?: Record<string, string>;
};

export type TemplateListQuery = {
  name?: string;
  limit?: number;
};

const PREFIX = "/api/gse";

function enc(id: string): string {
  return encodeURIComponent(id);
}

function buildQuery(q: TemplateListQuery): string {
  const params = new URLSearchParams();
  if (q.name) {
    params.set("name", q.name);
  }
  if (typeof q.limit === "number") {
    params.set("limit", String(q.limit));
  }
  const s = params.toString();
  return s ? `?${s}` : "";
}

export class GseJobTemplateAdapter {
  constructor(private readonly http: HttpClient) {}

  listTemplates(q: TemplateListQuery = {}): Promise<JobTemplate[]> {
    return this.http
      .request<JobTemplate[]>({ method: "GET", url: `${PREFIX}/job-templates${buildQuery(q)}` })
      .then((r) => r.body);
  }

  getTemplate(templateId: string): Promise<JobTemplate> {
    return this.http
      .request<JobTemplate>({ method: "GET", url: `${PREFIX}/job-templates/${enc(templateId)}` })
      .then((r) => r.body);
  }

  createTemplate(req: TemplateInput): Promise<JobTemplate> {
    return this.http
      .request<JobTemplate>({ method: "POST", url: `${PREFIX}/job-templates`, body: req })
      .then((r) => r.body);
  }

  updateTemplate(templateId: string, req: TemplateInput): Promise<JobTemplate> {
    return this.http
      .request<JobTemplate>({
        method: "PUT",
        url: `${PREFIX}/job-templates/${enc(templateId)}`,
        body: req,
      })
      .then((r) => r.body);
  }

  deleteTemplate(templateId: string): Promise<void> {
    return this.http
      .request<void>({ method: "DELETE", url: `${PREFIX}/job-templates/${enc(templateId)}` })
      .then(() => undefined);
  }

  submitTemplate(templateId: string, req: TemplateSubmitRequest): Promise<Job> {
    return this.http
      .request<Job>({
        method: "POST",
        url: `${PREFIX}/job-templates/${enc(templateId)}/submit`,
        body: req,
      })
      .then((r) => r.body);
  }

  saveJobAsTemplate(jobId: string, name: string): Promise<JobTemplate> {
    return this.http
      .request<JobTemplate>({
        method: "POST",
        url: `${PREFIX}/jobs/${enc(jobId)}/save-as-template`,
        body: { name },
      })
      .then((r) => r.body);
  }
}
