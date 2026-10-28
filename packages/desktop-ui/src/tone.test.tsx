import "@testing-library/jest-dom/vitest";
import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { paletteIndex } from "./avatar";
import { ToneOverridesProvider, incomingToneSeed, toneIndex, toneStyle, useToneStyle } from "./tone";

describe("conversation tones", () => {
  it("uses a saved override without changing the automatic identity", () => {
    expect(toneIndex("+15550001111")).toBe(10);
    expect(toneIndex("+15550001111", { "+15550001111": 61 })).toBe(61);
    expect(toneStyle("+15550001111", { "+15550001111": 61 })).toEqual({
      "--peppy-tone": "var(--peppy-avatar-61, var(--peppy-accent))",
      "--peppy-tone-text": "var(--peppy-avatar-glyph, var(--peppy-accent-text))",
    });
  });

  it("ignores inherited object members as overrides", () => {
    const automatic = toneIndex("constructor");
    expect(toneIndex("constructor", {})).toBe(automatic);
    expect(toneIndex("constructor")).toBe(automatic);
    expect(toneIndex("toString", {})).toBe(paletteIndex("toString"));
    expect(toneIndex("__proto__", {})).toBe(paletteIndex("__proto__"));
    expect(toneIndex("hasOwnProperty", {})).toBe(paletteIndex("hasOwnProperty"));
  });

  it("falls back to the automatic slot for invalid own override values", () => {
    const automatic = toneIndex("+15550001111");
    for (const value of [-1, 120, 256, 1.5, Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
      expect(toneIndex("+15550001111", { "+15550001111": value })).toBe(automatic);
    }
  });

  it("accepts boundary palette slots as overrides", () => {
    expect(toneIndex("+15550001111", { "+15550001111": 0 })).toBe(0);
    expect(toneIndex("+15550001111", { "+15550001111": 119 })).toBe(119);
  });

  it("uses the normalized sender for group incoming messages", () => {
    expect(incomingToneSeed("group:+15550001111,+15550002222", "+1 (555) 000-3333")).toBe("+15550003333");
    expect(incomingToneSeed("group:+15550001111,+15550002222")).toBe("group:+15550001111,+15550002222");
  });

  it("provides overrides to styled descendants", () => {
    function StyledTone() {
      return <span data-tone style={useToneStyle("+15550001111")} />;
    }

    const { container } = render(<ToneOverridesProvider overrides={{ "+15550001111": 61 }}><StyledTone /></ToneOverridesProvider>);
    expect(container.querySelector("[data-tone]")).toHaveStyle("--peppy-tone: var(--peppy-avatar-61, var(--peppy-accent))");
  });
});
