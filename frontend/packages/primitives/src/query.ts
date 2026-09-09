import type { AppError } from "./error";

export type QueryStatus = "idle" | "loading" | "success" | "error";

export type QueryRecord<T = unknown> = {
  status: QueryStatus;
  data?: T;
  error?: AppError;
};

export interface QueryStore {
  get<T>(key: string): QueryRecord<T>;
  setLoading(key: string): void;
  setSuccess<T>(key: string, data: T): void;
  setError(key: string, error: AppError): void;
  subscribe(key: string, listener: () => void): () => void;
}
