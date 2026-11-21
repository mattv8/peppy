import type { ContactView } from "../bridge/bridge";

export type ContactNameFormat = "first-last" | "last-first";

export const NAME_FORMAT_STORAGE_KEY = "peppy.contacts.nameFormat.v1";

const DEFAULT_NAME_FORMAT: ContactNameFormat = "first-last";

function isContactNameFormat(value: unknown): value is ContactNameFormat {
  return value === "first-last" || value === "last-first";
}

/** Reads the saved name order; storage failures and unknown values fall back to the default. */
export function readNameFormat(): ContactNameFormat {
  try {
    const stored = localStorage.getItem(NAME_FORMAT_STORAGE_KEY);
    return isContactNameFormat(stored) ? stored : DEFAULT_NAME_FORMAT;
  } catch {
    return DEFAULT_NAME_FORMAT;
  }
}

/** Saves the name order; a storage failure only means the choice is not remembered. */
export function persistNameFormat(value: ContactNameFormat): void {
  try {
    localStorage.setItem(NAME_FORMAT_STORAGE_KEY, value);
  } catch {
    // Storage can be unavailable (quota, privacy mode); the in-memory choice still applies.
  }
}

/**
 * Formats a contact name in the chosen order. "first-last" keeps the stored display
 * name (it may be a nickname such as "Mom") and only falls back to the structured parts.
 * "last-first" reorders the structured parts when both are present, else keeps the display name.
 */
export function formatContactName(
  contact: Pick<ContactView, "displayName" | "givenName" | "familyName">,
  format: ContactNameFormat,
): string {
  const given = contact.givenName?.trim() ?? "";
  const family = contact.familyName?.trim() ?? "";
  if (format === "last-first") {
    return given && family ? `${family}, ${given}` : contact.displayName;
  }
  if (contact.displayName.trim()) return contact.displayName;
  return [given, family].filter(Boolean).join(" ") || contact.displayName;
}
