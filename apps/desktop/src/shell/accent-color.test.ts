import { describe, expect, it } from "vitest";
import { accentAttribute, accentSwatchColor, CORAL_ACCENT_INDEX, parseAccentColor } from "./accent-color";

describe("accent color", () => {
  it.each([61, CORAL_ACCENT_INDEX])("accepts preset index %s", (value) => {
    expect(parseAccentColor(value)).toBe(value);
  });

  it.each([1, "61", 61.5, null])("defaults invalid stored value %j", (value) => {
    expect(parseAccentColor(value)).toBeNull();
  });

  it("uses no attribute or custom swatch for default coral", () => {
    expect(accentAttribute(null)).toBeUndefined();
    expect(accentAttribute(CORAL_ACCENT_INDEX)).toBeUndefined();
    expect(accentSwatchColor(null)).toBe("var(--peppy-accent-default)");
    expect(accentSwatchColor(CORAL_ACCENT_INDEX)).toBe("var(--peppy-accent-default)");
  });

  it("uses valid non-coral presets", () => {
    expect(accentAttribute(61)).toBe("61");
    expect(accentSwatchColor(61)).toBe("var(--peppy-accent-preset-61)");
  });
});
