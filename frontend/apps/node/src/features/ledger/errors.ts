import { asAppError, type AppError } from "@vectorman/primitives";

export function toAppError(e: unknown): AppError {
  return asAppError(e);
}
