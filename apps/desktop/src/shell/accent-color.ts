import { TONE_PRESETS } from "@peppy/desktop-ui";

export const CORAL_ACCENT_INDEX = 7;

const accentIndexes = new Set<number>(TONE_PRESETS.map(({ index }) => index));

export function parseAccentColor(value: unknown): number | null {
  return typeof value === "number" && Number.isInteger(value) && accentIndexes.has(value)
    ? value
    : null;
}

export function accentAttribute(value: number | null): string | undefined {
  const accentColor = parseAccentColor(value);
  return accentColor === null || accentColor === CORAL_ACCENT_INDEX ? undefined : String(accentColor);
}

export function accentSwatchColor(index: number | null): string {
  const accentColor = parseAccentColor(index);
  return accentColor === null || accentColor === CORAL_ACCENT_INDEX
    ? "var(--peppy-accent-default)"
    : `var(--peppy-accent-preset-${accentColor})`;
}
