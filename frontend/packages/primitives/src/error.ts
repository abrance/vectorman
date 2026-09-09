export type AppError = {
  code: string;
  message: string;
};

export function isAppError(value: unknown): value is AppError {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const rec = value as Record<string, unknown>;
  return typeof rec.code === "string" && typeof rec.message === "string";
}

export function asAppError(value: unknown): AppError {
  if (isAppError(value)) {
    return value;
  }
  if (value instanceof Error) {
    return { code: "query_failed", message: value.message };
  }
  return { code: "query_failed", message: "unknown error" };
}
