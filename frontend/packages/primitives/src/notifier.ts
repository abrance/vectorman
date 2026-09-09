import type { AppError } from "./error";

export type NoticeLevel = "success" | "warning" | "error";

export type Notice = {
  id: string;
  level: NoticeLevel;
  message: string;
};

export interface Notifier {
  success(message: string): void;
  warning(message: string): void;
  error(error: AppError): void;
  subscribe(listener: (notice: Notice) => void): () => void;
}
