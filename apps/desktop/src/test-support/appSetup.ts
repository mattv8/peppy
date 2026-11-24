import { cleanup } from "@testing-library/react";
import { vi } from "vitest";

export function resetAppStorage() {
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
}

export function cleanupAppTest() {
  cleanup();
  localStorage.removeItem("peppy.layout.v1");
  window.history.replaceState({}, "", "/");
  vi.unstubAllGlobals();
}
