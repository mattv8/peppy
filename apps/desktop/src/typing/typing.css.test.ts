import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

describe("typing CSS", () => {
  it("collapses the indicator when hidden", () => {
    const css = readFileSync(resolve(process.cwd(), "src/app.css"), "utf8");
    expect(css).toMatch(/#typing-indicator\[hidden\]\s*\{[^}]*display:\s*none/);
  });
});
