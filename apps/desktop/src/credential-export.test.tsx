import "@testing-library/jest-dom/vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";

/**
 * Regression test: fixture export rejection with real App and fixture bridge.
 * Verifies that local preview exports are rejected with an actionable error
 * visible in the settings section, and the button re-enables after failure.
 */

describe("fixture credential export rejection", () => {
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
    vi.restoreAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it.each([
    ["native"],
    ["browser"],
  ] as const)(
    "renders inline alert when %s preview export is rejected",
    async (hostKind) => {
      const props =
        hostKind === "browser"
          ? { hostKind: "browser" as const, fixedOrigin: "https://community.example" }
          : {};

      render(<App {...props} />);

      // Wait for the app to load and show conversation content
      await screen.findAllByText("Can you send over the estimate?");

      // Navigate to settings
      fireEvent.click(screen.getByRole("button", { name: "Settings" }));

      // Click export button
      const exportButton = screen.getByRole("button", { name: "Export credentials" });
      expect(exportButton).toBeEnabled();
      fireEvent.click(exportButton);

      // Verify inline alert appears in the credential export section
      const alert = await screen.findByRole("alert");
      const credentialExportSection = document.getElementById(
        "settings-credential-export"
      );
      expect(credentialExportSection).toBeInTheDocument();
      expect(alert.closest("#settings-credential-export")).toBeInTheDocument();

      // Verify button returns to enabled state
      await waitFor(() => expect(exportButton).toBeEnabled());

      // Verify success notice is hidden and empty
      const notice = document.getElementById("settings-credential-export-notice");
      expect(notice).toHaveClass("visually-hidden");
      expect(notice).toHaveTextContent("");
    }
  );
});
