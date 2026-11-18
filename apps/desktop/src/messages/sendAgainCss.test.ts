import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const css = readFileSync(resolve(process.cwd(), "src/app.css"), "utf8");
const block = (selector: string) => {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return [...css.matchAll(new RegExp(`(^|\\s|,|})${escaped}\\s*\\{([^}]*)\\}`, "gm"))].map((match) => match[2]).join(";");
};

describe("Send again popover CSS", () => {
  it("styles the popover as an opaque floating surface, as the inline status popover is", () => {
    const surface = block("[data-send-again-popover]");
    expect(surface).toMatch(/z-index:\s*20/);
    expect(surface).toMatch(/background:\s*var\(--peppy-surface-panel\)/);
    expect(surface).toMatch(/border:\s*1px solid var\(--peppy-border-strong\)/);
    expect(surface).toMatch(/box-shadow:\s*var\(--peppy-shadow-floating\)/);
    expect(surface).toMatch(/overflow:\s*auto/);
    expect(surface).toMatch(/color:\s*var\(--peppy-text-primary\)/);
  });

  it("has a rule for every data hook the popover renders", () => {
    // Attachment and recipient lines are styled through the summary block (`[data-send-again-summary] p`).
    const hooks = ["[data-send-again-body]", "[data-send-again-summary]", "[data-send-again-preview]", "[data-send-again-routes]", "[data-send-again-route]", "[data-send-again-reason]", "[data-send-again-none]", "[data-send-again-hint]", "[data-send-again-error]", "[data-send-again-actions]"];
    for (const hook of hooks) expect(css, hook).toContain(hook);
  });

  it("gives every route state a rule, and keeps unavailable routes muted", () => {
    expect(block('[data-send-again-route-state="eligible"]')).toMatch(/cursor:\s*pointer/);
    expect(block('[data-send-again-route-state="unavailable"]')).toMatch(/color:\s*var\(--peppy-text-muted\)/);
  });

  it("has forced-colors and coarse-pointer rules", () => {
    expect(css).toMatch(/@media \(forced-colors: active\) \{\s*\[data-send-again-popover\] \{ border-color: CanvasText; background: Canvas; box-shadow: none; \}/);
    expect(css).toMatch(/@media \(pointer: coarse\) \{ \[data-send-again-actions\] button \{ min-height: 44px; \} \}/);
  });

  it("keeps the recovery row's rule that the bubble layout depends on", () => {
    expect(css).toMatch(/\.message-meta \[data-message-recovery\] \{ display: flex; flex: 1 0 100%;/);
  });
});
