export const GROUP_NAME_MAX_LENGTH = 100;

/**
 * The name as it should be shown or compared: whitespace collapsed and trimmed, never shortened.
 * A name from Google is shown in full (the header and dialog ellipsise by layout, not by data).
 * Undefined means the group is unnamed.
 */
export function displayGroupName(value: string | undefined | null): string | undefined {
  const collapsed = value?.replace(/\s+/g, " ").trim();
  return collapsed || undefined;
}

/**
 * Name for the composer, where the user types: display rules plus the 100-character input limit,
 * counted by code point. An empty result means the group is unnamed, so the caller sends no name.
 */
export function normalizeGroupName(value: string | undefined | null): string | undefined {
  const display = displayGroupName(value);
  if (!display) return undefined;
  return Array.from(display).slice(0, GROUP_NAME_MAX_LENGTH).join("").trim() || undefined;
}
