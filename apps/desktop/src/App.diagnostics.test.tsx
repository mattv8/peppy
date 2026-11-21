import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, resetFixtureDiagnosticsForTests } from "./bridge/bridge";
import { hasDiagnosticsBridge } from "./settings/useDiagnostics";

const DIAGNOSTICS_METHODS = ["get_diagnostics_state", "set_diagnostics_enabled", "export_diagnostics_log", "clear_diagnostics_log"] as const;

async function openSettings() {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  fireEvent.click(document.querySelector<HTMLButtonElement>('[data-rail-item="settings"]')!);
  return within(screen.getByRole("navigation", { name: "Settings categories" })).getAllByRole("link");
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  resetFixtureDiagnosticsForTests();
});

describe("Diagnostics settings in the app", () => {
  it("lists Diagnostics last and renders its section when the host has every method", async () => {
    const links = await openSettings();

    expect(links.at(-1)).toHaveTextContent("Diagnostics");
    expect(links.at(-1)).toHaveAttribute("href", "#settings-diagnostics");
    expect(document.getElementById("settings-diagnostics")).toHaveAttribute("data-settings-section", "diagnostics");
  });

  it.each(DIAGNOSTICS_METHODS)("hides the entry and section when %s is missing", async method => {
    const original = bridge[method];
    Object.assign(bridge, { [method]: undefined });
    try {
      const links = await openSettings();

      expect(links.some(link => link.textContent === "Diagnostics")).toBe(false);
      expect(links.at(-1)).toHaveTextContent("Advanced");
      expect(document.getElementById("settings-diagnostics")).toBeNull();
    } finally {
      Object.assign(bridge, { [method]: original });
    }
  });

  it("recognises the bridge only when all four methods exist", () => {
    expect(hasDiagnosticsBridge(fixtureBridge)).toBe(true);
    for (const method of DIAGNOSTICS_METHODS) {
      expect(hasDiagnosticsBridge({ ...fixtureBridge, [method]: undefined })).toBe(false);
    }
  });
});
