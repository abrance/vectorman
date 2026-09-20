import type { HttpClient } from "@vectorman/primitives";

export type JobStatus =
  | "pending"
  | "dispatched"
  | "running"
  | "succeeded"
  | "failed"
  | "timeout"
  | "rejected"
  | "lost";

export type JobKind = "script" | "file_transfer";

export type FileEndpoint =
  | { type: "agent"; agent_id: string; path: string }
  | { type: "server_temp"; file_id?: string };

export type JobFileMeta = {
  file_id: string;
  file_name: string;
  size_bytes: number;
  sha256: string;
  created_at: string;
};

export type Job = {
  job_id: string;
  agent_id: string;
  interpreter: string;
  script: string;
  args?: string[];
  env?: Record<string, string>;
  working_dir?: string | null;
  timeout_secs: number;
  status: JobStatus;
  exit_code?: number | null;
  signal?: number | null;
  stdout?: string | null;
  stdout_truncated?: boolean;
  stderr?: string | null;
  stderr_truncated?: boolean;
  error?: string | null;
  template_id?: string | null;
  rerun_of?: string | null;
  created_at: string;
  dispatched_at?: string | null;
  started_at?: string | null;
  finished_at?: string | null;
  updated_at: string;
  kind?: JobKind;
  source?: FileEndpoint | null;
  destination?: FileEndpoint | null;
  file_name?: string | null;
  file_bytes?: number | null;
  file_sha256?: string | null;
  file_id?: string | null;
};

export type JobSubmit = {
  kind?: JobKind;
  agent_id?: string;
  interpreter?: string;
  script?: string;
  args?: string[];
  env?: Record<string, string>;
  working_dir?: string;
  timeout_secs?: number;
  source?: FileEndpoint;
  destination?: FileEndpoint;
};

export type JobRerunRequest = {
  agent_id?: string;
  interpreter?: string;
  script?: string;
  args?: string[];
  env?: Record<string, string>;
  working_dir?: string;
  timeout_secs?: number;
  dest_path?: string;
};

export type JobListQuery = {
  agent_id?: string;
  status?: JobStatus;
  limit?: number;
};

const PREFIX = "/api/gse";

function enc(id: string): string {
  return encodeURIComponent(id);
}

function buildQuery(q: JobListQuery): string {
  const params = new URLSearchParams();
  if (q.agent_id) {
    params.set("agent_id", q.agent_id);
  }
  if (q.status) {
    params.set("status", q.status);
  }
  if (typeof q.limit === "number") {
    params.set("limit", String(q.limit));
  }
  const s = params.toString();
  return s ? `?${s}` : "";
}

export class GseJobAdapter {
  constructor(private readonly http: HttpClient) {}

  submitJob(req: JobSubmit): Promise<Job> {
    return this.http.request<Job>({ method: "POST", url: `${PREFIX}/jobs`, body: req }).then((r) => r.body);
  }

  listJobs(q: JobListQuery = {}): Promise<Job[]> {
    return this.http
      .request<Job[]>({ method: "GET", url: `${PREFIX}/jobs${buildQuery(q)}` })
      .then((r) => r.body);
  }

  getJob(jobId: string): Promise<Job> {
    return this.http.request<Job>({ method: "GET", url: `${PREFIX}/jobs/${enc(jobId)}` }).then((r) => r.body);
  }

  rerunJob(jobId: string, req: JobRerunRequest = {}): Promise<Job> {
    return this.http
      .request<Job>({ method: "POST", url: `${PREFIX}/jobs/${enc(jobId)}/rerun`, body: req })
      .then((r) => r.body);
  }

  uploadJobFile(file: File): Promise<JobFileMeta> {
    const body = new FormData();
    body.append("file", file);
    return this.http.request<JobFileMeta>({ method: "POST", url: `${PREFIX}/job-files`, body }).then((r) => r.body);
  }

  listJobFiles(): Promise<JobFileMeta[]> {
    return this.http.request<JobFileMeta[]>({ method: "GET", url: `${PREFIX}/job-files` }).then((r) => r.body);
  }

  deleteJobFile(fileId: string): Promise<void> {
    return this.http.request<void>({ method: "DELETE", url: `${PREFIX}/job-files/${enc(fileId)}` }).then(() => undefined);
  }

  downloadJobFileUrl(fileId: string): string {
    return `${PREFIX}/job-files/${enc(fileId)}`;
  }
}
