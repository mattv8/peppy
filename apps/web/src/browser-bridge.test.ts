import { describe, expect, it, vi } from "vitest";
import type { HistorySearchAttemptInput, StartHistorySearchInput } from "@peppy/desktop-ui/bridge-contracts";
import { BrowserBridge, parseVaultRequestSnapshot } from "./browser-bridge";

class TestPort {
  public onmessage: ((event: MessageEvent) => void) | null = null;
  public onmessageerror: (() => void) | null = null;
  public readonly sent: unknown[] = [];
  public start = vi.fn();
  public close = vi.fn();
  public postMessage(message: unknown): void { this.sent.push(message); }
  public reply(message: unknown): void { this.onmessage?.({ data: message } as MessageEvent); }
}

function stubShowModal(showModal = vi.fn()): () => void {
  const original = Object.getOwnPropertyDescriptor(HTMLDialogElement.prototype, "showModal");
  Object.defineProperty(HTMLDialogElement.prototype, "showModal", { configurable: true, value: showModal });
  return () => {
    if (original) Object.defineProperty(HTMLDialogElement.prototype, "showModal", original);
    else delete (HTMLDialogElement.prototype as { showModal?: unknown }).showModal;
  };
}

describe("BrowserBridge", () => {
  it("parses only server-shaped vault request snapshots", () => {
    expect(parseVaultRequestSnapshot(JSON.stringify({ type: "snapshot", requests: [{ request_id: "request", display_name: "Taylor", client: "browser", state: "pending", created_at: "2026-10-12T00:00:00Z", expires_in_seconds: 60 }] }))).toEqual([{ requestId: "request", displayName: "Taylor", client: "browser", state: "pending", createdAt: "2026-10-12T00:00:00Z", expiresInSeconds: 60 }]);
    expect(() => parseVaultRequestSnapshot("{}")).toThrow("Invalid vault request snapshot.");
  });

  it("captures and scrubs an invite fragment before bridge use", async () => {
    history.replaceState(null, "", "/#invite=invite-token");
    vi.resetModules();
    const module = await import("./browser-bridge");
    const bridge = new module.BrowserBridge(new TestPort() as unknown as MessagePort);
    await expect(bridge.enrollment_pending_invite()).resolves.toBe(`${location.origin}/#invite=invite-token`);
    await expect(bridge.enrollment_pending_invite()).resolves.toBeNull();
    expect(location.hash).toBe("");
    bridge.dispose();
  });

  it("canonicalizes a captured invite without retaining its tracking path", async () => {
    history.replaceState(null, "", "/join/anything?tracking=value#invite=invite-token");
    vi.resetModules();
    const module = await import("./browser-bridge");
    const bridge = new module.BrowserBridge(new TestPort() as unknown as MessagePort);
    await expect(bridge.enrollment_pending_invite()).resolves.toBe(`${location.origin}/#invite=invite-token`);
    bridge.dispose();
  });

  it("maps bridge calls to typed worker RPC and receives replies", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const state = bridge.load_state("conversation-1");
    expect(port.sent).toEqual([{ id: 1, command: "load_state", args: { conversationId: "conversation-1" } }]);
    port.reply({ id: 1, ok: true, value: { mode: "browser" } });
    await expect(state).resolves.toEqual({ mode: "browser" });
  });

  it("forwards conversation pin mutations with the canonical arguments", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const pinned = bridge.set_conversation_pinned("conversation-1", true);

    expect(port.sent).toEqual([{ id: 1, command: "set_conversation_pinned", args: { conversationId: "conversation-1", pinned: true } }]);
    port.reply({ id: 1, ok: true, value: {} });
    await expect(pinned).resolves.toEqual({});
    bridge.dispose();
  });

  it("sums frozen unread batches after each checkpointed batch acknowledgement", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const marked = bridge.mark_all_read();

    expect(port.sent).toEqual([{ id: 1, command: "unread_message_batches", args: {} }]);
    port.reply({ id: 1, ok: true, value: [["message-1"], ["message-2"]] });
    await vi.waitFor(() => expect(port.sent[1]).toEqual({ id: 2, command: "mark_read_batch", args: { messageIds: ["message-1"] } }));
    port.reply({ id: 2, ok: true, value: "1" });
    await vi.waitFor(() => expect(port.sent[2]).toEqual({ id: 3, command: "mark_read_batch", args: { messageIds: ["message-2"] } }));
    port.reply({ id: 3, ok: true, value: "2" });
    await expect(marked).resolves.toBe("3");
    bridge.dispose();
  });

  it("returns zero without a batch mutation when the frozen selection is empty", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const marked = bridge.mark_all_read();

    port.reply({ id: 1, ok: true, value: [] });
    await expect(marked).resolves.toBe("0");
    expect(port.sent).toEqual([{ id: 1, command: "unread_message_batches", args: {} }]);
    bridge.dispose();
  });

  it("stops after a failed batch without resending earlier batches", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const marked = bridge.mark_all_read();

    port.reply({ id: 1, ok: true, value: [["message-1"], ["message-2"]] });
    await vi.waitFor(() => expect(port.sent[1]).toEqual({ id: 2, command: "mark_read_batch", args: { messageIds: ["message-1"] } }));
    port.reply({ id: 2, ok: true, value: "1" });
    await vi.waitFor(() => expect(port.sent[2]).toEqual({ id: 3, command: "mark_read_batch", args: { messageIds: ["message-2"] } }));
    port.reply({ id: 3, ok: false, error: { code: "core", message: "failed" } });
    await expect(marked).rejects.toMatchObject({ code: "core" });
    expect(port.sent).toHaveLength(3);
    bridge.dispose();
  });

  it("rejects a malformed batch acknowledgement without repeating the batch", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const marked = bridge.mark_all_read();

    port.reply({ id: 1, ok: true, value: [["message-1"]] });
    await vi.waitFor(() => expect(port.sent[1]).toEqual({ id: 2, command: "mark_read_batch", args: { messageIds: ["message-1"] } }));
    port.reply({ id: 2, ok: true, value: "01" });
    await expect(marked).rejects.toMatchObject({ code: "unavailable" });
    expect(port.sent).toHaveLength(2);
    bridge.dispose();
  });

  it("forwards flat metadata and search/history bridge contracts", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const state = bridge.load_state("conversation-1", undefined, { metadataOnly: true });
    const search = bridge.search_local({ query: "hello", kind: "message", limit: 6 });
    const index = bridge.advance_search_index({ budgetRows: 100 });
    const window = bridge.load_message_window({ direction: "around", conversationId: "conversation-1", messageId: "message-1" });
    expect(port.sent).toEqual([
      { id: 1, command: "load_state", args: { conversationId: "conversation-1", metadataOnly: true } },
      { id: 2, command: "search_local", args: { query: "hello", kind: "message", limit: 6 } },
      { id: 3, command: "advance_search_index", args: { budgetRows: 100 } },
      { id: 4, command: "load_message_window", args: { direction: "around", conversationId: "conversation-1", messageId: "message-1" } },
    ]);
    port.reply({ id: 1, ok: true, value: {} });
    port.reply({ id: 2, ok: true, value: { query: "hello", items: [], indexState: "indexing", hasMore: {} } });
    port.reply({ id: 3, ok: true, value: { indexState: "indexing", processed: 100, remaining: 1 } });
    port.reply({ id: 4, ok: true, value: { conversationId: "conversation-1", messages: [], hasOlder: false, hasNewer: false } });
    await expect(state).resolves.toEqual({});
    await expect(search).resolves.toMatchObject({ query: "hello" });
    await expect(index).resolves.toMatchObject({ processed: 100 });
    await expect(window).resolves.toMatchObject({ conversationId: "conversation-1" });
    bridge.dispose();
  });

  it("forwards history search commands with only the operation or attempt identity", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const operationId = "018f2c5e-6a3b-7c4d-8e5f-0123456789ab";
    const attemptId = "018f2c5e-6a3b-7c4d-8e5f-0123456789ac";
    const snapshot = { state: "idle", canSearchFurther: false, progressVersion: "0", indexVersion: "0" };
    const read = bridge.get_history_search();
    const started = bridge.start_history_search({ operationId, query: "private search text" } as StartHistorySearchInput);
    const cancelled = bridge.cancel_history_search({ attemptId, query: "private search text" } as HistorySearchAttemptInput);
    expect(port.sent).toEqual([
      { id: 1, command: "get_history_search", args: {} },
      { id: 2, command: "start_history_search", args: { operationId } },
      { id: 3, command: "cancel_history_search", args: { attemptId } },
    ]);
    port.reply({ id: 1, ok: true, value: snapshot });
    port.reply({ id: 2, ok: true, value: snapshot });
    port.reply({ id: 3, ok: true, value: snapshot });
    await expect(read).resolves.toEqual(snapshot);
    await expect(started).resolves.toEqual(snapshot);
    await expect(cancelled).resolves.toEqual(snapshot);
    bridge.dispose();
  });

  it("routes roster and revoke requests through the active browser worker", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const roster = bridge.list_devices();
    const revoked = bridge.revoke_device("device-id");
    expect(port.sent).toEqual([
      { id: 1, command: "list_devices", args: {} },
      { id: 2, command: "revoke_device", args: { deviceId: "device-id" } },
    ]);
    port.reply({ id: 1, ok: true, value: { currentDeviceId: "device-id", devices: [] } });
    port.reply({ id: 2, ok: false, error: { code: "owner_required", message: "Denied" } });
    await expect(roster).resolves.toEqual({ currentDeviceId: "device-id", devices: [] });
    await expect(revoked).rejects.toMatchObject({ code: "owner_required" });
    bridge.dispose();
  });

  it("routes scan request and status through the active browser worker", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const request = bridge.request_contact_sync("book-id");
    const status = bridge.get_contact_sync_request("book-id");
    expect(port.sent).toEqual([
      { id: 1, command: "request_contact_sync", args: { bookId: "book-id" } },
      { id: 2, command: "get_contact_sync_request", args: { bookId: "book-id" } },
    ]);
    port.reply({ id: 1, ok: true, value: { requestId: "request", bookId: "book-id", state: "queued", expiresAt: 1 } });
    port.reply({ id: 2, ok: true, value: null });
    await expect(request).resolves.toMatchObject({ state: "queued" });
    await expect(status).resolves.toBeNull();
    bridge.dispose();
  });

  it("forwards management commands only to the browser worker", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const management = bridge.enrollment_management();
    expect(port.sent).toEqual([{ id: 1, command: "enrollment_management", args: {} }]);
    port.reply({ id: 1, ok: true, value: { vault_id: "vault", devices: [], storage_bytes: 0, quota_bytes: 1, replay_retention_days: 30 } });
    await expect(management).resolves.toMatchObject({ vault_id: "vault" });
  });

  it("does not rotate after the worker observes a claimed phone", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const rotation = bridge.enrollment_rotate_phone("shared_secret");
    port.reply({ id: 1, ok: true, value: { mode: "community", stage: "waiting_phone" } });
    await vi.waitFor(() => expect(port.sent[1]).toEqual({ id: 2, command: "enrollment_cancel", args: { waitingOnly: true } }));
    port.reply({ id: 2, ok: true, value: { mode: "community", stage: "verify_phone", verificationCode: "123 456" } });

    await expect(rotation).resolves.toMatchObject({ stage: "verify_phone" });
    expect(port.sent).toHaveLength(2);
    bridge.dispose();
  });

  it("requires saving the generated community owner secret before forwarding it", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const discover = bridge.enrollment_discover();
      expect(port.sent).toEqual([{ id: 1, command: "enrollment_discover", args: {} }]);
      port.reply({ id: 1, ok: true, value: { mode: "community", stage: "setup" } });
      await expect(discover).resolves.toMatchObject({ stage: "setup" });

      const createOwner = bridge.enrollment_create_owner();
      (document.getElementById("browser-enrollment-owner-setupToken") as HTMLInputElement).value = "setup-token";
      (document.getElementById("browser-enrollment-owner-adminPassword") as HTMLInputElement).value = "admin-password";
      (document.getElementById("browser-enrollment-owner-adminPasswordConfirm") as HTMLInputElement).value = "admin-password";
      document.getElementById("browser-enrollment-owner-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(2));
      expect(port.sent[1]).toEqual({ id: 2, command: "enrollment_generate_secret", args: {} });
      port.reply({ id: 2, ok: true, value: { passphrase: "generated-vault-secret" } });
      await vi.waitFor(() => expect(document.getElementById("browser-enrollment-owner-secret")).not.toBeNull());

      const secret = document.getElementById("browser-enrollment-owner-secret-passphrase") as HTMLInputElement;
      const acknowledgement = document.getElementById("browser-enrollment-owner-secret-acknowledged") as HTMLInputElement;
      const submit = document.getElementById("browser-enrollment-owner-secret-submit") as HTMLButtonElement;
      expect(secret.value).toBe("generated-vault-secret");
      expect(secret.readOnly).toBe(true);
      expect(submit.disabled).toBe(true);
      document.getElementById("browser-enrollment-owner-secret-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      expect(port.sent).toHaveLength(2);

      acknowledgement.checked = true;
      acknowledgement.dispatchEvent(new Event("change"));
      expect(submit.disabled).toBe(false);
      document.getElementById("browser-enrollment-owner-secret-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(3));
      expect(port.sent[2]).toEqual({ id: 3, command: "enrollment_create_owner", args: { setupToken: "setup-token", adminPassword: "admin-password", passphrase: "generated-vault-secret" } });
      expect(secret.value).toBe("");
      expect(secret.isConnected).toBe(false);
      port.reply({ id: 3, ok: true, value: { mode: "community", stage: "ready", owner: true } });
      await expect(createOwner).resolves.toMatchObject({ stage: "ready" });
      expect(document.getElementById("browser-enrollment-owner")).toBeNull();
      expect(document.getElementById("browser-enrollment-owner-secret")).toBeNull();
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("copies only the generated secret without acknowledging or continuing", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const writeText = vi.fn().mockResolvedValue(undefined);
    const originalClipboard = navigator.clipboard;
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const discover = bridge.enrollment_discover();
      port.reply({ id: 1, ok: true, value: { mode: "community", stage: "setup", setupCodeRequired: false } });
      await discover;
      const createOwner = bridge.enrollment_create_owner();
      (document.getElementById("browser-enrollment-owner-adminPassword") as HTMLInputElement).value = "admin-password";
      (document.getElementById("browser-enrollment-owner-adminPasswordConfirm") as HTMLInputElement).value = "admin-password";
      document.getElementById("browser-enrollment-owner-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(2));
      port.reply({ id: 2, ok: true, value: { passphrase: "synthetic generated secret" } });
      const copy = await vi.waitFor(() => {
        const button = document.getElementById("browser-enrollment-owner-secret-copy");
        expect(button).toBeInstanceOf(HTMLButtonElement);
        return button as HTMLButtonElement;
      });

      copy.click();

      await vi.waitFor(() => expect(writeText).toHaveBeenCalledWith("synthetic generated secret"));
      expect(copy.textContent).toBe("Copied");
      expect((document.getElementById("browser-enrollment-owner-secret-acknowledged") as HTMLInputElement).checked).toBe(false);
      expect((document.getElementById("browser-enrollment-owner-secret-submit") as HTMLButtonElement).disabled).toBe(true);
      expect(port.sent).toHaveLength(2);
      (document.getElementById("browser-enrollment-owner-secret-cancel") as HTMLButtonElement).click();
      await vi.waitFor(() => expect(port.sent).toHaveLength(3));
      port.reply({ id: 3, ok: true, value: { mode: "community", stage: "setup", setupCodeRequired: false } });
      await createOwner;
    } finally {
      Object.defineProperty(navigator, "clipboard", { configurable: true, value: originalClipboard });
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("reports missing and rejected clipboard writes and allows a retry", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const writeText = vi.fn().mockRejectedValueOnce(new Error("denied")).mockResolvedValueOnce(undefined);
    const originalClipboard = navigator.clipboard;
    const clipboard: { writeText?: typeof writeText } = {};
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: clipboard });
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const discover = bridge.enrollment_discover();
      port.reply({ id: 1, ok: true, value: { mode: "hosted", stage: "setup" } });
      await discover;
      const createOwner = bridge.enrollment_create_owner();
      await vi.waitFor(() => expect(port.sent).toHaveLength(2));
      port.reply({ id: 2, ok: true, value: { passphrase: "synthetic generated secret" } });
      const copy = await vi.waitFor(() => {
        const button = document.getElementById("browser-enrollment-owner-secret-copy");
        expect(button).toBeInstanceOf(HTMLButtonElement);
        return button as HTMLButtonElement;
      });

      copy.click();
      await vi.waitFor(() => expect(copy.textContent).toBe("Could not copy. Try again."));
      clipboard.writeText = writeText;
      copy.click();
      await vi.waitFor(() => expect(copy.textContent).toBe("Could not copy. Try again."));
      copy.click();
      await vi.waitFor(() => expect(copy.textContent).toBe("Copied"));
      expect(writeText).toHaveBeenCalledTimes(2);
      expect(port.sent).toHaveLength(2);
      (document.getElementById("browser-enrollment-owner-secret-cancel") as HTMLButtonElement).click();
      await vi.waitFor(() => expect(port.sent).toHaveLength(3));
      port.reply({ id: 3, ok: true, value: { mode: "hosted", stage: "setup" } });
      await createOwner;
    } finally {
      Object.defineProperty(navigator, "clipboard", { configurable: true, value: originalClipboard });
      bridge.dispose();
      restoreShowModal();
    }
  });

  it.each(["resolve", "reject"])("does not restore copy feedback after close when the write settles with %s", async (outcome) => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    let settleCopy: (() => void) | undefined;
    const originalClipboard = navigator.clipboard;
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: vi.fn(() => new Promise<void>((resolve, reject) => {
      settleCopy = () => outcome === "resolve" ? resolve() : reject(new Error("Clipboard denied"));
    })) } });
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const discover = bridge.enrollment_discover();
      port.reply({ id: 1, ok: true, value: { mode: "hosted", stage: "setup" } });
      await discover;
      const createOwner = bridge.enrollment_create_owner();
      await vi.waitFor(() => expect(port.sent).toHaveLength(2));
      port.reply({ id: 2, ok: true, value: { passphrase: "synthetic generated secret" } });
      const copy = await vi.waitFor(() => {
        const button = document.getElementById("browser-enrollment-owner-secret-copy");
        expect(button).toBeInstanceOf(HTMLButtonElement);
        return button as HTMLButtonElement;
      });
      const status = document.getElementById("browser-enrollment-owner-secret-copy-status")!;
      copy.click();
      (document.getElementById("browser-enrollment-owner-secret-cancel") as HTMLButtonElement).click();

      settleCopy?.();
      await Promise.resolve();
      expect(copy.textContent).toBe("Copy secret");
      expect(status.textContent).toBe("");
      expect(document.getElementById("browser-enrollment-owner-secret")).toBeNull();
      await vi.waitFor(() => expect(port.sent).toHaveLength(3));
      port.reply({ id: 3, ok: true, value: { mode: "hosted", stage: "setup" } });
      await createOwner;
    } finally {
      Object.defineProperty(navigator, "clipboard", { configurable: true, value: originalClipboard });
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("omits the browser setup-code field for an explicitly open server", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const discover = bridge.enrollment_discover();
      port.reply({ id: 1, ok: true, value: { mode: "community", stage: "setup", setupCodeRequired: false } });
      await discover;

      const createOwner = bridge.enrollment_create_owner();
      expect(document.getElementById("browser-enrollment-owner-setupToken")).toBeNull();
      expect(document.getElementById("browser-enrollment-owner-form")?.getAttribute("data-setup-mode")).toBe("open");
      (document.getElementById("browser-enrollment-owner-adminPassword") as HTMLInputElement).value = "admin-password";
      (document.getElementById("browser-enrollment-owner-adminPasswordConfirm") as HTMLInputElement).value = "admin-password";
      (document.getElementById("browser-enrollment-owner-cancel") as HTMLButtonElement).click();
      await vi.waitFor(() => expect(port.sent).toHaveLength(2));
      port.reply({ id: 2, ok: true, value: { mode: "community", stage: "setup", setupCodeRequired: false } });
      await createOwner;
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("cancels owner setup and generated-secret acknowledgement without creating an owner", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const discover = bridge.enrollment_discover();
      port.reply({ id: 1, ok: true, value: { mode: "community", stage: "setup" } });
      await discover;

      const cancelSetup = bridge.enrollment_create_owner();
      (document.getElementById("browser-enrollment-owner-cancel") as HTMLButtonElement).click();
      await vi.waitFor(() => expect(port.sent).toHaveLength(2));
      expect(port.sent[1]).toEqual({ id: 2, command: "enrollment_status", args: {} });
      port.reply({ id: 2, ok: true, value: { mode: "community", stage: "setup" } });
      await expect(cancelSetup).resolves.toMatchObject({ stage: "setup" });

      const cancelSecret = bridge.enrollment_create_owner();
      (document.getElementById("browser-enrollment-owner-setupToken") as HTMLInputElement).value = "setup-token";
      (document.getElementById("browser-enrollment-owner-adminPassword") as HTMLInputElement).value = "admin-password";
      (document.getElementById("browser-enrollment-owner-adminPasswordConfirm") as HTMLInputElement).value = "admin-password";
      document.getElementById("browser-enrollment-owner-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(3));
      port.reply({ id: 3, ok: true, value: { passphrase: "generated-vault-secret" } });
      await vi.waitFor(() => expect(document.getElementById("browser-enrollment-owner-secret")).not.toBeNull());
      (document.getElementById("browser-enrollment-owner-secret-cancel") as HTMLButtonElement).click();
      await vi.waitFor(() => expect(port.sent).toHaveLength(4));
      expect(port.sent[3]).toEqual({ id: 4, command: "enrollment_status", args: {} });
      port.reply({ id: 4, ok: true, value: { mode: "community", stage: "setup" } });
      await expect(cancelSecret).resolves.toMatchObject({ stage: "setup" });
      expect(port.sent).not.toContainEqual(expect.objectContaining({ command: "enrollment_create_owner" }));
      expect(document.getElementById("browser-enrollment-owner-secret")).toBeNull();
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("creates a hosted owner only after acknowledging its generated secret", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const discover = bridge.enrollment_discover();
      port.reply({ id: 1, ok: true, value: { mode: "hosted", stage: "setup" } });
      await discover;

      const createOwner = bridge.enrollment_create_owner();
      expect(document.getElementById("browser-enrollment-owner")).toBeNull();
      await vi.waitFor(() => expect(port.sent).toHaveLength(2));
      expect(port.sent[1]).toEqual({ id: 2, command: "enrollment_generate_secret", args: {} });
      port.reply({ id: 2, ok: true, value: { passphrase: "hosted-vault-secret" } });
      await vi.waitFor(() => expect(document.getElementById("browser-enrollment-owner-secret")).not.toBeNull());
      const acknowledgement = document.getElementById("browser-enrollment-owner-secret-acknowledged") as HTMLInputElement;
      acknowledgement.checked = true;
      acknowledgement.dispatchEvent(new Event("change"));
      document.getElementById("browser-enrollment-owner-secret-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(3));
      expect(port.sent[2]).toEqual({ id: 3, command: "enrollment_create_owner", args: { passphrase: "hosted-vault-secret" } });
      port.reply({ id: 3, ok: true, value: { mode: "hosted", stage: "ready", owner: true } });
      await expect(createOwner).resolves.toMatchObject({ stage: "ready" });
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("retries a failed owner claim with the same private draft", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const discover = bridge.enrollment_discover();
      port.reply({ id: 1, ok: true, value: { mode: "community", stage: "setup" } });
      await discover;

      const firstAttempt = bridge.enrollment_create_owner();
      (document.getElementById("browser-enrollment-owner-setupToken") as HTMLInputElement).value = "setup-token";
      (document.getElementById("browser-enrollment-owner-adminPassword") as HTMLInputElement).value = "admin-password";
      (document.getElementById("browser-enrollment-owner-adminPasswordConfirm") as HTMLInputElement).value = "admin-password";
      document.getElementById("browser-enrollment-owner-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(2));
      port.reply({ id: 2, ok: true, value: { passphrase: "generated-vault-secret" } });
      await vi.waitFor(() => expect(document.getElementById("browser-enrollment-owner-secret")).not.toBeNull());
      const acknowledgement = document.getElementById("browser-enrollment-owner-secret-acknowledged") as HTMLInputElement;
      acknowledgement.checked = true;
      acknowledgement.dispatchEvent(new Event("change"));
      document.getElementById("browser-enrollment-owner-secret-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(3));
      const firstRequest = port.sent[2];
      port.reply({ id: 3, ok: true, value: { mode: "community", stage: "error", errorCode: "network-failed" } });
      await expect(firstAttempt).resolves.toMatchObject({ stage: "error" });

      const retry = bridge.enrollment_create_owner();
      expect(port.sent).toHaveLength(4);
      expect(firstRequest).toEqual({ id: 3, command: "enrollment_create_owner", args: { setupToken: "setup-token", adminPassword: "admin-password", passphrase: "generated-vault-secret" } });
      expect(port.sent[3]).toEqual({ id: 4, command: "enrollment_create_owner", args: { setupToken: "setup-token", adminPassword: "admin-password", passphrase: "generated-vault-secret" } });
      port.reply({ id: 4, ok: true, value: { mode: "community", stage: "ready", owner: true } });
      await expect(retry).resolves.toMatchObject({ stage: "ready" });
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("asks only for a corrected setup code after an invalid setup-token response", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const discover = bridge.enrollment_discover();
      port.reply({ id: 1, ok: true, value: { mode: "community", stage: "setup", setupCodeRequired: true } });
      await discover;

      const firstAttempt = bridge.enrollment_create_owner();
      (document.getElementById("browser-enrollment-owner-setupToken") as HTMLInputElement).value = "wrong-code";
      (document.getElementById("browser-enrollment-owner-adminPassword") as HTMLInputElement).value = "admin-password";
      (document.getElementById("browser-enrollment-owner-adminPasswordConfirm") as HTMLInputElement).value = "admin-password";
      document.getElementById("browser-enrollment-owner-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(2));
      port.reply({ id: 2, ok: true, value: { passphrase: "generated-vault-secret" } });
      await vi.waitFor(() => expect(document.getElementById("browser-enrollment-owner-secret")).not.toBeNull());
      const acknowledgement = document.getElementById("browser-enrollment-owner-secret-acknowledged") as HTMLInputElement;
      acknowledgement.checked = true;
      acknowledgement.dispatchEvent(new Event("change"));
      document.getElementById("browser-enrollment-owner-secret-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(3));
      port.reply({ id: 3, ok: true, value: { mode: "community", stage: "error", errorCode: "invalid_setup_token" } });
      await expect(firstAttempt).resolves.toMatchObject({ stage: "error" });

      const retry = bridge.enrollment_create_owner();
      expect(document.getElementById("browser-enrollment-owner-code")).not.toBeNull();
      expect(document.getElementById("browser-enrollment-owner-code-adminPassword")).toBeNull();
      (document.getElementById("browser-enrollment-owner-code-setupToken") as HTMLInputElement).value = "correct-code";
      document.getElementById("browser-enrollment-owner-code-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(4));
      expect(port.sent[3]).toEqual({ id: 4, command: "enrollment_create_owner", args: { setupToken: "correct-code", adminPassword: "admin-password", passphrase: "generated-vault-secret" } });
      port.reply({ id: 4, ok: true, value: { mode: "community", stage: "ready", owner: true } });
      await expect(retry).resolves.toMatchObject({ stage: "ready" });
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("keeps the owner draft when corrected setup-code entry is cancelled", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const discover = bridge.enrollment_discover();
      port.reply({ id: 1, ok: true, value: { mode: "community", stage: "setup", setupCodeRequired: true } });
      await discover;

      const firstAttempt = bridge.enrollment_create_owner();
      (document.getElementById("browser-enrollment-owner-setupToken") as HTMLInputElement).value = "wrong-code";
      (document.getElementById("browser-enrollment-owner-adminPassword") as HTMLInputElement).value = "admin-password";
      (document.getElementById("browser-enrollment-owner-adminPasswordConfirm") as HTMLInputElement).value = "admin-password";
      document.getElementById("browser-enrollment-owner-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(2));
      port.reply({ id: 2, ok: true, value: { passphrase: "generated-vault-secret" } });
      await vi.waitFor(() => expect(document.getElementById("browser-enrollment-owner-secret")).not.toBeNull());
      const acknowledgement = document.getElementById("browser-enrollment-owner-secret-acknowledged") as HTMLInputElement;
      acknowledgement.checked = true;
      acknowledgement.dispatchEvent(new Event("change"));
      document.getElementById("browser-enrollment-owner-secret-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(3));
      port.reply({ id: 3, ok: true, value: { mode: "community", stage: "error", errorCode: "invalid_setup_token" } });
      await firstAttempt;

      const cancelledCorrection = bridge.enrollment_create_owner();
      (document.getElementById("browser-enrollment-owner-code-cancel") as HTMLButtonElement).click();
      await vi.waitFor(() => expect(port.sent).toHaveLength(4));
      expect(port.sent[3]).toEqual({ id: 4, command: "enrollment_status", args: {} });
      port.reply({ id: 4, ok: true, value: { mode: "community", stage: "setup", setupCodeRequired: true } });
      await cancelledCorrection;

      const secondCorrection = bridge.enrollment_create_owner();
      expect(document.getElementById("browser-enrollment-owner-code")).not.toBeNull();
      expect(document.getElementById("browser-enrollment-owner-code-adminPassword")).toBeNull();
      (document.getElementById("browser-enrollment-owner-code-cancel") as HTMLButtonElement).click();
      await vi.waitFor(() => expect(port.sent).toHaveLength(5));
      port.reply({ id: 5, ok: true, value: { mode: "community", stage: "setup", setupCodeRequired: true } });
      await secondCorrection;
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("re-collects mismatched owner passwords without generating a secret and preserves the setup code", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const discover = bridge.enrollment_discover();
      port.reply({ id: 1, ok: true, value: { mode: "community", stage: "setup", setupCodeRequired: true } });
      await discover;

      const createOwner = bridge.enrollment_create_owner();
      (document.getElementById("browser-enrollment-owner-setupToken") as HTMLInputElement).value = "setup-token";
      (document.getElementById("browser-enrollment-owner-adminPassword") as HTMLInputElement).value = "matching-password";
      (document.getElementById("browser-enrollment-owner-adminPasswordConfirm") as HTMLInputElement).value = "different-password";
      document.getElementById("browser-enrollment-owner-form")!.dispatchEvent(new Event("submit", { cancelable: true }));

      await vi.waitFor(() => expect(document.querySelector("#browser-enrollment-owner [role=alert]")?.textContent).toBeTruthy());
      expect(port.sent).toHaveLength(1);
      expect((document.getElementById("browser-enrollment-owner-setupToken") as HTMLInputElement).value).toBe("setup-token");
      expect((document.getElementById("browser-enrollment-owner-adminPassword") as HTMLInputElement).value).toBe("");
      expect((document.getElementById("browser-enrollment-owner-adminPasswordConfirm") as HTMLInputElement).value).toBe("");

      (document.getElementById("browser-enrollment-owner-cancel") as HTMLButtonElement).click();
      await vi.waitFor(() => expect(port.sent).toHaveLength(2));
      expect(port.sent[1]).toEqual({ id: 2, command: "enrollment_status", args: {} });
      port.reply({ id: 2, ok: true, value: { mode: "community", stage: "setup" } });
      await createOwner;
      expect(port.sent).not.toContainEqual(expect.objectContaining({ command: "enrollment_create_owner" }));
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("rejects invalid owner-password byte lengths before generating a secret", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const discover = bridge.enrollment_discover();
      port.reply({ id: 1, ok: true, value: { mode: "community", stage: "setup", setupCodeRequired: false } });
      await discover;

      const createOwner = bridge.enrollment_create_owner();
      (document.getElementById("browser-enrollment-owner-adminPassword") as HTMLInputElement).value = "short-pass";
      (document.getElementById("browser-enrollment-owner-adminPasswordConfirm") as HTMLInputElement).value = "short-pass";
      document.getElementById("browser-enrollment-owner-form")!.dispatchEvent(new Event("submit", { cancelable: true }));

      await vi.waitFor(() => expect(document.querySelector("#browser-enrollment-owner [role=alert]")?.textContent).toBeTruthy());
      expect(port.sent).toHaveLength(1);
      (document.getElementById("browser-enrollment-owner-cancel") as HTMLButtonElement).click();
      await vi.waitFor(() => expect(port.sent).toHaveLength(2));
      port.reply({ id: 2, ok: true, value: { mode: "community", stage: "setup", setupCodeRequired: false } });
      await createOwner;
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("confirms corrected vault passwords before retrying an invalid password", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const firstAttempt = bridge.enrollment_create_vault({ username: "taylor" });
      (document.getElementById("browser-enrollment-vault-adminPassword") as HTMLInputElement).value = "original-password";
      (document.getElementById("browser-enrollment-vault-adminPasswordConfirm") as HTMLInputElement).value = "original-password";
      document.getElementById("browser-enrollment-vault-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(1));
      expect(port.sent[0]).toEqual({ id: 1, command: "enrollment_generate_secret", args: {} });
      port.reply({ id: 1, ok: true, value: { passphrase: "generated-vault-secret" } });
      await vi.waitFor(() => expect(document.getElementById("browser-enrollment-owner-secret")).not.toBeNull());
      const acknowledgement = document.getElementById("browser-enrollment-owner-secret-acknowledged") as HTMLInputElement;
      acknowledgement.checked = true;
      acknowledgement.dispatchEvent(new Event("change"));
      document.getElementById("browser-enrollment-owner-secret-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(2));
      port.reply({ id: 2, ok: true, value: { mode: "community", stage: "error", errorCode: "invalid_admin_password" } });
      await expect(firstAttempt).resolves.toMatchObject({ stage: "error" });

      const cancelledCorrection = bridge.enrollment_create_vault({ username: "taylor" });
      (document.getElementById("browser-enrollment-vault-password-cancel") as HTMLButtonElement).click();
      await vi.waitFor(() => expect(port.sent).toHaveLength(3));
      expect(port.sent[2]).toEqual({ id: 3, command: "enrollment_status", args: {} });
      port.reply({ id: 3, ok: true, value: { mode: "community", stage: "invite_approved" } });
      await expect(cancelledCorrection).resolves.toMatchObject({ stage: "invite_approved" });

      const retry = bridge.enrollment_create_vault({ username: "taylor" });
      (document.getElementById("browser-enrollment-vault-password-adminPassword") as HTMLInputElement).value = "corrected-password";
      (document.getElementById("browser-enrollment-vault-password-adminPasswordConfirm") as HTMLInputElement).value = "different-password";
      document.getElementById("browser-enrollment-vault-password-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(document.querySelector("#browser-enrollment-vault-password [role=alert]")?.textContent).toBeTruthy());
      expect(port.sent).toHaveLength(3);

      (document.getElementById("browser-enrollment-vault-password-adminPassword") as HTMLInputElement).value = "corrected-password";
      (document.getElementById("browser-enrollment-vault-password-adminPasswordConfirm") as HTMLInputElement).value = "corrected-password";
      document.getElementById("browser-enrollment-vault-password-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(4));
      expect(port.sent[3]).toEqual({ id: 4, command: "enrollment_create_vault", args: { username: "taylor", adminPassword: "corrected-password", passphrase: "generated-vault-secret" } });
      port.reply({ id: 4, ok: true, value: { mode: "community", stage: "ready" } });
      await expect(retry).resolves.toMatchObject({ stage: "ready" });
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("keeps browser sign-in as a single password entry", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const signIn = bridge.enrollment_sign_in();
      expect(document.getElementById("browser-enrollment-signin-adminPasswordConfirm")).toBeNull();
      (document.getElementById("browser-enrollment-signin-adminPassword") as HTMLInputElement).value = "existing-password";
      document.getElementById("browser-enrollment-signin-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(1));
      expect(port.sent[0]).toEqual({ id: 1, command: "enrollment_sign_in", args: { adminPassword: "existing-password" } });
      port.reply({ id: 1, ok: true, value: { mode: "community", stage: "ready" } });
      await expect(signIn).resolves.toMatchObject({ stage: "ready" });
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("surfaces host denials instead of pretending native features succeeded", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const result = bridge.save_draft({ id: "draft", conversationId: "conversation", text: "hello", recipientIds: [], attachmentIds: [], expectedRevision: "1" });
    port.reply({ id: 1, ok: false, error: { code: "invalid-request", message: "Denied" } });
    await expect(result).rejects.toMatchObject({ code: "invalid-request", message: "Denied" });
    await expect(bridge.open_composer()).rejects.toMatchObject({ code: "unsupported" });
    await expect(bridge.get_app_lock_status()).rejects.toMatchObject({ code: "unsupported" });
  });

  it("round-trips discard requests and moved-draft details", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const discarded = bridge.discard_draft({ id: "draft", conversationId: "conversation", expectedRevision: "1" });
    expect(port.sent).toEqual([{ id: 1, command: "discard_draft", args: { input: { id: "draft", conversationId: "conversation", expectedRevision: "1" } } }]);
    port.reply({ id: 1, ok: false, error: { code: "draft-moved", message: "Moved", conversationId: "conversation-2", currentRevision: "2", draftId: "draft-2" } });
    await expect(discarded).rejects.toMatchObject({ code: "draft-moved", conversationId: "conversation-2", currentRevision: "2", draftId: "draft-2" });
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

  it("uses a fresh timeout for each read batch and stops at a timed-out later batch", async () => {
    vi.useFakeTimers();
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort, 100);
    const marked = bridge.mark_all_read();

    port.reply({ id: 1, ok: true, value: [["message-1"], ["message-2"]] });
    await vi.advanceTimersByTimeAsync(0);
    expect(port.sent[1]).toMatchObject({ command: "mark_read_batch" });
    await vi.advanceTimersByTimeAsync(99);
    port.reply({ id: 2, ok: true, value: "1" });
    await vi.advanceTimersByTimeAsync(0);
    expect(port.sent[2]).toMatchObject({ command: "mark_read_batch", args: { messageIds: ["message-2"] } });
    const rejection = expect(marked).rejects.toMatchObject({ code: "timeout" });
    await vi.advanceTimersByTimeAsync(100);

    await rejection;
    expect(port.sent).toHaveLength(3);
    bridge.dispose();
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

  it("releases only the exact media URL so stale cleanup cannot revoke a newer open", async () => {
    const port = new TestPort();
    const originalCreate = URL.createObjectURL;
    const originalRevoke = URL.revokeObjectURL;
    const create = vi.fn().mockReturnValueOnce("blob:old").mockReturnValueOnce("blob:new");
    const revoke = vi.fn();
    Object.assign(URL, { createObjectURL: create, revokeObjectURL: revoke });
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const oldOpen = bridge.open_media("attachment");
    port.reply({ id: 1, ok: true, value: { state: "ready", bytes: new ArrayBuffer(1), mediaType: "video/mp4", byteSize: 1 } });
    await expect(oldOpen).resolves.toMatchObject({ url: "blob:old" });
    const newOpen = bridge.open_media("attachment");
    port.reply({ id: 2, ok: true, value: { state: "ready", bytes: new ArrayBuffer(1), mediaType: "video/mp4", byteSize: 1 } });
    await expect(newOpen).resolves.toMatchObject({ url: "blob:new" });
    const close = bridge.close_media("attachment", "blob:old");
    expect(revoke).toHaveBeenCalledWith("blob:old");
    expect(revoke).not.toHaveBeenCalledWith("blob:new");
    expect(port.sent.at(-1)).toMatchObject({ command: "close_media", args: { id: "attachment", url: "blob:old" } });
    port.reply({ id: 3, ok: true, value: undefined });
    await expect(close).resolves.toBeUndefined();
    await expect(bridge.close_media("attachment", "blob:old")).resolves.toBeUndefined();
    const lastClose = bridge.close_media("attachment", "blob:new");
    expect(port.sent.at(-1)).toMatchObject({ command: "close_media", args: { id: "attachment", url: "blob:new" } });
    port.reply({ id: 4, ok: true, value: undefined });
    await expect(lastClose).resolves.toBeUndefined();
    Object.assign(URL, { createObjectURL: originalCreate, revokeObjectURL: originalRevoke });
  });

  it("approves a phone without a shared-secret dialog or passphrase payload", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const approval = bridge.enrollment_approve();
      expect(document.getElementById("browser-enrollment-approve")).toBeNull();
      expect(port.sent).toEqual([{ id: 1, command: "enrollment_approve", args: {} }]);
      port.reply({ id: 1, ok: true, value: { mode: "community", stage: "phone_connected" } });
      await expect(approval).resolves.toMatchObject({ stage: "phone_connected" });
      expect(document.getElementById("browser-enrollment-approve")).toBeNull();
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("submits the browser unlock dialog once with its passphrase", async () => {
    const port = new TestPort();
    const showModal = vi.fn();
    const restoreShowModal = stubShowModal(showModal);
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const unlock = bridge.unlock_sync();
      const input = document.getElementById("browser-unlock-passphrase") as HTMLInputElement;
      input.value = "synthetic-passphrase";
      document.getElementById("browser-unlock-form")!.dispatchEvent(new Event("submit", { cancelable: true }));

      expect(showModal).toHaveBeenCalledOnce();
      await vi.waitFor(() => expect(port.sent).toHaveLength(1));
      expect(port.sent).toEqual([{ id: 1, command: "unlock", args: { passphrase: "synthetic-passphrase" } }]);
      port.reply({ id: 1, ok: true, value: undefined });
      await expect(unlock).resolves.toBeUndefined();
      expect(input.value).toBe("");
      expect(input.isConnected).toBe(false);
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("cancels the browser unlock dialog without sending an RPC", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const unlock = bridge.unlock_sync();
      const input = document.getElementById("browser-unlock-passphrase") as HTMLInputElement;
      input.value = "synthetic-passphrase";
      (document.getElementById("browser-unlock-cancel") as HTMLButtonElement).click();

      await expect(unlock).resolves.toBeUndefined();
      expect(port.sent).toEqual([]);
      expect(input.value).toBe("");
      expect(input.isConnected).toBe(false);
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("propagates a rejected browser unlock RPC", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const unlock = bridge.unlock_sync();
      const input = document.getElementById("browser-unlock-passphrase") as HTMLInputElement;
      input.value = "synthetic-passphrase";
      document.getElementById("browser-unlock-form")!.dispatchEvent(new Event("submit", { cancelable: true }));
      await vi.waitFor(() => expect(port.sent).toHaveLength(1));
      port.reply({ id: 1, ok: false, error: { code: "invalid-passphrase", message: "Rejected" } });

      await expect(unlock).rejects.toMatchObject({ code: "invalid-passphrase", message: "Rejected" });
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
  });

  it("cancels the browser unlock dialog on Escape without sending an RPC", async () => {
    const port = new TestPort();
    const restoreShowModal = stubShowModal();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    try {
      const unlock = bridge.unlock_sync();
      const dialog = document.getElementById("browser-unlock") as HTMLDialogElement;
      const input = document.getElementById("browser-unlock-passphrase") as HTMLInputElement;
      input.value = "synthetic-passphrase";
      dialog.dispatchEvent(new Event("cancel", { cancelable: true }));

      await expect(unlock).resolves.toBeUndefined();
      expect(port.sent).toEqual([]);
      expect(input.value).toBe("");
      expect(input.isConnected).toBe(false);
    } finally {
      bridge.dispose();
      restoreShowModal();
    }
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

  it("sends a content-free browser test notification only when granted", async () => {
    const original = globalThis.Notification;
    const posted = vi.fn();
    class TestNotification {
      public static permission: NotificationPermission = "granted";
      public static failToConstruct = false;
      public static async requestPermission(): Promise<NotificationPermission> { return TestNotification.permission; }
      public constructor(title: string, options: NotificationOptions) {
        if (TestNotification.failToConstruct) throw new Error("blocked");
        posted(title, options);
      }
    }
    Object.defineProperty(globalThis, "Notification", { configurable: true, value: TestNotification });
    try {
      const bridge = new BrowserBridge(new TestPort() as unknown as MessagePort);
      await expect(bridge.request_notification_permission()).resolves.toEqual({ status: "granted", testSent: true, canOpenSettings: false });
      expect(posted).toHaveBeenCalledWith("Peppy", { body: "Test notification: desktop banners are working." });

      TestNotification.permission = "denied";
      await expect(bridge.request_notification_permission()).resolves.toEqual({ status: "denied", testSent: false, canOpenSettings: false });
      expect(posted).toHaveBeenCalledTimes(1);

      TestNotification.permission = "granted";
      TestNotification.failToConstruct = true;
      await expect(bridge.request_notification_permission()).resolves.toEqual({ status: "granted", testSent: false, canOpenSettings: false });
      expect(posted).toHaveBeenCalledTimes(1);
      bridge.dispose();
    } finally {
      Object.defineProperty(globalThis, "Notification", { configurable: true, value: original });
    }
  });
});
