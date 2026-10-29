import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createSeenReactionLedger, type SeenReactionLedger, type SeenReactionScope } from "./seen";

const conversation = "11111111-1111-4111-8111-111111111111";
const otherConversation = "22222222-2222-4222-8222-222222222222";
const reaction = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const otherReaction = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const scope = { mode: "desktop", origin: "https://example.com" };

describe("seen reaction ledger", () => {
  let values: Map<string, string>;
  let storage: Storage;
  let ledgers: SeenReactionLedger[];
  const open = (overrides: SeenReactionScope = scope) => {
    const ledger = createSeenReactionLedger(overrides);
    ledgers.push(ledger);
    return ledger;
  };
  const emit = (key: string | null, newValue: string | null = "1") => {
    window.dispatchEvent(new StorageEvent("storage", { key, newValue }));
  };

  beforeEach(() => {
    values = new Map();
    ledgers = [];
    storage = {
      get length() { return values.size; },
      key: vi.fn(() => { throw new Error("must not scan storage"); }),
      getItem: vi.fn((key: string) => values.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => { values.set(key, value); }),
      removeItem: (key: string) => { values.delete(key); },
      clear: () => { values.clear(); },
    };
    vi.stubGlobal("localStorage", storage);
  });

  afterEach(() => {
    ledgers.forEach(ledger => ledger.dispose());
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("constructs without writes or listeners and reads an empty ledger synchronously", () => {
    const listen = vi.spyOn(window, "addEventListener");
    const ledger = open();
    expect(listen).not.toHaveBeenCalled();
    expect(storage.setItem).not.toHaveBeenCalled();
    expect(ledger.has(conversation, reaction)).toBe(false);
  });

  it("persists independent event keys once and reopens synchronously", () => {
    const ledger = open();
    ledger.mark(conversation, [reaction, reaction, otherReaction]);
    ledger.mark(conversation, [reaction]);
    expect(values.size).toBe(2);
    expect([...values.values()]).toEqual(["1", "1"]);
    expect(storage.setItem).toHaveBeenCalledTimes(2);
    expect(open().has(conversation, reaction)).toBe(true);
    expect(open().has(conversation, otherReaction)).toBe(true);
    expect(ledger.has(otherConversation, reaction)).toBe(false);
  });

  it("isolates mode and origin while canonicalizing origin case, port and path", () => {
    open().mark(conversation, [reaction]);
    expect(open({ ...scope, origin: "HTTPS://EXAMPLE.COM:443/path/" }).has(conversation, reaction)).toBe(true);
    expect(open({ ...scope, mode: "browser" }).has(conversation, reaction)).toBe(false);
    expect(open({ ...scope, origin: "https://other.example.com" }).has(conversation, reaction)).toBe(false);
  });

  it("keeps missing or invalid scopes and non-UUID fixture IDs memory-only", () => {
    for (const invalidScope of [{ mode: "desktop" }, { mode: "", origin: scope.origin }, { ...scope, origin: "bad origin" }]) {
      const ledger = open(invalidScope);
      ledger.mark(conversation, [reaction]);
      expect(ledger.has(conversation, reaction)).toBe(true);
    }
    const ledger = open();
    ledger.mark("", [reaction]);
    ledger.mark(conversation, [""]);
    ledger.mark("fixture-conversation", ["fixture-reaction"]);
    expect(ledger.has("", reaction)).toBe(false);
    expect(ledger.has(conversation, "")).toBe(false);
    expect(ledger.has("fixture-conversation", "fixture-reaction")).toBe(true);
    expect(values.size).toBe(0);
  });

  it("rejects malformed values and never caches a negative read", () => {
    open().mark(conversation, [reaction]);
    const key = [...values.keys()][0];
    for (const value of ["true", "0", "{}", "", "\"1\""]) {
      values.set(key, value);
      expect(open().has(conversation, reaction)).toBe(false);
    }
    const ledger = open();
    expect(ledger.has(conversation, reaction)).toBe(false);
    values.set(key, "1");
    expect(ledger.has(conversation, reaction)).toBe(true);
  });

  it("falls back to monotonic memory when reads, writes or storage access fail", () => {
    vi.mocked(storage.getItem).mockImplementation(() => { throw new Error("denied"); });
    vi.mocked(storage.setItem).mockImplementation(() => { throw new Error("quota"); });
    const ledger = open();
    expect(ledger.has(conversation, reaction)).toBe(false);
    ledger.mark(conversation, [reaction]);
    ledger.mark(conversation, [reaction]);
    expect(ledger.has(conversation, reaction)).toBe(true);
    expect(storage.setItem).toHaveBeenCalledTimes(1);
    vi.stubGlobal("localStorage", undefined);
    const unavailable = open();
    unavailable.mark(conversation, [reaction]);
    expect(unavailable.has(conversation, reaction)).toBe(true);
  });

  it("notifies local subscribers once per new batch and detaches on unsubscribe or dispose", () => {
    const ledger = open();
    const first = vi.fn();
    const second = vi.fn();
    const stop = ledger.subscribe(first);
    ledger.subscribe(second);
    ledger.mark(conversation, [reaction, otherReaction]);
    ledger.mark(conversation, [reaction]);
    expect(first).toHaveBeenCalledTimes(1);
    stop();
    emit([...values.keys()][0]);
    expect(first).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenCalledTimes(2);
    ledger.dispose();
    emit([...values.keys()][0]);
    expect(second).toHaveBeenCalledTimes(2);
  });

  it("merges two clients marking different events before receiving storage events", () => {
    const first = open();
    const second = open();
    const firstChanged = vi.fn();
    const secondChanged = vi.fn();
    first.subscribe(firstChanged);
    second.subscribe(secondChanged);
    expect(first.has(conversation, otherReaction)).toBe(false);
    expect(second.has(conversation, reaction)).toBe(false);
    first.mark(conversation, [reaction]);
    second.mark(conversation, [otherReaction]);
    for (const key of values.keys()) emit(key);
    for (const ledger of [first, second, open()]) {
      expect(ledger.has(conversation, reaction)).toBe(true);
      expect(ledger.has(conversation, otherReaction)).toBe(true);
    }
    expect(firstChanged).toHaveBeenCalledTimes(3);
    expect(secondChanged).toHaveBeenCalledTimes(3);
    emit("unrelated");
    open({ ...scope, mode: "browser" }).mark(conversation, [reaction]);
    emit([...values.keys()][2]);
    expect(firstChanged).toHaveBeenCalledTimes(3);
  });

  it("keeps observed marks monotonic after clear but restarts empty", () => {
    const ledger = open();
    const changed = vi.fn();
    ledger.subscribe(changed);
    ledger.mark(conversation, [reaction]);
    storage.clear();
    emit(null, null);
    expect(changed).toHaveBeenCalledTimes(2);
    expect(ledger.has(conversation, reaction)).toBe(true);
    expect(open().has(conversation, reaction)).toBe(false);
    expect(ledger.has(conversation, otherReaction)).toBe(false);
  });

  it("owns one listener until the final subscription ends and supports effect resubscription", () => {
    const add = vi.spyOn(window, "addEventListener");
    const remove = vi.spyOn(window, "removeEventListener");
    const ledger = open();
    const changed = vi.fn();
    const firstStop = ledger.subscribe(changed);
    const secondStop = ledger.subscribe(changed);
    expect(add.mock.calls.filter(([name]) => name === "storage")).toHaveLength(1);
    firstStop();
    expect(remove.mock.calls.filter(([name]) => name === "storage")).toHaveLength(0);
    secondStop();
    expect(remove.mock.calls.filter(([name]) => name === "storage")).toHaveLength(1);
    const stopAgain = ledger.subscribe(changed);
    ledger.mark(conversation, [reaction]);
    expect(changed).toHaveBeenCalledTimes(1);
    stopAgain();
    ledger.dispose();
    ledger.subscribe(changed);
    emit([...values.keys()][0]);
    expect(changed).toHaveBeenCalledTimes(1);
  });

  it("catches a denied localStorage getter and does not scan or write during reads", () => {
    const ledger = open();
    ledger.has(conversation, reaction);
    expect(storage.getItem).toHaveBeenCalledTimes(1);
    expect(storage.key).not.toHaveBeenCalled();
    expect(storage.setItem).not.toHaveBeenCalled();
    Object.defineProperty(globalThis, "localStorage", {
      configurable: true,
      get() { throw new Error("denied getter"); },
    });
    expect(ledger.has(conversation, reaction)).toBe(false);
    ledger.mark(conversation, [reaction]);
    expect(ledger.has(conversation, reaction)).toBe(true);
  });

  it("remembers valid external events monotonically without importing malformed marks", () => {
    open().mark(conversation, [reaction]);
    const key = [...values.keys()][0];
    values.clear();
    const ledger = open();
    ledger.subscribe(() => {});
    emit(key, "true");
    expect(ledger.has(conversation, reaction)).toBe(false);
    emit(key);
    emit(key, null);
    expect(ledger.has(conversation, reaction)).toBe(true);
    expect(open().has(conversation, reaction)).toBe(false);
  });
});
