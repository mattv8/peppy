import { cleanup } from "@testing-library/react";
import { vi } from "vitest";
import { replaceWindowProperty } from "./browserStubs";

/** Gives the test an empty in-memory localStorage and restores the original when the test finishes. */
export function resetAppStorage() {
  const values = new Map<string, string>();
  replaceWindowProperty("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
    clear: () => values.clear(),
  }, false);
}

export function cleanupAppTest() {
  cleanup();
  localStorage.removeItem("peppy.layout.v1");
  window.history.replaceState({}, "", "/");
  vi.unstubAllGlobals();
}
