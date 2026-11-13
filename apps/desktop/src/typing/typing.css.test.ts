import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

describe("typing CSS contracts", () => {
  it("keeps dots accessible to forced colors and motion preferences", () => {
    const css = readFileSync(resolve(process.cwd(), "src/app.css"), "utf8");
    expect(css).toMatch(/\.typing-dots i\s*\{[^}]*animation:\s*typing-dot/);
    expect(css).toMatch(/prefers-reduced-motion:\s*reduce[\s\S]*?\.typing-dots i/);
    expect(css).toMatch(/forced-colors:\s*active[\s\S]*?\.typing-dots i/);
    expect(css).toMatch(/#typing-indicator-label\s*\{[^}]*display:\s*block/);
  });
});
