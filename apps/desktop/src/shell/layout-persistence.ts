import type { AttachmentLayout, NavigationPosition, PopoutMode, RecipientPosition } from "@peppy/desktop-ui";
import { parseAccentColor } from "./accent-color";

const LAYOUT_KEY = "peppy.layout.v1";

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
  theme?: "light" | "dark" | "system";
  groupSenderAvatars?: boolean;
  composerAttachmentLayout?: AttachmentLayout;
};

type StoredLayout = Record<string, unknown>;

function parseStoredLayout(value: string | null): StoredLayout | undefined {
  try {
    const stored: unknown = JSON.parse(value ?? "{}");
    return typeof stored === "object" && stored !== null && !Array.isArray(stored)
      ? stored as StoredLayout
      : undefined;
  } catch {
    return undefined;
  }
}

export function readAccentColor(): number | null {
  return parseAccentColor(readStoredLayout()?.accentColor);
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

function readStoredLayout(): StoredLayout | undefined {
  try {
    return parseStoredLayout(localStorage.getItem(LAYOUT_KEY));
  } catch {
    return undefined;
  }
}

export function readContactsDetailWidth(): number | undefined {
  const value = readStoredLayout()?.contactsDetailWidth;
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

export function readCompactTimestamps(): boolean {
  return readLayoutBoolean("compactTimestamps");
}

export function readGroupSenderAvatars(): boolean {
  return readLayoutBoolean("groupSenderAvatars");
}

export function readComposerAttachmentLayout(value?: string | null): AttachmentLayout {
  const layout = value === undefined ? readStoredLayout() : parseStoredLayout(value);
  return layout?.composerAttachmentLayout === "rows" ? "rows" : "tiles";
}

function readLayoutBoolean(key: string): boolean {
  const value = readStoredLayout()?.[key];
  return typeof value === "boolean" ? value : true;
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
