import type { AppError } from "./error";

export type HttpMethod = "GET" | "POST" | "PUT" | "PATCH" | "DELETE";

export type RequestContext = {
  timeoutMs: number;
  signal?: AbortSignal;
  traceId?: string;
};

export type HttpRequest = {
  method: HttpMethod;
  url: string;
  headers?: Record<string, string>;
  body?: unknown;
  context?: RequestContext;
};

export type HttpResponse<T = unknown> = {
  status: number;
  body: T;
};

export interface HttpClient {
  request<T>(req: HttpRequest): Promise<HttpResponse<T>>;
}

export type MapInput = {
  error?: unknown;
  status?: number;
  body?: unknown;
};

export interface ErrorMapper {
  map(input: MapInput): AppError;
}
