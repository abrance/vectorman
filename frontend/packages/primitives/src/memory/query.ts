import type { AppError } from "../error";
import type { QueryRecord, QueryStore } from "../query";

const IDLE: QueryRecord = { status: "idle" };

export class MemoryQueryStore implements QueryStore {
  private readonly records = new Map<string, QueryRecord>();
  private readonly listeners = new Map<string, Set<() => void>>();

  get<T>(key: string): QueryRecord<T> {
    const rec = this.records.get(key);
    if (!rec) {
      return IDLE as QueryRecord<T>;
    }
    return rec as QueryRecord<T>;
  }

  setLoading(key: string): void {
    const prev = this.records.get(key);
    this.records.set(key, {
      status: "loading",
      data: prev?.data,
      error: prev?.error,
    });
    this.emit(key);
  }

  setSuccess<T>(key: string, data: T): void {
    this.records.set(key, { status: "success", data });
    this.emit(key);
  }

  setError(key: string, error: AppError): void {
    const prev = this.records.get(key);
    this.records.set(key, { status: "error", data: prev?.data, error });
    this.emit(key);
  }

  subscribe(key: string, listener: () => void): () => void {
    let set = this.listeners.get(key);
    if (!set) {
      set = new Set();
      this.listeners.set(key, set);
    }
    set.add(listener);
    return () => {
      set?.delete(listener);
    };
  }

  private emit(key: string): void {
    const set = this.listeners.get(key);
    if (!set) {
      return;
    }
    for (const listener of set) {
      listener();
    }
  }
}
