export type Session = {
  token?: string;
  subject?: string;
};

export interface AuthSession {
  get(): Session | null;
  set(session: Session): void;
  clear(): void;
}
