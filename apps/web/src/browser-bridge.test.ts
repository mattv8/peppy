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

function stubShowModal(showModal = vi.fn()): () => void {
  const original = Object.getOwnPropertyDescriptor(HTMLDialogElement.prototype, "showModal");
  Object.defineProperty(HTMLDialogElement.prototype, "showModal", { configurable: true, value: showModal });
  return () => {
    if (original) Object.defineProperty(HTMLDialogElement.prototype, "showModal", original);
    else delete (HTMLDialogElement.prototype as { showModal?: unknown }).showModal;
  };
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

  it("forwards management commands only to the browser worker", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const management = bridge.enrollment_management();
    expect(port.sent).toEqual([{ id: 1, command: "enrollment_management", args: {} }]);
    port.reply({ id: 1, ok: true, value: { vault_id: "vault", devices: [], storage_bytes: 0, quota_bytes: 1, replay_retention_days: 30 } });
    await expect(management).resolves.toMatchObject({ vault_id: "vault" });
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

  it("surfaces host denials instead of pretending native features succeeded", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const result = bridge.save_draft({ id: "draft", conversationId: "conversation", text: "hello", recipientIds: [], attachmentIds: [], expectedRevision: "1" });
    port.reply({ id: 1, ok: false, error: { code: "invalid-request", message: "Denied" } });
    await expect(result).rejects.toMatchObject({ code: "invalid-request", message: "Denied" });
    await expect(bridge.open_composer()).rejects.toMatchObject({ code: "unsupported" });
    await expect(bridge.get_app_lock_status()).rejects.toMatchObject({ code: "unsupported" });
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
});
