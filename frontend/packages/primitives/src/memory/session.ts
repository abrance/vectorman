import type { AuthSession, Session } from "../session";

export class MemoryAuthSession implements AuthSession {
  private session: Session | null = null;

  get(): Session | null {
    return this.session;
  }

  set(session: Session): void {
    this.session = session;
  }

  clear(): void {
    this.session = null;
  }
}
