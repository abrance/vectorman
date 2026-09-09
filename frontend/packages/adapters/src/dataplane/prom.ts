import type { ErrorMapper, HttpClient } from "@vectorman/primitives";

export type PromEnvelope = {
  status: string;
  data?: unknown;
  error?: string;
  errorType?: string;
};

export class PromQueryAdapter {
  constructor(
    private readonly http: HttpClient,
    private readonly mapper: ErrorMapper,
  ) {}

  queryInstant(expr: string, time?: string): Promise<PromEnvelope> {
    const q = new URLSearchParams({ query: expr });
    if (time) {
      q.set("time", time);
    }
    return this.get(`/api/prom/api/v1/query?${q.toString()}`);
  }

  queryRange(expr: string, start: string, end: string, step: string): Promise<PromEnvelope> {
    const q = new URLSearchParams({ query: expr, start, end, step });
    return this.get(`/api/prom/api/v1/query_range?${q.toString()}`);
  }

  private async get(url: string): Promise<PromEnvelope> {
    const envelope = (await this.http.request<PromEnvelope>({ method: "GET", url })).body;
    if (envelope.status === "error") {
      throw this.mapper.map({ body: envelope });
    }
    return envelope;
  }
}
