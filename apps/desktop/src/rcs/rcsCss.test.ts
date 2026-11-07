import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const css = readFileSync(resolve(process.cwd(), "src/app.css"), "utf8");
/** Every declaration block whose selector list names `selector`, joined in source order. */
const rule = (selector: string) => {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return [...css.matchAll(new RegExp(`(^|\\s|,|})${escaped}\\s*\\{([^}]*)\\}`, "gm"))].map((match) => match[2]).join(";");
};

describe("RCS message and header CSS contracts", () => {
  it("lays the route hint on its own line with no UA margin", () => {
    expect(css).toMatch(/\.message-meta \[data-message-route-hint\] \{ margin: 0; flex: 1 0 100%; \}/);
  });

  it("lets the title shrink to six characters before the pills", () => {
    const title = rule("[data-header-title-row] [data-header-title]");
    expect(title).toMatch(/min-width:\s*6ch/);
    expect(title).toMatch(/text-overflow:\s*ellipsis/);
    expect(rule("#conversation-rcs-badge, #conversation-rcs-e2ee-badge")).toMatch(/flex:\s*none/);
  });

  it("lets the encryption pill ellipsise while the RCS pill keeps its size", () => {
    const e2ee = rule("#conversation-rcs-e2ee-badge");
    expect(e2ee).toMatch(/flex:\s*0\s+1\s+auto/);
    expect(e2ee).toMatch(/min-width:\s*0/);
    expect(e2ee).toMatch(/text-overflow:\s*ellipsis/);
  });

  it("uses a neutral border for the attributed encryption pill", () => {
    expect(rule("#conversation-rcs-e2ee-badge")).toMatch(/border-color:\s*var\(--peppy-border-strong\)/);
  });
});
