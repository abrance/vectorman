import type { AppError } from "../error";
import type { Notice, NoticeLevel, Notifier } from "../notifier";

const QUEUE_LIMIT = 50;

export class MemoryNotifier implements Notifier {
  private readonly queue: Notice[] = [];
  private readonly listeners = new Set<(notice: Notice) => void>();
  private seq = 0;

  success(message: string): void {
    this.push("success", message);
  }

  warning(message: string): void {
    this.push("warning", message);
  }

  error(error: AppError): void {
    this.push("error", error.message);
  }

  subscribe(listener: (notice: Notice) => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  private push(level: NoticeLevel, message: string): void {
    const notice: Notice = { id: String(++this.seq), level, message };
    this.queue.push(notice);
    if (this.queue.length > QUEUE_LIMIT) {
      this.queue.shift();
    }
    for (const listener of this.listeners) {
      listener(notice);
    }
  }
}
