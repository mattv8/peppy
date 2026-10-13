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
