import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { NavigationPosition, RecipientPosition } from "@peppy/desktop-ui";
import {
  persistLayoutPatch,
  persistRecipientPosition,
  readContactsDetailWidth,
} from "./layout-persistence";

const LAYOUT_KEY = "peppy.layout.v1";
const recipientPosition: RecipientPosition = { x: 24, y: 48 };
const storedValues = new Map<string, string>();
const memoryStorage = {
  getItem: (key: string) => storedValues.get(key) ?? null,
  setItem: (key: string, value: string) => storedValues.set(key, value),
  clear: () => storedValues.clear(),
};

beforeEach(() => {
  vi.stubGlobal("localStorage", memoryStorage);
  localStorage.clear();
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("layout persistence", () => {
  it("does not write when the saved layout JSON is malformed", () => {
    localStorage.setItem(LAYOUT_KEY, "{");
    const write = vi.spyOn(memoryStorage, "setItem");

    persistLayoutPatch({ listWidth: 320 });

    expect(write).not.toHaveBeenCalled();
  });

  it.each(["null", "[]", "false", "42", '"layout"'])(
    "merges a patch into an empty layout for a saved nonobject %s",
    (savedLayout) => {
      localStorage.setItem(LAYOUT_KEY, savedLayout);

      persistLayoutPatch({ listCollapsed: true });

      expect(localStorage.getItem(LAYOUT_KEY)).toBe('{"listCollapsed":true}');
    },
  );

  it("preserves sibling keys and explicit null values", () => {
    localStorage.setItem(
      LAYOUT_KEY,
      JSON.stringify({ composerHeight: null, sibling: "kept" }),
    );

    persistLayoutPatch({ listWidth: 320 });

    expect(JSON.parse(localStorage.getItem(LAYOUT_KEY)!)).toEqual({
      composerHeight: null,
      sibling: "kept",
      listWidth: 320,
    });
  });

  it("reads only a finite saved contact detail width", () => {
    localStorage.setItem(LAYOUT_KEY, JSON.stringify({ contactsDetailWidth: 360 }));
    expect(readContactsDetailWidth()).toBe(360);
    localStorage.setItem(LAYOUT_KEY, JSON.stringify({ contactsDetailWidth: "360" }));
    expect(readContactsDetailWidth()).toBeUndefined();
  });

  it("re-reads sibling keys before each patch", () => {
    persistLayoutPatch({ listWidth: 320 });
    localStorage.setItem(LAYOUT_KEY, JSON.stringify({ sibling: "newer" }));

    persistLayoutPatch({ listCollapsed: true });

    expect(JSON.parse(localStorage.getItem(LAYOUT_KEY)!)).toEqual({
      sibling: "newer",
      listCollapsed: true,
    });
  });

  it("preserves recipientAnchor for ordinary patches and removes it only for recipient movement", () => {
    localStorage.setItem(
      LAYOUT_KEY,
      JSON.stringify({ recipientAnchor: "bottom-left", sibling: true }),
    );

    persistLayoutPatch({
      navigationPosition: "title-bar" as NavigationPosition,
      recipientPosition,
    });
    expect(JSON.parse(localStorage.getItem(LAYOUT_KEY)!)).toMatchObject({
      recipientAnchor: "bottom-left",
      recipientPosition,
    });

    persistRecipientPosition(recipientPosition);

    expect(JSON.parse(localStorage.getItem(LAYOUT_KEY)!)).toEqual({
      sibling: true,
      navigationPosition: "title-bar",
      recipientPosition,
    });
  });

  it("swallows unavailable storage read failures", () => {
    vi.stubGlobal("localStorage", {
      getItem: () => {
        throw new Error("unavailable");
      },
      setItem: vi.fn(),
    });

    expect(() => persistLayoutPatch({ listWidth: 320 })).not.toThrow();
    expect(() => persistRecipientPosition(recipientPosition)).not.toThrow();
  });

  it("swallows an absent storage global", () => {
    vi.stubGlobal("localStorage", undefined);

    expect(() => persistLayoutPatch({ listWidth: 320 })).not.toThrow();
    expect(() => persistRecipientPosition(recipientPosition)).not.toThrow();
  });

  it("swallows unavailable storage write failures", () => {
    vi.stubGlobal("localStorage", {
      getItem: () => "{}",
      setItem: () => {
        throw new Error("unavailable");
      },
    });

    expect(() => persistLayoutPatch({ listWidth: 320 })).not.toThrow();
    expect(() => persistRecipientPosition(recipientPosition)).not.toThrow();
  });
});
