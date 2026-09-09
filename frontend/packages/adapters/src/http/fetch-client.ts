import type {
  AuthSession,
  ErrorMapper,
  HttpClient,
  HttpRequest,
  HttpResponse,
  RequestContext,
} from "@vectorman/primitives";

const DEFAULT_TIMEOUT_MS = 15000;

function mergeSignals(timeoutMs: number, outer?: AbortSignal): { signal: AbortSignal; cancel: () => void } {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  const onOuter = () => controller.abort();
  if (outer) {
    if (outer.aborted) {
      controller.abort();
    } else {
      outer.addEventListener("abort", onOuter, { once: true });
    }
  }
  return {
    signal: controller.signal,
    cancel: () => {
      clearTimeout(timer);
      if (outer) {
        outer.removeEventListener("abort", onOuter);
      }
    },
  };
}

function newTraceId(): string {
  return globalThis.crypto?.randomUUID?.() ?? `t-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

export class FetchHttpClient implements HttpClient {
  constructor(
    private readonly session: AuthSession,
    private readonly mapper: ErrorMapper,
    private readonly fetchImpl: typeof fetch = (...args: Parameters<typeof fetch>) => globalThis.fetch(...args),
  ) {}

  async request<T>(req: HttpRequest): Promise<HttpResponse<T>> {
    const ctx: RequestContext = {
      timeoutMs: req.context?.timeoutMs ?? DEFAULT_TIMEOUT_MS,
      signal: req.context?.signal,
      traceId: req.context?.traceId ?? newTraceId(),
    };
    const headers: Record<string, string> = { ...(req.headers ?? {}) };
    headers["X-Trace-Id"] = ctx.traceId ?? newTraceId();
    const token = this.session.get()?.token;
    if (token) {
      headers.Authorization = `Bearer ${token}`;
    }

    let body: string | undefined;
    if (req.body !== undefined) {
      try {
        body = JSON.stringify(req.body);
        if (!headers["Content-Type"]) {
          headers["Content-Type"] = "application/json";
        }
      } catch (error) {
        throw this.mapper.map({ error });
      }
    }

    const merged = mergeSignals(ctx.timeoutMs, ctx.signal);
    try {
      const res = await this.fetchImpl(req.url, {
        method: req.method,
        headers,
        body,
        signal: merged.signal,
      });
      const text = await res.text();
      let parsed: unknown = null;
      if (text.length > 0) {
        try {
          parsed = JSON.parse(text);
        } catch (error) {
          throw this.mapper.map({ error, status: res.status, body: text });
        }
      }
      if (!res.ok) {
        throw this.mapper.map({ status: res.status, body: parsed });
      }
      return { status: res.status, body: parsed as T };
    } catch (error) {
      if (error && typeof error === "object" && "code" in error && "message" in error) {
        throw error;
      }
      throw this.mapper.map({ error });
    } finally {
      merged.cancel();
    }
  }
}
