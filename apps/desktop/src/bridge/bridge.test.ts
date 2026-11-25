import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const eventMock = vi.hoisted(() => ({
  handler: undefined as ((event: { payload: unknown }) => void) | undefined,
  windowHandlers: new Map<string, (event: { payload: unknown }) => void>(),
  stop: vi.fn(),
  listen: vi.fn(),
  windowStop: vi.fn(),
  windowListen: vi.fn(),
}));
const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

vi.mock("@tauri-apps/api/event", () => ({
  listen: eventMock.listen,
}));

vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => ({
    listen: (event: string, handler: (event: { payload: unknown }) => void) =>
      eventMock.windowListen(event, handler),
  }),
}));

import { DIAGNOSTICS_METHOD_NAMES, MISSING_HOST_METHOD_NAMES, fixtureBridge, missingHostBridge, normalizeComputerClaimant, resetFixtureDiagnosticsForTests, tauriBridge } from "./bridge";
import { hasDiagnosticsBridge } from "../settings/useDiagnostics";

describe("missing native host bridge", () => {
  it("preserves fixture method order and excludes diagnostics", () => {
    const expected = Object.keys(fixtureBridge).filter(key => !DIAGNOSTICS_METHOD_NAMES.has(key));
    expect(MISSING_HOST_METHOD_NAMES).toEqual(expected);
    expect(Object.keys(missingHostBridge)).toEqual(expected);
  });

  it("provides no-op subscriptions and rejects other methods", async () => {
    for (const key of ["subscribe", "subscribe_maximize_button_hover", "subscribe_drafts", "subscribe_typing", "subscribe_lifecycle", "subscribe_lifecycle_finished"] as const) {
      const unsubscribe = (missingHostBridge[key] as () => () => void)();
      expect(typeof unsubscribe).toBe("function");
      expect(unsubscribe()).toBeUndefined();
    }
    for (const key of ["load_state", "save_draft"] as const) {
      await expect((missingHostBridge[key] as () => Promise<unknown>)()).rejects.toEqual({
        code: "missing-native-host",
        message: `Native host is required for ${key}.`,
      });
    }
  });
});

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

describe("pasted attachment bridge", () => {
  beforeEach(() => invokeMock.mockReset());

  it("sends each pasted image as raw bytes with encoded attachment headers", async () => {
    invokeMock.mockResolvedValue({ id: "attachment-1", name: "café.png", mediaType: "image/png", byteSize: 3, state: "ready" });
    const bytes = new Uint8Array([1, 2, 3]).buffer;

    await expect(tauriBridge.prepare_pasted_attachments([{ bytes, name: "café.png", mediaType: "image/png" }])).resolves.toMatchObject({ views: [{ id: "attachment-1" }], failures: [] });

    expect(invokeMock).toHaveBeenCalledWith("prepare_pasted_attachment", bytes, {
      headers: { "peppy-attachment-name": "caf%C3%A9.png", "peppy-attachment-type": "image/png" },
    });
  });

  it("prepares pasted images sequentially, retains successful views, and falls back for empty names", async () => {
    const first = new Uint8Array([1]).buffer;
    const second = new Uint8Array([2]).buffer;
    invokeMock
      .mockResolvedValueOnce({ id: "attachment-1", name: "first.png", mediaType: "image/png", byteSize: 1, state: "ready" })
      .mockRejectedValueOnce(new Error("disk full"));

    await expect(tauriBridge.prepare_pasted_attachments([
      { bytes: first, name: "first.png", mediaType: "image/png" },
      { bytes: second, name: "", mediaType: "image/png" },
    ])).resolves.toEqual({
      views: [{ id: "attachment-1", name: "first.png", mediaType: "image/png", byteSize: 1, state: "ready" }],
      failures: ["Pasted image: disk full"],
    });

    expect(invokeMock).toHaveBeenNthCalledWith(1, "prepare_pasted_attachment", first, {
      headers: { "peppy-attachment-name": "first.png", "peppy-attachment-type": "image/png" },
    });
    expect(invokeMock).toHaveBeenNthCalledWith(2, "prepare_pasted_attachment", second, {
      headers: { "peppy-attachment-name": "pasted-image", "peppy-attachment-type": "image/png" },
    });
  });

  it("reports a native plain-object rejection reason", async () => {
    invokeMock.mockRejectedValueOnce({ message: "Image is too large" });

    await expect(tauriBridge.prepare_pasted_attachments([
      { bytes: new Uint8Array([1]).buffer, name: "large.png", mediaType: "image/png" },
    ])).resolves.toEqual({ views: [], failures: ["large.png: Image is too large"] });
  });
});

describe("bridge subscriptions", () => {
  beforeEach(() => {
    invokeMock.mockClear();
    eventMock.handler = undefined;
    eventMock.stop.mockReset();
    eventMock.listen.mockReset();
    eventMock.listen.mockImplementation(async (_event: string, handler: (event: { payload: unknown }) => void) => {
      eventMock.handler = handler;
      return eventMock.stop;
    });
    eventMock.windowStop.mockReset();
    eventMock.windowListen.mockReset();
    eventMock.windowListen.mockImplementation(async (event: string, handler: (event: { payload: unknown }) => void) => {
      eventMock.windowHandlers.set(event, handler);
      return eventMock.windowStop;
    });
    eventMock.windowHandlers.clear();
  });

  afterEach(() => vi.useRealTimers());

  it("retries state registration and delivers subsequent events", async () => {
    vi.useFakeTimers();
    eventMock.listen.mockRejectedValueOnce(new Error("listen failed"));
    const listener = vi.fn();
    const dispose = tauriBridge.subscribe(listener);
    await vi.dynamicImportSettled();
    await vi.advanceTimersByTimeAsync(1000);
    expect(eventMock.listen).toHaveBeenCalledTimes(2);
    expect(eventMock.handler).toEqual(expect.any(Function));
    eventMock.handler?.({ payload: null });
    expect(listener).toHaveBeenCalledTimes(2);
    dispose();
    expect(eventMock.stop).toHaveBeenCalledOnce();
  });

  it("catches up once after state registration", async () => {
    const listener = vi.fn();
    const dispose = tauriBridge.subscribe(listener);
    await vi.waitFor(() => expect(listener).toHaveBeenCalledOnce());
    dispose();
  });

  it("keeps the registration when the catch-up listener throws", async () => {
    vi.useFakeTimers();
    const listener = vi.fn().mockImplementationOnce(() => { throw new Error("listener failed"); });
    const dispose = tauriBridge.subscribe(listener);
    await vi.dynamicImportSettled();
    expect(listener).toHaveBeenCalledOnce();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(eventMock.listen).toHaveBeenCalledOnce();
    eventMock.handler?.({ payload: null });
    expect(listener).toHaveBeenCalledTimes(2);
    dispose();
    expect(eventMock.stop).toHaveBeenCalledOnce();
  });

  it("unlistens without catching up when state registration finishes after disposal", async () => {
    let finishRegistration: ((stop: () => void) => void) | undefined;
    eventMock.listen.mockImplementationOnce(() => new Promise(resolve => { finishRegistration = resolve; }));
    const listener = vi.fn();
    const dispose = tauriBridge.subscribe(listener);
    await vi.waitFor(() => expect(eventMock.listen).toHaveBeenCalledOnce());
    dispose();
    finishRegistration?.(eventMock.stop);
    await vi.waitFor(() => expect(eventMock.stop).toHaveBeenCalledOnce());
    expect(listener).not.toHaveBeenCalled();
  });

  it("cancels state registration retries on disposal", async () => {
    vi.useFakeTimers();
    eventMock.listen.mockRejectedValue(new Error("listen failed"));
    const dispose = tauriBridge.subscribe(vi.fn());
    await vi.dynamicImportSettled();
    dispose();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(eventMock.listen).toHaveBeenCalledOnce();
  });

  it("doubles state registration retry delays up to 30 seconds", async () => {
    vi.useFakeTimers();
    eventMock.listen.mockRejectedValue(new Error("listen failed"));
    const dispose = tauriBridge.subscribe(vi.fn());
    await vi.dynamicImportSettled();
    for (const [index, delay] of [1000, 2000, 4000, 8000, 16000, 30000, 30000].entries()) {
      await vi.advanceTimersByTimeAsync(delay - 1);
      expect(eventMock.listen).toHaveBeenCalledTimes(index + 1);
      await vi.advanceTimersByTimeAsync(1);
      expect(eventMock.listen).toHaveBeenCalledTimes(index + 2);
    }
    dispose();
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

  it("forwards draft conflict and notice actions with native camelCase arguments", async () => {
    invokeMock.mockResolvedValue({ id: "draft-1" });
    await tauriBridge.resolve_draft_conflict({ draftId: "draft-1", keepBranchId: null });
    await tauriBridge.acknowledge_draft_notice("draft-1");
    expect(invokeMock).toHaveBeenNthCalledWith(1, "resolve_draft_conflict", { draftId: "draft-1", keepBranchId: null });
    expect(invokeMock).toHaveBeenNthCalledWith(2, "acknowledge_draft_notice", { draftId: "draft-1" });
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

  it("resolves draft readiness only after the native window listener is installed", async () => {
    let finishRegistration: ((stop: () => void) => void) | undefined;
    eventMock.windowListen.mockImplementationOnce((event: string, handler: (event: { payload: unknown }) => void) => new Promise((resolve) => {
      eventMock.windowHandlers.set(event, handler);
      finishRegistration = resolve;
    }));
    const subscription = tauriBridge.subscribe_drafts?.(() => undefined);
    let ready = false;
    void subscription?.ready?.then(() => { ready = true; });

    await vi.waitFor(() => expect(eventMock.windowListen).toHaveBeenCalledWith("peppy://draft", expect.any(Function)));
    await Promise.resolve();
    expect(ready).toBe(false);

    finishRegistration?.(eventMock.windowStop);
    await expect(subscription?.ready).resolves.toBeUndefined();
    expect(ready).toBe(true);
    subscription?.();
    expect(eventMock.windowStop).toHaveBeenCalledOnce();
  });

  it("rejects draft readiness when native registration fails without an unhandled rejection", async () => {
    const failure = new Error("listen denied");
    eventMock.windowListen.mockImplementationOnce(async () => { throw failure; });
    const subscription = tauriBridge.subscribe_drafts?.(() => undefined);

    await expect(subscription?.ready).rejects.toBe(failure);
    subscription?.();
    expect(eventMock.windowStop).not.toHaveBeenCalled();
  });

  it("keeps legacy draft callers working when they never await readiness", async () => {
    eventMock.windowListen.mockImplementationOnce(async () => { throw new Error("listen denied"); });
    const unhandled = vi.fn();
    process.on("unhandledRejection", unhandled);
    try {
      const dispose = tauriBridge.subscribe_drafts?.(() => undefined);
      expect(dispose).toEqual(expect.any(Function));
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(unhandled).not.toHaveBeenCalled();
    } finally {
      process.off("unhandledRejection", unhandled);
    }
  });

  it("unlistens immediately when a draft subscription is disposed before readiness resolves", async () => {
    let finishRegistration: ((stop: () => void) => void) | undefined;
    eventMock.windowListen.mockImplementationOnce(() => new Promise((resolve) => {
      finishRegistration = resolve;
    }));
    const subscription = tauriBridge.subscribe_drafts?.(() => undefined);
    await vi.waitFor(() => expect(eventMock.windowListen).toHaveBeenCalledOnce());
    subscription?.();
    expect(eventMock.windowStop).not.toHaveBeenCalled();

    finishRegistration?.(eventMock.windowStop);
    await expect(subscription?.ready).resolves.toBeUndefined();
    expect(eventMock.windowStop).toHaveBeenCalledOnce();
  });

  it("exposes typing commands with the declared native arguments", async () => {
    await tauriBridge.send_typing?.({ conversationId: "conversation-id", state: "started" });
    await tauriBridge.set_typing_sharing?.({ enabled: false });

    expect(invokeMock).toHaveBeenCalledWith("send_typing", { conversationId: "conversation-id", state: "started" });
    expect(invokeMock).toHaveBeenCalledWith("set_typing_sharing", { enabled: false });
  });

  it("delivers validated typing events and unsubscribes", async () => {
    const listener = vi.fn();
    const subscription = tauriBridge.subscribe_typing?.(listener);
    await subscription?.ready;
    const handler = eventMock.windowHandlers.get("peppy://typing");

    handler?.({ payload: { conversationId: "conversation-id", state: "started", expiresAtMs: "123" } });
    handler?.({ payload: { conversationId: "conversation-id", state: "stopped" } });
    handler?.({ payload: { conversationId: "conversation-id", state: "started", expiresAtMs: 123 } });
    handler?.({ payload: { conversationId: "conversation-id", state: "started", origin: "vault_device" } });

    expect(listener).toHaveBeenCalledTimes(2);
    subscription?.();
    expect(eventMock.windowStop).toHaveBeenCalledOnce();
  });

  it("passes rename arguments to the native command", async () => {
    await tauriBridge.rename_device("device-id", "Phone");
    expect(invokeMock).toHaveBeenCalledWith("rename_device", { deviceId: "device-id", name: "Phone" });
  });

  it("passes lifecycle send failure reasons to the native command", async () => {
    await tauriBridge.acknowledge_lifecycle("request", false, "send");
    expect(invokeMock).toHaveBeenCalledWith("acknowledge_lifecycle", { id: "request", ok: false, reason: "send" });
  });

  it("renames a fixture device and rejects missing devices", async () => {
    const roster = await fixtureBridge.list_devices();
    const device = roster.devices.find(item => !item.revoked)!;
    await fixtureBridge.rename_device(device.id, "  Renamed  ");
    expect((await fixtureBridge.list_devices()).devices.find(item => item.id === device.id)?.name).toBe("Renamed");
    await fixtureBridge.rename_device(device.id, device.name);
    await expect(fixtureBridge.rename_device("missing", "Phone")).rejects.toMatchObject({ code: "device_not_found" });
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
    await tauriBridge.set_conversation_folder("conversation-id", "archived");
    await tauriBridge.request_sender_unblock("conversation-id");
    await tauriBridge.set_conversation_tone("contact:+15555550100", null);
    await tauriBridge.get_history_search();
    await tauriBridge.start_history_search({ operationId: "d2719a52-d647-49ec-bb81-6da2a24e8cc2", query: "must not cross the bridge" } as never);
    await tauriBridge.cancel_history_search({ attemptId: "e2719a52-d647-49ec-bb81-6da2a24e8cc2", query: "must not cross the bridge" } as never);
    await tauriBridge.attachment_retention_status?.();
    await tauriBridge.attachment_retention_state?.("attachment-id");
    await tauriBridge.hosted_delete_account();
    await tauriBridge.startup_status();
    await tauriBridge.set_open_in_background(true);
    await tauriBridge.open_login_items();
    await tauriBridge.purchase_options();
    await tauriBridge.store_products();
    await tauriBridge.store_purchase("product-id");
    await tauriBridge.store_restore();
    await tauriBridge.open_store_subscriptions();
    await tauriBridge.open_play_subscriptions();
    await tauriBridge.billing_summary();
    await tauriBridge.webhooks_list();
    await tauriBridge.webhook_create({ name: null, url: "https://hooks.example.test", events: ["message.received"], filterMode: "all", filter: [], paused: false });
    await tauriBridge.webhook_update("00000000-0000-4000-8000-000000000001", { name: "Updated", url: "https://hooks.example.test", events: ["message.received"], filterMode: "all", filter: [], paused: false });
    await tauriBridge.webhook_delete("00000000-0000-4000-8000-000000000001");
    await tauriBridge.webhook_reveal_secret("00000000-0000-4000-8000-000000000001");
    await tauriBridge.webhook_rotate_secret("00000000-0000-4000-8000-000000000001");
    await tauriBridge.webhook_test("00000000-0000-4000-8000-000000000001");

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
    expect(invokeMock).toHaveBeenNthCalledWith(12, "set_conversation_folder", { conversationId: "conversation-id", folder: "archived" });
    expect(invokeMock).toHaveBeenNthCalledWith(13, "request_sender_unblock", { conversationId: "conversation-id" });
    expect(invokeMock).toHaveBeenNthCalledWith(14, "set_conversation_tone", { seed: "contact:+15555550100", paletteIndex: null });
    expect(invokeMock).toHaveBeenNthCalledWith(15, "get_history_search", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(16, "start_history_search", { input: { operationId: "d2719a52-d647-49ec-bb81-6da2a24e8cc2" } });
    expect(invokeMock).toHaveBeenNthCalledWith(17, "cancel_history_search", { input: { attemptId: "e2719a52-d647-49ec-bb81-6da2a24e8cc2" } });
    expect(invokeMock).toHaveBeenNthCalledWith(18, "attachment_retention_status", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(19, "attachment_retention_state", { attachmentId: "attachment-id" });
    expect(invokeMock).toHaveBeenNthCalledWith(20, "hosted_delete_account", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(21, "startup_status", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(22, "set_open_in_background", { enabled: true });
    expect(invokeMock).toHaveBeenNthCalledWith(23, "open_login_items", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(24, "purchase_options", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(25, "store_products", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(26, "store_purchase", { productId: "product-id" });
    expect(invokeMock).toHaveBeenNthCalledWith(27, "store_restore", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(28, "open_store_subscriptions", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(29, "open_play_subscriptions", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(30, "billing_summary", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(31, "webhooks_list", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(32, "webhook_create", { input: { name: null, url: "https://hooks.example.test", events: ["message.received"], filterMode: "all", filter: [], paused: false } });
    expect(invokeMock).toHaveBeenNthCalledWith(33, "webhook_update", { id: "00000000-0000-4000-8000-000000000001", input: { name: "Updated", url: "https://hooks.example.test", events: ["message.received"], filterMode: "all", filter: [], paused: false } });
    expect(invokeMock).toHaveBeenNthCalledWith(34, "webhook_delete", { id: "00000000-0000-4000-8000-000000000001" });
    expect(invokeMock).toHaveBeenNthCalledWith(35, "webhook_reveal_secret", { id: "00000000-0000-4000-8000-000000000001" });
    expect(invokeMock).toHaveBeenNthCalledWith(36, "webhook_rotate_secret", { id: "00000000-0000-4000-8000-000000000001" });
    expect(invokeMock).toHaveBeenNthCalledWith(37, "webhook_test", { id: "00000000-0000-4000-8000-000000000001" });
  });

  it("sends contact book lookup and name pin commands with their arguments", async () => {
    await tauriBridge.set_contact_book_lookup("book-id", false);
    await tauriBridge.list_contact_name_candidates("+12025550123", "device-id");
    await tauriBridge.set_contact_name_pin("+12025550123", "book-id", "contact-id", "device-id");
    await tauriBridge.clear_contact_name_pin("+12025550123");
    await tauriBridge.delete_contact_book("book-id");

    expect(invokeMock).toHaveBeenCalledWith("delete_contact_book", { bookId: "book-id" });
    expect(invokeMock).toHaveBeenCalledWith("set_contact_book_lookup", { bookId: "book-id", enabled: false });
    expect(invokeMock).toHaveBeenCalledWith("list_contact_name_candidates", { address: "+12025550123", sourceDeviceId: "device-id" });
    expect(invokeMock).toHaveBeenCalledWith("set_contact_name_pin", { address: "+12025550123", sourceDeviceId: "device-id", bookId: "book-id", contactId: "contact-id" });
    expect(invokeMock).toHaveBeenCalledWith("clear_contact_name_pin", { address: "+12025550123", sourceDeviceId: undefined });
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

  it("keeps fixture webhook secrets consistent across create, reveal, and rotation", async () => {
    const created = await fixtureBridge.webhook_create({
      name: "Fixture webhook",
      url: "https://hooks.example.test/fixture",
      events: ["message.received"],
      filterMode: "all",
      filter: [],
      paused: false,
    });

    expect(await fixtureBridge.webhook_reveal_secret(created.webhook.id)).toEqual({ id: created.webhook.id, secret: created.secret });
    const firstRotation = await fixtureBridge.webhook_rotate_secret(created.webhook.id);
    expect(await fixtureBridge.webhook_reveal_secret(created.webhook.id)).toEqual(firstRotation);
    const secondRotation = await fixtureBridge.webhook_rotate_secret(created.webhook.id);
    expect(secondRotation.secret).not.toBe(firstRotation.secret);
    expect(await fixtureBridge.webhook_reveal_secret(created.webhook.id)).toEqual(secondRotation);
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

  it("updates only an established fixture conversation folder and keeps its pin state", async () => {
    const before = (await fixtureBridge.load_state()).conversations.find(conversation => conversation.id === "conv-aurora");
    await fixtureBridge.set_conversation_folder("conv-aurora", "spam");

    const after = (await fixtureBridge.load_state()).conversations.find(conversation => conversation.id === "conv-aurora");
    expect(after?.folder).toBe("spam");
    expect(after?.pinned).toBe(before?.pinned);
    await expect(fixtureBridge.set_conversation_folder("missing", "archived")).rejects.toMatchObject({ code: "not-found" });
  });

  it("requests a fixture sender unblock for an established conversation", async () => {
    await fixtureBridge.request_sender_unblock("conv-aurora");
    expect((await fixtureBridge.load_state()).conversations.find(conversation => conversation.id === "conv-aurora")?.unblock).toBe("pending");
    await expect(fixtureBridge.request_sender_unblock("missing")).rejects.toMatchObject({ code: "not-found" });
  });

  it("persists fixture tone overrides and clears them with automatic", async () => {
    await fixtureBridge.set_conversation_tone("contact:+15555550100", 60);
    expect((await fixtureBridge.load_state()).toneOverrides).toEqual({ "contact:+15555550100": 60 });

    await fixtureBridge.set_conversation_tone("contact:+15555550100", null);
    expect((await fixtureBridge.load_state()).toneOverrides).toEqual({});
  });

  it("sends cursor page requests with the book and request arguments", async () => {
    const request = { cursor: "cursor-token", through: "contact-id", sort: { key: "given" as const, direction: "desc" as const } };
    await tauriBridge.list_contacts_page("book-id", request);

    expect(invokeMock).toHaveBeenCalledWith("list_contacts_page", { bookId: "book-id", request });
  });

  it("pages fixture contacts in list order and resumes from an opaque cursor", async () => {
    const listed = await fixtureBridge.list_contacts("book-pixel8", "", 0);
    const first = await fixtureBridge.list_contacts_page("book-pixel8", {});
    expect(first.contacts).toEqual(listed);
    expect(first.total).toBeGreaterThan(listed.length);
    expect(first.nextCursor).toBe(btoa("fixture-offset:200"));

    const resumed = await fixtureBridge.list_contacts_page("book-pixel8", { cursor: btoa("fixture-offset:1") });
    expect(resumed.contacts).toEqual(await fixtureBridge.list_contacts("book-pixel8", "", 1));
    expect(resumed.total).toBe(first.total);

    const tail = await fixtureBridge.list_contacts_page("book-pixel8", { cursor: btoa("fixture-offset:400") });
    expect(tail.contacts).toHaveLength(first.total - 400);
    expect(tail).not.toHaveProperty("nextCursor");

    const givenDesc = { key: "given" as const, direction: "desc" as const };
    expect((await fixtureBridge.list_contacts_page("book-pixel8", { sort: givenDesc })).contacts).toEqual(await fixtureBridge.list_contacts("book-pixel8", "", 0, givenDesc));
  });

  it("deletes only a removed phone's fixture book, for every device, and reports its contact count", async () => {
    await expect(fixtureBridge.delete_contact_book("book-pixel8")).rejects.toMatchObject({ code: "contact-book-not-deletable" });
    await expect(fixtureBridge.delete_contact_book("missing")).rejects.toMatchObject({ code: "contact-book-not-deletable" });

    const removed = (await fixtureBridge.list_contact_books()).find(book => book.sourceRevoked)!;
    await expect(fixtureBridge.delete_contact_book(removed.id)).resolves.toEqual({ bookId: removed.id, contacts: removed.contactCount });
    expect((await fixtureBridge.list_contact_books()).some(book => book.id === removed.id)).toBe(false);
  });

  it("refuses fixture deletion with the host's offline and sync-not-current codes, leaving the book in place", async () => {
    // This describe resets modules, so the gate setter must come from the same module instance as fixtureBridge.
    const { setFixtureContactDeleteGateForTests } = await import("./bridge");
    const removed = (await fixtureBridge.list_contact_books()).find(book => book.sourceRevoked)!;
    try {
      setFixtureContactDeleteGateForTests("offline");
      await expect(fixtureBridge.delete_contact_book(removed.id)).rejects.toMatchObject({ code: "offline", message: "Connect to the server to delete this book for all devices." });
      setFixtureContactDeleteGateForTests("sync-not-current");
      await expect(fixtureBridge.delete_contact_book(removed.id)).rejects.toMatchObject({ code: "sync-not-current", message: "Finish syncing to delete this book for all devices." });
    } finally {
      setFixtureContactDeleteGateForTests("ready");
    }
    expect((await fixtureBridge.list_contact_books()).some(book => book.id === removed.id)).toBe(true);
    await expect(fixtureBridge.delete_contact_book(removed.id)).resolves.toEqual({ bookId: removed.id, contacts: removed.contactCount });
  });

  it("returns fixture contacts through a requested contact, and reports whether it was found", async () => {
    const listed = await fixtureBridge.list_contacts("book-pixel8", "", 0);
    const first = await fixtureBridge.list_contacts_page("book-pixel8", {});
    expect(await fixtureBridge.list_contacts_page("book-pixel8", { through: listed[4].id })).toEqual({ contacts: listed, total: first.total, nextCursor: btoa("fixture-offset:200"), throughFound: true });
    expect(await fixtureBridge.list_contacts_page("book-pixel8", { through: "contact-missing" })).toEqual({ contacts: listed, total: first.total, nextCursor: btoa("fixture-offset:200"), throughFound: false });
    const all = [...listed, ...await fixtureBridge.list_contacts("book-pixel8", "", 200), ...await fixtureBridge.list_contacts("book-pixel8", "", 400)];
    const deep = await fixtureBridge.list_contacts_page("book-pixel8", { through: all[300].id });
    expect(deep.contacts).toEqual(all.slice(0, 301));
    expect(deep.throughFound).toBe(true);
    expect(deep.nextCursor).toBe(btoa("fixture-offset:301"));
  });

  it("fills fixture contact search hits with phones and match context when the name did not match", async () => {
    const page = await fixtureBridge.search_local({ query: "2025550124", kind: "contact" } as never);
    expect(page.items).toEqual([{ kind: "contact", bookId: "book-pixel8", contactId: "contact-river", title: "River Park", subtitle: "+12025550124", phones: ["+12025550124"], phoneCount: 1, bookLabel: "Android phone", matchField: "Phone", matchSnippet: "+12025550124" }]);
  });

  it("hides a fixture book from names until restored, and honors a pin only for a current candidate", async () => {
    await fixtureBridge.set_contact_book_lookup("book-pixel8", false);
    expect((await fixtureBridge.list_contact_books()).find(book => book.id === "book-pixel8")?.lookupExcluded).toBe(true);
    expect((await fixtureBridge.list_contact_name_candidates("+12025550123")).candidates).toEqual([]);

    await fixtureBridge.set_contact_book_lookup("book-pixel8", true);
    await fixtureBridge.set_contact_name_pin("+12025550123", "book-pixel8", "contact-aurora");
    expect(await fixtureBridge.list_contact_name_candidates("+12025550123")).toEqual({
      candidates: [{ bookId: "book-pixel8", contactId: "contact-aurora", displayName: "Aurora Chen" }],
      pinned: { bookId: "book-pixel8", contactId: "contact-aurora" },
      moreIndexing: false,
    });

    await fixtureBridge.clear_contact_name_pin("+12025550123");
    expect((await fixtureBridge.list_contact_name_candidates("+12025550123")).pinned).toBeNull();
    await expect(fixtureBridge.set_contact_name_pin("+12025550123", "book-pixel8", "contact-missing")).rejects.toMatchObject({ code: "not-found" });
    await expect(fixtureBridge.set_contact_book_lookup("missing", false)).rejects.toMatchObject({ code: "not-found" });
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

  it("rejects save and discard of a moved draft id with destination details", async () => {
    const source = await fixtureBridge.save_draft({ id: "", conversationId: "", expectedRevision: "0", text: "source", recipientIds: ["+15555550991"], attachmentIds: [] });
    const target = await fixtureBridge.save_draft({ id: "", conversationId: "", expectedRevision: "0", text: "target", recipientIds: ["+15555550992"], attachmentIds: [] });
    const moved = await fixtureBridge.save_draft({ ...source, expectedRevision: source.revision, recipientIds: target.recipientIds });
    expect(moved.id).toBe(target.id);
    expect(moved.conversationId).not.toBe(source.conversationId);
    const expected = { code: "draft-moved", details: { draftId: moved.id, conversationId: moved.conversationId, currentRevision: moved.revision } };
    const saveResult = await Promise.allSettled([fixtureBridge.save_draft({ ...source, expectedRevision: source.revision })]);
    const discardResult = await Promise.allSettled([fixtureBridge.discard_draft({ id: source.id, conversationId: source.conversationId, expectedRevision: source.revision })]);
    expect.soft(saveResult[0]).toMatchObject({ status: "rejected", reason: expected });
    expect.soft(discardResult[0]).toMatchObject({ status: "rejected", reason: expected });

    const updated = await fixtureBridge.save_draft({ ...moved, expectedRevision: moved.revision, text: "updated" });
    await expect(fixtureBridge.save_draft({ ...source, expectedRevision: source.revision })).rejects.toMatchObject({
      code: "draft-moved",
      details: { draftId: updated.id, conversationId: updated.conversationId, currentRevision: updated.revision },
    });
    const finalTarget = await fixtureBridge.save_draft({ id: "", conversationId: "", expectedRevision: "0", text: "final", recipientIds: ["+15555550993"], attachmentIds: [] });
    const movedAgain = await fixtureBridge.save_draft({ ...updated, expectedRevision: updated.revision, recipientIds: finalTarget.recipientIds });
    await expect(fixtureBridge.discard_draft({ id: source.id, conversationId: source.conversationId, expectedRevision: source.revision })).rejects.toMatchObject({
      code: "draft-moved",
      details: { draftId: movedAgain.id, conversationId: movedAgain.conversationId, currentRevision: movedAgain.revision },
    });
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

describe("diagnostics fixture and missing host", () => {
  afterEach(() => {
    resetFixtureDiagnosticsForTests();
  });

  it("seeds retained logs, grows them only while recording, and cancels an export with nothing retained", async () => {
    expect(await fixtureBridge.get_diagnostics_state!()).toMatchObject({ enabled: false, retainedBytes: 1.5 * 1024 * 1024, limitBytes: 4 * 1024 * 1024 });

    await fixtureBridge.set_diagnostics_enabled!({ enabled: true });
    const recordingBytes = (await fixtureBridge.get_diagnostics_state!()).retainedBytes;
    expect(recordingBytes).toBeGreaterThan(1.5 * 1024 * 1024);

    await fixtureBridge.set_diagnostics_enabled!({ enabled: false });
    const stoppedBytes = (await fixtureBridge.get_diagnostics_state!()).retainedBytes;
    expect(await fixtureBridge.get_diagnostics_state!()).toMatchObject({ enabled: false, retainedBytes: stoppedBytes });
    expect(stoppedBytes).toBe(recordingBytes);

    expect(await fixtureBridge.export_diagnostics_log!()).toEqual({ outcome: "saved" });
    await fixtureBridge.clear_diagnostics_log!();
    expect(await fixtureBridge.export_diagnostics_log!()).toEqual({ outcome: "cancelled" });
  });

  it("omits the diagnostics methods from the missing-host bridge", () => {
    expect(hasDiagnosticsBridge(missingHostBridge)).toBe(false);
    expect(missingHostBridge.get_diagnostics_state).toBeUndefined();
    expect(missingHostBridge.clear_diagnostics_log).toBeUndefined();
  });
});
