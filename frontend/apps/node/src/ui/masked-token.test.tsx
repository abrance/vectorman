import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { MemoryNotifier, MemoryQueryStore, type HttpClient } from "@vectorman/primitives";
import { GseAdminAdapter } from "@vectorman/adapters";
import { RuntimeProvider } from "../app/runtime";
import { MaskedToken } from "./masked-token";

afterEach(() => {
  cleanup();
});

function wrap(ui: React.ReactElement) {
  const notifier = new MemoryNotifier();
  const query = new MemoryQueryStore();
  const gse = new GseAdminAdapter({
    request: async () => ({ status: 200, body: null }),
  } as unknown as HttpClient);
  return render(
    <RuntimeProvider value={{ gse, query, notifier }}>{ui}</RuntimeProvider>,
  );
}

describe("MaskedToken", () => {
  it("masks by default and reveals on click", () => {
    wrap(<MaskedToken value="abc" />);
    expect(screen.getByText("••••")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /显\s*示/ }));
    expect(screen.getByText("abc")).toBeTruthy();
  });
});
