import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const css = readFileSync(resolve(process.cwd(), "src/app.css"), "utf8");
/** Every declaration block whose selector list names `selector`, joined in source order. */
const rule = (selector: string) => {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return [...css.matchAll(new RegExp(`(^|\\s|,|})${escaped}\\s*\\{([^}]*)\\}`, "gm"))].map((match) => match[2]).join(";");
};
const forcedColors = () => css.slice(css.lastIndexOf("/* Group conversations"));

describe("group conversation CSS contracts", () => {
  it("shares the history band look with timeline notices", () => {
    expect(css).toMatch(/\.history-load-band,\s*\.timeline-notice \{/);
    expect(css).toMatch(/\.timeline-notice::before,\s*\.timeline-notice::after \{/);
    expect(rule(".timeline-notice")).toMatch(/margin:\s*0 0 16px/);
  });

  it("wraps long notice text instead of clipping it", () => {
    expect(rule(".timeline-notice-text")).toMatch(/overflow-wrap:\s*anywhere/);
    expect(rule(".timeline-notice-text")).toMatch(/text-align:\s*center/);
  });

  it("gives the MMS badge the neutral border used by the encryption pill, and keeps it from shrinking", () => {
    const badge = rule("#conversation-mms-badge");
    expect(badge).toMatch(/border:\s*1px solid var\(--peppy-border-strong\)/);
    expect(badge).toMatch(/flex:\s*none/);
  });

  it("ellipsises a member name within its row", () => {
    expect(rule(".group-member-text bdi")).toMatch(/text-overflow:\s*ellipsis/);
    expect(rule(".group-member-text bdi")).toMatch(/white-space:\s*nowrap/);
  });

  it("wraps the group name in the field and the details value", () => {
    expect(rule("#group-details-facts bdi")).toMatch(/overflow-wrap:\s*anywhere/);
    expect(rule("#new-group-name")).toMatch(/height:\s*var\(--peppy-control-height\)/);
  });

  it("removes retired composer-card styling now owned by the shared inline-status primitive", () => {
    expect(css).not.toMatch(/#group-send-updated\[aria-disabled="true"\]/);
  });

  it("uses 44px targets for group details controls on coarse pointers", () => {
    const coarse = css.slice(css.indexOf("@media (pointer: coarse) {\n  #header-group-details"));
    for (const selector of ["#header-group-details", ".group-member-row", "[id^=\"group-message-edit-\"]"]) {
      expect(coarse).toContain(selector);
    }
    expect(coarse).toMatch(/min-height:\s*44px/);
  });

  it("keeps remaining group surfaces readable under forced colors, with no shadows", () => {
    const block = forcedColors();
    expect(block).toMatch(/@media \(forced-colors: active\)/);
    expect(block).toMatch(/#new-group-name \{ border-color: CanvasText; background: Canvas; color: CanvasText; box-shadow: none; \}/);
    expect(block).toMatch(/\.timeline-notice::before, \.timeline-notice::after \{ ?border-color: CanvasText; \}/);
  });

  it("does not animate the new surfaces", () => {
    const groupBlock = css.slice(css.lastIndexOf("/* Group conversations"));
    expect(groupBlock).not.toMatch(/animation|transition/);
  });
});
