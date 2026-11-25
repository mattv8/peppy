import { describe, expect, it, vi } from "vitest";
import type { MessageView, ReactionSubmitView } from "@peppy/desktop-ui/bridge-contracts";
import { PendingReactionStore } from "./usePendingReactions";

const input = { conversationId: "c", messageId: "m", route: "native" as const, emoji: "👍" };
const messages: MessageView[] = [];

function deferred() {
  let resolve!: (result: ReactionSubmitView) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<ReactionSubmitView>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

describe("PendingReactionStore", () => {
  it("retains accepted entries until a new messages array and guards duplicate calls", async () => {
    const store = new PendingReactionStore();
    const call = deferred();
    store.observe("c", messages);
    const result = store.submit(input, () => call.promise)!;
    const duplicate = vi.fn();
    expect(store.submit(input, duplicate)).toBeUndefined();
    expect(store.isInFlight("m")).toBe(true);
    expect(store.entries().get("m")?.request.requestId).toMatch(/^local:m:/);
    expect(store.inFlight()).toHaveLength(1);
    expect(store.observe("c", messages)).toEqual([]);
    call.resolve({ accepted: true, requestId: "real" });
    await result;
    expect(store.isInFlight("m")).toBe(false);
    expect(store.entries().get("m")?.request.requestId).toBe("real");
    expect(store.observe("c", messages)).toEqual([]);
    expect(store.observe("c", [...messages])).toHaveLength(1);
    expect(store.entries().size).toBe(0);
    expect(duplicate).not.toHaveBeenCalled();
  });

  it("holds provisional entries over snapshots but drops them when the call catches up", async () => {
    const store = new PendingReactionStore();
    const call = deferred();
    store.observe("c", messages);
    const result = store.submit(input, () => call.promise)!;
    store.observe("c", [...messages]);
    expect(store.entries().size).toBe(1);
    call.resolve({ accepted: true, requestId: "real" });
    await result;
    expect(store.entries().size).toBe(0);
  });

  it("clears refusals and rejections and exposes their settlement", async () => {
    const store = new PendingReactionStore();
    const refused = deferred();
    const result = store.submit(input, () => refused.promise)!;
    const settlement = store.inFlight()[0];
    refused.resolve({ accepted: false });
    await result;
    expect(await settlement).toBe("unsent");
    expect(store.entries().size).toBe(0);
    const rejected = deferred();
    const errorResult = store.submit(input, () => rejected.promise)!;
    const errorSettlement = store.inFlight().at(-1)!;
    rejected.reject(new Error("offline"));
    await expect(errorResult).rejects.toThrow("offline");
    expect(await errorSettlement).toBe("unsent");
    expect(store.entries().size).toBe(0);
  });

  it("does not resurrect entries after reset or conversation removal", async () => {
    const store = new PendingReactionStore();
    const call = deferred();
    const result = store.submit(input, () => call.promise)!;
    store.reset();
    call.resolve({ accepted: true, requestId: "real" });
    await result;
    expect(store.entries().size).toBe(0);
    const next = deferred();
    const nextResult = store.submit(input, () => next.promise)!;
    store.retainConversations(new Set());
    next.resolve({ accepted: true });
    await nextResult;
    expect(store.entries().size).toBe(0);
  });

  it("retains snapshot identity and the old flight across reset", async () => {
    const store = new PendingReactionStore();
    store.observe("c", messages);
    const old = deferred();
    const oldResult = store.submit(input, () => old.promise)!;
    store.reset();
    expect(store.inFlight()).toHaveLength(1);
    const next = deferred();
    const result = store.submit(input, () => next.promise)!;
    expect(store.observe("c", messages)).toEqual([]);
    next.resolve({ accepted: true, requestId: "real" });
    await result;
    expect(store.entries().get("m")?.request.requestId).toBe("real");
    old.resolve({ accepted: false });
    await oldResult;
    expect(store.entries().get("m")?.request.requestId).toBe("real");
  });
});
