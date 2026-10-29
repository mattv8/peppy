import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useSeenBadges, type SeenBadgeObservation } from "./useSeenBadges";

class ControlledObserver {
  static instances: ControlledObserver[] = [];
  nodes = new Set<Element>();
  observe = vi.fn((node: Element) => this.nodes.add(node));
  unobserve = vi.fn((node: Element) => this.nodes.delete(node));
  disconnect = vi.fn(() => this.nodes.clear());
  constructor(public callback: IntersectionObserverCallback, public options: IntersectionObserverInit) {
    ControlledObserver.instances.push(this);
  }
  emit(node: Element, ratio: number, area = 100) {
    const bounds = { x: 0, y: 0, top: 0, left: 0, bottom: 10, right: area / 10, width: area / 10, height: 10, toJSON() {} };
    this.callback([{
      target: node, intersectionRatio: ratio, isIntersecting: ratio > 0,
      boundingClientRect: bounds, intersectionRect: bounds, rootBounds: bounds, time: performance.now(),
    }], this as unknown as IntersectionObserver);
  }
}

function latestObserver() {
  return ControlledObserver.instances.at(-1)!;
}

function setup(overrides: Partial<SeenBadgeObservation> = {}) {
  const node = document.createElement("span");
  const input: SeenBadgeObservation = {
    root: document.createElement("div"), obscuredBottom: 64, enabled: true,
    canRecord: () => true, badges: [{ key: "chip", reactionIds: ["old"] }], onSeen: vi.fn(), ...overrides,
  };
  const hook = renderHook((props: SeenBadgeObservation) => useSeenBadges(props), { initialProps: input });
  act(() => hook.result.current("chip", node));
  return { ...hook, node, input };
}

beforeEach(() => {
  ControlledObserver.instances = [];
  vi.stubGlobal("IntersectionObserver", ControlledObserver);
  vi.spyOn(document, "hasFocus").mockReturnValue(true);
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("useSeenBadges", () => {
  it("requires half of the chip, not the enclosing bubble, and delivers once", () => {
    const { node, input } = setup();
    const observer = latestObserver();
    expect(observer.options).toEqual({ root: input.root, rootMargin: "0px 0px -64px 0px", threshold: [0, 0.5] });
    expect(observer.nodes.has(node)).toBe(true);
    act(() => observer.emit(node, 0.49));
    expect(input.onSeen).not.toHaveBeenCalled();
    act(() => observer.emit(node, 0.5));
    act(() => observer.emit(node, 1));
    expect(input.onSeen).toHaveBeenCalledExactlyOnceWith(["old"]);
    expect(node.hasAttribute("data-visible")).toBe(false);
  });

  it("waits for fresh geometry after provenance changes and fences obsolete callbacks", () => {
    const { node, input, rerender } = setup();
    const old = latestObserver();
    act(() => old.emit(node, 1));
    rerender({ ...input, badges: [{ key: "chip", reactionIds: ["old", "new"] }] });
    const fresh = latestObserver();
    expect(old.unobserve).toHaveBeenCalledWith(node);
    expect(fresh.nodes.has(node)).toBe(true);
    act(() => old.emit(node, 1));
    expect(input.onSeen).toHaveBeenCalledTimes(1);
    act(() => fresh.emit(node, 0));
    expect(input.onSeen).toHaveBeenCalledTimes(1);
    act(() => fresh.emit(node, 0.5));
    expect(input.onSeen).toHaveBeenLastCalledWith(["new"]);
  });

  it.each(["disabled", "unfocused", "hidden", "live gate"])("does not record when %s", (gate) => {
    if (gate === "unfocused") vi.mocked(document.hasFocus).mockReturnValue(false);
    if (gate === "hidden") vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    let allowed = true;
    const { node, input } = setup({ enabled: gate !== "disabled", canRecord: () => allowed });
    if (gate === "live gate") allowed = false;
    act(() => latestObserver()?.emit(node, 1));
    expect(input.onSeen).not.toHaveBeenCalled();
  });

  it.each(["focus", "visibilitychange"])("reobserves on %s without trusting a cached intersection", (event) => {
    const { node, input } = setup();
    vi.mocked(document.hasFocus).mockReturnValue(false);
    const old = latestObserver();
    act(() => old.emit(node, 1));
    vi.mocked(document.hasFocus).mockReturnValue(true);
    act(() => (event === "focus" ? window : document).dispatchEvent(new Event(event)));
    expect(latestObserver()).not.toBe(old);
    act(() => old.emit(node, 1));
    expect(input.onSeen).not.toHaveBeenCalled();
    act(() => latestObserver().emit(node, 1));
    expect(input.onSeen).toHaveBeenCalledExactlyOnceWith(["old"]);
  });

  it("reobserves on a caller rerender when the live transaction gate releases", () => {
    let allowed = false;
    const { node, input, rerender } = setup({ canRecord: () => allowed });
    act(() => latestObserver().emit(node, 1));
    allowed = true;
    rerender({ ...input });
    expect(input.onSeen).not.toHaveBeenCalled();
    act(() => latestObserver().emit(node, 1));
    expect(input.onSeen).toHaveBeenCalledExactlyOnceWith(["old"]);
  });

  it("waits for reactive root mount and known composer geometry", () => {
    const { node, input, rerender } = setup({ root: null, obscuredBottom: null });
    expect(ControlledObserver.instances).toHaveLength(0);
    rerender({ ...input, root: document.createElement("div") });
    expect(ControlledObserver.instances).toHaveLength(0);
    rerender({ ...input, root: document.createElement("div"), obscuredBottom: 0 });
    act(() => latestObserver().emit(node, 1));
    expect(input.onSeen).toHaveBeenCalledExactlyOnceWith(["old"]);
  });

  it("rejects zero-area entries and disconnects/fences on unmount", () => {
    const { node, input, unmount } = setup();
    const observer = latestObserver();
    act(() => observer.emit(node, 1, 0));
    expect(input.onSeen).not.toHaveBeenCalled();
    unmount();
    expect(observer.disconnect).toHaveBeenCalled();
    act(() => observer.emit(node, 1));
    expect(input.onSeen).not.toHaveBeenCalled();
  });

  it("keeps hints when IntersectionObserver is unavailable", () => {
    vi.stubGlobal("IntersectionObserver", undefined);
    const { input } = setup();
    expect(input.onSeen).not.toHaveBeenCalled();
  });

  it("fences callbacks while inactive/locked and requires fresh geometry after reenabling", () => {
    const { node, input, rerender } = setup();
    const old = latestObserver();
    rerender({ ...input, enabled: false });
    act(() => old.emit(node, 1));
    expect(input.onSeen).not.toHaveBeenCalled();
    rerender(input);
    expect(input.onSeen).not.toHaveBeenCalled();
    act(() => latestObserver().emit(node, 1));
    expect(input.onSeen).toHaveBeenCalledExactlyOnceWith(["old"]);
  });

  it("replaces root and node observations without retaining obsolete provenance", () => {
    const { node, input, rerender, result } = setup();
    const old = latestObserver();
    rerender({ ...input, root: document.createElement("div"), badges: [{ key: "chip", reactionIds: ["replacement"] }] });
    const priorNodeObserver = latestObserver();
    const replacement = document.createElement("span");
    act(() => result.current("chip", replacement));
    act(() => old.emit(node, 1));
    act(() => priorNodeObserver.emit(node, 1));
    act(() => latestObserver().emit(node, 1));
    expect(input.onSeen).not.toHaveBeenCalled();
    act(() => latestObserver().emit(replacement, 1));
    expect(input.onSeen).toHaveBeenCalledExactlyOnceWith(["replacement"]);
  });

  it("batches and deduplicates IDs across intersecting chips in one callback", () => {
    const { node, input, result } = setup({ badges: [
      { key: "chip", reactionIds: ["old", "shared"] },
      { key: "other", reactionIds: ["shared", "new"] },
    ] });
    const other = document.createElement("span");
    act(() => result.current("other", other));
    const observer = latestObserver();
    const entries: IntersectionObserverEntry[] = [];
    const originalCallback = observer.callback;
    observer.callback = (batch) => entries.push(...batch);
    observer.emit(node, 1);
    observer.emit(other, 1);
    act(() => originalCallback(entries, observer as unknown as IntersectionObserver));
    expect(input.onSeen).toHaveBeenCalledExactlyOnceWith(["old", "shared", "new"]);
  });
});
