import { afterEach, describe, expect, it, vi } from "vitest";
import { renderBrowserStopped } from "./browser-stopped";

afterEach(() => document.body.replaceChildren());

describe("browser stopped screen", () => {
  it.each([
    ["locked", "Peppy is locked", "This shared browser session was locked. Reload to reconnect securely."],
    ["locked-recovery-clear-failed", "Peppy is locked", "Sync is locked, but this browser may still remember your Vault key phrase."],
    ["already-open", "Peppy could not use this worker", "Another incompatible Peppy worker owns this browser session. Close that copy, then try again."],
    ["worker-error", "Peppy stopped", "Your running Peppy session stopped. Reload to reconnect securely."],
  ])("renders %s with focused heading and keyboard-reachable reload", (reason, heading, message) => {
    const container = document.createElement("div");
    document.body.append(container);
    const reload = vi.fn();

    renderBrowserStopped(container, reason, reload);

    const screen = container.querySelector("#browser-stopped");
    const title = container.querySelector<HTMLElement>("#browser-stopped-heading");
    const button = container.querySelector<HTMLButtonElement>("#browser-stopped-reload");
    expect(screen?.getAttribute("data-stop-reason")).toBe(reason);
    expect(screen?.getAttribute("aria-labelledby")).toBe(title?.id);
    expect(title?.textContent).toBe(heading);
    expect(title?.tabIndex).toBe(-1);
    expect(document.activeElement).toBe(title);
    expect(container.querySelector("#browser-stopped-message")?.textContent).toBe(message);
    expect(button?.textContent).toBe("Reload Peppy");
    expect(button?.tabIndex).toBe(0);
    button?.focus();
    expect(document.activeElement).toBe(button);
    button?.click();
    expect(reload).toHaveBeenCalledOnce();
  });

  it("keeps untrusted stop reasons out of HTML", () => {
    const container = document.createElement("div");
    document.body.append(container);
    const reason = '\"><img src=x onerror="alert(1)">';

    renderBrowserStopped(container, reason, vi.fn());

    expect(container.querySelector("#browser-stopped")?.getAttribute("data-stop-reason")).toBe(reason);
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("h1")?.textContent).toBe("Peppy stopped");
  });
});
