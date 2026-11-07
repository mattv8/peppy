import { act, cleanup, renderHook } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useAccentColor } from "./useAccentColor";

const LAYOUT_KEY = "peppy.layout.v1";
const storedValues = new Map<string, string>();
const memoryStorage = {
  getItem: (key: string) => storedValues.get(key) ?? null,
  setItem: (key: string, value: string) => storedValues.set(key, value),
  clear: () => storedValues.clear(),
};

function emitStorage(key: string | null, newValue: string | null): void {
  window.dispatchEvent(new StorageEvent("storage", { key, newValue }));
}

beforeEach(() => {
  vi.stubGlobal("localStorage", memoryStorage);
  localStorage.clear();
  document.documentElement.removeAttribute("data-accent");
  document.body.removeAttribute("data-accent");
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("useAccentColor", () => {
  it("reads the initial accent color and applies it immediately", () => {
    localStorage.setItem(LAYOUT_KEY, JSON.stringify({ accentColor: 61 }));

    const { result } = renderHook(() => useAccentColor());

    expect(result.current.accentColor).toBe(61);
    expect(document.documentElement.getAttribute("data-accent")).toBe("61");
    expect(document.body.getAttribute("data-accent")).toBe("61");
  });

  it("selects and persists an accent color", () => {
    const { result } = renderHook(() => useAccentColor());

    act(() => result.current.selectAccentColor(61));

    expect(result.current).toMatchObject({ accentColor: 61, error: undefined });
    expect(JSON.parse(localStorage.getItem(LAYOUT_KEY)!)).toEqual({ accentColor: 61 });
  });

  it("keeps a session selection and reports an error when persistence fails", () => {
    vi.stubGlobal("localStorage", {
      getItem: () => "{}",
      setItem: () => { throw new Error("unavailable"); },
    });
    const { result } = renderHook(() => useAccentColor());

    act(() => result.current.selectAccentColor(61));

    expect(result.current.accentColor).toBe(61);
    expect(result.current.error).toBe("Couldn't save the accent color. It will reset when Peppy restarts.");
  });

  it("adopts a changed accent from another window without writing", () => {
    const write = vi.spyOn(memoryStorage, "setItem");
    const { result } = renderHook(() => useAccentColor());
    storedValues.set(LAYOUT_KEY, JSON.stringify({ accentColor: 61 }));

    act(() => emitStorage(LAYOUT_KEY, JSON.stringify({ accentColor: 61 })));

    expect(result.current.accentColor).toBe(61);
    expect(write).not.toHaveBeenCalled();
  });

  it("resets after storage is cleared", () => {
    localStorage.setItem(LAYOUT_KEY, JSON.stringify({ accentColor: 61 }));
    const { result } = renderHook(() => useAccentColor());

    storedValues.delete(LAYOUT_KEY);
    act(() => emitStorage(null, null));

    expect(result.current.accentColor).toBeNull();
  });

  it("reconciles delayed storage events with the latest stored layout", () => {
    const { result } = renderHook(() => useAccentColor());

    act(() => result.current.selectAccentColor(61));
    act(() => result.current.selectAccentColor(60));
    act(() => emitStorage(LAYOUT_KEY, JSON.stringify({ accentColor: 61 })));

    expect(result.current.accentColor).toBe(60);
    expect(JSON.parse(localStorage.getItem(LAYOUT_KEY)!)).toEqual({ accentColor: 60 });
  });

  it.each([
    { label: "a removed layout key", initial: { accentColor: 61 }, stored: undefined, key: LAYOUT_KEY, expected: null, action: "none" },
    { label: "malformed layout JSON", initial: { accentColor: 61 }, stored: "{", key: LAYOUT_KEY, expected: null, action: "none" },
    { label: "an invalid stored accent", initial: { accentColor: 61 }, stored: { accentColor: "61" }, key: LAYOUT_KEY, expected: null, action: "none" },
    { label: "an unrelated storage key", initial: { accentColor: 61 }, stored: { accentColor: 60 }, key: "other", expected: 61, action: "none" },
    { label: "a successful sync after a persistence error", initial: {}, stored: { accentColor: 60 }, key: LAYOUT_KEY, expected: 60, action: "failed-61" },
    { label: "a saved preset after a failed session-only selection", initial: { accentColor: 61 }, stored: { accentColor: 61 }, key: LAYOUT_KEY, expected: 60, action: "failed-60" },
  ])("handles $label without writing", ({ initial, stored, key, expected, action }) => {
    localStorage.setItem(LAYOUT_KEY, JSON.stringify(initial));
    const { result } = renderHook(() => useAccentColor());
    if (action !== "none") {
      const failedWrite = vi.spyOn(memoryStorage, "setItem").mockImplementation(() => { throw new Error("unavailable"); });
      act(() => result.current.selectAccentColor(action === "failed-61" ? 61 : 60));
      failedWrite.mockRestore();
    }
    const write = vi.spyOn(memoryStorage, "setItem");
    if (stored === undefined) storedValues.delete(LAYOUT_KEY);
    else storedValues.set(LAYOUT_KEY, typeof stored === "string" ? stored : JSON.stringify(stored));

    act(() => emitStorage(key, null));

    expect(result.current.accentColor).toBe(expected);
    expect(result.current.error).toBe(action === "failed-60" ? "Couldn't save the accent color. It will reset when Peppy restarts." : undefined);
    expect(write).not.toHaveBeenCalled();
  });

  it("restores distinct pre-existing attributes after StrictMode replay and unmount", () => {
    document.documentElement.setAttribute("data-accent", "html-before");
    document.body.setAttribute("data-accent", "body-before");
    const { result, unmount } = renderHook(() => useAccentColor(), { wrapper: StrictMode });

    expect(document.documentElement.getAttribute("data-accent")).toBeNull();
    expect(document.body.getAttribute("data-accent")).toBeNull();

    act(() => result.current.selectAccentColor(61));
    unmount();
    expect(document.documentElement.getAttribute("data-accent")).toBe("html-before");
    expect(document.body.getAttribute("data-accent")).toBe("body-before");
  });

  it("removes attributes for default and coral", () => {
    const { result, unmount } = renderHook(() => useAccentColor());

    act(() => result.current.selectAccentColor(61));
    act(() => result.current.selectAccentColor(null));
    expect(document.documentElement.hasAttribute("data-accent")).toBe(false);
    expect(document.body.hasAttribute("data-accent")).toBe(false);

    act(() => result.current.selectAccentColor(7));
    expect(document.documentElement.hasAttribute("data-accent")).toBe(false);
    expect(document.body.hasAttribute("data-accent")).toBe(false);

    act(() => result.current.selectAccentColor(61));
    unmount();
    expect(document.documentElement.hasAttribute("data-accent")).toBe(false);
    expect(document.body.hasAttribute("data-accent")).toBe(false);
  });
});
