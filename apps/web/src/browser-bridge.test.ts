import { describe, expect, it, vi } from "vitest";
import { BrowserBridge } from "./browser-bridge";

class TestPort {
  public onmessage: ((event: MessageEvent) => void) | null = null;
  public onmessageerror: (() => void) | null = null;
  public readonly sent: unknown[] = [];
  public start = vi.fn();
  public close = vi.fn();
  public postMessage(message: unknown): void { this.sent.push(message); }
  public reply(message: unknown): void { this.onmessage?.({ data: message } as MessageEvent); }
}

describe("BrowserBridge", () => {
  it("maps bridge calls to typed worker RPC and receives replies", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const state = bridge.load_state("conversation-1");
    expect(port.sent).toEqual([{ id: 1, command: "load_state", args: { conversationId: "conversation-1" } }]);
    port.reply({ id: 1, ok: true, value: { mode: "browser" } });
    await expect(state).resolves.toEqual({ mode: "browser" });
  });

  it("surfaces host denials instead of pretending native features succeeded", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const result = bridge.save_draft({ id: "draft", conversationId: "conversation", text: "hello", recipientIds: [], attachmentIds: [], expectedRevision: "1" });
    port.reply({ id: 1, ok: false, error: { code: "invalid-request", message: "Denied" } });
    await expect(result).rejects.toMatchObject({ code: "invalid-request", message: "Denied" });
    await expect(bridge.open_composer()).rejects.toMatchObject({ code: "unsupported" });
  });

  it("opens only a validated billing destination in a new tab", async () => {
    const bridge = new BrowserBridge(new TestPort() as unknown as MessagePort);
    const open = vi.spyOn(window, "open").mockReturnValue(null);

    bridge.setAccountUrl("https://account.example.com/account");
    await bridge.hosted_open_billing();
    expect(open).toHaveBeenCalledWith("https://account.example.com/account", "_blank", "noopener,noreferrer");

    bridge.setAccountUrl(`https://${location.hostname}/account`);
    await expect(bridge.hosted_open_billing()).rejects.toMatchObject({ code: "unsupported" });
    expect(open).toHaveBeenCalledOnce();
  });

  it("notifies all subscriptions when the shared worker changes", () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const listener = vi.fn();
    const unsubscribe = bridge.subscribe(listener);
    port.reply({ event: "changed" });
    unsubscribe();
    port.reply({ event: "changed" });
    expect(listener).toHaveBeenCalledOnce();
  });

  it("holds startup behind the ready handshake and reports it once", () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const ready = vi.fn();
    bridge.onReady(ready);
    port.reply({ event: "ready", version: 1 });
    port.reply({ event: "ready", version: 1 });
    expect(ready).toHaveBeenCalledTimes(2);
  });

  it("rejects requests when a worker response times out", async () => {
    vi.useFakeTimers();
    const bridge = new BrowserBridge(new TestPort() as unknown as MessagePort, 1);
    const request = bridge.load_state();
    const rejection = expect(request).rejects.toMatchObject({ code: "timeout" });
    await vi.advanceTimersByTimeAsync(1);
    await rejection;
    vi.useRealTimers();
  });

  it("clears pending work and notifies the UI when the worker stops", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const stopped = vi.fn();
    bridge.onStopped(stopped);
    const request = bridge.load_state();
    const rejection = expect(request).rejects.toMatchObject({ code: "locked" });
    port.reply({ event: "stopped", reason: "locked" });
    await rejection;
    expect(stopped).toHaveBeenCalledWith("locked");
    expect(port.close).toHaveBeenCalledOnce();
  });

  it("settles startup errors that have no request id", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const request = bridge.load_state();
    const rejection = expect(request).rejects.toMatchObject({ code: "already-open" });
    port.reply({ id: -1, ok: false, error: { code: "already-open", message: "Unavailable" } });
    await rejection;
  });

  it("treats a cancelled attachment picker as an empty selection", async () => {
    const click = vi.spyOn(HTMLInputElement.prototype, "click").mockImplementation(function (this: HTMLInputElement) {
      this.dispatchEvent(new Event("cancel"));
    });
    const bridge = new BrowserBridge(new TestPort() as unknown as MessagePort);
    await expect(bridge.pick_attachments()).resolves.toEqual([]);
    click.mockRestore();
  });

  it("uses the browser attachment command and never publishes without confirmation", async () => {
    const port = new TestPort();
    const click = vi.spyOn(HTMLInputElement.prototype, "click").mockImplementation(function (this: HTMLInputElement) {
      Object.defineProperty(this, "files", { configurable: true, value: [{ name: "photo.jpg", type: "image/jpeg", size: 1, arrayBuffer: async () => new ArrayBuffer(1) }] });
      this.dispatchEvent(new Event("change"));
    });
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const prepared = bridge.pick_attachments();
    await vi.waitFor(() => expect(port.sent[0]).toMatchObject({ command: "prepare_attachment", args: { displayName: "photo.jpg", mediaType: "image/jpeg" } }));
    port.reply({ id: 1, ok: true, value: { id: "attachment", name: "photo.jpg", mediaType: "image/jpeg", byteSize: 1, state: "ready" } });
    await expect(prepared).resolves.toHaveLength(1);
    vi.spyOn(window, "confirm").mockReturnValue(false);
    await expect(bridge.publish_attachment("attachment")).resolves.toBeNull();
    expect(port.sent).toHaveLength(1);
    click.mockRestore();
  });

  it("rejects an oversized attachment before reading or sending it", async () => {
    const port = new TestPort();
    const click = vi.spyOn(HTMLInputElement.prototype, "click").mockImplementation(function (this: HTMLInputElement) {
      Object.defineProperty(this, "files", { configurable: true, value: [{ name: "large.bin", type: "application/octet-stream", size: 32 * 1024 * 1024 + 1, arrayBuffer: vi.fn() }] });
      this.dispatchEvent(new Event("change"));
    });
    await expect(new BrowserBridge(port as unknown as MessagePort).pick_attachments()).rejects.toMatchObject({ code: "attachment-too-large" });
    expect(port.sent).toEqual([]);
    click.mockRestore();
  });

  it("revokes downloaded attachment URLs when the shared session locks", async () => {
    vi.useFakeTimers();
    const port = new TestPort();
    const originalCreate = URL.createObjectURL;
    const originalRevoke = URL.revokeObjectURL;
    const create = vi.fn(() => "blob:attachment");
    const revoke = vi.fn();
    Object.assign(URL, { createObjectURL: create, revokeObjectURL: revoke });
    const click = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => undefined);
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const saved = bridge.save_attachment("attachment");
    port.reply({ id: 1, ok: true, value: { bytes: new ArrayBuffer(1), displayName: "file.txt", mediaType: "text/plain" } });
    await expect(saved).resolves.toBe(true);
    port.reply({ event: "stopped", reason: "locked" });
    expect(revoke).toHaveBeenCalledWith("blob:attachment");
    await vi.runAllTimersAsync();
    Object.assign(URL, { createObjectURL: originalCreate, revokeObjectURL: originalRevoke });
    click.mockRestore();
    vi.useRealTimers();
  });

  it("posts one granted browser banner and acknowledges only after the post succeeds", () => {
    const port = new TestPort();
    const original = globalThis.Notification;
    const posted = vi.fn();
    class TestNotification {
      public static permission: NotificationPermission = "granted";
      public onclose: (() => void) | null = null;
      public constructor(title: string, options: NotificationOptions) { posted(title, options); }
      public close(): void { this.onclose?.(); }
    }
    Object.defineProperty(globalThis, "Notification", { configurable: true, value: TestNotification });
    new BrowserBridge(port as unknown as MessagePort);
    port.reply({ event: "banner", candidate: { id: "candidate-1", title: "Private", body: "Body" } });
    expect(posted).toHaveBeenCalledWith("Private", { body: "Body", tag: "peppy-candidate-1" });
    expect(port.sent).toEqual([{ id: 1, command: "display_ack", args: { ids: ["candidate-1"] } }]);
    Object.defineProperty(globalThis, "Notification", { configurable: true, value: original });
  });

  it("does not acknowledge denied or failed banner posts and closes live banners on stop", () => {
    const port = new TestPort();
    const original = globalThis.Notification;
    const close = vi.fn();
    class TestNotification {
      public static permission: NotificationPermission = "granted";
      public onclose: (() => void) | null = null;
      public constructor() {}
      public close(): void { close(); this.onclose?.(); }
    }
    Object.defineProperty(globalThis, "Notification", { configurable: true, value: TestNotification });
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    port.reply({ event: "banner", candidate: { id: "candidate-1", title: "Private", body: "Body" } });
    port.reply({ event: "stopped", reason: "locked" });
    expect(close).toHaveBeenCalledOnce();
    TestNotification.permission = "denied";
    const denied = new TestPort();
    new BrowserBridge(denied as unknown as MessagePort);
    denied.reply({ event: "banner", candidate: { id: "candidate-2", title: "Private", body: "Body" } });
    expect(denied.sent).toEqual([]);
    Object.defineProperty(globalThis, "Notification", { configurable: true, value: original });
    bridge.dispose();
  });
});
