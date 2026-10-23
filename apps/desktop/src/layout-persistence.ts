import type { NavigationPosition, RecipientPosition } from "@peppy/desktop-ui";

const LAYOUT_KEY = "peppy.layout.v1";

type LayoutPatch = {
  listWidth?: number;
  listCollapsed?: boolean;
  composerHeight?: number | null;
  headComposerHeight?: number | null;
  recipientPosition?: RecipientPosition;
  navigationPosition?: NavigationPosition;
};

type StoredLayout = Record<string, unknown>;

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
