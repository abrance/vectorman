import type { HttpClient } from "@vectorman/primitives";

export type SqlResult = {
  columns: string[];
  rows: unknown[][];
};

export class SqlHttpAdapter {
  constructor(private readonly http: HttpClient) {}

  execute(sql: string, params: unknown[] = []): Promise<SqlResult> {
    return this.http
      .request<SqlResult>({
        method: "POST",
        url: "/api/sql/v1/sql",
        body: { sql, params },
      })
      .then((r) => r.body);
  }
}
