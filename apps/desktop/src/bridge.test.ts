import { beforeEach, describe, expect, it, vi } from "vitest";

const eventMock = vi.hoisted(() => ({
  handler: undefined as ((event: { payload: unknown }) => void) | undefined,
  windowHandlers: new Map<string, (event: { payload: unknown }) => void>(),
  stop: vi.fn(),
  windowStop: vi.fn(),
  windowListen: vi.fn(),
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
    listen: (event: string, handler: (event: { payload: unknown }) => void) =>
      eventMock.windowListen(event, handler),
  }),
}));

import { fixtureBridge, missingHostBridge, normalizeComputerClaimant, tauriBridge } from "./bridge";

describe("native computer claimant normalization", () => {
  const nativeView = (stage: string, errorCode?: string) => ({ stage, invitationId: "id", qrPayload: null, awaitingOwner: false, state: "consumed" as const, expiresInSeconds: 0, verificationCode: null, errorCode });

  it("normalizes a retained claimant from existing discovery, including finishing diagnostics", async () => {
    invokeMock.mockResolvedValueOnce({ mode: "community", stage: "locked", computerClaimant: nativeView("finishing", "host-state") });
    expect(await tauriBridge.enrollment_discover("https://community.example")).toMatchObject({ computerClaimant: { stage: "computer_finishing", errorCode: "host-state" } });
    expect(invokeMock).toHaveBeenCalledWith("enrollment_discover", { origin: "https://community.example" });
    invokeMock.mockClear();
  });

  it("preserves terminal native errors instead of treating them as expired", () => {
    expect(normalizeComputerClaimant(nativeView("error", "computer_remote_consumed"))).toMatchObject({ stage: "computer_error", errorCode: "computer_remote_consumed" });
    expect(normalizeComputerClaimant(nativeView("unknown"))).toMatchObject({ stage: "computer_error", errorCode: "computer_verification_failed" });
    expect(normalizeComputerClaimant(nativeView("expired"))).toMatchObject({ stage: "computer_expired" });
  });
});

describe("bridge subscriptions", () => {
  beforeEach(() => {
    eventMock.handler = undefined;
    eventMock.stop.mockReset();
    eventMock.windowStop.mockReset();
    eventMock.windowListen.mockReset();
    eventMock.windowListen.mockImplementation(async (event: string, handler: (event: { payload: unknown }) => void) => {
      eventMock.windowHandlers.set(event, handler);
      return eventMock.windowStop;
    });
    eventMock.windowHandlers.clear();
  });

  it("returns a disposer for both missing-host subscriptions", () => {
    expect(missingHostBridge.subscribe(() => undefined)).toEqual(expect.any(Function));
    expect(missingHostBridge.subscribe_lifecycle(() => undefined)).toEqual(expect.any(Function));
    expect(missingHostBridge.subscribe_lifecycle_finished(() => undefined)).toEqual(expect.any(Function));
    expect(missingHostBridge.subscribe_drafts?.(() => undefined)).toEqual(expect.any(Function));
    expect(fixtureBridge.subscribe_drafts?.(() => undefined)).toEqual(expect.any(Function));
  });

  it("uses the current webview listener and rejects malformed draft events", async () => {
    const listener = vi.fn();
    tauriBridge.subscribe_drafts?.(listener);
    await vi.waitFor(() => expect(eventMock.windowHandlers.get("peppy://draft")).toEqual(expect.any(Function)));
    const handler = eventMock.windowHandlers.get("peppy://draft");

    handler?.({ payload: { id: "draft", conversationId: "conversation", text: "text", recipientIds: [], attachmentIds: [], revision: "18446744073709551615" } });
    handler?.({ payload: { id: "draft", conversationId: "conversation", text: "text", recipientIds: [1], attachmentIds: [], revision: "1" } });
    handler?.({ payload: { id: "draft", conversationId: "conversation", text: "text", recipientIds: [], attachmentIds: [], revision: "01" } });

    expect(listener).toHaveBeenCalledOnce();
    expect(listener).toHaveBeenCalledWith({ id: "draft", conversationId: "conversation", text: "text", recipientIds: [], attachmentIds: [], revision: "18446744073709551615" });
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

  it("forwards maximize button regions and hover events through the current webview", async () => {
    const listener = vi.fn();
    const region = { x: 1, y: 2, width: 46, height: 32 };
    await tauriBridge.set_maximize_button_region(region);
    expect(invokeMock).toHaveBeenCalledWith("set_maximize_button_region", { region });

    tauriBridge.subscribe_maximize_button_hover(listener);
    await vi.waitFor(() => expect(eventMock.windowHandlers.get("peppy://maximize-button-hover")).toEqual(expect.any(Function)));
    const handler = eventMock.windowHandlers.get("peppy://maximize-button-hover");
    handler?.({ payload: "true" });
    handler?.({ payload: true });
    expect(listener).toHaveBeenCalledOnce();
    expect(listener).toHaveBeenCalledWith(true);
    invokeMock.mockClear();
  });

  it("safely disposes when the dynamic listener resolves after unmount", async () => {
    const dispose = tauriBridge.subscribe_lifecycle(() => undefined);
    dispose();
    await vi.waitFor(() => expect(eventMock.windowStop).toHaveBeenCalledOnce());
  });

  it("disposes a draft listener that registers after its caller unmounts", async () => {
    let finishRegistration: ((stop: () => void) => void) | undefined;
    eventMock.windowListen.mockImplementationOnce(() => new Promise((resolve) => {
      finishRegistration = resolve;
    }));
    const dispose = tauriBridge.subscribe_drafts?.(() => undefined);
    await vi.waitFor(() => expect(eventMock.windowListen).toHaveBeenCalledOnce());
    dispose?.();
    finishRegistration?.(eventMock.windowStop);
    await vi.waitFor(() => expect(eventMock.windowStop).toHaveBeenCalledOnce());
  });

  it("passes declared arguments to native commands", async () => {
    await tauriBridge.list_devices();
    await tauriBridge.revoke_device("device-id");
    await tauriBridge.request_contact_sync("book-id");
    await tauriBridge.get_contact_sync_request("book-id");
    await tauriBridge.restore_contact("book", "contact");
    await tauriBridge.enrollment_start_over();
    await tauriBridge.enrollment_rotate_phone("shared_secret");
    await tauriBridge.enrollment_cancel_phone();
    await tauriBridge.reveal_vault_secret();
    await tauriBridge.mark_all_read();
    await tauriBridge.set_conversation_pinned("conversation-id", true);
    await tauriBridge.set_conversation_tone("contact:+15555550100", null);
    await tauriBridge.get_history_search();
    await tauriBridge.start_history_search({ operationId: "d2719a52-d647-49ec-bb81-6da2a24e8cc2", query: "must not cross the bridge" } as never);
    await tauriBridge.cancel_history_search({ attemptId: "e2719a52-d647-49ec-bb81-6da2a24e8cc2", query: "must not cross the bridge" } as never);

    expect(invokeMock).toHaveBeenNthCalledWith(1, "list_devices", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(2, "revoke_device", { deviceId: "device-id" });
    expect(invokeMock).toHaveBeenNthCalledWith(3, "request_contact_sync", { bookId: "book-id" });
    expect(invokeMock).toHaveBeenNthCalledWith(4, "get_contact_sync_request", { bookId: "book-id" });
    expect(invokeMock).toHaveBeenNthCalledWith(5, "restore_contact", { bookId: "book", contactId: "contact" });
    expect(invokeMock).toHaveBeenNthCalledWith(6, "enrollment_start_over", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(7, "enrollment_rotate_phone", { method: "shared_secret" });
    expect(invokeMock).toHaveBeenNthCalledWith(8, "enrollment_cancel_phone", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(9, "reveal_vault_secret", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(10, "mark_all_read", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(11, "set_conversation_pinned", { conversationId: "conversation-id", pinned: true });
    expect(invokeMock).toHaveBeenNthCalledWith(12, "set_conversation_tone", { seed: "contact:+15555550100", paletteIndex: null });
    expect(invokeMock).toHaveBeenNthCalledWith(13, "get_history_search", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(14, "start_history_search", { input: { operationId: "d2719a52-d647-49ec-bb81-6da2a24e8cc2" } });
    expect(invokeMock).toHaveBeenNthCalledWith(15, "cancel_history_search", { input: { attemptId: "e2719a52-d647-49ec-bb81-6da2a24e8cc2" } });
  });

  it("opens a media viewer with its conversation, attachment, and message", async () => {
    await tauriBridge.open_media_viewer?.("conversation-id", "attachment-id", "message-id");

    expect(invokeMock).toHaveBeenCalledWith("open_media_viewer", {
      conversationId: "conversation-id",
      attachmentId: "attachment-id",
      messageId: "message-id",
    });
  });
});

describe("fixture draft saves", () => {
  let fixtureBridge: (typeof import("./bridge"))["fixtureBridge"];

  beforeEach(async () => {
    vi.resetModules();
    ({ fixtureBridge } = await import("./bridge"));
  });

  it("marks every fixture conversation unread count once", async () => {
    expect(await fixtureBridge.mark_all_read()).toBe("2");
    expect(await fixtureBridge.mark_all_read()).toBe("0");
    const state = await fixtureBridge.load_state();
    expect(state.conversations.every(conversation => conversation.unread === 0)).toBe(true);
  });

  it("updates only an established fixture conversation pin state", async () => {
    await fixtureBridge.set_conversation_pinned("conv-aurora", true);

    expect((await fixtureBridge.load_state()).conversations.find(conversation => conversation.id === "conv-aurora")?.pinned).toBe(true);
    await expect(fixtureBridge.set_conversation_pinned("missing", true)).rejects.toMatchObject({ code: "not-found" });
  });

  it("persists fixture tone overrides and clears them with automatic", async () => {
    await fixtureBridge.set_conversation_tone("contact:+15555550100", 60);
    expect((await fixtureBridge.load_state()).toneOverrides).toEqual({ "contact:+15555550100": 60 });

    await fixtureBridge.set_conversation_tone("contact:+15555550100", null);
    expect((await fixtureBridge.load_state()).toneOverrides).toEqual({});
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
