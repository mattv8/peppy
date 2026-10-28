import type { NavigationPosition, RecipientPosition } from "@peppy/desktop-ui";

const LAYOUT_KEY = "peppy.layout.v1";

type LayoutPatch = {
  listWidth?: number;
  listCollapsed?: boolean;
  composerHeight?: number | null;
  headComposerHeight?: number | null;
  recipientPosition?: RecipientPosition;
  navigationPosition?: NavigationPosition;
  contactsDetailWidth?: number;
  compactTimestamps?: boolean;
  theme?: "light" | "dark" | "system";
};

type StoredLayout = Record<string, unknown>;

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
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(LAYOUT_KEY) ?? "{}");
    if (typeof stored !== "object" || stored === null || Array.isArray(stored)) return true;
    const value = (stored as StoredLayout).compactTimestamps;
    return typeof value === "boolean" ? value : true;
  } catch {
    return true;
  }
}

function updateLayout(update: (layout: StoredLayout) => StoredLayout): void {
  try {
    const stored = JSON.parse(localStorage.getItem(LAYOUT_KEY) ?? "{}");
    const layout =
      typeof stored === "object" && stored !== null && !Array.isArray(stored)
        ? stored
        : {};
    localStorage.setItem(LAYOUT_KEY, JSON.stringify(update(layout)));
  } catch {
    /* Storage is optional in embedded previews. */
  }
}

export function persistLayoutPatch(patch: LayoutPatch = {}): void {
  updateLayout((layout) => ({ ...layout, ...patch }));
}

export function persistRecipientPosition(position: RecipientPosition): void {
  updateLayout((layout) => {
    delete layout.recipientAnchor;
    return { ...layout, recipientPosition: position };
  });
}
