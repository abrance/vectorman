import { describe, expect, it } from "vitest";
import { JsonErrorMapper } from "./mapper";

const mapper = new JsonErrorMapper();

describe("JsonErrorMapper", () => {
  it("maps backend code and error field", () => {
    const err = mapper.map({ status: 500, body: { code: "query_failed", error: "boom" } });
    expect(err).toEqual({ code: "query_failed", message: "boom" });
  });

  it("maps backend code and message field", () => {
    const err = mapper.map({ body: { code: "not_found", message: "missing" } });
    expect(err).toEqual({ code: "not_found", message: "missing" });
  });

  it("maps Prom errorType", () => {
    const err = mapper.map({ body: { status: "error", errorType: "unimplemented", error: "rate" } });
    expect(err).toEqual({ code: "unimplemented", message: "rate" });
  });

  it("maps AbortError to unavailable", () => {
    const abort = new Error("aborted");
    abort.name = "AbortError";
    expect(mapper.map({ error: abort }).code).toBe("unavailable");
  });

  it("maps TypeError to unavailable", () => {
    expect(mapper.map({ error: new TypeError("failed to fetch") }).code).toBe("unavailable");
  });

  it("maps SyntaxError to invalid_argument", () => {
    expect(mapper.map({ error: new SyntaxError("bad json") }).code).toBe("invalid_argument");
  });

  it("maps http status without body", () => {
    expect(mapper.map({ status: 404 }).code).toBe("not_found");
    expect(mapper.map({ status: 400 }).code).toBe("invalid_argument");
    expect(mapper.map({ status: 401 }).code).toBe("unauthorized");
    expect(mapper.map({ status: 403 }).code).toBe("unauthorized");
    expect(mapper.map({ status: 500 }).code).toBe("query_failed");
  });
});
