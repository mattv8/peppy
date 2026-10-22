import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { App } from "./App";

afterEach(cleanup);

/* Node's experimental global localStorage is an unusable stub under jsdom; App reads it while rendering. */
beforeEach(() => {
  const values = new Map<string, string>();
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
      removeItem: (key: string) => values.delete(key),
      clear: () => values.clear(),
    },
  });
});

describe("enrollment settings", () => {
  it("does not expose QR or credential-file actions in settings", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Settings" }));
    await screen.findByRole("region", { name: "Settings" });
    expect(document.getElementById("settings-pair-phone")).not.toBeInTheDocument();
    expect(document.getElementById("settings-credential-import")).not.toBeInTheDocument();
    expect(document.getElementById("settings-credential-export")).not.toBeInTheDocument();
  });
});
