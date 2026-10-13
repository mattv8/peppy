import { beforeEach, describe, expect, it, vi } from "vitest";

const eventMock = vi.hoisted(() => ({
  handler: undefined as ((event: { payload: unknown }) => void) | undefined,
  windowHandlers: new Map<string, (event: { payload: unknown }) => void>(),
  stop: vi.fn(),
  windowStop: vi.fn(),
}));
const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_event: string, handler: (event: { payload: unknown }) => void) => {
    eventMock.handler = handler;
    return eventMock.stop;
  }),
}));

vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => ({
    listen: vi.fn(async (event: string, handler: (event: { payload: unknown }) => void) => {
      eventMock.windowHandlers.set(event, handler);
      return eventMock.windowStop;
    }),
  }),
}));

import { missingHostBridge, tauriBridge } from "./bridge";

describe("bridge subscriptions", () => {
  beforeEach(() => {
    eventMock.handler = undefined;
    eventMock.stop.mockReset();
    eventMock.windowStop.mockReset();
    eventMock.windowHandlers.clear();
  });

  it("returns a disposer for both missing-host subscriptions", () => {
    expect(missingHostBridge.subscribe(() => undefined)).toEqual(expect.any(Function));
    expect(missingHostBridge.subscribe_lifecycle(() => undefined)).toEqual(expect.any(Function));
    expect(missingHostBridge.subscribe_lifecycle_finished(() => undefined)).toEqual(expect.any(Function));
  });

  it("rejects malformed lifecycle payloads before invoking the listener", async () => {
    const listener = vi.fn();
    tauriBridge.subscribe_lifecycle(listener);
    await vi.waitFor(() => expect(eventMock.windowHandlers.get("peppy://lifecycle-request")).toEqual(expect.any(Function)));
    const handler = eventMock.windowHandlers.get("peppy://lifecycle-request");

    handler?.({ payload: { id: "request", action: "delete" } });
    handler?.({ payload: { id: 4, action: "quit" } });
    handler?.({ payload: { id: "request", action: "collapse" } });

    expect(listener).toHaveBeenCalledOnce();
    expect(listener).toHaveBeenCalledWith({ id: "request", action: "collapse" });
  });

  it("uses the current webview listener and validates lifecycle completion", async () => {
    const listener = vi.fn();
    tauriBridge.subscribe_lifecycle_finished(listener);
    await vi.waitFor(() => expect(eventMock.windowHandlers.get("peppy://lifecycle-finished")).toEqual(expect.any(Function)));
    const handler = eventMock.windowHandlers.get("peppy://lifecycle-finished");
    handler?.({ payload: { id: "request", ok: "yes" } });
    handler?.({ payload: { id: "request", ok: false } });
    expect(listener).toHaveBeenCalledOnce();
    expect(listener).toHaveBeenCalledWith({ id: "request", ok: false });
  });

  it("safely disposes when the dynamic listener resolves after unmount", async () => {
    const dispose = tauriBridge.subscribe_lifecycle(() => undefined);
    dispose();
    await vi.waitFor(() => expect(eventMock.windowStop).toHaveBeenCalledOnce());
  });

  it("passes declared arguments to native commands", async () => {
    await tauriBridge.restore_contact("book", "contact");
    await tauriBridge.enrollment_start_over();
    await tauriBridge.enrollment_rotate_phone("shared_secret");
    await tauriBridge.enrollment_cancel_phone();
    await tauriBridge.reveal_vault_secret();

    expect(invokeMock).toHaveBeenNthCalledWith(1, "restore_contact", { bookId: "book", contactId: "contact" });
    expect(invokeMock).toHaveBeenNthCalledWith(2, "enrollment_start_over", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(3, "enrollment_rotate_phone", { method: "shared_secret" });
    expect(invokeMock).toHaveBeenNthCalledWith(4, "enrollment_cancel_phone", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(5, "reveal_vault_secret", undefined);
  });
});

describe("fixture draft saves", () => {
  let fixtureBridge: (typeof import("./bridge"))["fixtureBridge"];

  beforeEach(async () => {
    vi.resetModules();
    ({ fixtureBridge } = await import("./bridge"));
  });

  it("reuses one blank draft without overwriting its content", async () => {
    const blank = await fixtureBridge.save_draft({ id: "", conversationId: "", text: "", recipientIds: [], attachmentIds: [], expectedRevision: "0" });
    const filled = await fixtureBridge.save_draft({ id: blank.id, conversationId: blank.conversationId, text: "Keep this", recipientIds: [], attachmentIds: [], expectedRevision: blank.revision });
    const reopened = await fixtureBridge.save_draft({ id: "", conversationId: "", text: "", recipientIds: [], attachmentIds: [], expectedRevision: "0" });

    expect(reopened).toMatchObject({ id: filled.id, text: "Keep this", revision: filled.revision });
    await expect(fixtureBridge.save_draft({ id: "", conversationId: "", text: "Discard this", recipientIds: [], attachmentIds: [], expectedRevision: "0" })).rejects.toMatchObject({
      code: "draft-moved",
      details: { conversationId: filled.conversationId, draftId: filled.id, currentRevision: filled.revision },
    });
  });

  it("does not reuse a reply draft as a new blank draft", async () => {
    const reply = await fixtureBridge.save_draft({ id: "", conversationId: "conv-aurora", text: "Reply", recipientIds: [], attachmentIds: [], expectedRevision: "0" });
    const blank = await fixtureBridge.save_draft({ id: "", conversationId: "", text: "", recipientIds: [], attachmentIds: [], expectedRevision: "0" });

    expect(blank.conversationId).not.toBe(reply.conversationId);
    expect((await fixtureBridge.load_state(blank.conversationId)).draft).toMatchObject({ id: blank.id, text: "" });
  });

  it("reuses exact draft-only recipient sets without changing stored chip order", async () => {
    const original = await fixtureBridge.save_draft({ id: "", conversationId: "", text: "", recipientIds: ["+12025550181", "+12025550180"], attachmentIds: [], expectedRevision: "0" });
    const reused = await fixtureBridge.save_draft({ id: "", conversationId: "", text: "", recipientIds: ["(202) 555-0180", "+12025550181"], attachmentIds: [], expectedRevision: "0" });
    const state = await fixtureBridge.load_state();

    expect(reused).toMatchObject({ id: original.id, recipientIds: ["+12025550181", "+12025550180"] });
    expect(state.conversations.filter((conversation) => conversation.id === original.conversationId)).toHaveLength(1);
    await expect(fixtureBridge.save_draft({ id: "", conversationId: "", text: "Conflicting", recipientIds: ["+12025550180", "+12025550181"], attachmentIds: [], expectedRevision: "0" })).rejects.toMatchObject({
      code: "draft-moved",
      details: { conversationId: original.conversationId, draftId: original.id, currentRevision: original.revision },
    });
  });

  it("adopts canonical history recipients and merges an existing target draft", async () => {
    const target = await fixtureBridge.save_draft({ id: "", conversationId: "conv-aurora", text: "Existing target", recipientIds: ["+12025550123"], attachmentIds: ["target-file"], expectedRevision: "0" });
    const blank = await fixtureBridge.save_draft({ id: "", conversationId: "", text: "", recipientIds: [], attachmentIds: [], expectedRevision: "0" });
    const saved = await fixtureBridge.save_draft({ id: blank.id, conversationId: blank.conversationId, text: "Local text", recipientIds: ["(202) 555-0123"], attachmentIds: ["local-file"], expectedRevision: blank.revision });

    expect(saved).toMatchObject({ id: target.id, conversationId: "conv-aurora", recipientIds: ["+12025550123"], text: "Existing target\nLocal text", attachmentIds: ["target-file", "local-file"] });
    expect(Number(saved.revision)).toBeGreaterThan(Number(target.revision));
  });

  it("forks recipient changes after adopting history instead of changing that history", async () => {
    const blank = await fixtureBridge.save_draft({ id: "", conversationId: "", text: "", recipientIds: [], attachmentIds: [], expectedRevision: "0" });
    const adopted = await fixtureBridge.save_draft({ id: blank.id, conversationId: blank.conversationId, text: "Reply", recipientIds: ["+12025550123"], attachmentIds: [], expectedRevision: blank.revision });
    const forked = await fixtureBridge.save_draft({ id: adopted.id, conversationId: adopted.conversationId, text: "Group reply", recipientIds: ["+12025550123", "+12025550125"], attachmentIds: [], expectedRevision: adopted.revision });
    const state = await fixtureBridge.load_state(forked.conversationId);

    expect(forked.conversationId).not.toBe("conv-aurora");
    expect(state.draft).toMatchObject({ recipientIds: ["+12025550123", "+12025550125"], text: "Group reply" });
    expect((await fixtureBridge.load_state("conv-aurora")).conversations.find((conversation) => conversation.id === "conv-aurora")?.participants).toEqual(["+12025550123"]);
  });

  it("rejects stale and mismatched saves, and makes empty draft discard idempotent", async () => {
    const blank = await fixtureBridge.save_draft({ id: "", conversationId: "", text: "", recipientIds: [], attachmentIds: [], expectedRevision: "0" });
    await expect(fixtureBridge.save_draft({ id: blank.id, conversationId: blank.conversationId, text: "Late", recipientIds: [], attachmentIds: [], expectedRevision: "0" })).rejects.toMatchObject({ code: "stale-draft" });
    await expect(fixtureBridge.save_draft({ id: blank.id, conversationId: "wrong-conversation", text: "", recipientIds: [], attachmentIds: [], expectedRevision: blank.revision })).rejects.toMatchObject({ code: "invalid-draft" });

    await fixtureBridge.discard_draft({ id: blank.id, conversationId: blank.conversationId, expectedRevision: blank.revision });
    await expect(fixtureBridge.discard_draft({ id: blank.id, conversationId: blank.conversationId, expectedRevision: blank.revision })).resolves.toBeUndefined();

    const nextBlank = await fixtureBridge.save_draft({ id: "", conversationId: "", text: "", recipientIds: [], attachmentIds: [], expectedRevision: "0" });
    const addressed = await fixtureBridge.save_draft({ id: nextBlank.id, conversationId: nextBlank.conversationId, text: "Keep", recipientIds: ["+12025550199"], attachmentIds: [], expectedRevision: nextBlank.revision });
    await expect(fixtureBridge.discard_draft({ id: addressed.id, conversationId: addressed.conversationId, expectedRevision: addressed.revision })).rejects.toMatchObject({ code: "draft-not-discardable" });
  });
});
