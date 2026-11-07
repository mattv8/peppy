import type { NavigationPosition, PopoutMode, RecipientPosition } from "@peppy/desktop-ui";
import { parseAccentColor } from "./accent-color";

const LAYOUT_KEY = "peppy.layout.v1";
let knownShareTyping: boolean | undefined;

type LayoutPatch = {
  listWidth?: number;
  listCollapsed?: boolean;
  composerHeight?: number | null;
  headComposerHeight?: number | null;
  recipientPosition?: RecipientPosition;
  navigationPosition?: NavigationPosition;
  popoutMode?: PopoutMode;
  contactsDetailWidth?: number;
  compactTimestamps?: boolean;
  shareTyping?: boolean;
  theme?: "light" | "dark" | "system";
  groupSenderAvatars?: boolean;
};

type StoredLayout = Record<string, unknown>;

export function readAccentColor(): number | null {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(LAYOUT_KEY) ?? "{}");
    return typeof stored === "object" && stored !== null && !Array.isArray(stored)
      ? parseAccentColor((stored as StoredLayout).accentColor)
      : null;
  } catch {
    return null;
  }
}

export function persistAccentColor(value: number | null): boolean {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(LAYOUT_KEY) ?? "{}");
    const layout: StoredLayout = typeof stored === "object" && stored !== null && !Array.isArray(stored)
      ? stored as StoredLayout
      : {};
    const accentColor = parseAccentColor(value);
    if (accentColor === null) delete layout.accentColor;
    else layout.accentColor = accentColor;
    // localStorage has no atomic read/merge/write operation across windows.
    localStorage.setItem(LAYOUT_KEY, JSON.stringify(layout));
    return true;
  } catch {
    return false;
  }
}

export function readContactsDetailWidth(): number | undefined {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(LAYOUT_KEY) ?? "{}");
    if (typeof stored !== "object" || stored === null || Array.isArray(stored)) return undefined;
    const value = (stored as StoredLayout).contactsDetailWidth;
    return typeof value === "number" && Number.isFinite(value) ? value : undefined;
  } catch {
    return undefined;
  }
}

export function readCompactTimestamps(): boolean {
  return readLayoutBoolean("compactTimestamps");
}

export function readShareTyping(): boolean {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(LAYOUT_KEY) ?? "{}");
    if (typeof stored !== "object" || stored === null || Array.isArray(stored)) return knownShareTyping ?? true;
    const value = (stored as StoredLayout).shareTyping;
    if (typeof value === "boolean") knownShareTyping = value;
    else if (value === undefined) knownShareTyping = true;
    return knownShareTyping ?? true;
  } catch {
    return knownShareTyping ?? true;
  }
}

export function readGroupSenderAvatars(): boolean {
  return readLayoutBoolean("groupSenderAvatars");
}

function readLayoutBoolean(key: string): boolean {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(LAYOUT_KEY) ?? "{}");
    if (typeof stored !== "object" || stored === null || Array.isArray(stored)) return true;
    const value = (stored as StoredLayout)[key];
    return typeof value === "boolean" ? value : true;
  } catch {
    return true;
  }
}

function updateLayout(update: (layout: StoredLayout) => StoredLayout): boolean {
  try {
    const stored = JSON.parse(localStorage.getItem(LAYOUT_KEY) ?? "{}");
    const layout =
      typeof stored === "object" && stored !== null && !Array.isArray(stored)
        ? stored
        : {};
    localStorage.setItem(LAYOUT_KEY, JSON.stringify(update(layout)));
    return true;
  } catch {
    /* Storage is optional in embedded previews. */
    return false;
  }
}

export function persistLayoutPatch(patch: LayoutPatch = {}): boolean {
  const persisted = updateLayout((layout) => ({ ...layout, ...patch }));
  if (persisted && typeof patch.shareTyping === "boolean") knownShareTyping = patch.shareTyping;
  return persisted;
}

export function persistRecipientPosition(position: RecipientPosition): void {
  updateLayout((layout) => {
    delete layout.recipientAnchor;
    return { ...layout, recipientPosition: position };
  });
}
