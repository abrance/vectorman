import type { AppError } from "../error";
import type { ErrorMapper, MapInput } from "../http";

function asRecord(body: unknown): Record<string, unknown> | null {
  if (typeof body === "object" && body !== null) {
    return body as Record<string, unknown>;
  }
  return null;
}

export class JsonErrorMapper implements ErrorMapper {
  map(input: MapInput): AppError {
    const err = input.error;
    if (typeof DOMException !== "undefined" && err instanceof DOMException && err.name === "AbortError") {
      return { code: "unavailable", message: "request timed out or aborted" };
    }
    if (err instanceof Error && err.name === "AbortError") {
      return { code: "unavailable", message: "request timed out or aborted" };
    }
    if (err instanceof TypeError) {
      return { code: "unavailable", message: err.message || "network error" };
    }
    if (err instanceof SyntaxError) {
      return { code: "invalid_argument", message: "response body is not valid JSON" };
    }

    const rec = asRecord(input.body);
    if (rec) {
      if (rec.status === "error") {
        const code = typeof rec.errorType === "string" && rec.errorType.length > 0 ? rec.errorType : "query_failed";
        const message = typeof rec.error === "string" ? rec.error : "query failed";
        return { code, message };
      }
      if (typeof rec.code === "string") {
        const message =
          typeof rec.error === "string"
            ? rec.error
            : typeof rec.message === "string"
              ? rec.message
              : rec.code;
        return { code: rec.code, message };
      }
    }

    const status = input.status;
    if (status === 404) {
      return { code: "not_found", message: "not found" };
    }
    if (status === 400) {
      return { code: "invalid_argument", message: "invalid argument" };
    }
    if (status === 401 || status === 403) {
      return { code: "unauthorized", message: "unauthorized" };
    }
    if (typeof status === "number" && status >= 400) {
      return { code: "query_failed", message: `http ${status}` };
    }

    if (err instanceof Error) {
      return { code: "query_failed", message: err.message };
    }
    return { code: "query_failed", message: "unknown error" };
  }
}
