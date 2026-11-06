import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { AttachmentView, DesktopSnapshot, Draft, DraftInput, PreparedPastedAttachments } from "@peppy/desktop-ui/bridge-contracts";
import { App, hasInFlightAttachmentPreparation } from "./App";
import { bridge, fixtureBridge } from "./bridge/bridge";

// Captured before any test saves a draft, so every test starts from the pristine fixture conversations.
const pristine = structuredClone(await fixtureBridge.load_state());

let stateListener: (() => void) | undefined;
let encryptionState: DesktopSnapshot["encryption"]["state"] = "unlocked";
let drafts = new Map<string, Draft>();
let pastedCount = 0;

beforeEach(() => {
  const values = new Map<string, string>();
  Object.defineProperty(window, "localStorage", { configurable: true, value: {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
    clear: () => values.clear(),
  } });
  encryptionState = "unlocked";
  drafts = new Map();
  pastedCount = 0;
  stateListener = undefined;
  vi.spyOn(bridge, "load_state").mockImplementation(async (id) => {
    const active = id && pristine.conversations.some((conversation) => conversation.id === id) ? id : pristine.activeConversationId;
    return {
      ...structuredClone(pristine),
      activeConversationId: active,
      draft: drafts.get(active!),
      encryption: { state: encryptionState },
    };
  });
  vi.spyOn(bridge, "subscribe").mockImplementation((listener) => {
    stateListener = listener;
    return () => {};
  });
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => {});
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => {});
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  vi.spyOn(bridge, "pick_attachments").mockResolvedValue([]);
  vi.spyOn(bridge, "save_draft").mockImplementation(async (input: DraftInput) => storeDraft(input));
  vi.spyOn(bridge, "retry_attachment").mockResolvedValue();
  vi.spyOn(bridge, "resolve_draft_conflict");
  vi.spyOn(bridge, "prepare_pasted_attachments").mockImplementation(async (files) => ({
    views: files.map((file) => pastedView(file.name)),
    failures: [],
  }));
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  localStorage.clear();
});

/** In-memory draft store: keeps saves isolated per test instead of sharing the fixture bridge's module state. */
function storeDraft(input: DraftInput): Draft {
  const current = drafts.get(input.conversationId);
  const saved: Draft = {
    id: current?.id ?? `draft-${input.conversationId}`,
    conversationId: input.conversationId,
    text: input.text,
    recipientIds: input.recipientIds,
    attachmentIds: input.attachmentIds,
    gatewayId: input.gatewayId,
    simId: input.simId,
    revision: String(Number(current?.revision ?? 0) + 1),
  };
  drafts.set(input.conversationId, saved);
  return saved;
}

function pastedView(name: string, overrides: Partial<AttachmentView> = {}): AttachmentView {
  pastedCount += 1;
  return { id: `pasted-${pastedCount}`, name, mediaType: "image/png", byteSize: 1, state: "ready", draftState: "local", ...overrides };
}

function imageFile(name: string): File {
  const file = new File(["x"], name, { type: "image/png" });
  if (typeof file.arrayBuffer !== "function") {
    Object.defineProperty(file, "arrayBuffer", { value: async () => new TextEncoder().encode("x").buffer });
  }
  return file;
}

function pasteFiles(textarea: HTMLElement, names: string[], text = "") {
  return fireEvent.paste(textarea, {
    clipboardData: {
      files: names.map(imageFile),
      items: [],
      types: names.length ? ["Files"] : ["text/plain"],
      getData: (type: string) => (type === "text/plain" || type === "text" ? text : ""),
    },
  });
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

async function renderComposer() {
  render(<App />);
  const textarea = await screen.findByLabelText("Message");
  await waitFor(() => expect(textarea).not.toHaveAttribute("readonly"));
  return textarea as HTMLTextAreaElement;
}

function conversationButton(id: string) {
  return document.querySelector<HTMLButtonElement>(`[data-conversation-id="${id}"]`)!;
}

describe("composer pasted attachments", () => {
  it("does not resolve a conflict while a picker is preparing attachments", async () => {
    const conversationId = pristine.activeConversationId!;
    const picker = deferred<AttachmentView[]>();
    drafts.set(conversationId, {
      id: "conflicted-draft",
      conversationId,
      text: "",
      recipientIds: [],
      attachmentIds: [],
      revision: "1",
      sync: { state: "synced", expiresAt: null, conflicts: [{ branchId: "other", text: "other", recipientIds: [], attachmentIds: [] }], notice: null, sendRequiresConnection: false },
    });
    vi.mocked(bridge.pick_attachments).mockReturnValueOnce(picker.promise);
    const textarea = await renderComposer();

    fireEvent.click(screen.getByRole("button", { name: "Add attachment" }));
    await waitFor(() => expect(bridge.pick_attachments).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByRole("button", { name: "Review" }));
    fireEvent.click(screen.getByRole("radio", { name: /This device/ }));
    fireEvent.click(screen.getByRole("button", { name: "Use selected version" }));

    await waitFor(() => expect(bridge.resolve_draft_conflict).not.toHaveBeenCalled());
    expect(screen.getByRole("alert")).toHaveTextContent("Wait for images to finish preparing before choosing a version.");
    expect(document.querySelector('[data-draft-notice="attachment-feedback"]')).toBeNull();
    await act(async () => picker.resolve([]));
    expect(textarea).toBeInTheDocument();
  });

  it("matches attachment preparation through a resolved draft key", () => {
    const preparations = new Map([["new-draft", new Set([Symbol("preparing")])]]);
    const resolve = (key: string) => key === "new-draft" ? "conv-aurora" : key;

    expect(hasInFlightAttachmentPreparation(preparations, "conv-aurora", resolve)).toBe(true);
  });

  it("does not focus the newly selected composer after resolving another draft conflict", async () => {
    const conversationId = pristine.activeConversationId!;
    const resolution = deferred<Draft>();
    const conflictedDraft: Draft = {
      id: "conflicted-draft",
      conversationId,
      text: "",
      recipientIds: [],
      attachmentIds: [],
      revision: "1",
      sync: { state: "synced", expiresAt: null, conflicts: [{ branchId: "other", text: "other", recipientIds: [], attachmentIds: [] }], notice: null, sendRequiresConnection: false },
    };
    drafts.set(conversationId, conflictedDraft);
    vi.mocked(bridge.resolve_draft_conflict).mockReturnValueOnce(resolution.promise);
    await renderComposer();

    fireEvent.click(screen.getByRole("button", { name: "Review" }));
    fireEvent.click(screen.getByRole("radio", { name: /This device/ }));
    fireEvent.click(screen.getByRole("button", { name: "Use selected version" }));
    await waitFor(() => expect(bridge.resolve_draft_conflict).toHaveBeenCalledOnce());
    fireEvent.click(conversationButton("conv-river"));
    await waitFor(() => expect(conversationButton("conv-river")).toHaveAttribute("aria-current", "location"));
    const riverTextarea = screen.getByLabelText("Message");

    await act(async () => resolution.resolve({ ...conflictedDraft, sync: { ...conflictedDraft.sync!, conflicts: [] } }));

    expect(document.activeElement).not.toBe(riverTextarea);
  });

  it("hydrates a saved draft image from the authoritative host view", async () => {
    const conversationId = pristine.activeConversationId!;
    drafts.set(conversationId, {
      id: "saved-image-draft",
      conversationId,
      text: "",
      recipientIds: [],
      attachmentIds: ["saved-image"],
      attachments: [{ id: "saved-image", name: "saved-photo.png", mediaType: "image/png", byteSize: 42, state: "ready", previewUrl: "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==", draftState: "uploaded" }],
      revision: "1",
    });

    await renderComposer();

    expect(await screen.findByText("saved-photo.png")).toBeInTheDocument();
    expect(within(document.querySelector("#attachment-tray-region")!).getByText("42 B")).toBeInTheDocument();
  });

  it("renders and persists the attachment preview setting", async () => {
    render(<App />);
    await screen.findByLabelText("Message");
    fireEvent.click(screen.getByRole("button", { name: /settings/i }));
    const tiles = await screen.findByRole("radio", { name: "Preview tiles" });
    expect(tiles).toBeChecked();
    fireEvent.click(screen.getByRole("radio", { name: "Compact rows" }));
    expect(JSON.parse(localStorage.getItem("peppy.layout.v1") ?? "{}")).toMatchObject({ composerAttachmentLayout: "rows" });
  });

  it("applies storage updates and resets layout when the key is cleared", async () => {
    render(<App />);
    await screen.findByLabelText("Message");
    window.dispatchEvent(new StorageEvent("storage", { key: "peppy.layout.v1", newValue: JSON.stringify({ composerAttachmentLayout: "rows" }) }));
    await waitFor(() => expect(document.querySelector("#attachment-tray-region")).toHaveAttribute("data-attachment-layout", "rows"));
    window.dispatchEvent(new StorageEvent("storage", { key: "peppy.layout.v1", newValue: null }));
    await waitFor(() => expect(document.querySelector("#attachment-tray-region")).toHaveAttribute("data-attachment-layout", "tiles"));
    window.dispatchEvent(new StorageEvent("storage", { key: "peppy.layout.v1", newValue: JSON.stringify({ composerAttachmentLayout: "rows" }) }));
    await waitFor(() => expect(document.querySelector("#attachment-tray-region")).toHaveAttribute("data-attachment-layout", "rows"));
    window.dispatchEvent(new StorageEvent("storage", { key: "peppy.layout.v1", newValue: "{}" }));
    await waitFor(() => expect(document.querySelector("#attachment-tray-region")).toHaveAttribute("data-attachment-layout", "tiles"));
    window.dispatchEvent(new StorageEvent("storage", { key: null, newValue: null }));
    await waitFor(() => expect(document.querySelector("#attachment-tray-region")).toHaveAttribute("data-attachment-layout", "tiles"));
  });

  it("sends pasted images to the bridge and appends the returned attachment to the active draft", async () => {
    const textarea = await renderComposer();
    pasteFiles(textarea, ["image.png"]);

    await waitFor(() => expect(bridge.prepare_pasted_attachments).toHaveBeenCalledWith([
      { bytes: expect.anything(), name: "image.png", mediaType: "image/png" },
    ]));
    const [{ bytes }] = vi.mocked(bridge.prepare_pasted_attachments).mock.calls[0][0];
    expect(Array.from(new Uint8Array(bytes))).toEqual([120]);
    expect(await screen.findByRole("button", { name: "Remove image.png" })).toBeInTheDocument();
    await waitFor(() => expect(bridge.save_draft).toHaveBeenLastCalledWith(
      expect.objectContaining({ conversationId: "conv-aurora", attachmentIds: ["pasted-1"] }),
    ));
  });

  it("caps rapid concurrent pastes using in-flight slots and reports the overflow", async () => {
    const pending: Array<ReturnType<typeof deferred<PreparedPastedAttachments>>> = [];
    vi.mocked(bridge.prepare_pasted_attachments).mockImplementation(() => {
      const call = deferred<PreparedPastedAttachments>();
      pending.push(call);
      return call.promise;
    });
    const textarea = await renderComposer();
    const first = Array.from({ length: 6 }, (_, index) => `first-${index}.png`);
    const second = Array.from({ length: 6 }, (_, index) => `second-${index}.png`);

    pasteFiles(textarea, first);
    await waitFor(() => expect(bridge.prepare_pasted_attachments).toHaveBeenCalledTimes(1));
    pasteFiles(textarea, second);
    await waitFor(() => expect(bridge.prepare_pasted_attachments).toHaveBeenCalledTimes(2));
    expect(vi.mocked(bridge.prepare_pasted_attachments).mock.calls[1][0].map((file) => file.name)).toEqual(second.slice(0, 4));

    pasteFiles(textarea, ["late.png"]);
    expect(await screen.findByText("You can attach up to 10 images.")).toBeInTheDocument();
    expect(bridge.prepare_pasted_attachments).toHaveBeenCalledTimes(2);

    await act(async () => {
      pending[0].resolve({ views: first.map((name) => pastedView(name)), failures: [] });
      pending[1].resolve({ views: second.slice(0, 4).map((name) => pastedView(name)), failures: [] });
    });
    expect(await screen.findByText("10 images")).toBeInTheDocument();
  });

  it("reserves in-flight pasted attachment slots when adding picker attachments", async () => {
    const pending = deferred<PreparedPastedAttachments>();
    vi.mocked(bridge.prepare_pasted_attachments).mockReturnValueOnce(pending.promise);
    vi.mocked(bridge.pick_attachments).mockResolvedValueOnce([
      pastedView("picked-one.png", { id: "picked-1" }),
      pastedView("picked-two.png", { id: "picked-2" }),
    ]);
    const textarea = await renderComposer();
    const pasted = Array.from({ length: 9 }, (_, index) => `pasted-${index}.png`);
    pasteFiles(textarea, pasted);
    await waitFor(() => expect(bridge.prepare_pasted_attachments).toHaveBeenCalledOnce());

    fireEvent.click(screen.getByRole("button", { name: "Add attachment" }));
    expect(await screen.findByText("You can attach up to 10 images.")).toBeInTheDocument();
    await waitFor(() => expect(bridge.save_draft).toHaveBeenCalledWith(
      expect.objectContaining({ attachmentIds: ["picked-1"] }),
    ));
    expect(screen.getByRole("button", { name: "Remove picked-one.png" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Remove picked-two.png" })).not.toBeInTheDocument();

    await act(async () => pending.resolve({ views: pasted.map((name) => pastedView(name)), failures: [] }));
    expect(await screen.findByText("10 images")).toBeInTheDocument();
  });

  it("reports a prepare rejection as a non-send notice and keeps the draft text and attachments", async () => {
    const textarea = await renderComposer();
    pasteFiles(textarea, ["kept.png"]);
    await screen.findByRole("button", { name: "Remove kept.png" });
    vi.mocked(bridge.prepare_pasted_attachments).mockRejectedValueOnce(new Error("disk full"));
    fireEvent.change(textarea, { target: { value: "hello" } });
    pasteFiles(textarea, ["image.png"]);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Couldn't add pasted image: disk full");
    expect(alert.textContent).not.toMatch(/Not sent:/);
    expect(textarea).toHaveValue("hello");
    expect(screen.getByRole("button", { name: "Remove kept.png" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Remove image.png" })).not.toBeInTheDocument();
    await waitFor(() => expect(drafts.get("conv-aurora")).toMatchObject({ text: "hello", attachmentIds: ["pasted-1"] }));
  });

  it("keeps successful pasted images and reports per-file failures on partial success", async () => {
    vi.mocked(bridge.prepare_pasted_attachments).mockResolvedValueOnce({
      views: [pastedView("good.png")],
      failures: ["Couldn't add broken.bmp."],
    });
    const textarea = await renderComposer();
    pasteFiles(textarea, ["good.png", "broken.bmp"]);

    expect(await screen.findByRole("button", { name: "Remove good.png" })).toBeInTheDocument();
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't add broken.bmp.");
  });

  it("appends the result to the draft that was active when the paste started", async () => {
    const call = deferred<PreparedPastedAttachments>();
    vi.mocked(bridge.prepare_pasted_attachments).mockReturnValueOnce(call.promise);
    const textarea = await renderComposer();
    pasteFiles(textarea, ["image.png"]);
    await waitFor(() => expect(bridge.prepare_pasted_attachments).toHaveBeenCalledOnce());

    fireEvent.click(conversationButton("conv-river"));
    await waitFor(() => expect(conversationButton("conv-river")).toHaveAttribute("aria-current", "location"));
    await act(async () => call.resolve({ views: [pastedView("image.png")], failures: [] }));

    await waitFor(() => expect(bridge.save_draft).toHaveBeenCalledWith(
      expect.objectContaining({ conversationId: "conv-aurora", attachmentIds: ["pasted-1"] }),
    ));
    expect(screen.queryByRole("button", { name: "Remove image.png" })).not.toBeInTheDocument();

    fireEvent.click(conversationButton("conv-aurora"));
    expect(await screen.findByRole("button", { name: "Remove image.png" })).toBeInTheDocument();
  });

  it("discards a paste result that resolves after the session is locked", async () => {
    const call = deferred<PreparedPastedAttachments>();
    vi.mocked(bridge.prepare_pasted_attachments).mockReturnValueOnce(call.promise);
    const textarea = await renderComposer();
    pasteFiles(textarea, ["image.png"]);
    await waitFor(() => expect(bridge.prepare_pasted_attachments).toHaveBeenCalledOnce());

    encryptionState = "locked";
    const loadsBeforeLock = vi.mocked(bridge.load_state).mock.calls.length;
    await act(async () => stateListener?.());
    await waitFor(() => expect(vi.mocked(bridge.load_state).mock.calls.length).toBeGreaterThan(loadsBeforeLock));
    await act(async () => call.resolve({ views: [pastedView("image.png")], failures: [] }));

    encryptionState = "unlocked";
    await act(async () => stateListener?.());
    await waitFor(() => expect(screen.getByLabelText("Message")).toBeInTheDocument());
    expect(screen.queryByRole("button", { name: "Remove image.png" })).not.toBeInTheDocument();
    expect(vi.mocked(bridge.save_draft).mock.calls.some(([input]) => input.attachmentIds.includes("pasted-1"))).toBe(false);
  });

  it("retries a failed pasted attachment through the bridge and reports retry failures", async () => {
    vi.mocked(bridge.prepare_pasted_attachments).mockResolvedValueOnce({
      views: [pastedView("retry.png", { state: "failed", retryable: true, error: "upload failed" })],
      failures: [],
    });
    const textarea = await renderComposer();
    pasteFiles(textarea, ["retry.png"]);
    fireEvent.click(await screen.findByRole("button", { name: "Retry retry.png" }));
    await waitFor(() => expect(bridge.retry_attachment).toHaveBeenCalledWith("pasted-1"));

    vi.mocked(bridge.prepare_pasted_attachments).mockResolvedValueOnce({
      views: [pastedView("offline.png", { state: "failed", retryable: true, error: "upload failed" })],
      failures: [],
    });
    pasteFiles(textarea, ["offline.png"]);
    vi.mocked(bridge.retry_attachment).mockRejectedValueOnce(new Error("offline"));
    fireEvent.click(await screen.findByRole("button", { name: "Retry offline.png" }));
    expect(await screen.findByText("Couldn't retry attachment: offline")).toBeInTheDocument();
  });
});
