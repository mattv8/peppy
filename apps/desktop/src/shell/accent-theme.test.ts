import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { TONE_PRESETS } from "@peppy/desktop-ui";

const testFilePath = fileURLToPath(import.meta.url);
const accentThemePath = resolve(dirname(testFilePath), "accent-theme.css");
const palettePath = resolve(dirname(testFilePath), "../../../../packages/desktop-ui/avatar-palette.json");
const CORAL_ACCENT_INDEX = 7;
const ACCENT_TOKENS = [
  "--peppy-accent", "--peppy-accent-hover", "--peppy-accent-text", "--peppy-focus", "--peppy-focus-ring", "--peppy-focus-ring-alpha", "--peppy-primary", "--peppy-primary-text", "--peppy-bubble-self-surface", "--peppy-bubble-self-text", "--peppy-bubble-self-border",
];

function rule(css: string, selector: string, offset = 0): { selector: string; body: string } {
  const selectorStart = css.indexOf(selector, offset);
  expect(selectorStart).toBeGreaterThanOrEqual(0);
  const bodyStart = css.indexOf("{", selectorStart);
  const bodyEnd = css.indexOf("}", bodyStart);
  return { selector: css.slice(selectorStart, bodyStart), body: css.slice(bodyStart + 1, bodyEnd) };
}

function atRule(css: string, header: string): { body: string; start: number; end: number } {
  const start = css.indexOf(header);
  expect(start).toBeGreaterThanOrEqual(0);
  const bodyStart = css.indexOf("{", start);
  let depth = 1;
  for (let index = bodyStart + 1; index < css.length; index += 1) {
    if (css[index] === "{") depth += 1;
    if (css[index] === "}") depth -= 1;
    if (depth === 0) return { body: css.slice(bodyStart + 1, index), start, end: index + 1 };
  }
  throw new Error(`Unclosed CSS rule: ${header}`);
}

function property(body: string, name: string): string {
  const value = body.match(new RegExp(`${name}:\\s*([^;]+);`))?.[1]?.trim();
  expect(value).toBeDefined();
  return value!;
}

function relativeLuminance(hex: string): number {
  const normalized = hex.length === 4 ? `#${hex.slice(1).split("").map((channel) => channel.repeat(2)).join("")}` : hex;
  const channels = normalized.slice(1).match(/.{2}/g)!.map((channel) => Number.parseInt(channel, 16) / 255);
  const [red, green, blue] = channels.map((channel) => channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4);
  return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
}

function contrast(first: string, second: string): number {
  const [lighter, darker] = [relativeLuminance(first), relativeLuminance(second)].sort((left, right) => right - left);
  return (lighter + 0.05) / (darker + 0.05);
}

function mix(first: string, second: string, firstWeight: number): string {
  const firstChannels = first.slice(1).match(/.{2}/g)!.map((channel) => Number.parseInt(channel, 16));
  const secondChannels = second.slice(1).match(/.{2}/g)!.map((channel) => Number.parseInt(channel, 16));
  const channels = firstChannels.map((channel, index) => Math.round(channel * firstWeight + secondChannels[index] * (1 - firstWeight)));
  return `#${channels.map((channel) => channel.toString(16).padStart(2, "0")).join("")}`;
}

describe("accent theme", () => {
  it("defines every preset and default in each theme context", async () => {
    const css = await readFile(accentThemePath, "utf8");
    const dark = rule(css, ":is(.theme-system, .theme-dark)").body;
    const light = rule(css, ".theme-light {").body;
    const systemLight = rule(css, ".theme-system {", css.indexOf("@media (prefers-color-scheme: light)")).body;

    for (const block of [dark, light, systemLight]) {
      expect(block).toContain("--peppy-accent-default:");
      for (const { index } of TONE_PRESETS) expect(block).toContain(`--peppy-accent-preset-${index}:`);
    }
    expect(property(dark, "--peppy-accent-default")).toBe("#ff8a73");
    expect(property(light, "--peppy-accent-default")).toBe("#c9422d");
    expect(property(systemLight, "--peppy-accent-default")).toBe("#c9422d");
    expect(css.match(/--peppy-accent-preset-7: var\(--peppy-accent-default\);/g)).toHaveLength(3);
  });

  it("uses raw avatar colors for light hover and complete theme-specific overrides", async () => {
    const css = await readFile(accentThemePath, "utf8");
    const forcedColors = atRule(css, "@media (forced-colors: none)");
    const dark = rule(forcedColors.body, ":is(.theme-system, .theme-dark):is(");
    const light = rule(forcedColors.body, ".theme-light:is(");
    const systemLight = rule(forcedColors.body, ".theme-system:is(");

    expect(css).not.toMatch(/@media \(forced-colors: active\)/);
    expect(`${css.slice(0, forcedColors.start)}${css.slice(forcedColors.end)}`).not.toContain("[data-accent");
    for (const { index } of TONE_PRESETS) {
      if (index === CORAL_ACCENT_INDEX) {
        expect(forcedColors.body).not.toContain(`[data-accent="${index}"]`);
        continue;
      }
      for (const override of [dark, light, systemLight]) expect(override.selector).toContain(`[data-accent="${index}"]`);
      const selection = rule(forcedColors.body, `[data-accent="${index}"]`);
      expect(property(selection.body, "--peppy-accent-selected")).toBe(`var(--peppy-accent-preset-${index})`);
      expect(property(selection.body, "--peppy-accent-base")).toBe(`var(--peppy-avatar-${index})`);
    }
    for (const token of ACCENT_TOKENS) {
      expect(dark.body).toContain(`${token}:`);
      expect(light.body).toContain(`${token}:`);
      expect(systemLight.body).toContain(`${token}:`);
    }
    expect(property(dark.body, "--peppy-accent-hover")).toBe("color-mix(in srgb, var(--peppy-accent-selected) 88%, #fff)");
    expect(property(light.body, "--peppy-accent-hover")).toBe("color-mix(in srgb, var(--peppy-accent-base) 76%, #000)");
    expect(property(systemLight.body, "--peppy-accent-hover")).toBe("color-mix(in srgb, var(--peppy-accent-base) 76%, #000)");
    expect(css).not.toContain("--peppy-brand:");
  });

  it("keeps the shipped accent, hover, text, and focus colors readable", async () => {
    const css = await readFile(accentThemePath, "utf8");
    const forcedColors = atRule(css, "@media (forced-colors: none)");
    const dark = rule(forcedColors.body, ":is(.theme-system, .theme-dark):is(").body;
    const light = rule(forcedColors.body, ".theme-light:is(").body;
    const darkText = property(dark, "--peppy-accent-text");
    const lightText = property(light, "--peppy-accent-text");
    const palette = JSON.parse(await readFile(palettePath, "utf8")) as [string, string][];

    for (const { index } of TONE_PRESETS) {
      const [lightBase, darkBase] = palette[index];
      const darkHover = mix(darkBase, "#ffffff", 0.88);
      const lightPreset = mix(lightBase, "#000000", 0.88);
      const lightHover = mix(lightBase, "#000000", 0.76);

      expect(contrast(darkBase, darkText)).toBeGreaterThanOrEqual(4.5);
      expect(contrast(darkHover, darkText)).toBeGreaterThanOrEqual(4.5);
      expect(contrast(darkBase, "#313131")).toBeGreaterThanOrEqual(4.5);
      expect(contrast(lightPreset, lightText)).toBeGreaterThanOrEqual(4.5);
      expect(contrast(lightPreset, "#f8f8f8")).toBeGreaterThanOrEqual(4.5);
      expect(contrast(lightHover, lightText)).toBeGreaterThanOrEqual(4.5);
      expect(contrast(darkBase, "#181818")).toBeGreaterThanOrEqual(3);
      expect(contrast(lightPreset, "#f8f8f8")).toBeGreaterThanOrEqual(3);
    }
  });
});
