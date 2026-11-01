import "@testing-library/jest-dom/vitest";
import { StrictMode } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, onTestFinished, vi } from "vitest";
import { conversationAvatar, formatDateTime, isoDateTime, paletteIndex } from "@peppy/desktop-ui";
import { App, DraftStore, searchRecipientSuggestions } from "./App";
import {
  bridge,
  fixtureBridge,
  type ConversationView,
  type DesktopSnapshot,
  type Draft,
  type DraftInput,
  type EnrollmentView,
  type GatewayView,
  type ContactBookView,
  type SendDraftInput,
} from "./bridge";

/**
 * In-memory stand-in for the native host contract (apps/desktop/src-tauri/src/session.rs):
 * an empty draft ID asks the host to create the draft (and, with an empty conversation ID, a new
 * conversation); saves are CAS on `expectedRevision`; sends use the STORED draft and clear its
 * message fields while retaining its identity, route, and recipients.
 */
type HostError = { code: string; message: string };
const findNewConversationAction = async () => {
  const action = within(document.getElementById("desktop-titlebar")!).getByRole("button", { name: "New conversation" });
  await waitFor(() => expect(action).toBeEnabled());
  return action;
};
const findEmptyConversationAction = () => waitFor(() => {
  const action = document.getElementById("new-conversation-cta");
  expect(action).toBeVisible();
  return action!;
});

describe("group sender attribution", () => {
  it("shows sender labels and run avatars only for group conversations", async () => {
    host.conversations[0] = {
      ...host.conversations[0],
      participants: ["+18015551234", "+18015551235"],
      messages: [
        { id: "sender-first", revision: "1", sender: "other", senderAddress: "+18015551234", body: "First", timestamp: "1788434460000", attachments: [] },
        { id: "sender-last", revision: "2", sender: "other", senderAddress: "+18015551234", body: "Last", timestamp: "1788434461000", attachments: [] },
      ],
    };
    host.contactResolution = { "+18015551234": { bookId: "book-1", contactId: "contact-1", displayName: "Ada" } };
    render(<App />);

    await screen.findByText("First");
    expect(document.querySelector('[data-sender-label="+18015551234"]')).toHaveTextContent("Ada");
    expect(document.querySelector('[data-message-row="sender-first"]')).toHaveAttribute("data-sender-run-start");
    expect(document.querySelector('[data-message-row="sender-last"]')).toHaveAttribute("data-sender-run-end");
    expect(document.querySelector('[data-message-row="sender-first"] [data-sender-avatar]')).toBeNull();
    expect(document.querySelector('[data-message-row="sender-last"] [data-sender-avatar="+18015551234"]')).toBeInTheDocument();
    expect(document.querySelector('[data-message-row="sender-first"] .sr-only')).toHaveTextContent("From Ada:");
  });

  it("opens an unknown sender in a prefilled Contacts create form", async () => {
    host.conversations[0] = {
      ...host.conversations[0],
      participants: ["+18015551234", "+18015551235"],
      messages: [{ id: "unknown-sender", revision: "1", sender: "other", senderAddress: "+18015551234", body: "Hello", timestamp: "1788434460000", attachments: [] }],
    };
    render(<App />);

    fireEvent.click((await screen.findByText("(801) 555-1234")).closest("button")!);

    expect(await screen.findByRole("form", { name: "Create contact" })).toBeInTheDocument();
    expect(screen.getByLabelText("Phone 1 number")).toHaveValue("+18015551234");
  });

  it("opens a known sender's contact target", async () => {
    host.conversations[0] = {
      ...host.conversations[0],
      participants: ["+18015551234", "+18015551235"],
      messages: [{ id: "known-sender", revision: "1", sender: "other", senderAddress: "+18015551234", body: "Hello", timestamp: "1788434460000", attachments: [] }],
    };
    host.contactResolution = { "+18015551234": { bookId: "book-1", contactId: "contact-1", displayName: "Ada" } };
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "Open Ada in Contacts" }));

    expect(await screen.findByRole("region", { name: "Contacts browser" })).toBeInTheDocument();
  });

  it("blocks sender navigation when flushing the active draft fails", async () => {
    host.conversations[0] = {
      ...host.conversations[0],
      participants: ["+18015551234", "+18015551235"],
      messages: [{ id: "failed-sender", revision: "1", sender: "other", senderAddress: "+18015551234", body: "Hello", timestamp: "1788434460000", attachments: [] }],
    };
    render(<App />);
    await waitFor(() => expect(document.querySelector('[data-message-id="failed-sender"]')).toBeInTheDocument());
    host.failSaves = { code: "offline", message: "offline" };
    type("Unsaved draft");
    await waitFor(() => expect(bridge.save_draft).toHaveBeenCalled());

    fireEvent.click((await screen.findByText("(801) 555-1234")).closest("button")!);

    expect(await screen.findByText("Sender not opened: offline")).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Contacts browser" })).not.toBeInTheDocument();
  });

  it("renders sender labels and avatars as non-interactive while the vault is locked", async () => {
    host.encryption = { state: "locked" };
    host.conversations[0] = {
      ...host.conversations[0],
      participants: ["+18015551234", "+18015551235"],
      messages: [{ id: "locked-sender", revision: "1", sender: "other", senderAddress: "+18015551234", body: "Hello", timestamp: "1788434460000", attachments: [] }],
    };
    render(<App />);

    const label = await screen.findByText("(801) 555-1234");
    expect(label.tagName).toBe("SPAN");
    expect(document.querySelector("[data-sender-avatar]")).not.toHaveAttribute("data-clickable");
  });

  it("keeps sender labels while hiding persisted sender avatars", async () => {
    localStorage.setItem("peppy.layout.v1", JSON.stringify({ groupSenderAvatars: false }));
    host.conversations[0] = {
      ...host.conversations[0],
      participants: ["+18015551234", "+18015551235"],
      messages: [{ id: "sender-hidden-avatar", revision: "1", sender: "other", senderAddress: "+18015551234", body: "Hello", timestamp: "1788434460000", attachments: [] }],
    };
    render(<App />);

    expect(await screen.findByText("(801) 555-1234")).toBeInTheDocument();
    expect(document.querySelector("[data-sender-avatar]")).not.toBeInTheDocument();
    expect(document.querySelector("[data-sender-gutter]")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const avatars = await screen.findByRole("checkbox", { name: "Show sender avatars in group conversations" });
    expect(avatars).not.toBeChecked();
    fireEvent.click(avatars);
    expect(JSON.parse(localStorage.getItem("peppy.layout.v1")!)).toMatchObject({ groupSenderAvatars: true });
  });

  it("ends sender runs after visible compact timestamps but not when compact timestamps are disabled", async () => {
    host.conversations[0] = {
      ...host.conversations[0],
      participants: ["+18015551234", "+18015551235"],
      messages: [
        { id: "sender-time-first", revision: "1", sender: "other", senderAddress: "+18015551234", body: "First", timestamp: "1788434460000", attachments: [] },
        { id: "sender-time-second", revision: "2", sender: "other", senderAddress: "+18015551234", body: "Second", timestamp: "1788441660000", attachments: [] },
        { id: "sender-time-third", revision: "3", sender: "other", senderAddress: "+18015551234", body: "Third", timestamp: "1788448860000", attachments: [] },
      ],
    };
    const view = render(<App />);
    await screen.findByText("Third");
    expect(document.querySelector('[data-message-row="sender-time-first"]')).toHaveAttribute("data-sender-run-end");
    expect(document.querySelectorAll("[data-sender-label]")).toHaveLength(3);

    view.unmount();
    localStorage.setItem("peppy.layout.v1", JSON.stringify({ compactTimestamps: false }));
    render(<App />);
    await screen.findByText("Third");
    expect(document.querySelector('[data-message-row="sender-time-first"]')).not.toHaveAttribute("data-sender-run-end");
    expect(document.querySelectorAll("[data-sender-label]")).toHaveLength(1);
  });

  it("breaks same-sender runs at reaction notices", async () => {
    host.conversations[0] = {
      ...host.conversations[0],
      participants: ["+18015551234", "+18015551235"],
      messages: [
        { id: "before-reaction", revision: "1", sender: "other", senderAddress: "+18015551234", body: "Before", timestamp: "1788434460000", attachments: [] },
        { id: "reaction-boundary", revision: "2", sender: "other", senderAddress: "+18015551234", body: "Reacted 😆 to “Before”", timestamp: "1788434461000", attachments: [] },
        { id: "after-reaction", revision: "3", sender: "other", senderAddress: "+18015551234", body: "After", timestamp: "1788434462000", attachments: [] },
      ],
    };
    render(<App />);

    await screen.findByText("After");
    expect(document.querySelector('[data-message-row="before-reaction"]')).toHaveAttribute("data-sender-run-end");
    expect(document.querySelector('[data-message-row="after-reaction"]')).toHaveAttribute("data-sender-run-start");
    expect(document.querySelectorAll("[data-sender-label]")).toHaveLength(2);
  });

  it("keeps same-sender messages within an hour in one compact timestamp run", async () => {
    host.conversations[0] = {
      ...host.conversations[0],
      participants: ["+18015551234", "+18015551235"],
      messages: [
        { id: "hidden-time-first", revision: "1", sender: "other", senderAddress: "+18015551234", body: "First", timestamp: "1788434460000", attachments: [] },
        { id: "hidden-time-last", revision: "2", sender: "other", senderAddress: "+18015551234", body: "Last", timestamp: "1788434520000", attachments: [] },
      ],
    };
    render(<App />);

    await screen.findByText("Last");
    expect(document.querySelector('[data-message-row="hidden-time-first"]')).not.toHaveAttribute("data-sender-run-end");
    expect(document.querySelector('[data-message-row="hidden-time-last"]')).toHaveAttribute("data-sender-run-end");
    expect(document.querySelectorAll("[data-sender-label]")).toHaveLength(1);
  });

  it("wires sender run edges, labels, and avatars through grouped images", async () => {
    const image = (id: string) => ({ id: `${id}-image`, name: `${id}.jpg`, mediaType: "image/jpeg", byteSize: 1, state: "ready" as const });
    host.conversations[0] = {
      ...host.conversations[0],
      participants: ["+18015551234", "+18015551235"],
      messages: [
        { id: "image-run-first", revision: "1", sender: "other", senderAddress: "+18015551234", body: "", timestamp: "1788434460000", transport: "mms", attachments: [image("first")] },
        { id: "image-run-last", revision: "2", sender: "other", senderAddress: "+18015551234", body: "", timestamp: "1788434461000", transport: "mms", attachments: [image("last")] },
        { id: "after-images", revision: "3", sender: "other", senderAddress: "+18015551234", body: "After images", timestamp: "1788434462000", attachments: [] },
      ],
    };
    host.contactResolution = { "+18015551234": { bookId: "book-1", contactId: "contact-1", displayName: "Ada" } };
    render(<App />);

    await screen.findByText("After images");
    const imageGroup = document.querySelector('[data-message-group="image-run-first"]')!;
    expect(imageGroup).toHaveAttribute("data-sender-run-start");
    expect(imageGroup).not.toHaveAttribute("data-sender-run-end");
    expect(imageGroup.querySelector('[data-sender-label="+18015551234"]')).toHaveTextContent("Ada");
    expect(imageGroup.querySelector("[data-sender-avatar]")).toBeNull();
    expect(document.querySelector('[data-message-row="after-images"]')).toHaveAttribute("data-sender-run-end");
    expect(document.querySelector('[data-message-row="after-images"] [data-sender-avatar="+18015551234"]')).toBeInTheDocument();
  });
});

describe("conversation add-contact action", () => {
  const writableBook: ContactBookView = {
    id: "book-1",
    deviceName: "Phone",
    state: "active",
    capabilities: { canWrite: true, canDelete: true, supportsNotes: true, supportsPhoto: true },
    contactCount: 0,
    pendingEditCount: 0,
  };

  it("opens a prefilled contact create form for the raw participant address", async () => {
    host.conversations[0] = { ...host.conversations[0], participants: ["+18015551234"] };
    host.contactBooks = [writableBook];
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "Add (801) 555-1234 to contacts" }));

    expect(await screen.findByRole("form", { name: "Create contact" })).toBeInTheDocument();
    expect(screen.getByLabelText("Phone 1 number")).toHaveValue("+18015551234");
  });

  it("hides for saved contacts, groups, drafts, and unusable books", async () => {
    host.conversations[0] = { ...host.conversations[0], participants: ["+18015551234"] };
    host.contactBooks = [writableBook];
    host.contactResolution = { "+18015551234": { bookId: "book-1", contactId: "contact-1", displayName: "Ada" } };
    const view = render(<App />);
    await screen.findByText("Hello from Aurora");
    expect(screen.queryByRole("button", { name: /Add .* to contacts/ })).not.toBeInTheDocument();
    view.unmount();

    host.contactResolution = undefined;
    host.conversations[0] = { ...host.conversations[0], participants: ["+18015551234", "+18015551235"] };
    render(<App />);
    await screen.findByText("Hello from Aurora");
    expect(screen.queryByRole("button", { name: /Add .* to contacts/ })).not.toBeInTheDocument();
    cleanup();

    host.conversations[0] = { ...host.conversations[0], participants: ["+18015551234"], draftOnly: true };
    render(<App />);
    await screen.findByText("Hello from Aurora");
    expect(screen.queryByRole("button", { name: /Add .* to contacts/ })).not.toBeInTheDocument();
    cleanup();

    host.conversations[0] = { ...host.conversations[0], draftOnly: undefined };
    host.contactBooks = [{ ...writableBook, capabilities: { ...writableBook.capabilities, canWrite: false } }];
    render(<App />);
    await screen.findByText("Hello from Aurora");
    expect(screen.queryByRole("button", { name: /Add .* to contacts/ })).not.toBeInTheDocument();
  });
});
const SMS_ONLY: GatewayView = {
  id: "gw-phone",
  name: "Phone",
  simId: "sim-1",
  online: false,
  simulated: false,
  supportsSms: true,
  supportsMms: false,
};
describe("draft lifecycle flushing", () => {
  it("continues saving newer typing after a fresh draft receives its first host identity", async () => {
    const first = deferred<Draft>();
    const inputs: DraftInput[] = [];
    const store = new DraftStore(async input => {
      inputs.push(input);
      if (inputs.length === 1) return first.promise;
      return { ...input, id: "draft-created", revision: "2" };
    }, { changed: () => {}, rekeyed: () => {} });
    const initialSave = store.edit("new:1", { text: "first" });
    await vi.waitFor(() => expect(inputs).toHaveLength(1));
    const newerSave = store.edit("new:1", { text: "newer" });
    first.resolve({ id: "draft-created", conversationId: "created", text: "first", recipientIds: [], attachmentIds: [], revision: "1" });
    await expect(initialSave).resolves.toEqual({ ok: true });
    await expect(newerSave).resolves.toEqual({ ok: true });
    expect(inputs).toMatchObject([{ conversationId: "", text: "first" }, { conversationId: "created", text: "newer", expectedRevision: "1" }]);
    expect(store.get("created")?.content.text).toBe("newer");
    expect(store.retainedText("created")).toBeUndefined();
  });

  it("attempts every dirty slot and reports the first failed save", async () => {
    const persisted: string[] = [];
    const store = new DraftStore(async input => {
      persisted.push(input.conversationId);
      if (input.conversationId === "one") throw { message: "first failed" };
      return { id: `draft-${input.conversationId}`, conversationId: input.conversationId, text: input.text, recipientIds: input.recipientIds, attachmentIds: input.attachmentIds, revision: "1" };
    }, { changed: () => {}, rekeyed: () => {} });
    store.adopt("one", undefined);
    store.adopt("two", undefined);
    void store.edit("one", { text: "a" });
    void store.edit("two", { text: "b" });
    expect(await store.flushAll()).toEqual({ ok: false, error: "first failed", key: "one" });
    expect(persisted).toEqual(expect.arrayContaining(["one", "two"]));
  });

  it("serializes concurrent flushing behind queued revisions", async () => {
    const first = deferred<Draft>();
    const inputs: DraftInput[] = [];
    let active = 0;
    let maximumActive = 0;
    const store = new DraftStore(async input => {
      inputs.push(input);
      active += 1;
      maximumActive = Math.max(maximumActive, active);
      const saved = inputs.length === 1 ? await first.promise : { ...input, id: "draft-one", revision: "2" };
      active -= 1;
      return saved;
    }, { changed: () => {}, rekeyed: () => {} });
    store.adopt("one", undefined);
    const firstEdit = store.edit("one", { text: "a" });
    await vi.waitFor(() => expect(inputs).toHaveLength(1));
    const secondEdit = store.edit("one", { text: "b" });
    const flush = store.flushAll();
    first.resolve({ id: "draft-one", conversationId: "one", text: "a", recipientIds: [], attachmentIds: [], revision: "1" });
    await Promise.all([firstEdit, secondEdit, flush]);
    expect(inputs).toMatchObject([{ text: "a", expectedRevision: "0" }, { text: "b", expectedRevision: "1" }]);
    expect(maximumActive).toBe(1);
  });
});

describe("setup landing routing", () => {
  const signedOut = { available: false, signedIn: false, accountLabel: null, classification: null, entitlement: null, access: null, hasVault: false, resumable: false } as const;
  const enrollment = (stage: "server" | "ready" | "waiting_phone" | "phone_connected", overrides = {}) => ({ mode: "community" as const, stage, ...overrides });
  const landingSnapshot = async (overrides: Partial<DesktopSnapshot>) => {
    const snapshot = await fixtureBridge.load_state();
    vi.spyOn(bridge, "load_state").mockResolvedValue({ ...snapshot, activeConversationId: undefined, ...overrides });
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(signedOut);
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(enrollment("server"));
    vi.spyOn(bridge, "enrollment_status").mockResolvedValue(enrollment("ready", { owner: true, hasGateway: false }));
    vi.spyOn(bridge, "enrollment_add_phone").mockResolvedValue(enrollment("waiting_phone", { qrPayload: "enrollment", expiresInSeconds: 300 }));
    vi.spyOn(bridge, "enrollment_cancel").mockResolvedValue();
  };
  afterEach(() => localStorage.removeItem("peppy.setup.mode"));

  it("uses enrollment discovery for a fresh server-required desktop", async () => {
    await landingSnapshot({ connection: { state: "offline", errorCode: "server-required" } });
    render(<App />);
    fireEvent.change(await screen.findByRole("combobox", { name: /server mode/i }), { target: { value: "self-hosted" } });
    fireEvent.change(screen.getByLabelText(/server url/i, { selector: "#self-hosted-url-input" }), { target: { value: "https://server.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledWith("https://server.example"));
  });

  it("reconciles a stale browser claim result after a connected-owner subscription snapshot", async () => {
    const creation = deferred<EnrollmentView>();
    const initial = await fixtureBridge.load_state();
    let current: DesktopSnapshot = {
      ...initial,
      activeConversationId: undefined,
      connection: { state: "offline" as const, errorCode: "server-required" },
    };
    vi.spyOn(bridge, "load_state").mockImplementation(async () => current);
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(signedOut);
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(enrollment("server", { stage: "setup" }));
    vi.spyOn(bridge, "enrollment_create_owner").mockReturnValue(creation.promise);
    vi.spyOn(bridge, "enrollment_status").mockResolvedValue(enrollment("ready", { owner: true, hasGateway: false }));
    vi.spyOn(bridge, "enrollment_add_phone").mockResolvedValue(enrollment("waiting_phone", { qrPayload: "enrollment", expiresInSeconds: 300 }));

    render(<App hostKind="browser" fixedOrigin="https://community.example" />);
    fireEvent.click(await screen.findByRole("button", { name: "Create server" }));
    current = {
      ...current,
      connection: { state: "connected", origin: "https://community.example" },
      deviceRole: "owner",
      gateways: [],
    };
    await act(async () => hint?.());
    await act(async () => creation.resolve(enrollment("server", { stage: "setup" })));

    await waitFor(() => expect(bridge.enrollment_status).toHaveBeenCalledOnce());
    await waitFor(() => expect(bridge.enrollment_add_phone).toHaveBeenCalledWith("transfer"));
    expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
  });

  it.each([
    ["native", {}, "https://127.0.0.1:7000"],
    ["browser", { hostKind: "browser" as const, fixedOrigin: "https://community.example" }, "https://community.example"],
  ])("waits for the host connection before showing owner recovery actions in the %s host", async (hostKind, appProps, origin) => {
    const creation = deferred<EnrollmentView>();
    const status = deferred<EnrollmentView>();
    const invitation = deferred<EnrollmentView>();
    const initial = await fixtureBridge.load_state();
    let current: DesktopSnapshot = {
      ...initial,
      activeConversationId: undefined,
      connection: { state: "offline", origin, errorCode: "server-required" },
      encryption: { state: "locked" },
      gateways: [],
      deviceRole: undefined,
    };
    vi.spyOn(bridge, "load_state").mockImplementation(async () => current);
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(signedOut);
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(enrollment("server", { stage: "setup", origin: current.connection.origin, setupCodeRequired: false }));
    vi.spyOn(bridge, "enrollment_create_owner").mockReturnValue(creation.promise);
    vi.spyOn(bridge, "enrollment_status").mockReturnValue(status.promise);
    vi.spyOn(bridge, "enrollment_add_phone").mockReturnValue(invitation.promise);

    if (hostKind === "native") localStorage.setItem("peppy.setup.mode", "self-hosted");
    render(<App {...appProps} />);
    if (hostKind === "native") {
      fireEvent.change(await screen.findByLabelText(/server url/i, { selector: "#self-hosted-url-input" }), { target: { value: origin } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    }
    fireEvent.click(await screen.findByRole("button", { name: "Create server" }));

    current = {
      ...current,
      connection: { state: "offline", origin, errorCode: "connecting" },
      encryption: { state: "unlocked" },
      deviceRole: "owner",
      gateways: [],
    };
    await act(async () => hint?.());
    await act(async () => creation.resolve(enrollment("server", { stage: "setup", origin: current.connection.origin, setupCodeRequired: false })));

    await waitFor(() => expect(bridge.enrollment_status).toHaveBeenCalledOnce());
    expect(screen.getByRole("button", { name: "Create server" })).toBeDisabled();
    await act(async () => status.resolve(enrollment("ready", { owner: true, hasGateway: false, origin: current.connection.origin })));
    await waitFor(() => expect(bridge.enrollment_add_phone).toHaveBeenCalledWith("transfer"));
    expect(bridge.enrollment_add_phone).toHaveBeenCalledOnce();
    await act(async () => invitation.resolve(enrollment("ready", { owner: true, hasGateway: false, origin })));
    expect(screen.queryByRole("button", { name: "Show connection code" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Set up phone later" })).not.toBeInTheDocument();

    current = {
      ...current,
      connection: { state: "connected", origin },
    };
    await act(async () => hint?.());

    expect(await screen.findByRole("button", { name: "Show connection code" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Set up phone later" })).toBeEnabled();
  });

  it("rechecks a remounted connected owner until its stale status becomes ready", async () => {
    const initial = await fixtureBridge.load_state();
    let current: DesktopSnapshot = {
      ...initial,
      activeConversationId: undefined,
      connection: { state: "offline", origin: "https://community.example", errorCode: "server-required" },
      encryption: { state: "locked" },
      deviceRole: undefined,
      gateways: [],
    };
    vi.spyOn(bridge, "load_state").mockImplementation(async () => current);
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(signedOut);
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(enrollment("server", { stage: "setup" }));
    vi.spyOn(bridge, "enrollment_status")
      .mockResolvedValueOnce(enrollment("server", { stage: "setup" }))
      .mockResolvedValueOnce(enrollment("ready", { owner: true, hasGateway: false }));
    vi.spyOn(bridge, "enrollment_add_phone").mockResolvedValue(enrollment("waiting_phone", { qrPayload: "enrollment", expiresInSeconds: 300 }));

    render(<App hostKind="browser" fixedOrigin="https://community.example" />);
    expect(await screen.findByRole("button", { name: "Create server" })).toBeInTheDocument();
    vi.useFakeTimers();
    try {
      current = { ...current, connection: { state: "offline", origin: "https://community.example", errorCode: "connecting" } };
      await act(async () => hint?.());
      expect(document.getElementById("setup-landing")).not.toBeInTheDocument();
      current = { ...current, connection: { state: "connected", origin: "https://community.example" }, deviceRole: "owner" };
      current = { ...current, encryption: { state: "unlocked" } };
      await act(async () => hint?.());
      expect(document.getElementById("enrollment-checking")).toBeInTheDocument();
      await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });

      expect(screen.queryByRole("button", { name: "Create server" })).not.toBeInTheDocument();
      expect(screen.getByRole("heading", { name: /connect your phone/i })).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps a locked connected owner on the workbench", async () => {
    await landingSnapshot({
      connection: { state: "connected", origin: "https://example.test" },
      encryption: { state: "locked" },
      gateways: [],
      deviceRole: "owner",
    });

    render(<App />);

    await waitFor(() => expect(document.getElementById("desktop-shell")).toBeInTheDocument());
    expect(document.getElementById("setup-landing")).not.toBeInTheDocument();
    expect(bridge.enrollment_status).not.toHaveBeenCalled();
  });

  it("opens owner add phone enrollment from the workbench even with a gateway", async () => {
    await landingSnapshot({ connection: { state: "connected", origin: "https://example.test" }, gateways: [SMS_ONLY], deviceRole: "owner" });
    vi.mocked(bridge.enrollment_status).mockResolvedValue(enrollment("ready", { owner: true, hasGateway: true }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Add phone" }));
    expect(await screen.findByText("Connect your phone")).toBeInTheDocument();
  });

  it("defers automatic phone setup per origin while header Add phone remains available", async () => {
    const refreshedSnapshot = {
      ...await fixtureBridge.load_state(),
      activeConversationId: undefined,
      connection: { state: "connected" as const, origin: "https://example.test" },
      gateways: [],
      deviceRole: "owner" as const,
    };
    const refresh = deferred<DesktopSnapshot>();
    await landingSnapshot({ connection: refreshedSnapshot.connection, gateways: [], deviceRole: "owner" });
    vi.mocked(bridge.enrollment_status).mockResolvedValue(enrollment("ready", { owner: true, hasGateway: false }));
    vi.spyOn(bridge, "enrollment_cancel_phone").mockResolvedValue(enrollment("ready", { owner: true }));
    render(<App />);
    const defer = await screen.findByRole("button", { name: "Set up phone later" });
    vi.mocked(bridge.load_state).mockReturnValue(refresh.promise);

    fireEvent.click(defer);
    await waitFor(() => expect(localStorage.getItem("peppy.setup.phoneDeferred:https://example.test")).toBe("1"));
    expect(document.getElementById("setup-landing")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Add phone" }));
    expect(await screen.findByRole("button", { name: "Set up phone later" })).toBeInTheDocument();

    await act(async () => refresh.resolve(refreshedSnapshot));
    expect(screen.getByRole("button", { name: "Set up phone later" })).toBeInTheDocument();
  });

  it("returns to the selected conversation when add phone is cancelled", async () => {
    const cancellation = deferred<void>();
    await landingSnapshot({ connection: { state: "connected", origin: "https://example.test" }, gateways: [SMS_ONLY], deviceRole: "owner" });
    vi.mocked(bridge.enrollment_add_phone).mockResolvedValue(enrollment("waiting_phone", { qrPayload: "enrollment", expiresInSeconds: 300 }));
    vi.mocked(bridge.enrollment_cancel).mockReturnValue(cancellation.promise);
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Add phone" }));
    await screen.findByRole("img", { name: /pairing qr/i });
    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));
    expect(bridge.enrollment_cancel).toHaveBeenCalledOnce();
    expect(document.getElementById("conversation-stage")).not.toBeInTheDocument();
    await act(async () => cancellation.resolve());
    await waitFor(() => expect(document.getElementById("conversation-stage")).toBeInTheDocument());
  });

  it("refreshes after phone enrollment without requesting the legacy sync unlock", async () => {
    await landingSnapshot({ mode: "browser", connection: { state: "offline" }, encryption: { state: "preview" } });
    vi.mocked(bridge.enrollment_discover).mockResolvedValue(enrollment("phone_connected"));
    const unlock = vi.spyOn(bridge, "unlock_sync");
    render(<App hostKind="browser" fixedOrigin="https://community.example" />);
    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledTimes(2));
    expect(unlock).not.toHaveBeenCalled();
  });

  it("starts native phone pairing without invoking the sync unlock for an unlocked owner", async () => {
    await landingSnapshot({
      connection: { state: "connected", origin: "https://example.test" },
      encryption: { state: "unlocked" },
      gateways: [SMS_ONLY],
      deviceRole: "owner",
    });
    vi.mocked(bridge.enrollment_status).mockResolvedValue(enrollment("ready", { owner: true, hasGateway: true }));
    const unlock = vi.spyOn(bridge, "unlock_sync");
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Add phone" }));
    expect(await screen.findByText("Connect your phone")).toBeInTheDocument();
    expect(unlock).not.toHaveBeenCalled();
  });

  it("keeps an unlocked native session that is offline without a selected conversation on the workbench", async () => {
    await landingSnapshot({ connection: { state: "offline", errorCode: "offline" }, encryption: { state: "unlocked" } });
    render(<App />);
    await waitFor(() => expect(document.getElementById("desktop-shell")).toBeInTheDocument());
    expect(document.getElementById("setup-landing")).not.toBeInTheDocument();
    expect(document.getElementById("system-ribbon")).toHaveTextContent("Server offline");
    expect(document.getElementById("system-ribbon")).toHaveTextContent("Peppy will reconnect automatically.");
    /* One global ribbon: a direct child of the shell, outside the content pane. */
    const ribbon = document.getElementById("system-ribbon")!;
    expect(ribbon.parentElement).toBe(document.getElementById("desktop-shell"));
    expect(ribbon.closest("#desktop-body")).toBeNull();
    expect(ribbon.nextElementSibling?.id).toBe("system-ribbon-announcement");
    expect(document.querySelectorAll("#system-ribbon")).toHaveLength(1);
    expect(document.querySelector("[data-status-trigger]")).toHaveAttribute("data-status-tone", "warning");
    expect(document.getElementById("connection-status")).toHaveAttribute("data-server-unreachable");
  });

  it("retains initial setup through a credentialed outage and clears it after reconnecting", async () => {
    const creation = deferred<EnrollmentView>();
    const initial = await fixtureBridge.load_state();
    let current: DesktopSnapshot = {
      ...initial,
      activeConversationId: undefined,
      connection: { state: "offline", origin: "https://example.test", errorCode: "server-required" },
      encryption: { state: "locked" },
      gateways: [],
      deviceRole: undefined,
    };
    vi.spyOn(bridge, "load_state").mockImplementation(async () => current);
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(signedOut);
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(enrollment("server", { stage: "setup" }));
    vi.spyOn(bridge, "enrollment_create_owner").mockReturnValue(creation.promise);

    render(<App hostKind="browser" fixedOrigin="https://example.test" />);
    fireEvent.click(await screen.findByRole("button", { name: "Create server" }));
    current = {
      ...current,
      connection: { state: "offline", origin: "https://example.test", errorCode: "server-unavailable" },
      encryption: { state: "unlocked" },
      deviceRole: "owner",
      gateways: [SMS_ONLY],
    };
    await act(async () => hint?.());

    expect(document.getElementById("setup-landing")).toBeInTheDocument();
    expect(document.getElementById("system-ribbon")).toHaveTextContent("Server offline");
    current = { ...current, connection: { state: "connected", origin: "https://example.test" } };
    await act(async () => hint?.());
    await waitFor(() => expect(document.getElementById("setup-landing")).not.toBeInTheDocument());
  });

  it("does not present an offline banner while a connection is unknown", async () => {
    await landingSnapshot({ connection: { state: "offline", errorCode: "connecting" }, encryption: { state: "unlocked" } });
    render(<App />);
    await waitFor(() => expect(document.getElementById("desktop-shell")).toBeInTheDocument());
    expect(document.getElementById("system-ribbon")).not.toBeInTheDocument();
    expect(document.getElementById("system-ribbon-announcement")).toHaveTextContent("");
  });

  it("keeps the onboarding header when SetupLanding is displayed", async () => {
    await landingSnapshot({ connection: { state: "offline", errorCode: "server-required" } });
    render(<App />);
    await waitFor(() => expect(document.getElementById("setup-landing")).toBeInTheDocument());
    expect(document.querySelector("[data-header-title]")?.textContent).toBe("Set up Peppy");
  });

  it("shows the neutral Peppy header for a completed empty inbox", async () => {
    await landingSnapshot({
      connection: { state: "connected", origin: "https://example.test" },
      encryption: { state: "unlocked" },
      gateways: [SMS_ONLY],
      deviceRole: "owner",
      conversations: [],
    });
    render(<App />);
    await waitFor(() => expect(document.getElementById("desktop-shell")).toBeInTheDocument());
    expect(document.getElementById("setup-landing")).not.toBeInTheDocument();
    await waitFor(() => expect(document.querySelector("[data-header-title]")?.textContent).toBe("Peppy"));
  });

  it("shows the neutral Peppy header for a deferred owner without a phone", async () => {
    localStorage.setItem("peppy.setup.phoneDeferred:https://example.test", "1");
    await landingSnapshot({
      connection: { state: "connected", origin: "https://example.test" },
      encryption: { state: "unlocked" },
      gateways: [],
      deviceRole: "owner",
      conversations: [],
    });
    render(<App />);
    await waitFor(() => expect(document.getElementById("desktop-shell")).toBeInTheDocument());
    expect(document.getElementById("setup-landing")).not.toBeInTheDocument();
    expect(document.querySelector("[data-header-title]")?.textContent).toBe("Peppy");
  });
});

const MMS_SIM: GatewayView = { ...SMS_ONLY, simId: "sim-2", supportsMms: true, mmsContentVersion: 2 };

function createHost() {
  let created = 0;
  const host = {
    gateways: [SMS_ONLY, MMS_SIM] as GatewayView[],
    connection: {
      state: "connected",
      origin: "https://example.test",
    } as DesktopSnapshot["connection"],
    encryption: { state: "unlocked" } as DesktopSnapshot["encryption"],
    desktop: { trayAvailable: true, startAtLogin: false, startupSupported: true, background: false } as NonNullable<DesktopSnapshot["desktop"]>,
    head: { enabled: false, capability: "unsupported" } as DesktopSnapshot["head"],
    conversations: [
      {
        id: "aurora",
        name: "Aurora",
        preview: "Hello",
        unread: 1,
        messages: [
          {
            id: "m-aurora",
            revision: "1",
            sender: "other",
            body: "Hello from Aurora",
            timestamp: "now",
            attachments: [
              {
                id: "photo-1",
                name: "photo.png",
                mediaType: "image/png",
                byteSize: 10,
                state: "ready",
                transfer: "download",
                retryable: true,
              },
            ],
          },
        ],
      },
      {
        id: "river",
        name: "River",
        preview: "Hi",
        unread: 0,
        messages: [
          {
            id: "m-river",
            revision: "1",
            sender: "other",
            body: "Hello from River",
            timestamp: "now",
            attachments: [],
          },
        ],
      },
    ] as ConversationView[],
    drafts: new Map<string, Draft>(),
    failSaves: undefined as HostError | undefined,
    contactResolution: undefined as DesktopSnapshot["contactResolution"],
    contactBooks: [] as ContactBookView[],
    notifications: [] as DesktopSnapshot["notifications"],
    appFilters: [] as DesktopSnapshot["appFilters"],
    sent: [] as Draft[],
    load(conversationId?: string): DesktopSnapshot {
      const known = [
        ...host.conversations.map((c) => c.id),
        ...host.drafts.keys(),
      ];
      const active =
        conversationId && known.includes(conversationId)
          ? conversationId
          : known[0];
      const listed = host.conversations.map((c) => ({
        ...c,
        messages: c.id === active ? c.messages : [],
      }));
      const draftOnly = [...host.drafts.values()]
        .filter(
          (d) => !host.conversations.some((c) => c.id === d.conversationId),
        )
        .map((d) => ({
          id: d.conversationId,
          name: d.recipientIds.join(", ") || "New message",
          preview: "Draft",
          unread: 0,
          messages: [],
          draftOnly: true,
          recipientlessDraft: d.recipientIds.length === 0,
        }));
      return {
        version: "1",
        mode: "native",
        connection: host.connection,
        encryption: host.encryption,
        gateways: host.gateways,
        conversations: [...listed, ...draftOnly],
        activeConversationId: active,
        draft: active ? host.drafts.get(active) : undefined,
        desktop: host.desktop,
        head: host.head,
        pendingCount: 0,
        quarantineCount: 0,
        notifications: host.notifications,
        contactResolution: host.contactResolution,
        contactBooks: host.contactBooks,
        appFilters: host.appFilters,
        notificationPreferences: {
          messageBanners: true,
          mirroredBanners: true,
          preview: "full",
        },
      };
    },
    save(input: DraftInput): Draft {
      if (host.failSaves) throw host.failSaves;
      let current: Draft | undefined;
      if (input.id === "") {
        const conversationId = input.conversationId || `conv-new-${++created}`;
        current = host.drafts.get(conversationId) ?? {
          id: `draft-${conversationId}`,
          conversationId,
          text: "",
          recipientIds: [],
          attachmentIds: [],
          revision: "0",
        };
      } else {
        current = [...host.drafts.values()].find((d) => d.id === input.id);
        if (!current)
          throw {
            code: "not-found",
            message: "The requested stored item was not found.",
          } satisfies HostError;
      }
      if (
        input.conversationId &&
        input.conversationId !== current.conversationId
      )
        throw {
          code: "invalid-draft",
          message: "The draft does not belong to this conversation.",
        } satisfies HostError;
      if (current.revision !== input.expectedRevision)
        throw {
          code: "stale-draft",
          message: `The draft changed elsewhere (current revision ${current.revision}); both versions were kept.`,
        } satisfies HostError;
      const saved: Draft = {
        ...current,
        text: input.text,
        recipientIds: input.recipientIds,
        attachmentIds: input.attachmentIds,
        gatewayId: input.gatewayId ?? current.gatewayId,
        simId: input.simId ?? current.simId,
        revision: String(Number(current.revision) + 1),
      };
      host.drafts.set(saved.conversationId, saved);
      return saved;
    },
    send(input: SendDraftInput) {
      const stored = [...host.drafts.values()].find((d) => d.id === input.id);
      if (!stored)
        throw {
          code: "not-found",
          message: "The requested stored item was not found.",
        } satisfies HostError;
      if (stored.revision !== input.expectedRevision)
        throw {
          code: "stale-draft",
          message: "The draft changed elsewhere; both versions were kept.",
        } satisfies HostError;
      host.sent.push({
        ...stored,
        gatewayId: input.gatewayId,
        simId: input.simId,
      });
      const cleared: Draft = {
        ...stored,
        text: "",
        attachmentIds: [],
        revision: String(Number(stored.revision) + 1),
      };
      host.drafts.set(cleared.conversationId, cleared);
      return {
        accepted: true,
        status: "queued-local" as const,
        revision: cleared.revision,
      };
    },
    discard(input: { id: string; conversationId: string; expectedRevision: string }) {
      const draft = host.drafts.get(input.conversationId);
      if (!draft || draft.id !== input.id || draft.revision !== input.expectedRevision) throw { code: "stale-draft", message: "The draft changed elsewhere." } satisfies HostError;
      if (draft.recipientIds.length) throw { code: "draft-not-discardable", message: "Only recipientless drafts can be deleted." } satisfies HostError;
      host.drafts.delete(input.conversationId);
    },
  };
  return host;
}

let host: ReturnType<typeof createHost>;
let hint: (() => void) | undefined;
const hintListeners = new Set<() => void>();
const broadcastHint = () => {
  for (const listener of [...hintListeners]) listener();
};
const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
};
// Native accepts a send before its response returns, so the host row clears first.
const delayAcceptedSend = () => {
  const response = deferred<void>();
  vi.mocked(bridge.send_draft).mockImplementationOnce(async (input) => {
    const accepted = host.send(input);
    await response.promise;
    return accepted;
  });
  return response;
};
// The destination row the host reports after a draft-moved rejection of the send.
const rejectSendAsMovedToRiver = () => {
  host.drafts.set("river", { id: "draft-river", conversationId: "river", text: "destination hello", recipientIds: [], attachmentIds: [], revision: "1" });
  vi.mocked(bridge.send_draft).mockRejectedValueOnce({
    code: "draft-moved",
    message: "moved",
    details: { conversationId: "river", draftId: "draft-river", currentRevision: "4" },
  });
};
const stubClipboard = () => {
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
  onTestFinished(() => {
    delete (navigator as { clipboard?: unknown }).clipboard;
  });
  return writeText;
};
const message = () => screen.getByLabelText("Message") as HTMLTextAreaElement;
const type = (value: string) =>
  fireEvent.change(message(), { target: { value } });
const routeValue = (gateway: GatewayView) =>
  `${encodeURIComponent(gateway.id)} ${encodeURIComponent(gateway.simId)}`;
const chooseGateway = async (gateway: GatewayView) => {
  fireEvent.click(screen.getByRole("combobox", { name: "Phone" }));
  const option = (await screen.findAllByRole("option")).find(
    (candidate) => candidate.getAttribute("data-gateway-choice") === routeValue(gateway),
  );
  if (!option) throw new Error(`Gateway choice was not rendered: ${routeValue(gateway)}`);
  fireEvent.click(option);
};
const openComposerWindow = (conversationId: string, head = false) =>
  window.history.replaceState(
    {},
    "",
    `/?window=composer&conversationId=${conversationId}${head ? "&head=1" : ""}`,
  );
class TestResizeObserver {
  static observers: TestResizeObserver[] = [];
  readonly targets = new Set<Element>();

  constructor(private readonly callback: ResizeObserverCallback) {
    TestResizeObserver.observers.push(this);
  }

  observe(target: Element) {
    this.targets.add(target);
  }

  disconnect() {
    this.targets.clear();
  }

  static resize(...targets: Element[]) {
    for (const observer of TestResizeObserver.observers) {
      const entries = targets
        .filter((target) => observer.targets.has(target))
        .map((target) => ({ target } as ResizeObserverEntry));
      if (entries.length) observer.callback(entries, observer as unknown as ResizeObserver);
    }
  }

  static reset() {
    TestResizeObserver.observers = [];
  }
}
const mockMessageListGeometry = (geometry: { overlay: number; viewport: number; scrollHeight?: number }) => {
  const offsetHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight");
  const clientHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight");
  const scrollHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollHeight");
  Object.defineProperties(HTMLElement.prototype, {
    offsetHeight: {
      configurable: true,
      get() { return this.id === "composer-area" ? geometry.overlay : 0; },
    },
    clientHeight: {
      configurable: true,
      get() { return this.id === "message-list" ? geometry.viewport : 0; },
    },
    scrollHeight: {
      configurable: true,
      get() { return this.id === "message-list" ? geometry.scrollHeight ?? 1000 : 0; },
    },
  });
  return () => {
    for (const [name, descriptor] of [["offsetHeight", offsetHeight], ["clientHeight", clientHeight], ["scrollHeight", scrollHeight]] as const) {
      if (descriptor) Object.defineProperty(HTMLElement.prototype, name, descriptor);
      else delete (HTMLElement.prototype as unknown as Record<string, unknown>)[name];
    }
  };
};
let lifecycle: ((request: { id: string; action: "quit" | "close" | "collapse" }) => void) | undefined;
let lifecycleDispose: ReturnType<typeof vi.fn>;
let lifecycleFinished: ((result: { id: string; ok: boolean }) => void) | undefined;
let lifecycleFinishedDispose: ReturnType<typeof vi.fn>;
let draftHint: ((draft: Draft) => void) | undefined;
let draftDispose: ReturnType<typeof vi.fn>;

beforeEach(() => {
  const values = new Map<string, string>();
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
      removeItem: (key: string) => values.delete(key),
      clear: () => values.clear(),
    },
  });
  vi.restoreAllMocks();
  host = createHost();
  hint = undefined;
  hintListeners.clear();
  lifecycle = undefined;
  lifecycleDispose = vi.fn();
  lifecycleFinished = undefined;
  lifecycleFinishedDispose = vi.fn();
  draftHint = undefined;
  draftDispose = vi.fn();
  vi.spyOn(bridge, "load_state").mockImplementation(async (id) =>
    host.load(id),
  );
  vi.spyOn(bridge, "save_draft").mockImplementation(async (input) =>
    host.save(input),
  );
  vi.spyOn(bridge, "send_draft").mockImplementation(async (input) =>
    host.send(input),
  );
  vi.spyOn(bridge, "discard_draft").mockImplementation(async (input) => host.discard(input));
  vi.spyOn(bridge, "pick_attachments").mockResolvedValue([
    {
      id: "file-1",
      name: "file.png",
      mediaType: "image/png",
      byteSize: 2,
      state: "ready",
    },
  ]);
  vi.spyOn(bridge, "retry_attachment").mockResolvedValue();
  vi.spyOn(bridge, "save_attachment").mockResolvedValue(true);
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  vi.spyOn(bridge, "publish_attachment").mockResolvedValue(null);
  vi.spyOn(bridge, "open_composer").mockResolvedValue();
  vi.spyOn(bridge, "close_composer").mockResolvedValue();
  vi.spyOn(bridge, "close_head_panel").mockResolvedValue();
  vi.spyOn(bridge, "popout_conversation").mockResolvedValue({ headCreated: true });
  vi.spyOn(bridge, "set_start_at_login").mockResolvedValue();
  vi.spyOn(bridge, "acknowledge_lifecycle").mockResolvedValue();
  vi.spyOn(bridge, "subscribe_lifecycle").mockImplementation(listener => {
    lifecycle = listener;
    return lifecycleDispose;
  });
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockImplementation(listener => {
    lifecycleFinished = listener;
    return lifecycleFinishedDispose;
  });
  vi.spyOn(bridge, "subscribe").mockImplementation((listener) => {
    hintListeners.add(listener);
    hint = broadcastHint;
    return () => { hintListeners.delete(listener); };
  });
  const draftBridge = bridge as typeof bridge & { subscribe_drafts(listener: (draft: Draft) => void): () => void };
  vi.spyOn(draftBridge, "subscribe_drafts").mockImplementation(listener => {
    draftHint = listener;
    return draftDispose;
  });
});
afterEach(() => {
  cleanup();
  localStorage.removeItem("peppy.layout.v1");
  window.history.replaceState({}, "", "/");
  vi.unstubAllGlobals();
});

describe("lazy conversation history", () => {
  it("marks the initial conversation load busy", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    const refresh = deferred<DesktopSnapshot>();
    vi.mocked(bridge.load_state).mockReturnValueOnce(refresh.promise);
    fireEvent.click(screen.getByRole("button", { name: /River/ }));

    expect(await screen.findByRole("log", { name: "Messages" })).toHaveAttribute("aria-busy", "true");
  });

  it("loads older history after an upward scroll enters one list viewport from the top", async () => {
    const restoreGeometry = mockMessageListGeometry({ overlay: 0, viewport: 100, scrollHeight: 1_000 });
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    const page = deferred<{ conversationId: string; messages: ConversationView["messages"]; hasMore: boolean }>();
    const loadHistory = vi.spyOn(bridge, "load_history").mockReturnValue(page.promise);
    try {
      render(<App />);
      const list = await screen.findByRole("log", { name: "Messages" });
      await screen.findByRole("button", { name: "Load older messages" });
      await waitFor(() => expect(list).not.toHaveAttribute("data-scroll-programmatic"));
      list.scrollTop = 300;
      fireEvent.scroll(list);
      list.scrollTop = 100;
      fireEvent.scroll(list);

      await waitFor(() => expect(loadHistory).toHaveBeenCalledOnce());
      list.scrollTop = 0;
      fireEvent.scroll(list);
      expect(loadHistory).toHaveBeenCalledOnce();

      await act(async () => page.resolve({ conversationId: conversation.id, messages: [], hasMore: true }));
      expect(loadHistory).toHaveBeenCalledOnce();
    } finally {
      restoreGeometry();
    }
  });

  it("does not auto-load history for downward or programmatic scrolls", async () => {
    const restoreGeometry = mockMessageListGeometry({ overlay: 0, viewport: 100, scrollHeight: 1_000 });
    host.conversations[0].hasOlderMessages = true;
    const loadHistory = vi.spyOn(bridge, "load_history").mockResolvedValue({
      conversationId: "aurora",
      messages: [],
      hasMore: true,
    });
    try {
      render(<App />);
      const list = await screen.findByRole("log", { name: "Messages" });
      await screen.findByRole("button", { name: "Load older messages" });
      await waitFor(() => expect(list).not.toHaveAttribute("data-scroll-programmatic"));
      list.setAttribute("data-scroll-programmatic", "");
      list.scrollTop = 50;
      fireEvent.scroll(list);
      list.removeAttribute("data-scroll-programmatic");
      list.scrollTop = 150;
      fireEvent.scroll(list);
      list.setAttribute("data-scroll-programmatic", "");
      list.scrollTop = 0;
      fireEvent.scroll(list);

      expect(loadHistory).not.toHaveBeenCalled();
    } finally {
      restoreGeometry();
    }
  });

  it("loads history from upward wheel input at the top of a short list", async () => {
    const restoreGeometry = mockMessageListGeometry({ overlay: 0, viewport: 100, scrollHeight: 100 });
    host.conversations[0].hasOlderMessages = true;
    const loadHistory = vi.spyOn(bridge, "load_history").mockResolvedValue({
      conversationId: "aurora",
      messages: [],
      hasMore: false,
    });
    try {
      render(<App />);
      const list = await screen.findByRole("log", { name: "Messages" });
      await screen.findByRole("button", { name: "Load older messages" });
      await waitFor(() => expect(list).not.toHaveAttribute("data-scroll-programmatic"));
      list.scrollTop = 0;
      fireEvent.wheel(list, { deltaY: -1 });

      await waitFor(() => expect(loadHistory).toHaveBeenCalledOnce());
    } finally {
      restoreGeometry();
    }
  });

  it("blocks automatic history input after an error until Retry is explicitly activated", async () => {
    const restoreGeometry = mockMessageListGeometry({ overlay: 0, viewport: 100, scrollHeight: 1_000 });
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    const loadHistory = vi.spyOn(bridge, "load_history")
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce({ conversationId: conversation.id, messages: [], hasMore: false });
    try {
      render(<App />);
      const list = await screen.findByRole("log", { name: "Messages" });
      fireEvent.click(await screen.findByRole("button", { name: "Load older messages" }));
      await screen.findByRole("button", { name: "Retry loading older messages" });
      list.scrollTop = 0;
      fireEvent.scroll(list);
      fireEvent.wheel(list, { deltaY: -1 });
      expect(loadHistory).toHaveBeenCalledOnce();

      fireEvent.click(screen.getByRole("button", { name: "Retry loading older messages" }));
      await waitFor(() => expect(loadHistory).toHaveBeenCalledTimes(2));
    } finally {
      restoreGeometry();
    }
  });

  it("ignores downward wheels and duplicate upward wheels while a history page is in flight", async () => {
    const restoreGeometry = mockMessageListGeometry({ overlay: 0, viewport: 100, scrollHeight: 1_000 });
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    const page = deferred<{ conversationId: string; messages: ConversationView["messages"]; hasMore: boolean }>();
    const loadHistory = vi.spyOn(bridge, "load_history").mockReturnValue(page.promise);
    try {
      render(<App />);
      const list = await screen.findByRole("log", { name: "Messages" });
      await screen.findByRole("button", { name: "Load older messages" });
      await waitFor(() => expect(list).not.toHaveAttribute("data-scroll-programmatic"));
      list.scrollTop = 0;
      fireEvent.wheel(list, { deltaY: 1 });
      expect(loadHistory).not.toHaveBeenCalled();
      fireEvent.wheel(list, { deltaY: -1 });
      await waitFor(() => expect(loadHistory).toHaveBeenCalledOnce());
      fireEvent.wheel(list, { deltaY: -1 });
      expect(loadHistory).toHaveBeenCalledOnce();
      await act(async () => page.resolve({ conversationId: conversation.id, messages: [], hasMore: false }));
    } finally {
      restoreGeometry();
    }
  });

  it("stops automatic history input after exhaustion", async () => {
    const restoreGeometry = mockMessageListGeometry({ overlay: 0, viewport: 100, scrollHeight: 1_000 });
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    const loadHistory = vi.spyOn(bridge, "load_history").mockResolvedValue({ conversationId: conversation.id, messages: [], hasMore: false });
    try {
      render(<App />);
      const list = await screen.findByRole("log", { name: "Messages" });
      await screen.findByRole("button", { name: "Load older messages" });
      await waitFor(() => expect(list).not.toHaveAttribute("data-scroll-programmatic"));
      list.scrollTop = 0;
      fireEvent.wheel(list, { deltaY: -1 });
      await waitFor(() => expect(loadHistory).toHaveBeenCalledOnce());
      await screen.findByText("Beginning of conversation");
      list.scrollTop = 0;
      fireEvent.scroll(list);
      fireEvent.wheel(list, { deltaY: -1 });
      expect(loadHistory).toHaveBeenCalledOnce();
    } finally {
      restoreGeometry();
    }
  });

  it("loads a second history page only after a fresh upward wheel", async () => {
    const restoreGeometry = mockMessageListGeometry({ overlay: 0, viewport: 100, scrollHeight: 1_000 });
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    const loadHistory = vi.spyOn(bridge, "load_history")
      .mockResolvedValueOnce({ conversationId: conversation.id, messages: [], hasMore: true })
      .mockResolvedValueOnce({ conversationId: conversation.id, messages: [], hasMore: false });
    try {
      render(<App />);
      const list = await screen.findByRole("log", { name: "Messages" });
      await screen.findByRole("button", { name: "Load older messages" });
      await waitFor(() => expect(list).not.toHaveAttribute("data-scroll-programmatic"));
      list.scrollTop = 0;
      fireEvent.wheel(list, { deltaY: -1 });
      await waitFor(() => expect(loadHistory).toHaveBeenCalledOnce());
      await waitFor(() => expect(document.getElementById("history-load-band")).toHaveAttribute("data-history-state", "idle"));
      await waitFor(() => expect(list).not.toHaveAttribute("data-scroll-programmatic"));
      expect(loadHistory).toHaveBeenCalledOnce();
      fireEvent.wheel(list, { deltaY: -1 });
      await waitFor(() => expect(loadHistory).toHaveBeenCalledTimes(2));
    } finally {
      restoreGeometry();
    }
  });

  it("clears a detached older-page pending state and reports a mismatched conversation response", async () => {
    const conversation = host.conversations[0];
    const target = conversation.messages[0];
    const older = deferred<Awaited<ReturnType<typeof bridge.load_message_window>>>();
    vi.spyOn(bridge, "search_local").mockResolvedValue({
      query: "target",
      items: [{ kind: "message", conversationId: conversation.id, messageId: target.id, title: "Aurora", snippet: target.body, timestamp: target.timestamp }],
      indexState: "ready",
      hasMore: { conversation: false, message: false, contact: false },
    });
    vi.spyOn(bridge, "load_message_window")
      .mockResolvedValueOnce({ conversationId: conversation.id, messages: [target], hasOlder: true, hasNewer: true })
      .mockReturnValueOnce(older.promise);
    render(<App />);
    await screen.findByText(target.body);
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "target" } });
    const result = await waitFor(() => {
      const hit = document.querySelector<HTMLElement>("[data-search-hit]");
      expect(hit).not.toBeNull();
      return hit!;
    });
    fireEvent.click(result!);
    await screen.findByRole("button", { name: "Load older messages" });

    fireEvent.click(screen.getByRole("button", { name: "Load older messages" }));
    expect((await screen.findByRole("button", { name: "Load newer messages" }))).toBeDisabled();
    await act(async () => older.resolve({ conversationId: "river", messages: [], hasOlder: true, hasNewer: true }));
    await screen.findByRole("alert");
    expect(screen.getByRole("button", { name: "Load newer messages" })).not.toBeDisabled();
  });

  it("keeps keyboard focus in history controls while loading and returns it to the list at exhaustion", async () => {
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    const page = deferred<{ conversationId: string; messages: ConversationView["messages"]; hasMore: boolean }>();
    vi.spyOn(bridge, "load_history").mockReturnValue(page.promise);
    render(<App />);

    const load = await screen.findByRole("button", { name: "Load older messages" });
    load.focus();
    fireEvent.click(load);

    const status = await waitFor(() => {
      const indicator = document.getElementById("history-load-indicator");
      expect(indicator).toHaveAttribute("role", "status");
      return indicator!;
    });
    expect(document.activeElement).toBe(document.getElementById("history-load-band"));
    expect(status.closest("[aria-busy='true']")).toBeNull();

    await act(async () => page.resolve({ conversationId: conversation.id, messages: [], hasMore: false }));
    await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("log", { name: "Messages" })));
  });

  it("does not steal focus after a manual history load when the reader moves to the composer", async () => {
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    const page = deferred<{ conversationId: string; messages: ConversationView["messages"]; hasMore: boolean }>();
    vi.spyOn(bridge, "load_history").mockReturnValue(page.promise);
    render(<App />);

    const load = await screen.findByRole("button", { name: "Load older messages" });
    load.focus();
    fireEvent.click(load);
    const composer = await screen.findByLabelText("Message");
    composer.focus();
    await act(async () => page.resolve({ conversationId: conversation.id, messages: [], hasMore: false }));

    await waitFor(() => expect(document.activeElement).toBe(composer));
  });

  it("adopts a draft payload without loading state again", async () => {
    render(<App />);
    await screen.findByLabelText("Message");
    await screen.findByText("Hello from Aurora");
    const loadCount = vi.mocked(bridge.load_state).mock.calls.length;

    await act(async () => draftHint?.({
      id: "draft-aurora",
      conversationId: "aurora",
      text: "Saved in another window",
      recipientIds: [],
      attachmentIds: [],
      revision: "1",
    }));

    expect(message()).toHaveValue("Saved in another window");
    expect(bridge.load_state).toHaveBeenCalledTimes(loadCount);
  });

  it("does not adopt a locked draft payload into an existing clean slot after unlock", async () => {
    host.drafts.set("aurora", {
      id: "draft-aurora",
      conversationId: "aurora",
      text: "clean stored text",
      recipientIds: [],
      attachmentIds: [],
      revision: "1",
    });
    render(<App />);
    await waitFor(() => expect(message()).toHaveValue("clean stored text"));
    host.encryption = { state: "locked" };
    await act(async () => hint?.());
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Search Peppy" })).toBeDisabled());

    await act(async () => draftHint?.({
      id: "draft-aurora",
      conversationId: "aurora",
      text: "stale locked payload",
      recipientIds: [],
      attachmentIds: [],
      revision: "99",
    }));
    host.encryption = { state: "unlocked" };
    await act(async () => hint?.());
    await waitFor(() => expect(message()).toHaveValue("clean stored text"));
    expect(message()).not.toHaveValue("stale locked payload");
  });

  it("disposes the draft subscription on unmount", async () => {
    const view = render(<App />);
    await screen.findByLabelText("Message");
    view.unmount();
    expect(draftDispose).toHaveBeenCalledOnce();
  });

  it("coalesces state-event refreshes into one trailing load", async () => {
    const first = deferred<DesktopSnapshot>();
    const second = deferred<DesktopSnapshot>();
    const snapshot = await fixtureBridge.load_state();

    render(<App />);
    await screen.findByLabelText("Message");
    const callsBeforeRefresh = vi.mocked(bridge.load_state).mock.calls.length;
    vi.mocked(bridge.load_state)
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    await act(async () => {
      hint?.();
    });
    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledTimes(callsBeforeRefresh + 1));
    await act(async () => {
      hint?.();
      hint?.();
    });
    expect(bridge.load_state).toHaveBeenCalledTimes(callsBeforeRefresh + 1);

    await act(async () => first.resolve(snapshot));
    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledTimes(callsBeforeRefresh + 2));
    await act(async () => second.resolve(snapshot));
  });

  it("renders compact message metadata with truthful delivery and phone-history timing", async () => {
    localStorage.setItem("peppy.layout.v1", JSON.stringify({ compactTimestamps: false }));
    const incoming = host.conversations[0].messages[0];
    Object.assign(incoming, { timestamp: "1788435180000", attachments: [] });
    host.conversations[0].messages.push(
      {
        ...incoming,
        id: "m-delivered-time",
        sender: "self",
        body: "Delivered with evidence",
        timestamp: "1788435100000",
        status: "delivery-confirmed",
        statusAt: "1788435180000",
      },
      {
        ...incoming,
        id: "m-delivered-provider-time",
        sender: "self",
        body: "Delivered with provider time",
        timestamp: "1788435100000",
        status: "delivery-confirmed",
        statusAt: "not-a-date",
      },
      {
        ...incoming,
        id: "m-delivered-without-time",
        sender: "self",
        body: "Delivered without timing evidence",
        timestamp: "not-a-date",
        status: "delivery-confirmed",
        statusAt: undefined,
      },
      {
        ...incoming,
        id: "m-phone-history",
        sender: "self",
        body: "Phone history",
        timestamp: "1788435100000",
        status: undefined,
      },
      {
        ...incoming,
        id: "m-incoming-history",
        sender: "other",
        body: "Incoming history",
        timestamp: "1788435100000",
        status: undefined,
      },
      {
        ...incoming,
        id: "m-sent-without-status-time",
        sender: "self",
        body: "Sent without a status time",
        timestamp: "1788435100000",
        status: "sent",
        statusAt: undefined,
      },
    );
    render(<App />);

    await screen.findByText("Delivered with evidence");
    const deliveredMeta = document.querySelector<HTMLElement>('[data-message-meta="m-delivered-time"]')!;
    const deliveredIcon = within(deliveredMeta).getByRole("img", { name: "Delivery confirmed" });
    expect(deliveredIcon).toHaveAttribute("title", "Delivery confirmed");
    expect(deliveredMeta).toHaveTextContent(formatDateTime("1788435180000")!);
    expect(deliveredMeta).not.toHaveTextContent("Delivery confirmed");
    expect(deliveredMeta.querySelector(".message-meta-separator")).not.toBeInTheDocument();
    expect(deliveredMeta.querySelector("time")!.compareDocumentPosition(deliveredIcon) & Node.DOCUMENT_POSITION_FOLLOWING)
      .toBe(Node.DOCUMENT_POSITION_FOLLOWING);

    const providerTimeMeta = document.querySelector<HTMLElement>('[data-message-meta="m-delivered-provider-time"]')!;
    const providerTimeIcon = within(providerTimeMeta).getByRole("img", { name: /sent time shown because delivery time is unavailable/i });
    expect(providerTimeIcon)
      .toHaveAttribute("title", "Delivery confirmed; sent time shown because delivery time is unavailable");
    expect(providerTimeMeta.querySelector("time")).toHaveTextContent(formatDateTime("1788435100000")!);
    expect(providerTimeMeta.querySelector("time")!.compareDocumentPosition(providerTimeIcon) & Node.DOCUMENT_POSITION_FOLLOWING)
      .toBe(Node.DOCUMENT_POSITION_FOLLOWING);

    const unavailableTimeMeta = document.querySelector<HTMLElement>('[data-message-meta="m-delivered-without-time"]')!;
    expect(within(unavailableTimeMeta).getByRole("img", { name: /time unavailable/i }))
      .toHaveAttribute("title", "Delivery confirmed; time unavailable");
    expect(unavailableTimeMeta.querySelector("time")).not.toBeInTheDocument();

    const phoneHistoryIcon = document.querySelector<HTMLElement>('[data-message-phone-icon="m-phone-history"]')!;
    expect(phoneHistoryIcon).toHaveAttribute("role", "img");
    expect(phoneHistoryIcon).toHaveAttribute("aria-label", "From phone history");
    expect(phoneHistoryIcon).toHaveAttribute("title", "From phone history");
    expect(document.querySelector('[data-message-phone-icon="m-incoming-history"]')).not.toBeInTheDocument();
    expect(document.querySelector('[data-message-phone-icon="m-sent-without-status-time"]')).not.toBeInTheDocument();
    expect(document.querySelector('[data-message-meta="m-sent-without-status-time"] time')).not.toBeInTheDocument();
  });

  it("renders receipt and status evidence below their bubbles without exposing routing metadata", async () => {
    localStorage.setItem("peppy.layout.v1", JSON.stringify({ compactTimestamps: false }));
    const incoming = host.conversations[0].messages[0];
    Object.assign(incoming, {
      timestamp: "1788435180000",
      subject: "Estimate",
      transport: "mms",
      participants: ["+12025550123"],
    });
    host.conversations[0].messages.push(
      {
        ...incoming,
        id: "m-sent",
        sender: "self",
        body: "Sent from status evidence",
        timestamp: "1788435100000",
        status: "sent",
        statusAt: "1788435180000",
        subject: undefined,
        transport: undefined,
        participants: undefined,
        attachments: [],
      },
      {
        ...incoming,
        id: "m-queued",
        sender: "self",
        body: "Queued locally",
        status: "queued-local",
        statusAt: "1788435180000",
        attachments: [],
      },
      {
        ...incoming,
        id: "m-failed",
        sender: "self",
        body: "Failed",
        status: "failed-confirmed",
        statusAt: "1788435180000",
        attachments: [],
      },
      {
        ...incoming,
        id: "m-unknown",
        sender: "self",
        body: "Unknown",
        status: "unknown",
        statusAt: "1788435180000",
        attachments: [],
      },
      {
        ...incoming,
        id: "m-sent-without-status-time",
        sender: "self",
        body: "Sent without a status time",
        timestamp: "1788435180000",
        status: "sent",
        statusAt: undefined,
        attachments: [],
      },
      {
        ...incoming,
        id: "m-provider-history",
        sender: "self",
        body: "Provider history",
        timestamp: "1788435180000",
        status: undefined,
        statusAt: undefined,
        attachments: [],
      },
      {
        ...incoming,
        id: "m-invalid",
        sender: "other",
        body: "Invalid timestamp",
        timestamp: "not-a-date",
        status: undefined,
        attachments: [],
      },
      {
        ...incoming,
        id: "m-attachment-only",
        body: "",
        subject: undefined,
        attachments: incoming.attachments,
      },
    );
    render(<App />);

    await screen.findByText("Hello from Aurora");
    const incomingBubble = document.querySelector<HTMLElement>('[data-message-id="m-aurora"]')?.closest("article")!;
    const incomingMeta = document.querySelector<HTMLElement>('[data-message-meta="m-aurora"]')!;
    const sentMeta = document.querySelector<HTMLElement>('[data-message-meta="m-sent"]')!;
    expect(incomingMeta.parentElement).toHaveAttribute("data-message-row", "m-aurora");
    expect(incomingMeta).toHaveTextContent(formatDateTime("1788435180000")!);
    expect(incomingMeta.querySelector("time")).toHaveAttribute("dateTime", isoDateTime("1788435180000")!);
    expect(sentMeta).toHaveAttribute("data-status", "sent");
    expect(sentMeta).toHaveTextContent(formatDateTime("1788435180000")!);
    expect(sentMeta).not.toHaveTextContent(formatDateTime("1788435100000")!);
    expect(within(sentMeta).getByRole("img", { name: "Sent" })).toHaveAttribute("title", "Sent");
    expect(sentMeta.querySelector(".message-meta-separator")).not.toBeInTheDocument();
    expect(document.querySelector('[data-message-meta="m-queued"] time')).not.toBeInTheDocument();
    expect(document.querySelector('[data-message-meta="m-queued"]')).toHaveTextContent("Queued locally");
    expect(document.querySelector('[data-message-meta="m-failed"]')).toHaveTextContent("Failed");
    expect(document.querySelector('[data-message-meta="m-unknown"]')).toHaveTextContent("Unknown delivery state — not retried");
    const sentWithoutTime = document.querySelector<HTMLElement>('[data-message-meta="m-sent-without-status-time"]')!;
    expect(sentWithoutTime.querySelector("time")).not.toBeInTheDocument();
    expect(sentWithoutTime).toHaveTextContent("Sent");
    expect(document.querySelector('[data-message-meta="m-provider-history"] time')).toHaveTextContent(formatDateTime("1788435180000")!);
    expect(document.querySelector('[data-message-meta="m-provider-history"] time')).toHaveAttribute("dateTime", isoDateTime("1788435180000")!);
    expect(document.querySelector('[data-message-meta="m-invalid"]')).not.toBeInTheDocument();
    expect(within(incomingBubble).getByText("Subject: Estimate")).toBeInTheDocument();
    expect(screen.queryByText(/Transport: MMS/)).not.toBeInTheDocument();
    expect(screen.queryByText(/Participants:/)).not.toBeInTheDocument();
    expect(incomingBubble.querySelector('[data-attachment-id="photo-1"]')).toBeInTheDocument();
    expect(document.querySelector('[data-message-id="m-attachment-only"] p')).not.toBeInTheDocument();
  });

  it("uses icon-only progress receipts when their status time is valid", async () => {
    const incoming = host.conversations[0].messages[0];
    const cases = [
      ["gateway-persisted", "Received by phone"],
      ["preparing", "Preparing"],
      ["submitted", "Submitted"],
    ] as const;
    host.conversations[0].messages.push(...cases.map(([status], index) => ({
      ...incoming,
      id: `m-${status}`,
      sender: "self" as const,
      body: status,
      status,
      statusAt: String(1788435180000 + index),
      attachments: [],
    })));
    render(<App />);

    for (const [status, label] of cases) {
      const meta = await waitFor(() => {
        const element = document.querySelector<HTMLElement>(`[data-message-meta="m-${status}"]`);
        if (!element) throw new Error(`Missing ${status} receipt metadata`);
        return element;
      });
      expect(within(meta).getByRole("img", { name: label })).toHaveAttribute("title", label);
      expect(meta).not.toHaveTextContent(label);
      expect(meta.querySelector(".message-meta-separator")).not.toBeInTheDocument();
    }
  });

  it("suppresses earlier burst times by default and immediately restores them when disabled", async () => {
    const first = host.conversations[0].messages[0];
    Object.assign(first, { timestamp: "1788435100000", attachments: [] });
    host.conversations[0].messages.push({
      ...first,
      id: "m-phone-history",
      sender: "self",
      body: "Phone history",
      timestamp: "1788435110000",
      status: undefined,
    }, {
      ...first,
      id: "m-delivery-fallback",
      sender: "self",
      body: "Delivery fallback",
      timestamp: "1788435120000",
      status: "delivery-confirmed",
      statusAt: "not-a-date",
    }, {
      ...first,
      id: "m-sent-progress",
      sender: "self",
      body: "Sent progress",
      timestamp: "1788435130000",
      status: "sent",
      statusAt: "1788435130000",
    }, {
      ...first,
      id: "m-burst-keeper",
      body: "Later message",
      timestamp: "1788435180000",
    });

    const view = render(<App />);
    await screen.findByText("Later message");

    const hiddenBubble = document.querySelector<HTMLElement>('[data-message-id="m-aurora"]')!;
    expect(document.querySelector('[data-message-meta="m-aurora"]')).not.toBeInTheDocument();
    expect(hiddenBubble).toHaveAttribute("title", formatDateTime("1788435100000")!);
    expect(document.querySelector('[data-message-meta="m-burst-keeper"] time')).toHaveTextContent(formatDateTime("1788435180000")!);
    const phoneHistory = document.querySelector<HTMLElement>('[data-message-meta="m-phone-history"]')!;
    expect(within(phoneHistory).getByRole("img", { name: "From phone history" })).toBeInTheDocument();
    expect(phoneHistory.querySelector("time")).not.toBeInTheDocument();
    const deliveryFallback = document.querySelector<HTMLElement>('[data-message-meta="m-delivery-fallback"]')!;
    expect(within(deliveryFallback).getByRole("img", { name: "Delivery confirmed; delivery time unavailable" })).toHaveAttribute("title", "Delivery confirmed; delivery time unavailable");
    expect(deliveryFallback.querySelector("time")).not.toBeInTheDocument();
    const sentProgress = document.querySelector<HTMLElement>('[data-message-meta="m-sent-progress"]')!;
    expect(within(sentProgress).getByRole("img", { name: "Sent" })).toBeInTheDocument();
    expect(sentProgress.querySelector("time")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const compactTimestamps = await screen.findByRole("checkbox", { name: "Show times only after a pause" });
    expect(compactTimestamps).toBeChecked();
    fireEvent.click(compactTimestamps);
    fireEvent.click(screen.getByRole("button", { name: "Conversations" }));

    expect(document.querySelector('[data-message-meta="m-aurora"] time')).toHaveTextContent(formatDateTime("1788435100000")!);
    expect(document.querySelector<HTMLElement>('[data-message-id="m-aurora"]')).not.toHaveAttribute("title");
    expect(JSON.parse(localStorage.getItem("peppy.layout.v1")!)).toMatchObject({ compactTimestamps: false });

    view.unmount();
    render(<App />);
    await screen.findByText("Later message");
    expect(document.querySelector('[data-message-meta="m-aurora"] time')).toHaveTextContent(formatDateTime("1788435100000")!);
  });

  it("retains an eligible keeper behind a trailing queued message", async () => {
    const eligible = host.conversations[0].messages[0];
    Object.assign(eligible, { timestamp: "1788435100000", attachments: [] });
    host.conversations[0].messages.push({
      ...eligible,
      id: "m-queued-trailing",
      sender: "self",
      body: "Queued trailing",
      timestamp: "1788435110000",
      status: "queued-local",
    });

    render(<App />);

    await screen.findByText("Queued trailing");
    expect(document.querySelector('[data-message-meta="m-aurora"] time')).toHaveTextContent(formatDateTime("1788435100000")!);
    expect(document.querySelector('[data-message-meta="m-queued-trailing"] time')).not.toBeInTheDocument();
  });

  it("uses timestamps for bursts while live status and arrival changes replace the keeper", async () => {
    const eligible = host.conversations[0].messages[0];
    Object.assign(eligible, { timestamp: "1788435100000", attachments: [] });
    const transitioning = {
      ...eligible,
      id: "m-transitioning",
      sender: "self" as const,
      body: "Transitioning",
      timestamp: "1788435110000",
      status: "queued-local" as const,
    };
    host.conversations[0].messages.push(transitioning);

    render(<App />);
    await screen.findByText("Transitioning");
    expect(document.querySelector('[data-message-meta="m-aurora"] time')).toHaveTextContent(formatDateTime("1788435100000")!);

    Object.assign(transitioning, { status: "sent" as const, statusAt: "1788445100000" });
    await act(async () => hint?.());
    await waitFor(() => {
      expect(document.querySelector('[data-message-meta="m-aurora"] time')).not.toBeInTheDocument();
      expect(document.querySelector('[data-message-meta="m-transitioning"] time')).toHaveTextContent(formatDateTime("1788445100000")!);
    });

    host.conversations[0].messages.push({
      ...eligible,
      id: "m-new-arrival",
      body: "New arrival",
      timestamp: "1788435120000",
    });
    await act(async () => hint?.());
    await waitFor(() => {
      expect(document.querySelector('[data-message-meta="m-transitioning"] time')).not.toBeInTheDocument();
      expect(document.querySelector('[data-message-meta="m-new-arrival"] time')).toHaveTextContent(formatDateTime("1788435120000")!);
    });

    host.conversations[0].messages.push({
      ...eligible,
      id: "m-failed-hidden",
      sender: "self",
      body: "Failed",
      timestamp: "1788435130000",
      status: "failed-confirmed",
      statusAt: "1788435130000",
    }, {
      ...eligible,
      id: "m-final-keeper",
      body: "Final keeper",
      timestamp: "1788435140000",
    });
    await act(async () => hint?.());
    await waitFor(() => {
      const failed = document.querySelector<HTMLElement>('[data-message-meta="m-failed-hidden"]')!;
      expect(failed).toHaveTextContent("Failed");
      expect(failed.querySelector("time")).not.toBeInTheDocument();
      expect(failed.querySelector(".message-meta-separator")).not.toBeInTheDocument();
      expect(document.querySelector('[data-message-meta="m-final-keeper"] time')).toHaveTextContent(formatDateTime("1788435140000")!);
    });
  });

  it("retains only valid loaded image previews across an anchored state refresh", async () => {
    const conversation = host.conversations[0];
    conversation.messages[0].attachments = [
      { ...conversation.messages[0].attachments[0], id: "kept-preview", name: "kept.png", previewUrl: "blob:kept" },
      { ...conversation.messages[0].attachments[0], id: "not-ready-preview", name: "not-ready.png", previewUrl: "blob:not-ready" },
      { ...conversation.messages[0].attachments[0], id: "removed-preview", name: "removed.png", previewUrl: "blob:removed" },
    ];
    render(<App />);
    expect(await screen.findByRole("img", { name: "kept.png" })).toHaveAttribute("src", "blob:kept");

    const refreshed = structuredClone(host.load(conversation.id));
    refreshed.conversations[0].messages[0].attachments = [
      { ...conversation.messages[0].attachments[0], previewUrl: undefined },
      { ...conversation.messages[0].attachments[1], state: "failed", previewUrl: undefined },
    ];
    vi.mocked(bridge.load_state).mockResolvedValueOnce(refreshed);
    await act(async () => hint?.());

    expect(bridge.load_state).toHaveBeenLastCalledWith(conversation.id, conversation.messages[0].id);
    expect(screen.getByRole("img", { name: "kept.png" })).toHaveAttribute("src", "blob:kept");
    expect(screen.queryByRole("img", { name: "not-ready.png" })).not.toBeInTheDocument();
    expect(document.querySelector('[data-attachment-id="removed-preview"]')).not.toBeInTheDocument();
  });

  it("requests subscription refreshes from the first loaded message anchor", async () => {
    const conversation = host.conversations[0];
    conversation.messages.unshift({
      ...conversation.messages[0],
      id: "m-first-loaded",
      body: "First loaded message",
      attachments: [],
    });
    render(<App />);
    await screen.findByText("First loaded message");

    await act(async () => hint?.());

    expect(bridge.load_state).toHaveBeenLastCalledWith(conversation.id, "m-first-loaded");
  });

  it("preserves the visible message position when a subscription refresh prepends history", async () => {
    const restoreGeometry = mockMessageListGeometry({ overlay: 0, viewport: 100 });
    try {
      const conversation = host.conversations[0];
      render(<App />);
      await screen.findByText("Hello from Aurora");
      const messageRow = document.querySelector<HTMLElement>('[data-message-id="m-aurora"]')!;
      const list = document.getElementById("message-list") as HTMLDivElement;
      vi.spyOn(list, "getBoundingClientRect").mockReturnValue({ top: 0, bottom: 100 } as DOMRect);
      vi.spyOn(messageRow, "getBoundingClientRect").mockImplementation(() => ({
        top: document.querySelector('[data-message-id="m-prepended"]') ? 70 : 20,
        bottom: document.querySelector('[data-message-id="m-prepended"]') ? 90 : 40,
      } as DOMRect));
      list.scrollTop = 200;
      fireEvent.scroll(list);

      const refreshed = host.load(conversation.id);
      refreshed.conversations[0].messages = [
        { ...conversation.messages[0], id: "m-prepended", body: "Prepended history", attachments: [] },
        ...refreshed.conversations[0].messages,
      ];
      vi.mocked(bridge.load_state).mockResolvedValueOnce(refreshed);

      await act(async () => hint?.());

      expect(list.scrollTop).toBe(250);
    } finally {
      restoreGeometry();
    }
  });

  it("preserves the visible message position when a refresh inserts above it with a stable first row", async () => {
    const restoreGeometry = mockMessageListGeometry({ overlay: 0, viewport: 100 });
    try {
      const conversation = host.conversations[0];
      conversation.messages = [
        { ...conversation.messages[0], id: "m-first", body: "First message", attachments: [] },
        { ...conversation.messages[0], id: "m-visible", body: "Visible message", attachments: [] },
        { ...conversation.messages[0], id: "m-last", body: "Last message", attachments: [] },
      ];
      render(<App />);
      await screen.findByText("Visible message");
      const visibleRow = document.querySelector<HTMLElement>('[data-message-id="m-visible"]')!;
      const list = document.getElementById("message-list") as HTMLDivElement;
      vi.spyOn(list, "getBoundingClientRect").mockReturnValue({ top: 0, bottom: 100 } as DOMRect);
      vi.spyOn(visibleRow, "getBoundingClientRect").mockImplementation(() => ({
        top: document.querySelector('[data-message-id="m-middle"]') ? 60 : 20,
        bottom: document.querySelector('[data-message-id="m-middle"]') ? 80 : 40,
      } as DOMRect));
      vi.spyOn(document.querySelector<HTMLElement>('[data-message-id="m-first"]')!, "getBoundingClientRect")
        .mockReturnValue({ top: -20, bottom: 0 } as DOMRect);
      list.scrollTop = 200;
      fireEvent.scroll(list);

      const refreshed = host.load(conversation.id);
      refreshed.conversations[0].messages = [
        conversation.messages[0],
        { ...conversation.messages[0], id: "m-middle", body: "Middle message", attachments: [] },
        ...conversation.messages.slice(1),
      ];
      vi.mocked(bridge.load_state).mockResolvedValueOnce(refreshed);

      await act(async () => hint?.());

      expect(list.scrollTop).toBe(240);
    } finally {
      restoreGeometry();
    }
  });

  it("keeps the reader pinned to the bottom when a subscription refresh prepends history", async () => {
    const restoreGeometry = mockMessageListGeometry({ overlay: 0, viewport: 100, scrollHeight: 1_000 });
    try {
      const conversation = host.conversations[0];
      render(<App />);
      await screen.findByText("Hello from Aurora");
      const list = document.getElementById("message-list") as HTMLDivElement;
      const refreshed = host.load(conversation.id);
      refreshed.conversations[0].messages = [
        { ...conversation.messages[0], id: "m-prepended", body: "Prepended history", attachments: [] },
        ...refreshed.conversations[0].messages,
      ];
      vi.mocked(bridge.load_state).mockResolvedValueOnce(refreshed);

      await act(async () => hint?.());

      expect(list.scrollTop).toBe(list.scrollHeight);
    } finally {
      restoreGeometry();
    }
  });

  it("retains a prepended prefix and exhaustion when its deferred refresh resolves later", async () => {
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    const refresh = deferred<DesktopSnapshot>();
    const page = deferred<{ conversationId: string; messages: ConversationView["messages"]; hasMore: boolean }>();
    vi.mocked(bridge.load_state).mockImplementationOnce(async (id) => host.load(id)).mockReturnValueOnce(refresh.promise);
    vi.spyOn(bridge, "load_history").mockReturnValueOnce(page.promise);
    render(<App />);
    await screen.findByText("Hello from Aurora");

    await act(async () => hint?.());
    fireEvent.click(screen.getByRole("button", { name: "Load older messages" }));
    await act(async () => page.resolve({
      conversationId: conversation.id,
      messages: [{ ...conversation.messages[0], id: "m-older-prefix", body: "Older prefix", attachments: [] }],
      hasMore: false,
    }));
    expect(await screen.findByText("Older prefix")).toBeInTheDocument();
    expect(document.getElementById("history-beginning-marker")).toBeInTheDocument();

    const refreshed = host.load(conversation.id);
    refreshed.conversations[0].hasOlderMessages = true;
    refreshed.conversations[0].messages.push({
      ...conversation.messages[0],
      id: "m-arrived-during-refresh",
      body: "Arrived during refresh",
      attachments: [],
    });
    await act(async () => refresh.resolve(refreshed));

    expect(screen.getByText("Older prefix")).toBeInTheDocument();
    expect(screen.getByText("Arrived during refresh")).toBeInTheDocument();
    expect(document.getElementById("history-beginning-marker")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Load older messages" })).not.toBeInTheDocument();
  });

  it("restores history loading when a refresh reports older messages", async () => {
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    vi.spyOn(bridge, "load_history").mockResolvedValue({
      conversationId: conversation.id,
      messages: [{ ...conversation.messages[0], id: "m-older-prefix", body: "Older prefix", attachments: [] }],
      hasMore: false,
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Load older messages" }));
    expect(await screen.findByText("Older prefix")).toBeInTheDocument();
    expect(document.getElementById("history-beginning-marker")).toBeInTheDocument();

    const refreshed = host.load(conversation.id);
    refreshed.conversations[0].hasOlderMessages = true;
    vi.mocked(bridge.load_state).mockResolvedValueOnce(refreshed);
    await act(async () => hint?.());

    expect(document.getElementById("history-beginning-marker")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Load older messages" })).toBeInTheDocument();
    expect(document.querySelector('#history-load-band[data-history-state="idle"]')).toBeInTheDocument();
  });

  it("keeps the beginning marker when a subscription refresh confirms history is exhausted", async () => {
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    vi.spyOn(bridge, "load_history").mockResolvedValue({
      conversationId: conversation.id,
      messages: [{ ...conversation.messages[0], id: "m-older-prefix", body: "Older prefix", attachments: [] }],
      hasMore: false,
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Load older messages" }));
    expect(await screen.findByText("Older prefix")).toBeInTheDocument();

    const refreshed = host.load(conversation.id);
    refreshed.conversations[0].hasOlderMessages = false;
    vi.mocked(bridge.load_state).mockResolvedValueOnce(refreshed);
    await act(async () => hint?.());

    expect(document.getElementById("history-beginning-marker")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Load older messages" })).not.toBeInTheDocument();
  });

  it("does not apply a subscription anchor while an older page is loading", async () => {
    const restoreGeometry = mockMessageListGeometry({ overlay: 0, viewport: 100 });
    try {
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    const page = deferred<{ conversationId: string; messages: ConversationView["messages"]; hasMore: boolean }>();
    vi.spyOn(bridge, "load_history").mockReturnValueOnce(page.promise);
    render(<App />);
    await screen.findByText("Hello from Aurora");
    const messageRow = document.querySelector<HTMLElement>('[data-message-id="m-aurora"]')!;
    const rowBounds = vi.spyOn(messageRow, "getBoundingClientRect");
    const list = document.getElementById("message-list") as HTMLDivElement;
    list.scrollTop = 200;
    fireEvent.scroll(list);
    fireEvent.click(screen.getByRole("button", { name: "Load older messages" }));
    rowBounds.mockClear();

    const refreshed = host.load(conversation.id);
    refreshed.conversations[0].messages = [
      { ...conversation.messages[0], id: "m-prepended", body: "Prepended history", attachments: [] },
      ...refreshed.conversations[0].messages,
    ];
    vi.mocked(bridge.load_state).mockResolvedValueOnce(refreshed);
    await act(async () => hint?.());

    expect(rowBounds).not.toHaveBeenCalled();
    await act(async () => page.resolve({ conversationId: conversation.id, messages: [], hasMore: true }));
    } finally {
      restoreGeometry();
    }
  });

  it("renders every arrival in an anchored refresh larger than fifty messages", async () => {
    const conversation = host.conversations[0];
    render(<App />);
    await screen.findByText("Hello from Aurora");
    const refreshed = host.load(conversation.id);
    refreshed.conversations[0].messages = Array.from({ length: 61 }, (_, index) => ({
      ...conversation.messages[0],
      id: index === 0 ? conversation.messages[0].id : `m-arrival-${index}`,
      body: index === 0 ? conversation.messages[0].body : `Arrival ${index}`,
      attachments: [],
    }));
    vi.mocked(bridge.load_state).mockResolvedValueOnce(refreshed);

    await act(async () => hint?.());

    expect(await screen.findByText("Arrival 60")).toBeInTheDocument();
    expect(document.querySelectorAll("[data-message-id]")).toHaveLength(61);
    expect(screen.getByText("Arrival 1")).toBeInTheDocument();
  });

  it("clears previews and discards an older page that resolves after lock", async () => {
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    conversation.messages[0].attachments[0].previewUrl = "blob:sensitive";
    const page = deferred<{ conversationId: string; messages: ConversationView["messages"]; hasMore: boolean }>();
    vi.spyOn(bridge, "load_history").mockReturnValueOnce(page.promise);
    render(<App />);
    expect(await screen.findByRole("img", { name: "photo.png" })).toHaveAttribute("src", "blob:sensitive");
    fireEvent.click(screen.getByRole("button", { name: "Load older messages" }));

    const locked = host.load(conversation.id);
    locked.connection = { state: "offline", origin: "https://example.test", errorCode: "app-locked" };
    locked.conversations = [];
    locked.activeConversationId = undefined;
    vi.mocked(bridge.load_state).mockResolvedValueOnce(locked);
    await act(async () => hint?.());
    expect(await screen.findByRole("heading", { name: "Peppy is locked" })).toBeInTheDocument();
    expect(screen.queryByRole("img", { name: "photo.png" })).not.toBeInTheDocument();

    await act(async () => page.resolve({
      conversationId: conversation.id,
      messages: [{ ...conversation.messages[0], id: "m-stale-after-lock", body: "Stale after lock", attachments: [] }],
      hasMore: false,
    }));
    expect(screen.queryByText("Stale after lock")).not.toBeInTheDocument();
  });

  it("does not update state when an older page resolves after unmount", async () => {
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    const page = deferred<{ conversationId: string; messages: ConversationView["messages"]; hasMore: boolean }>();
    vi.spyOn(bridge, "load_history").mockReturnValueOnce(page.promise);
    const { unmount } = render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Load older messages" }));
    unmount();

    await act(async () => page.resolve({
      conversationId: conversation.id,
      messages: [{ ...conversation.messages[0], id: "m-stale-after-unmount", body: "Stale after unmount", attachments: [] }],
      hasMore: false,
    }));

    expect(screen.queryByText("Stale after unmount")).not.toBeInTheDocument();
    expect(bridge.load_history).toHaveBeenCalledOnce();
  });

  it("shows a visible error when a history page belongs to another conversation", async () => {
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    vi.spyOn(bridge, "load_history").mockResolvedValue({
      conversationId: "river",
      messages: [{ ...conversation.messages[0], id: "m-wrong-conversation", body: "Wrong conversation", attachments: [] }],
      hasMore: false,
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Load older messages" }));

    expect(await screen.findByRole("alert")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Retry loading older messages" })).toBeInTheDocument();
    expect(screen.getByText("Hello from Aurora")).toBeInTheDocument();
    expect(screen.queryByText("Wrong conversation")).not.toBeInTheDocument();
  });

  it("keeps historical prepends out of the live region until the next frame", async () => {
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", vi.fn((callback: FrameRequestCallback) => {
      frames.push(callback);
      return frames.length;
    }));
    vi.spyOn(bridge, "load_history").mockResolvedValue({
      conversationId: conversation.id,
      messages: [{ ...conversation.messages[0], id: "m-quiet-history", body: "Quiet history", attachments: [] }],
      hasMore: false,
    });
    render(<App />);
    await screen.findByText("Hello from Aurora");
    frames.length = 0;
    fireEvent.click(screen.getByRole("button", { name: "Load older messages" }));

    expect(await screen.findByText("Quiet history")).toBeInTheDocument();
    const content = document.getElementById("message-list-content")!;
    expect(content).toHaveAttribute("aria-live", "off");
    await act(async () => frames.splice(0).forEach((callback) => callback(performance.now())));
    expect(content).not.toHaveAttribute("aria-live");
  });

  it("loads the initial snapshot and an older page under StrictMode", async () => {
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    vi.spyOn(bridge, "load_history").mockResolvedValue({
      conversationId: conversation.id,
      messages: [{ ...conversation.messages[0], id: "m-strict-before", body: "Strict history" }],
      hasMore: false,
    });
    render(<StrictMode><App /></StrictMode>);
    expect(await screen.findByText("Hello from Aurora")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Load older messages" }));
    expect(await screen.findByText("Strict history")).toBeInTheDocument();
  });

  it("loads an older page once from the manual fallback and retains chronological rows", async () => {
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    vi.spyOn(bridge, "load_history").mockResolvedValue({
      conversationId: conversation.id,
      messages: [{ ...conversation.messages[0], id: "m-before", body: "Older message" }],
      hasMore: false,
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Load older messages" }));
    expect(await screen.findByText("Older message")).toBeInTheDocument();
    expect(bridge.load_history).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: "Load older messages" })).not.toBeInTheDocument();
  });

  it("keeps rendered history when an older-page retry fails", async () => {
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    vi.spyOn(bridge, "load_history").mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce({
      conversationId: conversation.id,
      messages: [{ ...conversation.messages[0], id: "m-before", body: "Recovered history" }],
      hasMore: false,
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Load older messages" }));
    expect(await screen.findByRole("button", { name: "Retry loading older messages" })).toBeInTheDocument();
    expect(screen.getByText("Hello from Aurora")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Retry loading older messages" }));
    expect(await screen.findByText("Recovered history")).toBeInTheDocument();
  });

  it("discards an older-page response after switching conversations", async () => {
    const conversation = host.conversations[0];
    conversation.hasOlderMessages = true;
    const page = deferred<{ conversationId: string; messages: ConversationView["messages"]; hasMore: boolean }>();
    vi.spyOn(bridge, "load_history").mockReturnValueOnce(page.promise);
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Load older messages" }));
    fireEvent.click(screen.getByRole("button", { name: /River/ }));
    page.resolve({ conversationId: conversation.id, messages: [{ ...conversation.messages[0], id: "m-stale", body: "Stale history" }], hasMore: false });
    await screen.findByText("Hello from River");
    expect(screen.queryByText("Stale history")).not.toBeInTheDocument();
  });
});

describe("conversation pinning", () => {
  it("renders remotely pinned conversations first without changing the recency-selected conversation", async () => {
    host.conversations[1] = { ...host.conversations[1], pinned: true } as ConversationView;

    render(<App />);

    await screen.findByText("Hello from Aurora");
    expect([...document.querySelectorAll("[data-conversation-row]")].map((row) => row.getAttribute("data-conversation-row"))).toEqual(["river", "aurora"]);
    expect(document.querySelector("[data-header-title]")).toHaveTextContent("Aurora");
  });

  it("waits for durable pinning before refreshing and prevents duplicate requests", async () => {
    const pending = deferred<void>();
    const setPinned = vi.spyOn(bridge, "set_conversation_pinned").mockReturnValue(pending.promise);
    render(<App />);

    await screen.findByText("Hello from Aurora");
    const pin = document.querySelector<HTMLButtonElement>("[data-pin-conversation-id='aurora']")!;
    pin.focus();
    fireEvent.click(pin);
    fireEvent.click(pin);
    expect(setPinned).toHaveBeenCalledOnce();
    expect(setPinned).toHaveBeenCalledWith("aurora", true);
    expect(pin).toBeDisabled();
    document.body.tabIndex = -1;
    document.body.focus();
    expect(document.activeElement).toBe(document.body);

    host.conversations[0] = { ...host.conversations[0], pinned: true } as ConversationView;
    await act(async () => pending.resolve());
    await waitFor(() => expect(screen.getByRole("button", { name: "Unpin conversation" })).toHaveFocus());
    document.body.removeAttribute("tabindex");
  });

  it("reports a rejected pin mutation without reordering and re-enables the action", async () => {
    vi.spyOn(bridge, "set_conversation_pinned").mockRejectedValue(new Error("denied"));
    render(<App />);

    await screen.findByText("Hello from Aurora");
    fireEvent.click(document.querySelector<HTMLButtonElement>("[data-pin-conversation-id='aurora']")!);

    expect(await screen.findByRole("alert")).toHaveTextContent("Conversation pin not changed: denied");
    expect([...document.querySelectorAll("[data-conversation-row]")].map((row) => row.getAttribute("data-conversation-row"))).toEqual(["aurora", "river"]);
    expect(document.querySelector<HTMLButtonElement>("[data-pin-conversation-id='aurora']")).toBeEnabled();
  });

  it("reports a saved pin when the post-save refresh fails", async () => {
    vi.spyOn(bridge, "set_conversation_pinned").mockResolvedValue();
    render(<App />);

    await screen.findByText("Hello from Aurora");
    vi.mocked(bridge.load_state).mockRejectedValueOnce(new Error("offline"));
    fireEvent.click(document.querySelector<HTMLButtonElement>("[data-pin-conversation-id='aurora']")!);

    expect(await screen.findByRole("alert")).toHaveTextContent("Pinned, but the list could not refresh: offline");
  });

  it("hides pin controls while encryption is locked", async () => {
    host.encryption = { state: "locked" } as DesktopSnapshot["encryption"];
    render(<App />);

    await waitFor(() => expect(document.getElementById("desktop-shell")).toBeInTheDocument());
    expect(document.querySelector("[data-pin-conversation-id]")).not.toBeInTheDocument();
  });

  it("keeps local drafts ordered below pinned established conversations", async () => {
    host.conversations[1] = { ...host.conversations[1], pinned: true } as ConversationView;
    host.drafts.set("draft-one", { id: "draft-one", conversationId: "draft-one", text: "", recipientIds: ["one"], attachmentIds: [], revision: "1" });
    host.drafts.set("draft-two", { id: "draft-two", conversationId: "draft-two", text: "", recipientIds: ["two"], attachmentIds: [], revision: "1" });
    const saving = deferred<Draft>();
    vi.mocked(bridge.save_draft).mockReturnValue(saving.promise);
    render(<App />);

    fireEvent.click(await findNewConversationAction());
    await screen.findByLabelText("Recipients");

    const rows = [...document.querySelectorAll("[data-conversation-row]")].map((row) => row.getAttribute("data-conversation-row")!);
    const localDraft = rows.find((id) => id.startsWith("new:"))!;
    expect(rows[0]).toBe("river");
    expect(rows.indexOf("river")).toBeLessThan(rows.indexOf(localDraft));
    expect(rows.indexOf(localDraft)).toBeLessThan(rows.indexOf("aurora"));
    expect(rows.indexOf("draft-one")).toBeLessThan(rows.indexOf("draft-two"));
  });

  it("keeps the selected conversation and its local draft when a remote pin refreshes another row", async () => {
    render(<App />);

    await screen.findByText("Hello from Aurora");
    fireEvent.change(message(), { target: { value: "Keep this draft" } });
    host.conversations[1] = { ...host.conversations[1], pinned: true } as ConversationView;
    await act(async () => hint?.());

    await waitFor(() => expect(document.querySelector("[data-conversation-row]")?.getAttribute("data-conversation-row")).toBe("river"));
    expect(document.querySelector("[data-header-title]")).toHaveTextContent("Aurora");
    expect(message()).toHaveValue("Keep this draft");
  });
});

describe("new-recipient drafts", () => {
  it("hides the composer and exposes a draft CTA when no conversation is selected", async () => {
    host.conversations = [];
    render(<App />);

    const cta = await findEmptyConversationAction();
    expect(cta).toHaveAttribute("id", "new-conversation-cta");
    expect(screen.queryByLabelText("Composer")).not.toBeInTheDocument();
    expect(document.getElementById("conversation-stage")).toHaveStyle({
      "--composer-overlay-height": "0px",
    });

    fireEvent.click(cta);

    expect(await screen.findByLabelText("Recipients")).toBeInTheDocument();
    expect(screen.getByLabelText("Composer")).toBeInTheDocument();
    expect(document.getElementById("new-conversation-cta")).not.toBeInTheDocument();
  });

  it("starts an in-place draft from the main navbar", async () => {
    host.conversations = [];
    render(<App />);

    fireEvent.click(await findNewConversationAction());

    expect(await screen.findByLabelText("Recipients")).toBeInTheDocument();
    expect(document.getElementById("new-conversation-cta")).not.toBeInTheDocument();
  });

  it("does not show the CTA for a selected conversation", async () => {
    render(<App />);

    await screen.findByText("Hello from Aurora");

    expect(document.getElementById("new-conversation-cta")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Composer")).toBeInTheDocument();
  });

  it("does not show the CTA while a new draft save is pending", async () => {
    host.conversations = [];
    const save = deferred<Draft>();
    vi.mocked(bridge.save_draft).mockReturnValueOnce(save.promise);
    render(<App />);

    fireEvent.click(await findNewConversationAction());
    const recipients = await screen.findByLabelText("Recipients");
    fireEvent.change(recipients, { target: { value: "+1 202 555 0100" } });
    fireEvent.keyDown(recipients, { key: "Enter" });

    await waitFor(() => expect(bridge.save_draft).toHaveBeenCalledOnce());
    expect(document.getElementById("new-conversation-cta")).not.toBeInTheDocument();

    await act(async () => save.resolve({
      id: "draft-conv-new-1",
      conversationId: "conv-new-1",
      text: "",
      recipientIds: ["+12025550100"],
      attachmentIds: [],
      revision: "1",
    }));
  });

  it("restores the empty CTA after deleting the last blank draft", async () => {
    host.conversations = [];
    render(<App />);

    fireEvent.click(await findNewConversationAction());
    expect(await screen.findByLabelText("Recipients")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Delete draft" }));

    expect(await findEmptyConversationAction()).toBeInTheDocument();
    expect(screen.queryByLabelText("Composer")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Recipients")).not.toBeInTheDocument();
    expect(document.getElementById("conversation-stage")).toHaveStyle({
      "--composer-overlay-height": "0px",
    });
    expect(screen.queryByRole("button", { name: "Delete draft" })).not.toBeInTheDocument();

    fireEvent.click(await findEmptyConversationAction());
    expect(await screen.findByLabelText("Recipients")).toBeInTheDocument();
  });

  it("shows an initial load error without mounting the composer", async () => {
    vi.mocked(bridge.load_state).mockRejectedValue(new Error("Initial state unavailable"));
    render(<App />);

    expect(await screen.findByText("Initial state unavailable")).toHaveAttribute("role", "alert");
    expect(document.getElementById("new-conversation-cta")).toBeVisible();
    expect(screen.queryByLabelText("Composer")).not.toBeInTheDocument();
  });

  it("clears an applied refresh failure without dismissing action feedback", async () => {
    vi.mocked(bridge.load_state).mockRejectedValueOnce(new Error("Initial state unavailable"));
    render(<App />);
    await screen.findByRole("alert");

    vi.mocked(bridge.load_state).mockImplementation(async id => host.load(id));
    const retryRefresh = screen.getByRole("button", { name: "Retry refresh" });
    retryRefresh.focus();
    fireEvent.click(retryRefresh);
    const composer = await screen.findByLabelText("Message");
    await waitFor(() => expect(document.querySelector('[data-app-notice-source="refresh"]')).toBeNull());
    await waitFor(() => expect(composer).toHaveFocus());

    vi.mocked(bridge.pick_attachments).mockRejectedValueOnce(new Error("Picker unavailable"));
    fireEvent.click(screen.getByRole("button", { name: "Add attachment" }));
    const action = await screen.findByTestId("app-notice-action");
    expect(action).toHaveTextContent("Picker unavailable");
    expect(action).toHaveAttribute("data-app-notice-severity", "error");
    expect(within(action).getByRole("alert")).toBeInTheDocument();

    await act(async () => hint?.());
    expect(screen.getByTestId("app-notice-action")).toHaveTextContent("Picker unavailable");

    const dismiss = within(action).getByRole("button", { name: "Dismiss action notice" });
    dismiss.focus();
    fireEvent.click(dismiss);
    await waitFor(() => expect(composer).toHaveFocus());
  });

  it("keeps a persistent refresh error when a superseded navigation response succeeds", async () => {
    const superseded = deferred<DesktopSnapshot>();
    const queued = deferred<DesktopSnapshot>();
    render(<App />);
    await screen.findByText("Hello from Aurora");

    vi.mocked(bridge.load_state).mockRejectedValueOnce(new Error("Refresh unavailable"));
    await act(async () => hint?.());
    const refreshNotice = () => document.querySelector<HTMLElement>('[data-app-notice-source="refresh"]');
    await waitFor(() => expect(refreshNotice()).not.toBeNull());

    vi.mocked(bridge.load_state).mockReturnValueOnce(superseded.promise).mockReturnValueOnce(queued.promise);
    const callsBeforeNavigation = vi.mocked(bridge.load_state).mock.calls.length;
    fireEvent.click(screen.getByRole("button", { name: /River/ }));
    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledTimes(callsBeforeNavigation + 1));
    fireEvent.click(screen.getByRole("button", { name: /Aurora/ }));
    await act(async () => superseded.resolve(host.load("river")));
    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledTimes(callsBeforeNavigation + 2));

    const notice = refreshNotice()!;
    expect(notice).toBeInTheDocument();
    expect(within(notice).getByRole("button", { name: /^Retry(ing)? refresh$/ })).toBeInTheDocument();
    expect(within(notice).queryByRole("button", { name: /dismiss/i })).not.toBeInTheDocument();
    await act(async () => queued.resolve(host.load("aurora")));
  });

  it("keeps the application banner outside composer windows", async () => {
    render(<App />);
    await screen.findByLabelText("Message");
    vi.mocked(bridge.pick_attachments).mockRejectedValueOnce(new Error("Picker unavailable"));
    fireEvent.click(screen.getByRole("button", { name: "Add attachment" }));
    const mainBanner = await screen.findByTestId("app-notice-action");
    expect(document.getElementById("composer-area")).not.toContainElement(mainBanner);

    cleanup();
    openComposerWindow("aurora");
    render(<App />);
    await screen.findByLabelText("Message");
    vi.mocked(bridge.pick_attachments).mockRejectedValueOnce(new Error("Picker unavailable"));
    fireEvent.click(screen.getByRole("button", { name: "Add attachment" }));
    const detachedBanner = await screen.findByTestId("app-notice-action");
    expect(document.getElementById("composer-area")).not.toContainElement(detachedBanner);
  });

  it("keeps a new draft's recipient title when the inbox is empty", async () => {
    host.conversations = [];
    render(<App />);
    await waitFor(() => expect(document.getElementById("desktop-shell")).toBeInTheDocument());
    expect(document.querySelector("[data-header-title]")?.textContent).toBe("Peppy");
    const omni = await screen.findByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(omni, { target: { value: "+1 202 555 0100" } });
    fireEvent.click(await screen.findByText("Start conversation with (202) 555-0100"));
    await waitFor(() => expect(host.drafts.get("conv-new-1")?.recipientIds).toEqual(["+12025550100"]));
    await waitFor(() => expect(document.querySelector("[data-header-title]")?.textContent).toBe("(202) 555-0100"));
  });

  it("continues a guarded Omni start after a real outside pointer closes search", async () => {
    vi.spyOn(bridge, "list_contact_books").mockResolvedValue([{
      id: "book", deviceName: "Phone", state: "active", contactCount: 0,
      capabilities: { canWrite: true, canDelete: true, supportsNotes: true, supportsPhoto: true, supportsBirthday: true },
      lastSyncAt: "now", pendingEditCount: 0,
    }]);
    vi.spyOn(bridge, "list_contacts").mockResolvedValue([]);
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Contacts" }));
    await screen.findByRole("region", { name: "Contacts browser" });
    const newContact = await screen.findByLabelText("New contact");
    await waitFor(() => expect(newContact).toBeEnabled());
    fireEvent.click(newContact);
    fireEvent.change(await screen.findByLabelText("Given name"), { target: { value: "Unsaved" } });

    const omni = screen.getByRole("combobox", { name: "Search Peppy" });
    const startGuardedConversation = async () => {
      fireEvent.change(omni, { target: { value: "+1 202 555 0100" } });
      fireEvent.click(await screen.findByText("Start conversation with (202) 555-0100"));
    };
    await startGuardedConversation();
    const keepEditing = await screen.findByRole("button", { name: "Keep editing" });
    await waitFor(() => expect(omni).toHaveValue(""));
    expect(document.querySelector("[data-omni-search-results]")).not.toBeInTheDocument();
    fireEvent.pointerDown(keepEditing);
    fireEvent.click(keepEditing);
    expect(screen.getByLabelText("Given name")).toHaveValue("Unsaved");
    expect(host.drafts.has("conv-new-1")).toBe(false);

    await startGuardedConversation();
    const discard = await screen.findByRole("button", { name: "Discard" });
    await waitFor(() => expect(omni).toHaveValue(""));
    fireEvent.pointerDown(discard);
    fireEvent.click(discard);

    await waitFor(() => expect(host.drafts.get("conv-new-1")?.recipientIds).toEqual(["+12025550100"]));
  });

  it("resolves an exact later-page contact identity and preserves its stored short address", async () => {
    const firstPage = Array.from({ length: 200 }, (_, index) => ({
      id: `other-${index}`, bookId: "book", revision: "1", displayName: "Other", phones: [], emails: [], addresses: [],
    }));
    vi.spyOn(bridge, "search_local").mockResolvedValue({
      query: "Ada", items: [{ kind: "contact", bookId: "book", contactId: "ada", title: "Ada", subtitle: "Contact" }], indexState: "ready",
      hasMore: { conversation: false, message: false, contact: false },
    });
    const listContacts = vi.spyOn(bridge, "list_contacts").mockImplementation(async (_book, _query, offset) => offset === 0
      ? firstPage
      : [{ id: "ada", bookId: "book", revision: "1", displayName: "Ada", phones: [{ label: "Work", number: "short code / 1" }], emails: [], addresses: [] }]);
    render(<App />);
    const omni = await screen.findByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(omni, { target: { value: "Ada" } });
    fireEvent.click(await screen.findByRole("option", { name: "Start conversation with Ada" }));

    await waitFor(() => expect(host.drafts.get("conv-new-1")?.recipientIds).toEqual(["short code / 1"]));
    expect(listContacts).toHaveBeenNthCalledWith(2, "book", "", 200, { key: "family", direction: "asc" });
  });

  it("keeps a missing contact lookup inline and does not create a draft", async () => {
    vi.spyOn(bridge, "search_local").mockResolvedValue({
      query: "Missing", items: [{ kind: "contact", bookId: "book", contactId: "missing", title: "Missing", subtitle: "Contact" }], indexState: "ready",
      hasMore: { conversation: false, message: false, contact: false },
    });
    vi.spyOn(bridge, "list_contacts").mockResolvedValue([]);
    render(<App />);
    fireEvent.change(await screen.findByRole("combobox", { name: "Search Peppy" }), { target: { value: "Missing" } });
    fireEvent.click(await screen.findByRole("option", { name: "Start conversation with Missing" }));

    expect(await screen.findByText("This contact is no longer available.")).toBeInTheDocument();
    expect(host.drafts.size).toBe(0);
  });

  it("reuses a singleton or local draft but never a group conversation", async () => {
    host.conversations.push({ id: "single", name: "Single", preview: "", unread: 0, messages: [], participants: ["+12025550100"] });
    host.conversations.push({ id: "group", name: "Group", preview: "", unread: 0, messages: [], participants: ["+12025550101", "+12025550102"] });
    render(<App />);
    const omni = await screen.findByRole("combobox", { name: "Search Peppy" });

    fireEvent.change(omni, { target: { value: "+1 202 555 0100" } });
    fireEvent.click(await screen.findByText("Start conversation with (202) 555-0100"));
    await waitFor(() => expect(bridge.load_state).toHaveBeenLastCalledWith("single"));
    await waitFor(() => expect(document.activeElement).toBe(document.getElementById("composer-textarea")));

    host.failSaves = { code: "offline", message: "offline" };
    fireEvent.change(omni, { target: { value: "+1 202 555 0103" } });
    fireEvent.click(await screen.findByText("Start conversation with (202) 555-0103"));
    await screen.findByRole("alert");
    host.failSaves = undefined;
    fireEvent.change(omni, { target: { value: "+1 202 555 0103" } });
    fireEvent.click(await screen.findByText("Start conversation with (202) 555-0103"));
    await waitFor(() => expect(screen.getByRole("button", { name: "Remove (202) 555-0103" })).toBeInTheDocument());
    expect(host.drafts.size).toBe(0);

    fireEvent.change(omni, { target: { value: "+1 202 555 0101" } });
    fireEvent.click(await screen.findByText("Start conversation with (202) 555-0101"));
    await waitFor(() => expect([...host.drafts.values()].some((draft) => draft.recipientIds[0] === "+12025550101")).toBe(true));
  });

  it("cancels an immediate clean-Contacts start when search closes during a pending flush", async () => {
    vi.spyOn(bridge, "list_contact_books").mockResolvedValue([{
      id: "book", deviceName: "Phone", state: "active", contactCount: 0,
      capabilities: { canWrite: true, canDelete: true, supportsNotes: true, supportsPhoto: true, supportsBirthday: true }, lastSyncAt: "now", pendingEditCount: 0,
    }]);
    vi.spyOn(bridge, "list_contacts").mockResolvedValue([]);
    const pendingSave = deferred<Draft>();
    vi.spyOn(bridge, "save_draft").mockReturnValueOnce(pendingSave.promise);
    render(<App />);
    await screen.findByLabelText("Message");
    type("pending flush");
    await waitFor(() => expect(bridge.save_draft).toHaveBeenCalledOnce());
    fireEvent.click(await screen.findByRole("button", { name: "Contacts" }));
    const omni = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(omni, { target: { value: "+1 202 555 0100" } });
    fireEvent.click(await screen.findByText("Start conversation with (202) 555-0100"));
    fireEvent.keyDown(omni, { key: "Escape" });
    pendingSave.resolve({ id: "draft-aurora", conversationId: "aurora", text: "pending flush", recipientIds: [], attachmentIds: [], revision: "1" });
    await act(async () => { await pendingSave.promise; });

    expect(vi.mocked(bridge.save_draft).mock.calls.some(
      ([input]) => input.recipientIds.includes("+12025550100"),
    )).toBe(false);
    expect(screen.getByRole("region", { name: "Contacts browser" })).toBeInTheDocument();
  });

  it("keeps an immediate clean-Contacts flush failure inline in Omni", async () => {
    vi.spyOn(bridge, "list_contact_books").mockResolvedValue([{
      id: "book", deviceName: "Phone", state: "active", contactCount: 0,
      capabilities: { canWrite: true, canDelete: true, supportsNotes: true, supportsPhoto: true, supportsBirthday: true }, lastSyncAt: "now", pendingEditCount: 0,
    }]);
    vi.spyOn(bridge, "list_contacts").mockResolvedValue([]);
    render(<App />);
    await screen.findByLabelText("Message");
    host.failSaves = { code: "offline", message: "offline" };
    type("will fail");
    await waitFor(() => expect(bridge.save_draft).toHaveBeenCalledOnce());
    fireEvent.click(await screen.findByRole("button", { name: "Contacts" }));
    const omni = screen.getByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(omni, { target: { value: "+1 202 555 0100" } });
    fireEvent.click(await screen.findByText("Start conversation with (202) 555-0100"));

    expect(await screen.findByText("Could not start the conversation.")).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Retry starting conversation" })).toBeInTheDocument();
    expect(host.drafts.has("conv-new-1")).toBe(false);
  });

  it("deletes an empty recipientless draft directly and confirms before deleting content", async () => {
    host.drafts.set("empty-draft", { id: "draft-empty", conversationId: "empty-draft", text: "keep until confirmed", recipientIds: [], attachmentIds: [], revision: "1" });
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await waitFor(() => expect(document.querySelector('[data-conversation-id="empty-draft"]')).toBeInTheDocument());
    fireEvent.click(document.querySelector('[data-conversation-id="empty-draft"]')!);
    await waitFor(() => expect(document.getElementById("conversation-delete-draft")).toBeInTheDocument());
    const remove = document.getElementById("conversation-delete-draft")!;
    fireEvent.click(remove);
    expect(document.getElementById("conversation-delete-draft")).toHaveAccessibleName("Confirm delete draft");
    expect(host.drafts.has("empty-draft")).toBe(true);
    await act(async () => hint?.());
    expect(document.getElementById("conversation-delete-draft")).toHaveAccessibleName("Confirm delete draft");
    fireEvent.click(document.getElementById("conversation-delete-draft")!);
    await waitFor(() => expect(host.drafts.has("empty-draft")).toBe(false));
  });

  it("creates the draft with empty IDs, then saves and sends with the host-assigned IDs", async () => {
    const save = vi.mocked(bridge.save_draft);
    render(<App />);
    const omni = await screen.findByRole("combobox", { name: "Search Peppy" });
    fireEvent.change(omni, { target: { value: "+1 202 555 0100" } });
    fireEvent.click(await screen.findByText("Start conversation with (202) 555-0100"));

    await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
    expect(save.mock.calls[0][0]).toMatchObject({
      id: "",
      conversationId: "",
      recipientIds: ["+12025550100"],
      expectedRevision: "0",
    });
    await waitFor(() =>
      expect(bridge.load_state).toHaveBeenLastCalledWith("conv-new-1"),
    );
    expect(document.querySelector('[data-recipient-id="+12025550100"]')).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove (202) 555-0100" })).toBeInTheDocument();
    expect(screen.getByLabelText("Recipients")).toHaveValue("");

    type("first message");
    await waitFor(() => expect(save).toHaveBeenCalledTimes(2));
    expect(save.mock.calls[1][0]).toMatchObject({
      id: "draft-conv-new-1",
      conversationId: "conv-new-1",
      text: "first message",
      expectedRevision: "1",
    });

    await chooseGateway(SMS_ONLY);
    await waitFor(() =>
      expect(host.drafts.get("conv-new-1")?.simId).toBe("sim-1"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(host.sent).toHaveLength(1));
    expect(host.sent[0]).toMatchObject({
      conversationId: "conv-new-1",
      text: "first message",
      recipientIds: ["+12025550100"],
      gatewayId: "gw-phone",
      simId: "sim-1",
    });
    expect(host.drafts.get("conv-new-1")).toMatchObject({
      id: "draft-conv-new-1",
      conversationId: "conv-new-1",
      text: "",
      recipientIds: ["+12025550100"],
      attachmentIds: [],
      gatewayId: "gw-phone",
      simId: "sim-1",
      revision: "4",
    });
    expect(
      save.mock.calls.every(
        ([input]) => input.id === "" || input.id === "draft-conv-new-1",
      ),
    ).toBe(true);
  });

  it("keeps a rejected new recipient as an unsaved local draft that can be corrected", async () => {
    render(<App />);
    host.failSaves = {
      code: "invalid-recipient",
      message: "Recipients must be phone numbers.",
    };
    fireEvent.click(await findNewConversationAction());
    const picker = await screen.findByLabelText("Recipients");
    fireEvent.change(picker, { target: { value: "+1 202 555 0100" } });
    fireEvent.keyDown(picker, { key: "Enter" });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Draft not saved. Your text is kept.",
    );
    expect(document.querySelector('[data-recipient-id="+12025550100"]')).toBeInTheDocument();

    host.failSaves = undefined;
    fireEvent.click(screen.getByRole("button", { name: "Remove (202) 555-0100" }));
    fireEvent.change(screen.getByLabelText("Recipients"), {
      target: { value: "+1 202 555 0101" },
    });
    fireEvent.blur(screen.getByLabelText("Recipients"));
    await waitFor(() =>
      expect(host.drafts.get("conv-new-1")?.recipientIds).toEqual([
        "+12025550101",
      ]),
    );
    await waitFor(() =>
      expect(screen.queryByRole("alert")).not.toBeInTheDocument(),
    );
  });

  it("does not save an invalid new recipient", async () => {
    const save = vi.mocked(bridge.save_draft);
    render(<App />);
    fireEvent.click(await findNewConversationAction());
    const picker = await screen.findByLabelText("Recipients");
    await waitFor(() => expect(save).toHaveBeenCalledOnce());
    save.mockClear();
    fireEvent.change(picker, { target: { value: "alice@example" } });
    fireEvent.keyDown(picker, { key: "Enter" });

    expect(await screen.findByRole("alert")).toBeInTheDocument();
    expect(save).not.toHaveBeenCalled();
    expect(picker).toHaveValue("alice@example");
  });

  it("blocks Send while a recipient is pending, then saves the canonical number on commit", async () => {
    host.gateways = [MMS_SIM];
    render(<App />);
    fireEvent.click(await findNewConversationAction());
    const picker = await screen.findByLabelText("Recipients");
    fireEvent.change(picker, { target: { value: "+1 202 555 0100" } });
    fireEvent.keyDown(picker, { key: "Enter" });
    await waitFor(() => expect(host.drafts.get("conv-new-1")?.recipientIds).toEqual(["+12025550100"]));
    type("ready to send");
    await waitFor(() => expect(screen.getByRole("button", { name: "Send" })).toBeEnabled());

    const recipients = screen.getByLabelText("Recipients");
    fireEvent.change(recipients, { target: { value: "+1 202" } });
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
    expect(document.getElementById("unavailable-hint")).toHaveTextContent(
      "finish adding the recipient",
    );

    fireEvent.change(recipients, { target: { value: "+1 202 555 0101" } });
    await waitFor(() => expect(recipients).toHaveValue("+1 202 555 0101"));
    fireEvent.keyDown(recipients, { key: "Enter" });
    await waitFor(() =>
      expect(host.drafts.get("conv-new-1")?.recipientIds).toEqual([
        "+12025550100",
        "+12025550101",
      ]),
    );
    expect(screen.getByRole("button", { name: "Send" })).toBeEnabled();
  });

  it("keeps new-compose recipient editing after an incoming history match", async () => {
    host.gateways = [MMS_SIM];
    render(<App />);
    fireEvent.click(await findNewConversationAction());
    const picker = await screen.findByLabelText("Recipients");
    fireEvent.change(picker, { target: { value: "+1 202 555 0100" } });
    fireEvent.keyDown(picker, { key: "Enter" });
    await waitFor(() => expect(host.drafts.get("conv-new-1")).toBeDefined());

    type("reply after incoming message");
    const recipients = screen.getByLabelText("Recipients");
    fireEvent.change(recipients, { target: { value: "+1 202" } });
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();

    host.conversations.push({
      id: "conv-new-1",
      name: "+12025550100",
      preview: "Incoming while composing",
      unread: 1,
      messages: [{
        id: "m-conv-new-1",
        revision: "1",
        sender: "other",
        body: "Incoming while composing",
        timestamp: "now",
        attachments: [],
      }],
    });
    await act(async () => hint?.());

    expect(await screen.findByRole("region", { name: "Message recipients" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
  });

  it("keeps the second group recipient input focused while the first chip rekeys", async () => {
    host.gateways = [MMS_SIM];
    const firstSave = deferred<Draft>();
    const save = vi.mocked(bridge.save_draft).mockImplementationOnce(() => firstSave.promise);
    render(<App />);
    fireEvent.click(await findNewConversationAction());
    const picker = await screen.findByLabelText("Recipients");
    fireEvent.change(picker, { target: { value: "+1 202 555 0100" } });
    fireEvent.keyDown(picker, { key: "Enter" });
    await waitFor(() => expect(save).toHaveBeenCalledOnce());
    const recipients = screen.getByLabelText("Recipients");
    fireEvent.change(recipients, { target: { value: "+1 202 555 0101" } });
    recipients.focus();
    host.drafts.set("history", { id: "draft-history", conversationId: "history", text: "", recipientIds: ["+12025550100"], attachmentIds: [], revision: "1" });
    firstSave.resolve({ id: "draft-history", conversationId: "history", text: "", recipientIds: ["+12025550100"], attachmentIds: [], revision: "1" });
    await waitFor(() => expect(screen.getByLabelText("Recipients")).toBe(recipients));
    expect(recipients).toHaveFocus();
    expect(recipients).toHaveValue("+1 202 555 0101");
    fireEvent.keyDown(recipients, { key: "Enter" });
    await waitFor(() => expect(host.drafts.get("history")?.recipientIds).toEqual(["+12025550100", "+12025550101"]));
  });

  it("persists an empty recipient list after removing the last chip without restoring it on blur", async () => {
    host.gateways = [MMS_SIM];
    const save = vi.mocked(bridge.save_draft);
    render(<App />);
    fireEvent.click(await findNewConversationAction());
    const picker = await screen.findByLabelText("Recipients");
    fireEvent.change(picker, { target: { value: "+1 202 555 0100" } });
    fireEvent.keyDown(picker, { key: "Enter" });
    await waitFor(() => expect(host.drafts.get("conv-new-1")).toBeDefined());

    type("ready to send");
    await waitFor(() => expect(screen.getByRole("button", { name: "Send" })).toBeEnabled());
    const recipients = screen.getByLabelText("Recipients");
    fireEvent.change(recipients, { target: { value: "+1 202" } });
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Remove (202) 555-0100" }));
    await waitFor(() =>
      expect(host.drafts.get("conv-new-1")?.recipientIds).toEqual([]),
    );
    fireEvent.blur(recipients);
    expect(document.querySelector('[data-recipient-id="+12025550100"]')).not.toBeInTheDocument();
    expect(recipients).toHaveValue("+1 202");
    expect(save.mock.calls.some(([input]) => input.recipientIds.length === 0)).toBe(true);
  });

  it("removes the exact stored ID from a multi-recipient draft", async () => {
    render(<App />);
    fireEvent.click(await findNewConversationAction());
    const picker = await screen.findByLabelText("Recipients");
    fireEvent.change(picker, { target: { value: "+1 202 555 0100" } });
    fireEvent.keyDown(picker, { key: "Enter" });
    await waitFor(() =>
      expect(document.querySelector('[data-conversation-id="conv-new-1"]')).toBeInTheDocument(),
    );
    const recipients = screen.getByLabelText("Recipients");
    fireEvent.change(recipients, { target: { value: "+1 202 555 0101" } });
    await waitFor(() => expect(recipients).toHaveValue("+1 202 555 0101"));
    fireEvent.keyDown(recipients, { key: "Enter" });
    await waitFor(() =>
      expect(host.drafts.get("conv-new-1")?.recipientIds).toEqual([
        "+12025550100",
        "+12025550101",
      ]),
    );

    fireEvent.click(screen.getByRole("button", { name: "Remove (202) 555-0100" }));
    await waitFor(() =>
      expect(host.drafts.get("conv-new-1")?.recipientIds).toEqual([
        "+12025550101",
      ]),
    );
    expect(document.querySelector('[data-recipient-id="+12025550100"]')).not.toBeInTheDocument();
    expect(document.querySelector('[data-recipient-id="+12025550101"]')).toBeInTheDocument();
  });
});

describe("floating conversation actions", () => {
  it("opens the row target without changing selection", async () => {
    render(<App />);
    await waitFor(() => expect(document.querySelector('[data-header-title]')).toHaveTextContent("Aurora"));
    fireEvent.click(document.querySelector('[data-popout-conversation-id="river"]')!);
    await waitFor(() => expect(bridge.popout_conversation).toHaveBeenCalledWith("river"));
    expect(document.querySelector('[data-header-title]')).toHaveTextContent("Aurora");
  });

  it("blocks popout when saving the target draft fails", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    host.failSaves = { code: "io", message: "Disk full." };
    type("unsaved target");
    await screen.findByRole("alert");
    fireEvent.click(document.getElementById("header-popout-conversation")!);
    await screen.findByText(/Not opened/);
    expect(bridge.popout_conversation).not.toHaveBeenCalled();
  });

  it("uses the persisted conversation ID after a local draft is rekeyed", async () => {
    render(<App />);
    fireEvent.click(await findNewConversationAction());
    const picker = await screen.findByLabelText("Recipients");
    fireEvent.change(picker, { target: { value: "+1 202 555 0100" } });
    fireEvent.keyDown(picker, { key: "Enter" });
    await waitFor(() => expect(bridge.load_state).toHaveBeenLastCalledWith("conv-new-1"));
    fireEvent.click(document.getElementById("header-popout-conversation")!);
    await waitFor(() => expect(bridge.popout_conversation).toHaveBeenCalledWith("conv-new-1"));
  });
});

describe("conversation selection", () => {
  it("keeps an explicit selection refresh when a state refresh is in flight", async () => {
    const inFlight = deferred<DesktopSnapshot>();
    const trailing = deferred<DesktopSnapshot>();
    render(<App />);
    await screen.findByText("Hello from Aurora");
    const callsBeforeRefresh = vi.mocked(bridge.load_state).mock.calls.length;
    vi.mocked(bridge.load_state)
      .mockReturnValueOnce(inFlight.promise)
      .mockReturnValueOnce(trailing.promise);

    await act(async () => hint?.());
    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledTimes(callsBeforeRefresh + 1));
    fireEvent.click(screen.getByRole("button", { name: /River/ }));
    expect(bridge.load_state).toHaveBeenCalledTimes(callsBeforeRefresh + 1);

    await act(async () => inFlight.resolve(host.load("aurora")));
    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledTimes(callsBeforeRefresh + 2));
    expect(bridge.load_state).toHaveBeenLastCalledWith("river");
    await act(async () => trailing.resolve(host.load("river")));
  });

  it("loads the selected conversation's messages and stored draft from the host", async () => {
    host.drafts.set("river", {
      id: "draft-river",
      conversationId: "river",
      text: "river draft",
      recipientIds: ["+12025550100"],
      attachmentIds: [],
      revision: "4",
    });
    render(<App />);
    expect(await screen.findByText("Hello from Aurora")).toBeInTheDocument();
    type("aurora text");
    fireEvent.click(screen.getByRole("button", { name: /River/ }));
    await waitFor(() =>
      expect(bridge.load_state).toHaveBeenLastCalledWith("river"),
    );
    expect(await screen.findByText("Hello from River")).toBeInTheDocument();
    expect(screen.queryByText("Hello from Aurora")).not.toBeInTheDocument();
    await waitFor(() => expect(message()).toHaveValue("river draft"));
    await waitFor(() =>
      expect(host.drafts.get("aurora")?.text).toBe("aurora text"),
    );

    type("river edit");
    await waitFor(() =>
      expect(host.drafts.get("river")).toMatchObject({
        text: "river edit",
        revision: "5",
      }),
    );
  });

  it("ignores a slower response for a conversation that is no longer selected", async () => {
    const slow = deferred<DesktopSnapshot>();
    render(<App />);
    await screen.findByText("Hello from Aurora");
    vi.mocked(bridge.load_state).mockImplementationOnce(() => slow.promise);
    fireEvent.click(screen.getByRole("button", { name: /River/ }));
    fireEvent.click(screen.getByRole("button", { name: /Aurora/ }));
    expect(await screen.findByText("Hello from Aurora")).toBeInTheDocument();
    await act(async () => slow.resolve(host.load("river")));
    expect(screen.getByText("Hello from Aurora")).toBeInTheDocument();
  });
});

describe("draft durability", () => {
  it("clears to a fresh message buffer before an accepted send finishes", async () => {
    const response = delayAcceptedSend();
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("sent message");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("sent message"));

    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    try {
      await waitFor(() => expect(message()).toHaveValue(""));
      type("next message");
      expect(message()).toHaveValue("next message");
    } finally {
      response.resolve();
    }
  });

  it("keeps a new local composer writable without waiting for a stored draft", async () => {
    render(<App />);
    fireEvent.click(await findNewConversationAction());

    type("new local message");

    expect(message()).toHaveValue("new local message");
  });

  it("shows a quiet retry notice when saving the active draft fails", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    host.failSaves = { code: "io", message: "Disk full." };

    type("kept locally");

    const notice = await screen.findByRole("alert");
    expect(notice).toHaveAttribute("data-draft-notice");
    expect(within(notice).getByRole("button", { name: "Retry" })).toBeEnabled();
  });

  it("keeps ordinary autosaves quiet with no saved notice", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");

    type("first quiet edit");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("first quiet edit"));
    type("second quiet edit");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("second quiet edit"));
    const latestSave = vi.mocked(bridge.save_draft).mock.results.at(-1)?.value;
    await act(async () => { await latestSave; });

    expect(document.querySelector("[data-draft-notice]")).toBeNull();
    expect(screen.queryByText("Draft saved.")).not.toBeInTheDocument();
  });

  it("shows Draft saved. only after a retry recovers a failed save, then clears it after about 2s", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    host.failSaves = { code: "io", message: "Disk full." };
    type("recover me");
    const failure = await screen.findByRole("alert");
    const retry = within(failure).getByRole("button", { name: "Retry" });
    retry.focus();

    host.failSaves = undefined;
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      fireEvent.click(retry);

      const saved = await screen.findByText("Draft saved.");
      expect(saved.closest("[data-draft-notice]")).toHaveAttribute("data-draft-notice", "saved");
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
      // The action vanished, so focus returns to the composer and is not moved later.
      expect(message()).toHaveFocus();
      await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("recover me"));

      await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
      expect(screen.getByText("Draft saved.")).toBeInTheDocument();

      await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
      expect(screen.queryByText("Draft saved.")).not.toBeInTheDocument();
      expect(message()).toHaveFocus();
    } finally {
      vi.useRealTimers();
    }
  }, 10_000);

  it("keeps the next message buffer when an accepted send resolves after typing", async () => {
    const response = delayAcceptedSend();
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("sent message");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("sent message"));

    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    try {
      await waitFor(() => expect(message()).toHaveValue(""));
      type("next message");
    } finally {
      response.resolve();
    }

    await waitFor(() => expect(message()).toHaveValue("next message"));
    expect(host.sent[0]?.text).toBe("sent message");
  });

  it("restores a failed send without overwriting the next message buffer", async () => {
    const rejection = deferred<Awaited<ReturnType<typeof bridge.send_draft>>>();
    vi.mocked(bridge.send_draft).mockReturnValueOnce(rejection.promise);
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("sent message");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("sent message"));

    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    try {
      await waitFor(() => expect(message()).toHaveValue(""));
      type("next message");
    } finally {
      rejection.reject({ code: "offline", message: "offline" });
    }

    await waitFor(() => expect(message()).toHaveValue("sent message\nnext message"));
  });

  it("requires review instead of silently retrying a stale send", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("sent message");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("sent message"));
    host.drafts.set("aurora", {
      ...host.drafts.get("aurora")!,
      text: "changed in another window",
      revision: "99",
    });

    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    const notice = await screen.findByRole("alert");
    expect(notice).toHaveAttribute("data-draft-notice", "stale-send");
    expect(notice).toHaveAttribute("data-kind", "stale-send");
    expect(notice).toHaveTextContent("Not sent. This draft changed in another window. Review it, then send again.");
    expect(message()).toHaveValue("changed in another window");
    expect(bridge.send_draft).toHaveBeenCalledOnce();
    expect(host.sent).toHaveLength(0);

    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(host.sent).toHaveLength(1));
    expect(host.sent[0].text).toBe("changed in another window");
    expect(bridge.send_draft).toHaveBeenCalledTimes(2);
  });

  it("shows retry when draft subscription setup rejects", async () => {
    const ready = Promise.reject(new Error("draft listener unavailable"));
    void ready.catch(() => {});
    const unsubscribe = Object.assign(vi.fn(), { ready });
    const draftBridge = bridge as typeof bridge & {
      subscribe_drafts(listener: (draft: Draft) => void): (() => void) & { ready?: Promise<void> };
    };
    vi.mocked(draftBridge.subscribe_drafts).mockImplementation(() => unsubscribe);

    render(<App />);

    const notice = await screen.findByRole("alert");
    expect(notice).toHaveAttribute("data-draft-notice");
    expect(notice).toHaveTextContent("Draft updates unavailable");
  });

  it("serializes delayed saves against the acknowledged revision without replacing newer text", async () => {
    const first = deferred<Draft>();
    const save = vi
      .mocked(bridge.save_draft)
      .mockImplementationOnce(() => first.promise);
    render(<App />);
    await screen.findByText("Hello from Aurora");
    type("first");
    await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
    type("newer");
    first.resolve(host.save(save.mock.calls[0][0]));
    await waitFor(() => expect(save).toHaveBeenCalledTimes(2));
    expect(save.mock.calls[1][0]).toMatchObject({
      id: "draft-aurora",
      text: "newer",
      expectedRevision: "1",
    });
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("newer"));
    expect(message()).toHaveValue("newer");
  });

  it("blocks send after a rejected save, keeps the edits, and sends the latest text once a retry succeeds", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    await waitFor(() => expect(host.drafts.get("aurora")?.simId).toBe("sim-1"));
    type("saved text");
    await waitFor(() =>
      expect(host.drafts.get("aurora")?.text).toBe("saved text"),
    );

    host.failSaves = { code: "io", message: "Disk full." };
    type("latest text");
    const saveFailure = await screen.findByRole("alert");
    expect(saveFailure).toHaveAttribute("data-draft-notice", "save-failed");
    expect(saveFailure).toHaveTextContent("Draft not saved. Your text is kept.");
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(bridge.send_draft).not.toHaveBeenCalled();
    expect(screen.getByRole("alert")).toHaveAttribute("data-draft-notice", "save-failed");
    expect(message()).toHaveValue("latest text");

    host.failSaves = undefined;
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() =>
      expect(screen.queryByRole("alert")).not.toBeInTheDocument(),
    );
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(host.sent).toHaveLength(1));
    expect(host.sent[0].text).toBe("latest text");
    await waitFor(() => expect(message()).toHaveValue(""));
  });

  it("verifies the moved destination before any retained-text write and never overwrites it", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("send only after review");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("send only after review"));
    rejectSendAsMovedToRiver();
    const navigationLoad = deferred<DesktopSnapshot>();
    vi.mocked(bridge.load_state).mockReturnValueOnce(navigationLoad.promise);

    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledWith("river"));
    expect(host.drafts.get("river")?.text).toBe("destination hello");

    await act(async () => navigationLoad.resolve(host.load("river")));
    await waitFor(() => expect(document.querySelector("[data-header-title]")).toHaveTextContent("River"));
    const moved = await screen.findByText("Draft added to your existing conversation with River.");
    expect(moved.closest("[data-draft-notice]")).toHaveAttribute("data-draft-notice", "moved");
    expect(bridge.send_draft).toHaveBeenCalledTimes(1);
    expect(host.sent).toHaveLength(0);
    expect(host.drafts.get("river")?.text).toBe("destination hello");
    expect(document.querySelector("[data-draft-conflict-comparison]")).toBeNull();
  });

  it("copies the exact moved text on demand and keeps it when the clipboard write fails", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("send only after review");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("send only after review"));
    rejectSendAsMovedToRiver();
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const moved = (await screen.findByText("Draft added to your existing conversation with River.")).closest("[data-draft-notice]") as HTMLElement;
    const writeText = stubClipboard();
    writeText.mockRejectedValueOnce(new Error("clipboard denied"));

    fireEvent.click(within(moved).getByRole("button", { name: "Copy text of retained draft" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("send only after review"));
    expect(moved).toBeInTheDocument();
    expect(within(moved).getByRole("button", { name: "Copy text of retained draft" })).toBeEnabled();

    fireEvent.click(within(moved).getByRole("button", { name: "Copy text of retained draft" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledTimes(2));
    expect(writeText).toHaveBeenLastCalledWith("send only after review");
    expect(host.drafts.get("river")?.text).toBe("destination hello");
  });

  it("keeps the moved copy action through an ordinary background refresh", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("restore from source");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("restore from source"));
    rejectSendAsMovedToRiver();
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const moved = (await screen.findByText("Draft added to your existing conversation with River.")).closest("[data-draft-notice]") as HTMLElement;

    await act(async () => hint?.());

    expect(moved).toBeInTheDocument();
    const writeText = stubClipboard();
    fireEvent.click(within(moved).getByRole("button", { name: "Copy text of retained draft" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("restore from source"));
    expect(host.drafts.get("river")?.text).toBe("destination hello");
  });

  it("does not write the moved draft from a deferred lookup that resolves after a background refresh", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("restore after refresh");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("restore after refresh"));
    rejectSendAsMovedToRiver();
    const lookup = deferred<DesktopSnapshot>();
    vi.mocked(bridge.load_state).mockReturnValueOnce(lookup.promise);
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledWith("river"));

    await act(async () => hint?.());
    await act(async () => lookup.resolve(host.load("river")));

    expect(host.drafts.get("river")?.text).toBe("destination hello");
    expect(host.drafts.get("aurora")?.text).toBe("restore after refresh");
  });

  it("keeps the source text and offers Retry when the destination lookup fails", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("source feedback");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("source feedback"));
    rejectSendAsMovedToRiver();
    vi.mocked(bridge.load_state).mockRejectedValueOnce(new Error("load failed"));

    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    const failure = await screen.findByRole("alert");
    expect(failure).toHaveAttribute("data-draft-notice");
    expect(within(failure).getByRole("button", { name: "Retry" })).toBeEnabled();
    expect(host.drafts.get("aurora")?.text).toBe("source feedback");
    expect(host.drafts.get("river")?.text).toBe("destination hello");

    fireEvent.click(within(failure).getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    expect(host.drafts.get("aurora")?.text).toBe("source feedback");
  });

  it("keeps retained content recoverable and lifecycle unblocked when destination hydration fails", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("recover after failed load");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("recover after failed load"));
    vi.mocked(bridge.load_state).mockRejectedValue(new Error("load failed"));
    rejectSendAsMovedToRiver();

    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const failure = await screen.findByRole("alert");
    expect(within(failure).getByRole("button", { name: "Retry" })).toBeEnabled();
    expect(host.drafts.get("aurora")?.text).toBe("recover after failed load");

    await act(async () => lifecycle?.({ id: "request-close-after-recovery", action: "close" }));
    await waitFor(() => expect(bridge.acknowledge_lifecycle).toHaveBeenCalledWith("request-close-after-recovery", true));
  });

  it("does not apply retained content when destination hydration redirects", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("recover after redirect");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("recover after redirect"));
    const redirected = host.load("aurora");
    vi.mocked(bridge.load_state).mockResolvedValue(redirected);
    rejectSendAsMovedToRiver();

    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledWith("river"));
    expect(screen.queryByText("Draft added to your existing conversation with River.")).not.toBeInTheDocument();
    expect(host.drafts.get("river")?.text).toBe("destination hello");
    expect(host.drafts.get("aurora")?.text).toBe("recover after redirect");

    await act(async () => lifecycle?.({ id: "request-close-after-redirect", action: "close" }));
    await waitFor(() => expect(bridge.acknowledge_lifecycle).toHaveBeenCalledWith("request-close-after-redirect", true));
  });

  it("drops a destination recovery load after the active session changes", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("recover in original session");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("recover in original session"));
    rejectSendAsMovedToRiver();
    const moveLookup = deferred<DesktopSnapshot>();
    vi.mocked(bridge.load_state).mockReturnValueOnce(moveLookup.promise);
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledWith("river"));

    host.connection = { ...host.connection, origin: "https://other-session.test" };
    await act(async () => hint?.());
    await waitFor(() => expect(document.getElementById("connection-status")).toHaveTextContent("Connected"));
    await act(async () => moveLookup.resolve({ ...host.load("river"), connection: { state: "connected", origin: "https://example.test" } }));
    expect(host.drafts.get("river")?.text).toBe("destination hello");
    expect(screen.queryByText("Draft added to your existing conversation with River.")).not.toBeInTheDocument();
    expect(host.drafts.get("aurora")?.text).toBe("recover in original session");

    await act(async () => lifecycle?.({ id: "request-close-after-session-change", action: "close" }));
    await waitFor(() => expect(bridge.acknowledge_lifecycle).toHaveBeenCalledWith("request-close-after-session-change", true));
  });

  it("keeps late-save text visible and explicitly saves a deleted draft as new", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    host.failSaves = { code: "not-found", message: "deleted" };
    type("late local text");

    const failure = await screen.findByRole("alert");
    expect(failure).toHaveAttribute("data-draft-notice", "save-failed");
    const recover = within(failure).getByRole("button", { name: "Save as new draft" });
    expect(recover).toHaveAttribute("data-draft-action", "save-new");
    expect(message()).toHaveValue("late local text");
    expect(document.querySelector("[data-draft-conflict-comparison]")).toBeNull();

    host.failSaves = undefined;
    fireEvent.click(recover);
    await waitFor(() => expect(message()).toHaveValue("late local text"));

    await act(async () => hint?.());
    expect(document.querySelector("[data-draft-notice='save-failed']")).toBeNull();
    expect(message()).toHaveValue("late local text");
  });

  it("keeps local text when a stale save only diverges in text, with a quiet Undo for the replaced host text", async () => {
    host.drafts.set("aurora", {
      id: "draft-aurora",
      conversationId: "aurora",
      text: "stored",
      recipientIds: [],
      attachmentIds: [],
      revision: "1",
    });
    render(<App />);
    await waitFor(() => expect(message()).toHaveValue("stored"));
    host.drafts.set("aurora", {
      ...host.drafts.get("aurora")!,
      text: "other window",
      revision: "2",
    });
    type("mine");

    const replaced = (await screen.findByText("Replaced text from another window.")).closest("[data-draft-notice]") as HTMLElement;
    expect(replaced).toHaveAttribute("data-draft-notice", "replaced");
    expect(replaced).toHaveAttribute("role", "status");
    expect(message()).toHaveValue("mine");
    await waitFor(() =>
      expect(host.drafts.get("aurora")).toMatchObject({ text: "mine", revision: "3" }),
    );
    expect(document.querySelector("[data-draft-conflict-comparison]")).toBeNull();

    fireEvent.click(within(replaced).getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(message()).toHaveValue("other window"));
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("other window"));
  });

  it("adopts a same-recipient remote text change with an Undo that restores the previous text", async () => {
    host.drafts.set("aurora", { id: "draft-aurora", conversationId: "aurora", text: "stored", recipientIds: [], attachmentIds: [], revision: "1" });
    render(<App />);
    await waitFor(() => expect(message()).toHaveValue("stored"));
    host.drafts.set("aurora", { ...host.drafts.get("aurora")!, text: "from window two", revision: "2" });

    await act(async () => hint?.());

    expect(message()).toHaveValue("from window two");
    const updated = (await screen.findByText("Updated from another window.")).closest("[data-draft-notice]") as HTMLElement;
    expect(updated).toHaveAttribute("data-draft-notice", "remote-update");
    fireEvent.click(within(updated).getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(message()).toHaveValue("stored"));
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("stored"));
  });

  it("offers copy, not Undo, when a remote update changed the recipients of the local text", async () => {
    host.drafts.set("aurora", { id: "draft-aurora", conversationId: "aurora", text: "stored", recipientIds: [], attachmentIds: [], revision: "1" });
    render(<App />);
    await waitFor(() => expect(message()).toHaveValue("stored"));
    type("mine");
    await waitFor(() => expect(host.drafts.get("aurora")).toMatchObject({ text: "mine", revision: "2" }));
    host.drafts.set("aurora", { id: "draft-aurora", conversationId: "aurora", text: "other window", recipientIds: ["+15550100"], attachmentIds: [], revision: "3" });

    await act(async () => hint?.());

    const updated = (await screen.findByText("Updated from another window.")).closest("[data-draft-notice]") as HTMLElement;
    expect(within(updated).queryByRole("button", { name: "Undo" })).not.toBeInTheDocument();
    const writeText = stubClipboard();
    fireEvent.click(within(updated).getByRole("button", { name: "Copy text of retained draft" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("mine"));
    expect(message()).toHaveValue("other window");
  });

  it("keeps local text until explicit recovery recreates a draft deleted elsewhere", async () => {
    host.drafts.set("aurora", { id: "draft-aurora", conversationId: "aurora", text: "stored", recipientIds: [], attachmentIds: [], revision: "1" });
    render(<App />);
    await waitFor(() => expect(message()).toHaveValue("stored"));
    host.drafts.delete("aurora");
    type("local version");

    const failure = await screen.findByRole("alert");
    expect(failure).toHaveAttribute("data-draft-notice", "save-failed");
    const recover = within(failure).getByRole("button", { name: "Save as new draft" });
    expect(message()).toHaveValue("local version");
    expect(host.drafts.has("aurora")).toBe(false);
    fireEvent.click(recover);
    await waitFor(() => expect(message()).toHaveValue("local version"));
  });

  it("requires explicit confirmation before discarding a deleted draft's local changes", async () => {
    host.drafts.set("aurora", { id: "draft-aurora", conversationId: "aurora", text: "stored", recipientIds: [], attachmentIds: [], revision: "1" });
    render(<App />);
    await waitFor(() => expect(message()).toHaveValue("stored"));
    host.drafts.delete("aurora");
    type("local version");

    fireEvent.click(await screen.findByRole("button", { name: "Discard local changes" }));
    const confirmation = screen.getByRole("alertdialog", { name: "Discard local changes?" });
    expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus();
    fireEvent.keyDown(confirmation, { key: "Escape" });
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(message()).toHaveValue("local version");

    fireEvent.click(screen.getByRole("button", { name: "Discard local changes" }));
    fireEvent.click(screen.getByRole("button", { name: "Discard local changes" }));
    await waitFor(() => expect(document.querySelector("[data-draft-notice='save-failed']")).toBeNull());
    expect(bridge.discard_draft).not.toHaveBeenCalled();
    expect(host.drafts.has("aurora")).toBe(false);
  });

  it("reloads the conversation after a confirmed local discard so the composer leaves loading without an external event", async () => {
    host.drafts.set("aurora", { id: "draft-aurora", conversationId: "aurora", text: "stored", recipientIds: [], attachmentIds: [], revision: "1" });
    render(<App />);
    await waitFor(() => expect(message()).toHaveValue("stored"));
    host.drafts.delete("aurora");
    type("local version");
    await waitFor(() => expect(document.querySelector("[data-draft-notice='save-failed']")).not.toBeNull());
    vi.mocked(bridge.save_draft).mockClear();
    vi.mocked(bridge.discard_draft).mockClear();
    vi.mocked(bridge.load_state).mockClear();

    fireEvent.click(await screen.findByRole("button", { name: "Discard local changes" }));
    fireEvent.click(screen.getByRole("button", { name: "Discard local changes" }));

    await waitFor(() => expect(vi.mocked(bridge.load_state).mock.calls.some(([id]) => id === "aurora")).toBe(true));
    await waitFor(() => expect(message().readOnly).toBe(false));
    expect(message().placeholder).not.toBe("Loading draft…");
    expect(bridge.save_draft).not.toHaveBeenCalled();
    expect(bridge.discard_draft).not.toHaveBeenCalled();
  });

  it("keeps local text and retries when the catch-up lookup returns a mismatched identity", async () => {
    host.drafts.set("aurora", { id: "draft-aurora", conversationId: "aurora", text: "stored", recipientIds: [], attachmentIds: [], revision: "1" });
    render(<App />);
    await waitFor(() => expect(message()).toHaveValue("stored"));
    host.drafts.set("aurora", { ...host.drafts.get("aurora")!, text: "other writer", revision: "2" });
    const mismatched = { ...host.load("aurora"), draft: { id: "draft-river", conversationId: "river", text: "wrong conversation", recipientIds: [], attachmentIds: [], revision: "2" } };
    vi.mocked(bridge.load_state).mockResolvedValueOnce(mismatched);
    type("local version");

    const failure = await screen.findByRole("alert");
    expect(failure).toHaveAttribute("data-draft-notice");
    expect(within(failure).getByRole("button", { name: "Retry" })).toBeEnabled();
    expect(message()).toHaveValue("local version");
    expect(host.drafts.get("aurora")).toMatchObject({ id: "draft-aurora", text: "other writer", revision: "2" });
    expect(host.drafts.has("river")).toBe(false);
  });

  it("keeps newer typing when a catch-up lookup resolves after the user types again", async () => {
    host.drafts.set("aurora", { id: "draft-aurora", conversationId: "aurora", text: "stored", recipientIds: [], attachmentIds: [], revision: "1" });
    render(<App />);
    await waitFor(() => expect(message()).toHaveValue("stored"));
    host.drafts.set("aurora", { ...host.drafts.get("aurora")!, text: "other writer", revision: "2" });
    const catchUpLookup = deferred<DesktopSnapshot>();
    vi.mocked(bridge.load_state).mockReturnValueOnce(catchUpLookup.promise);
    type("local version");
    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledWith("aurora"));

    type("newer local version");
    await act(async () => catchUpLookup.resolve(host.load("aurora")));

    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("newer local version"));
    expect(message()).toHaveValue("newer local version");
  });

  it("surfaces a real save failure during a stale catch-up without losing local text or sending", async () => {
    host.drafts.set("aurora", { id: "draft-aurora", conversationId: "aurora", text: "stored", recipientIds: [], attachmentIds: [], revision: "1" });
    render(<App />);
    await waitFor(() => expect(message()).toHaveValue("stored"));
    await chooseGateway(SMS_ONLY);
    await waitFor(() => expect(host.drafts.get("aurora")?.simId).toBe("sim-1"));
    const routed = host.drafts.get("aurora")!;
    host.drafts.set("aurora", { ...routed, text: "other window", revision: String(Number(routed.revision) + 1) });
    vi.mocked(bridge.save_draft)
      .mockImplementationOnce(async (input) => host.save(input))
      .mockRejectedValueOnce({ code: "io", message: "Disk full." } satisfies HostError);
    type("mine");

    const failure = await screen.findByRole("alert");
    expect(failure).toHaveAttribute("data-draft-notice", "save-failed");
    expect(failure).toHaveTextContent("Draft not saved. Your text is kept.");
    expect(within(failure).getByRole("button", { name: "Retry" })).toBeEnabled();
    expect(message()).toHaveValue("mine");
    expect(host.drafts.get("aurora")).toMatchObject({ text: "other window", simId: "sim-1" });

    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(bridge.send_draft).not.toHaveBeenCalled();
    expect(message()).toHaveValue("mine");
  });

  it("retains dirty drafts across live state hints and adopts host changes once clean", async () => {
    const pending = deferred<Draft>();
    const save = vi
      .mocked(bridge.save_draft)
      .mockImplementationOnce(() => pending.promise);
    render(<App />);
    await screen.findByText("Hello from Aurora");
    type("keep local");
    await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
    host.drafts.set("aurora", {
      id: "draft-aurora",
      conversationId: "aurora",
      text: "host text",
      recipientIds: [],
      attachmentIds: [],
      revision: "7",
    });
    await act(async () => hint?.());
    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledTimes(2));
    expect(message()).toHaveValue("keep local");

    pending.resolve({
      id: "draft-aurora",
      conversationId: "aurora",
      text: "keep local",
      recipientIds: [],
      attachmentIds: [],
      revision: "8",
    });
    host.drafts.set("aurora", {
      id: "draft-aurora",
      conversationId: "aurora",
      text: "keep local",
      recipientIds: [],
      attachmentIds: [],
      revision: "8",
    });
    await waitFor(() =>
      expect(screen.queryByRole("alert")).not.toBeInTheDocument(),
    );
    host.drafts.set("aurora", {
      id: "draft-aurora",
      conversationId: "aurora",
      text: "edited in composer window",
      recipientIds: [],
      attachmentIds: [],
      revision: "9",
    });
    await act(async () => hint?.());
    await waitFor(() =>
      expect(message()).toHaveValue("edited in composer window"),
    );
  });

  it("keeps a draft whose save failed when a live hint arrives", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    host.failSaves = { code: "io", message: "Disk full." };
    type("unsaved");
    await screen.findByRole("alert");
    await act(async () => hint?.());
    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledTimes(2));
    expect(message()).toHaveValue("unsaved");
    expect(screen.getByRole("alert")).toBeInTheDocument();
  });

  it("shows composer send feedback only in the conversation that produced it", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    await waitFor(() => expect(host.drafts.get("aurora")?.simId).toBe("sim-1"));
    type("reply to Aurora");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("reply to Aurora"));

    vi.mocked(bridge.send_draft).mockResolvedValueOnce({ accepted: false, status: "failed-before-submit", reason: "Carrier refused the message." });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const feedback = await screen.findByText("Not sent: Carrier refused the message.");
    const notice = feedback.closest("[data-draft-notice]") as HTMLElement;
    expect(notice).toHaveAttribute("data-draft-notice", "send-failed");
    expect(notice).toHaveAttribute("role", "alert");
    expect(document.getElementById("composer-area")).toContainElement(notice);
    expect(document.getElementById("app-notice-banner")?.textContent ?? "").not.toContain("Carrier refused the message.");

    fireEvent.click(screen.getByRole("button", { name: /River/ }));
    await screen.findByText("Hello from River");
    expect(screen.queryByText(/Carrier refused the message\./)).not.toBeInTheDocument();
    expect(document.querySelector("[data-draft-notice='send-failed']")).toBeNull();
    expect(bridge.send_draft).toHaveBeenCalledTimes(1);
  });

  it("restores a send superseded by a refresh so typing persists and close is not held", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("restore after refresh");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("restore after refresh"));
    rejectSendAsMovedToRiver();
    const lookup = deferred<DesktopSnapshot>();
    vi.mocked(bridge.load_state).mockReturnValueOnce(lookup.promise);

    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledWith("river"));
    await act(async () => hint?.());
    await act(async () => lookup.resolve(host.load("river")));

    await waitFor(() => expect(message()).toHaveValue("restore after refresh"));
    type("restore after refresh\ntyped after refresh");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("restore after refresh\ntyped after refresh"));
    await act(async () => lifecycle?.({ id: "close-after-superseded-send", action: "close" }));
    await waitFor(() => expect(bridge.acknowledge_lifecycle).toHaveBeenCalledWith("close-after-superseded-send", true));
  });

  it("finishes bootstrap and applies remote events after a state hint during listener readiness", async () => {
    const ready = deferred<void>();
    const draftBridge = bridge as typeof bridge & { subscribe_drafts(listener: (draft: Draft) => void): (() => void) & { ready?: Promise<void> } };
    vi.mocked(draftBridge.subscribe_drafts).mockImplementation(listener => {
      draftHint = listener;
      return Object.assign(vi.fn(), { ready: ready.promise });
    });
    render(<App />);

    await act(async () => hint?.());
    await act(async () => ready.resolve());
    await screen.findByText("Hello from Aurora");
    const remote = { id: "draft-aurora", conversationId: "aurora", text: "ready remote", recipientIds: [], attachmentIds: [], revision: "5" };
    host.drafts.set("aurora", remote);
    await act(async () => draftHint?.(remote));

    await waitFor(() => expect(message()).toHaveValue("ready remote"));
    expect(document.getElementById("desktop-loading-state")).not.toBeInTheDocument();
  });

  it("P3 binds remote draft events after retrying an initial refresh failure", async () => {
    vi.mocked(bridge.load_state).mockRejectedValueOnce(new Error("Initial state unavailable"));
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "Retry refresh" }));
    await screen.findByText("Hello from Aurora");
    const remote = { id: "draft-aurora", conversationId: "aurora", text: "retried remote", recipientIds: [], attachmentIds: [], revision: "5" };
    host.drafts.set("aurora", remote);
    await act(async () => draftHint?.(remote));

    await waitFor(() => expect(message()).toHaveValue("retried remote"));
  });

  it("P4 clears stale-send feedback on edit and successful send while retaining the unsent text copy", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("local before stale send");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("local before stale send"));
    host.drafts.set("aurora", { ...host.drafts.get("aurora")!, text: "remote changed", revision: "99" });

    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const stale = await screen.findByRole("alert");
    expect(stale).toHaveAttribute("data-draft-notice", "stale-send");
    expect(within(stale).getByRole("button", { name: "Copy text of retained draft" })).toBeEnabled();
    type("remote changed and edited");
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(host.sent).toHaveLength(1));
    expect(document.querySelector("[data-draft-notice='stale-send']")).toBeNull();
  });

  it("P5 applies the explicit latest Undo to D, never obsolete A", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    type("A");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("A"));
    const a = host.drafts.get("aurora")!;
    const b = { ...a, text: "B", revision: String(Number(a.revision) + 1) };
    host.drafts.set("aurora", b);
    await act(async () => draftHint?.(b));
    await waitFor(() => expect(message()).toHaveValue("B"));
    type("C");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("C"));
    const c = host.drafts.get("aurora")!;
    host.drafts.set("aurora", { ...c, text: "D", revision: String(Number(c.revision) + 1) });
    type("E");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("E"));

    const undo = screen.queryByRole("button", { name: "Undo" });
    expect(message()).toHaveValue("E");
    if (undo) {
      fireEvent.click(undo);
      await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("D"));
      expect(message()).toHaveValue("D");
    } else {
      expect(host.drafts.get("aurora")?.text).toBe("E");
    }
    expect(message()).not.toHaveValue("A");
  });

  it("P14 surfaces Retry when draft listener readiness rejects after an early state hint", async () => {
    const ready = deferred<void>();
    const draftBridge = bridge as typeof bridge & { subscribe_drafts(listener: (draft: Draft) => void): (() => void) & { ready?: Promise<void> } };
    vi.mocked(draftBridge.subscribe_drafts).mockImplementation(() => Object.assign(vi.fn(), { ready: ready.promise }));
    render(<App />);

    await act(async () => hint?.());
    await act(async () => ready.reject(new Error("listener unavailable")));

    const retry = await screen.findByRole("button", { name: "Retry" });
    expect(document.getElementById("desktop-loading-state")).not.toBeInTheDocument();
    expect(screen.getByText(/Draft updates unavailable/)).toBeInTheDocument();
    fireEvent.click(retry);
  });

  it("P15 drains two clean review outcomes before acknowledging lifecycle close", async () => {
    let liveStore: DraftStore | undefined;
    const adopt = DraftStore.prototype.adopt;
    vi.spyOn(DraftStore.prototype, "adopt").mockImplementation(function captureStore(this: DraftStore, ...args: Parameters<typeof adopt>) {
      liveStore = this;
      return adopt.apply(this, args);
    });
    render(<App />);
    await screen.findByText("Hello from Aurora");
    if (!liveStore) throw new Error("Draft store was not initialized.");

    for (const key of ["aurora", "river"]) {
      const base = { id: `draft-${key}`, conversationId: key, text: `base ${key}`, recipientIds: [], attachmentIds: [], revision: "1" };
      host.drafts.set(key, base);
      liveStore.adopt(key, base);
      host.drafts.set(key, { ...base, text: `host ${key}`, recipientIds: ["+18015550000"], revision: "2" });
      await liveStore.edit(key, { text: `local ${key}` });
    }

    await act(async () => lifecycle?.({ id: "two-clean-reviews", action: "close" }));
    await waitFor(() => expect(bridge.acknowledge_lifecycle).toHaveBeenCalledWith("two-clean-reviews", true));
  });

  it("P16 selects the failed draft and Retry after lifecycle failure from Settings", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    host.failSaves = { code: "io", message: "Disk full." };
    type("unsaved in settings");
    await screen.findByRole("button", { name: "Retry" });
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    await screen.findByRole("region", { name: "Settings" });

    await act(async () => lifecycle?.({ id: "settings-save-failure", action: "close" }));

    await waitFor(() => expect(bridge.acknowledge_lifecycle).toHaveBeenCalledWith("settings-save-failure", false));
    expect(screen.queryByRole("region", { name: "Settings" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Retry" })).toBeEnabled();
    expect(message()).toHaveValue("unsaved in settings");
  });

  it("acknowledges close after a clean structural remote review", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    type("A");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("A"));
    const stored = host.drafts.get("aurora")!;
    host.drafts.set("aurora", { ...stored, recipientIds: ["+18015550000"], revision: String(Number(stored.revision) + 1) });
    type("AB");
    await waitFor(() => expect(message()).toHaveValue("AB"));

    await act(async () => lifecycle?.({ id: "close-after-clean-review", action: "close" }));
    await waitFor(() => expect(bridge.acknowledge_lifecycle).toHaveBeenCalledWith("close-after-clean-review", true));
  });

  it("keeps retained text actionable when clipboard support is unavailable", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("copy without clipboard");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("copy without clipboard"));
    rejectSendAsMovedToRiver();
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const notice = (await screen.findByText("Draft added to your existing conversation with River.")).closest("[data-draft-notice]") as HTMLElement;
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
    onTestFinished(() => { delete (navigator as { clipboard?: unknown }).clipboard; });

    fireEvent.click(within(notice).getByRole("button", { name: "Copy text of retained draft" }));
    await waitFor(() => expect(within(notice).getByRole("button", { name: "Copy text of retained draft" })).toBeEnabled());
    expect(notice).toBeInTheDocument();
  });

  it("does not let an offscreen flush failure hide the failing draft or its retry", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: /River/ }));
    await screen.findByText("Hello from River");
    host.failSaves = { code: "io", message: "River disk full." };
    type("river unsaved");
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: /Aurora/ }));
    await screen.findByText("Hello from Aurora");

    await act(async () => lifecycle?.({ id: "close-with-river-failure", action: "close" }));
    await waitFor(() => expect(bridge.acknowledge_lifecycle).toHaveBeenCalledWith("close-with-river-failure", false));
    expect(document.querySelector("[data-header-title]")).toHaveTextContent("River");
    const draftFailure = document.querySelector<HTMLElement>("[data-draft-notice='save-failed']")!;
    expect(draftFailure).toHaveTextContent("Draft not saved. Your text is kept.");
    expect(within(draftFailure).getByRole("button", { name: "Retry" })).toBeEnabled();
  });

  it("keeps the active draft alert when another conversation emits a background notice", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    host.failSaves = { code: "io", message: "Aurora disk full." };
    type("aurora unsaved");
    const activeAlert = await screen.findByRole("alert");
    expect(activeAlert).toHaveAttribute("data-draft-notice", "save-failed");
    const riverDraft = { id: "draft-river", conversationId: "river", text: "River updated elsewhere", recipientIds: [], attachmentIds: [], revision: "4" };
    host.drafts.set("river", riverDraft);

    await act(async () => draftHint?.(riverDraft));

    expect(screen.getByRole("alert")).toHaveAttribute("data-draft-notice", "save-failed");
    expect(message()).toHaveValue("aurora unsaved");
  });

  it("P8 discards an expired Aurora notice so a later Aurora update exposes its Undo despite River activity", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: /River/ }));
    await screen.findByText("Hello from River");
    type("River local");
    await waitFor(() => expect(host.drafts.get("river")?.text).toBe("River local"));
    fireEvent.click(screen.getByRole("button", { name: /Aurora/ }));
    await screen.findByText("Hello from Aurora");
    type("Aurora local");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("Aurora local"));
    const aurora = host.drafts.get("aurora")!;
    const firstRemote = { ...aurora, text: "Aurora remote", revision: String(Number(aurora.revision) + 1) };
    host.drafts.set("aurora", firstRemote);
    vi.useFakeTimers();
    try {
      await act(async () => draftHint?.(firstRemote));
      expect(screen.getByRole("button", { name: "Undo" })).toBeInTheDocument();
      await act(async () => { await vi.advanceTimersByTimeAsync(5_000); });
      expect(screen.queryByRole("button", { name: "Undo" })).not.toBeInTheDocument();

      const river = host.drafts.get("river")!;
      const riverRemote = { ...river, text: "River remote", revision: String(Number(river.revision) + 1) };
      host.drafts.set("river", riverRemote);
      await act(async () => draftHint?.(riverRemote));
      const secondRemote = { ...firstRemote, text: "Aurora remote again", revision: String(Number(firstRemote.revision) + 1) };
      host.drafts.set("aurora", secondRemote);
      await act(async () => draftHint?.(secondRemote));

      expect(screen.getByRole("button", { name: "Undo" })).toBeEnabled();
    } finally {
      vi.useRealTimers();
    }
  }, 10_000);

  it("P12 restores composer focus after fallback retained-text copy", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    type("local retained text");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("local retained text"));
    const local = host.drafts.get("aurora")!;
    const structuralRemote = { ...local, text: "remote structural", recipientIds: ["+18015550000"], revision: String(Number(local.revision) + 1) };
    host.drafts.set("aurora", structuralRemote);
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      await act(async () => draftHint?.(structuralRemote));
      expect(screen.getByRole("button", { name: "Copy text of retained draft" })).toBeInTheDocument();
      await act(async () => { await vi.advanceTimersByTimeAsync(4_250); });
      vi.useRealTimers();

      const copy = await screen.findByRole("button", { name: "Copy text of retained draft" });
      const clipboard = stubClipboard();
      copy.focus();
      fireEvent.click(copy);

      await waitFor(() => expect(clipboard).toHaveBeenCalledWith("local retained text"));
      await waitFor(() => expect(message()).toHaveFocus());
    } finally {
      vi.useRealTimers();
    }
  }, 12_000);

  it("flushes typing that arrives during send preparation before native sends the stored row", async () => {
    const firstSave = deferred<Draft>();
    const save = vi.mocked(bridge.save_draft).mockImplementationOnce(() => firstSave.promise);
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await chooseGateway(SMS_ONLY);
    type("older text");
    await waitFor(() => expect(save).toHaveBeenCalledOnce());

    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    type("newer text");
    firstSave.resolve(host.save(save.mock.calls[0][0]));

    await waitFor(() => expect(host.sent).toHaveLength(1));
    expect(host.sent[0]?.text).toBe("newer text");
    expect(host.drafts.get("aurora")?.text).toBe("");
  });
});

describe("composer window", () => {
  it("waits for the stored draft before mounting the composer editor", async () => {
    host.drafts.set("aurora", {
      id: "draft-aurora",
      conversationId: "aurora",
      text: "stored before the composer mounted",
      recipientIds: [],
      attachmentIds: [],
      revision: "4",
    });
    const pending = deferred<DesktopSnapshot>();
    vi.mocked(bridge.load_state).mockReturnValue(pending.promise);
    openComposerWindow("aurora");
    render(<App />);
    expect(screen.queryByLabelText("Message")).not.toBeInTheDocument();
    expect(bridge.save_draft).not.toHaveBeenCalled();
    await act(async () => pending.resolve(host.load("aurora")));
    expect(await screen.findByLabelText("Message")).toHaveValue("stored before the composer mounted");
    expect(bridge.save_draft).not.toHaveBeenCalled();
  });

  it("bootstraps head chrome and converts in place without losing the draft", async () => {
    openComposerWindow("aurora", true);
    render(<App />);
    expect(await screen.findByRole("banner", { name: "Floating conversation with Aurora" })).toBeInTheDocument();
    type("preserve while converting");
    await waitFor(() => expect(host.drafts.get("aurora")?.text).toBe("preserve while converting"));
    host.head = { ...host.head, panel: false };
    await act(async () => hint?.());
    await waitFor(() => expect(document.getElementById("desktop-titlebar")).toBeInTheDocument());
    expect(message()).toHaveValue("preserve while converting");
  });
  it("shows the draft's selected route as a read-only head pill", async () => {
    host.gateways = [{ ...SMS_ONLY, availability: "delayed", lastSeenAgeSeconds: 120 }, { ...MMS_SIM, availability: "connected" }];
    host.drafts.set("aurora", {
      id: "draft-aurora",
      conversationId: "aurora",
      text: "",
      recipientIds: [],
      attachmentIds: [],
      gatewayId: "gw-phone",
      simId: "sim-2",
      revision: "1",
    });
    openComposerWindow("aurora", true);
    render(<App />);

    const pill = await waitFor(() => {
      const element = document.getElementById("head-panel-route");
      if (!element) throw new Error("Missing head route pill");
      return element;
    });
    expect(pill).toHaveAttribute("title", "Phone: choose this phone and SIM in the main window.");
    expect(pill).toHaveAttribute("data-gateway-availability", "connected");
    expect(pill).toHaveTextContent("SIM 2");
    expect(pill).toHaveTextContent("Online");
    expect(pill.querySelector("[aria-hidden]")).not.toBeInTheDocument();
    expect(document.getElementById("head-panel-route-description")).toHaveTextContent("Phone, SIM 2. Phone and SIM are selected in the main window.");
    expect(screen.queryByRole("combobox", { name: "Phone" })).not.toBeInTheDocument();
  });
  it("numbers one SIM per gateway independently in route controls and the head pill", async () => {
    const backupGateway = { ...SMS_ONLY, id: "gw-backup", name: "Backup phone" };
    host.gateways = [SMS_ONLY, backupGateway];
    render(<App />);

    fireEvent.click(await screen.findByRole("combobox", { name: "Phone" }));
    const options = await screen.findAllByRole("option");
    for (const gateway of host.gateways) {
      const option = options.find((candidate) => candidate.getAttribute("data-gateway-choice") === routeValue(gateway));
      expect(option).toHaveTextContent("SIM 1");
    }

    cleanup();
    host.drafts.set("aurora", {
      id: "draft-aurora",
      conversationId: "aurora",
      text: "",
      recipientIds: [],
      attachmentIds: [],
      gatewayId: backupGateway.id,
      simId: backupGateway.simId,
      revision: "1",
    });
    openComposerWindow("aurora", true);
    render(<App />);

    await waitFor(() => expect(document.getElementById("head-panel-route")).toHaveTextContent("SIM 1"));
  });
  it("points no-route and stale head recovery to the main window", async () => {
    openComposerWindow("aurora", true);
    render(<App />);
    await waitFor(() => expect(document.getElementById("head-panel-route")).toHaveTextContent("No SIM chosen"));
    expect(document.getElementById("unavailable-hint")).toHaveTextContent("choose a phone and SIM in the main window");

    cleanup();
    host.gateways = [SMS_ONLY];
    host.drafts.set("aurora", {
      id: "draft-aurora",
      conversationId: "aurora",
      text: "",
      recipientIds: [],
      attachmentIds: [],
      gatewayId: "gw-phone",
      simId: "sim-9",
      revision: "1",
    });
    render(<App />);
    await waitFor(() => expect(document.getElementById("head-panel-route")).toHaveTextContent("SIM unavailable"));
    expect(document.getElementById("unavailable-hint")).toHaveTextContent("the selected phone/SIM is no longer reported; choose another route in the main window");

    cleanup();
    host.gateways = [];
    host.drafts.clear();
    render(<App />);
    await waitFor(() => expect(document.getElementById("head-panel-route")).toHaveTextContent("No phone"));
    expect(document.getElementById("unavailable-hint")).toHaveTextContent("no phone is configured");
  });
  it("renders each supported head route state without a picker", async () => {
    const selectedDraft = (simId: string) => ({
      id: "draft-aurora",
      conversationId: "aurora",
      text: "",
      recipientIds: [],
      attachmentIds: [],
      gatewayId: "gw-phone",
      simId,
      revision: "1",
    });
    const cases: Array<{ gateways: GatewayView[]; draft?: Draft; label: string }> = [
      { gateways: [{ ...SMS_ONLY, availability: "connected" }], label: "SIM 1 · Online" },
      { gateways: [{ ...SMS_ONLY, availability: "delayed", lastSeenAgeSeconds: 120 }], label: "SIM 1 · Seen 2m ago" },
      { gateways: [{ ...SMS_ONLY, availability: "delayed" }], label: "SIM 1 · Last seen" },
      { gateways: [{ ...SMS_ONLY, availability: "offline" }], label: "SIM 1 · Not responding" },
      { gateways: [SMS_ONLY, MMS_SIM], label: "No SIM chosen" },
      { gateways: [], label: "No phone" },
      { gateways: [{ ...SMS_ONLY, availability: "no_sms_sim" }], label: "No SMS SIM" },
      { gateways: [{ ...SMS_ONLY, availability: "unknown" }], label: "SIM 1 · Status unknown" },
      { gateways: [SMS_ONLY], draft: selectedDraft("sim-9"), label: "SIM unavailable" },
    ];

    for (const routeCase of cases) {
      cleanup();
      host.gateways = routeCase.gateways;
      host.drafts.clear();
      if (routeCase.draft) host.drafts.set("aurora", routeCase.draft);
      openComposerWindow("aurora", true);
      render(<App />);
      await waitFor(() => expect(document.getElementById("head-panel-route")).toHaveTextContent(routeCase.label.replace(" · ", "·")));
      expect(screen.queryByRole("combobox", { name: "Phone" })).not.toBeInTheDocument();
    }
  });
  it("closes a head panel only after its draft flushes", async () => {
    openComposerWindow("aurora", true);
    render(<App />);
    await screen.findByRole("banner", { name: "Floating conversation with Aurora" });
    type("text to save");
    await waitFor(() => expect(bridge.save_draft).toHaveBeenCalled());
    const saveDraftCalls = vi.mocked(bridge.save_draft).mock.calls.length;
    const saveOrder = vi.mocked(bridge.save_draft).mock.invocationCallOrder[saveDraftCalls - 1];
    fireEvent.click(screen.getByRole("button", { name: "Close bubble" }));
    await waitFor(() => expect(bridge.close_head_panel).toHaveBeenCalledOnce());
    const closeOrder = vi.mocked(bridge.close_head_panel).mock.invocationCallOrder[0];
    expect(saveOrder).toBeLessThan(closeOrder);
    expect(bridge.close_composer).not.toHaveBeenCalled();
  });

  it("marks the close button busy until the native close finishes", async () => {
    const pending = deferred<void>();
    vi.mocked(bridge.close_head_panel).mockReturnValueOnce(pending.promise);
    openComposerWindow("aurora", true);
    render(<App />);
    const close = await screen.findByRole("button", { name: "Close bubble" });
    fireEvent.click(close);
    expect(await screen.findByRole("button", { name: "Closing…" })).toHaveAttribute("aria-busy", "true");
    pending.resolve();
    await waitFor(() => expect(screen.getByRole("button", { name: "Close bubble" })).toBeEnabled());
  });

  it("keeps a head panel open when its draft flush fails", async () => {
    openComposerWindow("aurora", true);
    render(<App />);
    await screen.findByRole("banner", { name: "Floating conversation with Aurora" });
    host.failSaves = { code: "io", message: "Disk full." };
    type("do not lose me");
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "Close bubble" }));
    expect(await screen.findByText(/window stayed open/)).toBeInTheDocument();
    expect(bridge.close_head_panel).not.toHaveBeenCalled();
    expect(screen.getByRole("banner", { name: "Floating conversation with Aurora" })).toBeInTheDocument();
  });

  it("collapses a head panel without closing its bubble, including on Escape", async () => {
    openComposerWindow("aurora", true);
    render(<App />);
    await screen.findByRole("banner", { name: "Floating conversation with Aurora" });
    fireEvent.click(screen.getByRole("button", { name: "Collapse to bubble" }));
    await waitFor(() => expect(bridge.close_composer).toHaveBeenCalledOnce());
    expect(bridge.close_head_panel).not.toHaveBeenCalled();
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(bridge.close_composer).toHaveBeenCalledTimes(2));
  });

  it("collapses a head panel on Escape without a route picker", async () => {
    openComposerWindow("aurora", true);
    render(<App />);
    await waitFor(() => expect(document.getElementById("head-panel-route")).toHaveTextContent("No SIM chosen"));
    expect(screen.queryByRole("combobox", { name: "Phone" })).not.toBeInTheDocument();
    fireEvent.keyDown(message(), { key: "Escape" });
    await waitFor(() => expect(bridge.close_composer).toHaveBeenCalledOnce());
  });

  it("uses and persists the separate head composer height", async () => {
    localStorage.setItem("peppy.layout.v1", JSON.stringify({ composerHeight: 128, headComposerHeight: 160 }));
    openComposerWindow("aurora", true);
    render(<App />);
    const grip = await screen.findByRole("separator", { name: "Resize composer" });
    expect(grip).toHaveAttribute("aria-valuenow", "160");
    fireEvent.keyDown(grip, { key: "ArrowDown" });
    await waitFor(() => {
      const layout = JSON.parse(localStorage.getItem("peppy.layout.v1")!);
      expect(layout.composerHeight).toBe(128);
      expect(layout.headComposerHeight).not.toBe(160);
    });
  });

  it("re-applies a saved head composer height when a standalone composer becomes a panel", async () => {
    // JSDOM reports zero layout heights, which would clamp every value to the minimum. Give only the
    // composer chrome realistic heights (a 96px area with a one-line textarea and a 36px toolbar,
    // inside an 800px pane); every other element stays zero so chrome measures 74px, not a sibling sum.
    const layoutHeights: Record<string, number> = {
      "conversation-pane": 800,
      "composer-area": 96,
      "composer-textarea": 22,
      "composer-toolbar": 36,
    };
    vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(function (this: HTMLElement) {
      return layoutHeights[this.id] ?? 0;
    });
    localStorage.setItem("peppy.layout.v1", JSON.stringify({ composerHeight: 128, headComposerHeight: 112 }));
    openComposerWindow("aurora");
    render(<App />);
    expect((await screen.findByRole("separator", { name: "Resize composer" }))).toHaveAttribute("aria-valuenow", "128");
    host.head = { ...host.head, panel: true };
    await act(async () => hint?.());
    await waitFor(() => expect(document.getElementById("head-panel-header")).toBeInTheDocument());
    await waitFor(() => expect(screen.getByRole("separator", { name: "Resize composer" })).toHaveAttribute("aria-valuenow", "112"));
    expect(JSON.parse(localStorage.getItem("peppy.layout.v1")!).composerHeight).toBe(128);
  });
  it("loads the conversation named by the native URL and closes only after the draft is saved", async () => {
    openComposerWindow("aurora");
    render(<App />);
    await screen.findByText("Hello from Aurora");
    expect(bridge.load_state).toHaveBeenCalledWith("aurora");
    expect(screen.getByRole("banner")).toHaveAttribute(
      "data-tauri-drag-region",
    );
    expect(document.querySelector("#desktop-titlebar [data-connection-state=\"connected\"]")).toBeInTheDocument();
    expect(document.querySelector("#desktop-titlebar [data-disclosure]")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Server URL")).not.toBeInTheDocument();

    host.failSaves = { code: "io", message: "Disk full." };
    type("do not lose me");
    await screen.findByRole("alert");
    fireEvent.keyDown(window, { key: "Escape" });
    expect(await screen.findByText(/window stayed open/)).toBeInTheDocument();
    fireEvent.click(screen.getAllByRole("button", { name: "Close composer" })[0]);
    await waitFor(() =>
      expect(
        vi.mocked(bridge.save_draft).mock.calls.length,
      ).toBeGreaterThanOrEqual(3),
    );
    expect(bridge.close_composer).not.toHaveBeenCalled();
    expect(message()).toHaveValue("do not lose me");

    host.failSaves = undefined;
    fireEvent.keyDown(window, { key: "Escape", isComposing: true });
    await Promise.resolve();
    expect(bridge.close_composer).not.toHaveBeenCalled();
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(bridge.close_composer).toHaveBeenCalledTimes(1));
    expect(host.drafts.get("aurora")?.text).toBe("do not lose me");
  });

  it("offers a recipient field for a host-created draft-only conversation", async () => {
    host.drafts.set("conv-tray", {
      id: "draft-tray",
      conversationId: "conv-tray",
      text: "",
      recipientIds: [],
      attachmentIds: [],
      revision: "0",
    });
    openComposerWindow("conv-tray");
    render(<App />);
    const recipients = await screen.findByLabelText("Recipients");
    fireEvent.change(recipients, { target: { value: "+1 202 555 0100" } });
    fireEvent.keyDown(recipients, { key: "Enter" });
    await waitFor(() =>
      expect(host.drafts.get("conv-tray")).toMatchObject({
        id: "draft-tray",
        recipientIds: ["+12025550100"],
        revision: "1",
      }),
    );
  });

  it("uses floating heads for conversations and standalone composer for new messages", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(document.getElementById("header-popout-conversation")!);
    await waitFor(() => expect(bridge.popout_conversation).toHaveBeenCalledWith("aurora"));
    expect(bridge.open_composer).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "New message window" }));
    await waitFor(() => expect(bridge.open_composer).toHaveBeenLastCalledWith(undefined));
  });
});

describe("native lifecycle requests", () => {
  it("acks clean actions and disposes its listener", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    for (const action of ["quit", "close", "collapse"] as const)
      await act(async () => lifecycle?.({ id: `request-${action}`, action }));
    await waitFor(() => expect(bridge.acknowledge_lifecycle).toHaveBeenCalledTimes(3));
    cleanup();
    expect(lifecycleDispose).toHaveBeenCalledOnce();
  });

  it("freezes edits after acknowledgement until the targeted lifecycle finishes", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    await act(async () => lifecycle?.({ id: "request-switch", action: "close" }));
    await waitFor(() => expect(bridge.acknowledge_lifecycle).toHaveBeenCalledWith("request-switch", true));
    expect(document.getElementById("desktop-shell")).toHaveAttribute("inert");
    await act(async () => lifecycleFinished?.({ id: "other", ok: false }));
    expect(document.getElementById("desktop-shell")).toHaveAttribute("inert");
    await act(async () => lifecycleFinished?.({ id: "request-switch", ok: false }));
    expect(document.getElementById("desktop-shell")).not.toHaveAttribute("inert");
    cleanup();
    expect(lifecycleFinishedDispose).toHaveBeenCalledOnce();
  });

  it("attempts every dirty store and nacks when any save fails", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    host.failSaves = { code: "io", message: "Disk full." };
    type("unsaved aurora");
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: /River/ }));
    await screen.findByText("Hello from River");
    type("unsaved river");
    await waitFor(() => expect(message()).toHaveValue("unsaved river"));
    vi.mocked(bridge.save_draft).mockClear();
    await act(async () => lifecycle?.({ id: "request-quit", action: "quit" }));
    await waitFor(() => expect(bridge.acknowledge_lifecycle).toHaveBeenCalledWith("request-quit", false));
    expect(vi.mocked(bridge.save_draft).mock.calls.map(([input]) => input.conversationId)).toEqual(expect.arrayContaining(["aurora", "river"]));
  });
});

describe("gateway routes", () => {
  const storedDraft = (overrides: Partial<Draft> = {}): Draft => ({
    id: "draft-aurora",
    conversationId: "aurora",
    text: "MMS reply",
    recipientIds: [],
    attachmentIds: [],
    gatewayId: "gw-phone",
    simId: "sim-2",
    revision: "1",
    ...overrides,
  });

  it("requires a version-2-or-newer MMS route for text-only groups and keeps the draft", async () => {
    host.gateways = [{ ...MMS_SIM, mmsContentVersion: undefined }];
    host.drafts.set("aurora", storedDraft({ recipientIds: ["+15550001", "+15550002"] }));
    render(<App />);
    await waitFor(() => expect(message()).toHaveValue("MMS reply"));
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
    expect(screen.getByText(/needs MMS content version 2/)).toBeInTheDocument();
    expect(host.drafts.get("aurora")?.text).toBe("MMS reply");
  });

  it("accepts a newer MMS content version for a text-only group", async () => {
    host.gateways = [{ ...MMS_SIM, mmsContentVersion: 3 }];
    host.drafts.set("aurora", storedDraft({ recipientIds: ["+15550001", "+15550002"] }));
    render(<App />);
    await waitFor(() => expect(screen.getByRole("button", { name: "Send" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(bridge.send_draft).toHaveBeenCalled());
  });

  it("keeps text-only replies to an MMS conversation on an MMS route", async () => {
    host.gateways = [SMS_ONLY];
    host.conversations[0].messages[0].transport = "mms";
    host.drafts.set("aurora", storedDraft({ gatewayId: "gw-phone", simId: "sim-1" }));
    render(<App />);
    await waitFor(() => expect(screen.getByText(/does not support MMS attachments/)).toBeInTheDocument());
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
  });

  it("does not promote a current SMS reply because of an older picture message", async () => {
    host.gateways = [SMS_ONLY];
    const conversation = host.conversations[0];
    conversation.messages[0].transport = "mms";
    conversation.messages.push({
      ...conversation.messages[0], id: "latest-sms", revision: "2",
      transport: "sms", body: "Back to text", attachments: [],
    });
    host.drafts.set("aurora", storedDraft({ gatewayId: "gw-phone", simId: "sim-1" }));
    render(<App />);
    await waitFor(() => expect(message()).toHaveValue("MMS reply"));
    expect(screen.getByRole("button", { name: "Send" })).toBeEnabled();
  });

  it("sends group replies without an unavailable notice", async () => {
    host.gateways = [{ ...MMS_SIM }];
    host.conversations[0].participants = ["+15550001", "+15550002"];
    host.drafts.set("aurora", storedDraft());
    render(<App />);
    await waitFor(() => expect(message()).toHaveValue("MMS reply"));
    expect(screen.getByRole("button", { name: "Send" })).toBeEnabled();
    expect(screen.queryByText(/Replies unavailable/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(bridge.send_draft).toHaveBeenCalled());
  });

  it("removes a draft attachment through the CAS save path", async () => {
    host.drafts.set("aurora", storedDraft({ attachmentIds: ["file-1"] }));
    render(<App />);
    await screen.findByRole("button", { name: "Remove Attached file" });
    fireEvent.click(screen.getByRole("button", { name: "Remove Attached file" }));
    await waitFor(() => expect(host.drafts.get("aurora")?.attachmentIds).toEqual([]));
    expect(vi.mocked(bridge.save_draft).mock.calls.at(-1)?.[0]).toMatchObject({ attachmentIds: [] });
  });

  it("retries and saves the exact attachment ID without sending", async () => {
    render(<App />);
    await screen.findByRole("button", { name: "Retry download for photo.png" });
    fireEvent.click(screen.getByRole("button", { name: "Retry download for photo.png" }));
    fireEvent.click(screen.getByRole("button", { name: "Save photo.png" }));
    await waitFor(() => expect(bridge.retry_attachment).toHaveBeenCalledWith("photo-1"));
    expect(bridge.save_attachment).toHaveBeenCalledWith("photo-1");
    expect(bridge.send_draft).not.toHaveBeenCalled();
  });

  it("processes pending saves once per attachment while state hints overlap", async () => {
    const first = deferred<Awaited<ReturnType<typeof bridge.open_media>>>();
    const second = deferred<Awaited<ReturnType<typeof bridge.open_media>>>();
    const firstAttachment = host.conversations[0].messages[0].attachments[0];
    host.conversations[0].messages[0].attachments.push({ ...firstAttachment, id: "photo-2", name: "photo-two.png" });
    vi.mocked(bridge.save_attachment)
      .mockRejectedValueOnce({ code: "media-pending" })
      .mockRejectedValueOnce({ code: "media-pending" })
      .mockResolvedValue(true);
    vi.spyOn(bridge, "open_media")
      .mockImplementationOnce(() => first.promise)
      .mockImplementationOnce(() => second.promise);
    render(<App />);
    await screen.findByRole("button", { name: "Save photo.png" });

    fireEvent.click(screen.getByRole("button", { name: "Save photo.png" }));
    await screen.findByText(/Downloading photo.png/);
    await act(async () => hint?.());
    await waitFor(() => expect(bridge.open_media).toHaveBeenCalledWith("photo-1"));
    await act(async () => {
      hint?.();
      hint?.();
    });
    expect(bridge.open_media).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: "Save photo-two.png" }));
    await screen.findByText(/Downloading photo-two.png/);
    await act(async () => first.resolve({ state: "ready", url: "file:photo-1", mediaType: "image/png", byteSize: 1 }));
    await waitFor(() => expect(bridge.open_media).toHaveBeenCalledWith("photo-2"));
    await act(async () => second.resolve({ state: "ready", url: "file:photo-2", mediaType: "image/png", byteSize: 1 }));

    await waitFor(() => expect(bridge.save_attachment).toHaveBeenCalledWith("photo-2"));
    expect(bridge.open_media).toHaveBeenCalledTimes(2);
    expect(bridge.save_attachment).toHaveBeenCalledTimes(4);
  });

  it("stops pending attachment saves after unmount", async () => {
    const media = deferred<Awaited<ReturnType<typeof bridge.open_media>>>();
    vi.mocked(bridge.save_attachment).mockRejectedValueOnce({ code: "media-pending" });
    vi.spyOn(bridge, "open_media").mockImplementationOnce(() => media.promise);
    const rendered = render(<App />);
    await screen.findByRole("button", { name: "Save photo.png" });
    fireEvent.click(screen.getByRole("button", { name: "Save photo.png" }));
    await screen.findByText(/Downloading photo.png/);
    await act(async () => hint?.());
    await waitFor(() => expect(bridge.open_media).toHaveBeenCalledWith("photo-1"));
    rendered.unmount();
    await act(async () => media.resolve({ state: "ready", url: "file:photo-1", mediaType: "image/png", byteSize: 1 }));
    expect(bridge.save_attachment).toHaveBeenCalledOnce();
  });

  it("blocks an MMS lower-bound estimate that already exceeds the reported limit", async () => {
    host.gateways = [{ ...MMS_SIM, mmsMaxBytes: 1, mmsLimitSource: "carrier" }];
    host.drafts.set("aurora", storedDraft({ attachmentIds: ["file-1"] }));
    render(<App />);
    await screen.findByText(/estimate exceeds/);
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
  });

  it("distinguishes two SIMs on one gateway device and blocks MMS on the SMS-only SIM", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("combobox", { name: "Phone" }));
    const choices = await screen.findAllByRole("option");
    const values = choices.map((option) => option.getAttribute("data-gateway-choice"));
    expect(new Set(values.filter(Boolean)).size).toBe(2);
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
    fireEvent.keyDown(screen.getByRole("combobox", { name: "Phone" }), { key: "Escape" });

    fireEvent.click(screen.getByRole("button", { name: "Add attachment" }));
    await waitFor(() =>
      expect(host.drafts.get("aurora")?.attachmentIds).toEqual(["file-1"]),
    );
    await chooseGateway(SMS_ONLY);
    await waitFor(() =>
      expect(host.drafts.get("aurora")).toMatchObject({
        gatewayId: "gw-phone",
        simId: "sim-1",
      }),
    );
    expect(
      screen.getByText(/does not support MMS attachments/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();

    await chooseGateway(MMS_SIM);
    await waitFor(() =>
      expect(host.drafts.get("aurora")).toMatchObject({
        gatewayId: "gw-phone",
        simId: "sim-2",
      }),
    );
    expect(document.getElementById("unavailable-hint")).toHaveAttribute(
      "hidden",
    );
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() =>
      expect(bridge.send_draft).toHaveBeenCalledWith(
        expect.objectContaining({
          gatewayId: "gw-phone",
          simId: "sim-2",
          attachmentIds: ["file-1"],
        }),
      ),
    );
  });

  it("never reroutes a stored route that is no longer reported", async () => {
    host.gateways = [SMS_ONLY];
    host.drafts.set("aurora", {
      id: "draft-aurora",
      conversationId: "aurora",
      text: "hello",
      recipientIds: [],
      attachmentIds: [],
      gatewayId: "gw-phone",
      simId: "sim-9",
      revision: "2",
    });
    render(<App />);
    await waitFor(() => expect(message()).toHaveValue("hello"));
    const phone = screen.getByRole("combobox", { name: "Phone" });
    expect(phone.querySelector('[data-route-badge="device"]')).toHaveTextContent("Unavailable route");
    expect(phone.querySelector('[data-route-badge="sim"]')).toHaveTextContent("SIM sim-9");
    expect(
      screen.getByText(/no longer reported/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
    fireEvent.keyDown(message(), { key: "Enter" });
    await Promise.resolve();
    expect(bridge.send_draft).not.toHaveBeenCalled();
    expect(bridge.save_draft).not.toHaveBeenCalled();
  });

  it("blocks an offline route without discarding its draft", async () => {
    host.gateways = [{ ...SMS_ONLY, availability: "offline" }];
    host.drafts.set("aurora", storedDraft({ simId: "sim-1", text: "Keep this draft" }));
    render(<App />);

    await screen.findByDisplayValue("Keep this draft");
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
    expect(document.getElementById("gateway-presence-hint")).toHaveTextContent("isn't responding");
    expect(host.drafts.get("aurora")?.text).toBe("Keep this draft");
  });

  it("allows delayed and legacy SMS routes without verbose freshness warnings", async () => {
    host.gateways = [{ ...SMS_ONLY, availability: "delayed" }];
    host.drafts.set("aurora", storedDraft({ simId: "sim-1" }));
    render(<App />);
    expect(await screen.findByRole("button", { name: "Send" })).toBeEnabled();
    expect(document.getElementById("gateway-selector")).toHaveAttribute("data-route-state", "delayed");
    expect(document.getElementById("gateway-selector")).not.toHaveAttribute("aria-describedby");

    cleanup();
    host.gateways = [{ ...SMS_ONLY, availability: "unknown" }];
    render(<App />);
    expect(await screen.findByText(/Status unknown/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Send" })).toBeEnabled();
  });

  it("keeps an unsupported iPhone route visible but disabled", async () => {
    host.gateways = [{ ...SMS_ONLY, name: "My iPhone", availability: "unsupported_platform" }];
    render(<App />);
    fireEvent.click(await screen.findByRole("combobox", { name: "Phone" }));
    const option = await screen.findByRole("option", { name: /My iPhone/, hidden: true });
    expect(option).toHaveAttribute("aria-disabled", "true");
    expect(option).toHaveAttribute("data-gateway-availability", "unsupported_platform");
    await act(async () => { fireEvent.click(option); });
    expect(bridge.save_draft).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
  });

  it("does not save or revise a draft when the selected route is chosen again", async () => {
    render(<App />);
    await screen.findByRole("combobox", { name: "Phone" });

    await chooseGateway(SMS_ONLY);
    await waitFor(() => expect(host.drafts.get("aurora")).toMatchObject({ simId: "sim-1" }));
    const revision = host.drafts.get("aurora")!.revision;
    const saves = vi.mocked(bridge.save_draft).mock.calls.length;

    await chooseGateway(SMS_ONLY);
    await act(async () => {});

    expect(host.drafts.get("aurora")?.revision).toBe(revision);
    expect(bridge.save_draft).toHaveBeenCalledTimes(saves);
  });

  it("does not create an empty draft when an implicit route is reselected", async () => {
    host.gateways = [SMS_ONLY];
    render(<App />);
    await screen.findByRole("combobox", { name: "Phone" });

    await chooseGateway(SMS_ONLY);
    await act(async () => {});

    expect(bridge.save_draft).not.toHaveBeenCalled();
    expect(host.drafts.get("aurora")).toBeUndefined();
  });

});

describe("host state display", () => {
  it("hides OS window controls immediately for the browser host while native retains them", () => {
    const pending = deferred<DesktopSnapshot>();
    vi.mocked(bridge.load_state).mockReturnValue(pending.promise);
    const { rerender } = render(<App hostKind="browser" fixedOrigin="https://community.example" />);
    expect(document.getElementById("window-controls")).not.toBeInTheDocument();
    rerender(<App />);
    expect(document.getElementById("window-controls")).toBeInTheDocument();
  });

  it("locks browser snapshots only after saving drafts through a receiver-bound host capability", async () => {
    const lock = vi.fn().mockResolvedValue(undefined);
    bridge.lock_sync = function() {
      expect(this).toBe(bridge);
      return lock();
    };
    const snapshot = host.load();
    vi.mocked(bridge.load_state).mockResolvedValue({ ...snapshot, mode: "browser", desktop: undefined });
    render(<App hostKind="browser" fixedOrigin="https://community.example" />);
    await screen.findByText("Hello from Aurora");
    fireEvent.change(message(), { target: { value: "save before locking" } });
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const lockButton = screen.getByRole("button", { name: "Lock now" });
    fireEvent.click(lockButton);
    await waitFor(() => expect(lock).toHaveBeenCalledOnce());
    expect(bridge.save_draft).toHaveBeenCalled();
    expect(vi.mocked(bridge.save_draft).mock.invocationCallOrder[0]).toBeLessThan(lock.mock.invocationCallOrder[0]);
    delete bridge.lock_sync;
  });

  it("shows the remembered unlock warning and refreshes after browser lock cleanup fails", async () => {
    const lock = vi.fn().mockRejectedValue({ code: "recovery-clear-failed", message: "Browser recovery could not be forgotten." });
    bridge.lock_sync = function() { return lock(); };
    const snapshot = host.load();
    vi.mocked(bridge.load_state).mockResolvedValue({ ...snapshot, mode: "browser", desktop: undefined });
    const loadState = vi.mocked(bridge.load_state);
    render(<App hostKind="browser" fixedOrigin="https://community.example" />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.click(screen.getByRole("button", { name: "Lock now" }));

    expect(await screen.findByText("Sync is locked, but this browser may still remember your Vault key phrase.")).toBeInTheDocument();
    await waitFor(() => expect(loadState).toHaveBeenCalledTimes(2));
    delete bridge.lock_sync;
  });

  it("omits native update settings from the browser host", async () => {
    render(<App hostKind="browser" fixedOrigin="https://community.example" />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(document.querySelector('[data-settings-section="updates"]')).not.toBeInTheDocument();
  });

  it("loads browser attachment storage only when the account settings view opens", async () => {
    const retentionBridge = bridge as typeof bridge & Required<Pick<typeof bridge, "attachment_retention_status">>;
    const storageStatus = vi.spyOn(retentionBridge, "attachment_retention_status").mockResolvedValue({
      kind: "available",
      scopeId: "browser-scope",
      status: {
        usedBytes: 20_000_000_000,
        limitBytes: 75_000_000_000,
        baseBytes: 50_000_000_000,
        packBytes: 25_000_000_000,
        protectedBytes: 0,
        removedBytes: 0,
        cleanupState: "disabled",
        cleanupUnavailableReason: "cleanup_disabled",
        notice: { state: "grace_period", policyVersion: "hosted_attachment_retention_v1", presentedAtSeconds: 1, acknowledgedAtSeconds: 2, activationAtSeconds: 1_800_000_000 },
      },
    });
    render(<App hostKind="browser" fixedOrigin="https://app.example.com" accountUrl="https://account.example.com/account" />);
    await screen.findByText("Hello from Aurora");
    expect(storageStatus).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Settings" }));

    expect(await screen.findByText("Storage used")).toBeInTheDocument();
    expect(storageStatus).toHaveBeenCalledOnce();
    expect(screen.queryByRole("button", { name: "Sign out of Peppy" })).not.toBeInTheDocument();
  });

  it("preserves browser account links and native hosted account actions in settings order", async () => {
    const configure = vi.spyOn(bridge, "configure_server");
    render(<App hostKind="browser" fixedOrigin="https://app.example.com" accountUrl="https://account.example.com/account" />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));

    const section = document.querySelector('[data-settings-section="account-billing"]');
    const link = screen.getByRole("link", { name: /manage storage on the website/i });
    expect(section).toBeInTheDocument();
    expect(link).toHaveAttribute("href", "https://account.example.com/account");
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute("rel", "noopener noreferrer");
    expect(screen.queryByLabelText("Server URL")).not.toBeInTheDocument();
    expect(configure).not.toHaveBeenCalled();
    expect(localStorage.getItem("peppy.setup.mode")).toBeNull();
    expect(document.querySelector('[data-settings-section="sync"]')?.compareDocumentPosition(section!)).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
    cleanup();
    render(<App accountUrl="https://account.example.com/account" />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(screen.getByRole("button", { name: /manage storage on the website/i })).toBeInTheDocument();
  });

  it("uses the selected native mode to expose only its connection controls", async () => {
    const openBilling = vi.spyOn(bridge, "hosted_open_billing").mockResolvedValue();
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(document.getElementById("settings-connection-summary")).toHaveTextContent("Peppy Hosted · https://example.test");

    expect(screen.queryByLabelText("Server URL")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /manage storage on the website/i }));
    await waitFor(() => expect(openBilling).toHaveBeenCalledOnce());

    cleanup();
    localStorage.setItem("peppy.setup.mode", "self-hosted");
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(screen.queryByRole("button", { name: /open account & billing/i })).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("Change server"));
    expect(screen.getByLabelText("Server URL")).toBeInTheDocument();
  });



  it("keeps rejected unlock feedback in the sync section", async () => {
    host.encryption = { state: "locked" };
    const unlock = vi.spyOn(bridge, "unlock_sync").mockRejectedValue({ message: "Unlock rejected." });
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.click(screen.getByRole("button", { name: "Unlock sync natively" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Unlock rejected.");
    expect(alert.closest('[data-settings-section="sync"]')).toBeInTheDocument();
    expect(unlock).toHaveBeenCalledOnce();
  });

  it("offers native sync unlock to an unlocked native session and runs it only on click", async () => {
    host.encryption = { state: "unlocked" };
    const unlock = vi.spyOn(bridge, "unlock_sync").mockResolvedValue(undefined);
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const unlockButton = screen.getByRole("button", { name: "Unlock sync natively" });
    expect(unlock).not.toHaveBeenCalled();
    fireEvent.click(unlockButton);
    await waitFor(() => expect(unlock).toHaveBeenCalledOnce());
  });

  it.each(["credentials-required", "revoked"])("hides native sync unlock when the connection reports %s", async (errorCode) => {
    host.encryption = { state: "unlocked" };
    host.connection = { state: "connected", origin: "https://example.test", errorCode };
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(document.querySelector('[data-settings-section="sync"]')).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Unlock sync natively" })).not.toBeInTheDocument();
  });

  it("offers no sync unlock button to an unlocked browser session", async () => {
    const snapshot = host.load();
    vi.mocked(bridge.load_state).mockResolvedValue({ ...snapshot, mode: "browser", desktop: undefined, encryption: { state: "unlocked" } });
    render(<App hostKind="browser" fixedOrigin="https://community.example" />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(document.querySelector('[data-settings-section="sync"]')).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^unlock sync/i })).not.toBeInTheDocument();
  });























  it("supplies a neutral loading status before the native snapshot arrives", () => {
    const pending = deferred<DesktopSnapshot>();
    vi.mocked(bridge.load_state).mockReturnValue(pending.promise);
    render(<App />);
    const trigger = document.querySelector("[data-status-trigger]") as HTMLButtonElement;
    expect(trigger).toHaveAttribute("data-status-tone", "neutral");
    expect(trigger).toHaveAccessibleName(/loading status/i);
    expect(document.getElementById("desktop-shell")).toHaveAttribute("aria-busy", "true");
    expect(screen.getByText("Loading messaging state…")).toBeVisible();
    expect(document.getElementById("desktop-loading-state")).toBeVisible();
    expect(screen.queryByLabelText("Message")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(screen.queryByLabelText("Server URL")).not.toBeInTheDocument();
    fireEvent.click(trigger);
    expect(document.querySelector("[data-status-panel]")).toBeVisible();
    expect(document.querySelector("[data-status-panel]")).toHaveTextContent("Loading status…");
  });

  it("formats numeric conversation labels while preserving existing names", async () => {
    host.conversations[1].name = "+12025550100";
    render(<App />);
    expect(await screen.findByRole("button", { name: /Aurora/ })).toBeInTheDocument();
    const numericConversation = screen.getByRole("button", {
      name: /\(202\) 555-0100/,
    });
    fireEvent.click(numericConversation);
    await waitFor(() =>
      expect(document.querySelector("[data-header-title]")).toHaveTextContent(
        "(202) 555-0100",
      ),
    );
    expect(screen.getByRole("button", { name: /Aurora/ })).toBeInTheDocument();
  });







  it("replaces both panes with notifications and preserves the draft when returning", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), { target: { value: "Keep this draft" } });
    fireEvent.click(screen.getByRole("button", { name: /^Notifications/ }));
    expect(screen.getByRole("region", { name: "Notifications" })).toBeInTheDocument();
    expect(document.getElementById("thread-list")).not.toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "Open settings" }));
    expect(document.getElementById("notification-settings")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
    expect(document.getElementById("thread-list")).toBeVisible();
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue("Keep this draft");
  });

  it("locks vault-synced banner controls in settings while encryption is locked", async () => {
    host.encryption = { state: "locked" } as DesktopSnapshot["encryption"];
    render(<App />);
    await waitFor(() => expect(document.getElementById("desktop-shell")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));

    expect(screen.getByRole("region", { name: "Settings" })).toBeInTheDocument();
    expect(document.getElementById("notification-banner-locked-note")).toHaveTextContent("Unlock Peppy to change banner settings.");
    expect(screen.getByRole("switch", { name: "SMS/MMS banners" })).toBeDisabled();
    expect(screen.getByRole("switch", { name: "App notification banners" })).toBeDisabled();
    expect(screen.getByLabelText("Banner preview")).toBeDisabled();
  });

  it("switches to settings from the rail and applies the selected theme class", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(
      screen.getByRole("region", { name: "Settings" }),
    ).toBeInTheDocument();
    const trigger = document.querySelector("[data-status-trigger]") as HTMLButtonElement;
    fireEvent.click(trigger);
    const statusDetails = screen.getByRole("region", { name: "Status details" });
    expect(statusDetails).toBeVisible();
    expect(within(statusDetails).getByText("Carrier SMS/MMS not end-to-end encrypted")).toBeVisible();
    expect(within(statusDetails).getByText("Carrier SMS/MMS not end-to-end encrypted").closest("[data-disclosure]")).toHaveAttribute("title", "Carrier SMS/MMS not end-to-end encrypted");
    expect(within(statusDetails).getByText("Device sync encrypted").closest("[data-disclosure]")).toHaveAttribute("title", "Device sync encrypted");
    expect(statusDetails.querySelector("[data-status-details]")).toBeInTheDocument();
    expect(within(statusDetails).getByText(/Connected/).closest("#connection-status")).toHaveAttribute(
      "data-connection-state",
      "connected",
    );
    expect(statusDetails.querySelector("#connection-status")).toHaveAttribute("title", "Connected");
    expect(document.getElementById("thread-list")).not.toBeVisible();
    expect(
      screen.queryByRole("separator", { name: "Resize thread list" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "New message window" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("heading", { level: 1, name: "Settings" }),
    ).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Theme"), {
      target: { value: "dark" },
    });
    expect(document.getElementById("desktop-shell")).toHaveClass("theme-dark");
    fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
    expect(document.getElementById("thread-list")).toBeVisible();
    expect(
      screen.queryByRole("region", { name: "Settings" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText("Hello from Aurora")).toBeInTheDocument();
  });

  it("uses only rendered settings categories and updates the active category from navigation", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));

    const navigation = screen.getByRole("navigation", { name: "Settings categories" });
    expect(within(navigation).getByRole("link", { name: "Server" })).toHaveAttribute("aria-current", "location");
    expect(within(navigation).queryByRole("link", { name: "Advanced" })).not.toBeInTheDocument();
    const account = within(navigation).getByRole("link", { name: "Account" });
    fireEvent.click(account);

    expect(account).toHaveAttribute("aria-current", "location");
    expect(document.getElementById("settings-account-billing")).toHaveAttribute("data-settings-section", "account-billing");
    expect(within(navigation).getAllByRole("link").at(-1)).toHaveTextContent("Privacy");
    expect(document.getElementById("settings-privacy")).toHaveAttribute("data-settings-section", "privacy");
  });

  it.each([false, true])("scrolls only the settings content when selecting Account (reduced motion: %s)", async (reducedMotion) => {
    vi.stubGlobal("matchMedia", vi.fn((query: string) => ({
      matches: reducedMotion && query === "(prefers-reduced-motion: reduce)",
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })));
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));

    const content = document.getElementById("settings-content")!;
    const account = document.getElementById("settings-account-billing")!;
    content.scrollTop = 80;
    Object.defineProperty(content, "clientTop", { value: 2 });
    vi.spyOn(content, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 120, 500, 400));
    vi.spyOn(account, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 820, 500, 160));
    account.style.scrollMarginTop = "8px";
    const scrollContent = vi.fn();
    const scrollAncestors = vi.fn();
    const scrollWindow = vi.spyOn(window, "scrollTo");
    content.scrollTo = scrollContent;
    account.scrollIntoView = scrollAncestors;

    fireEvent.click(screen.getByRole("link", { name: "Account" }));

    expect(scrollContent).toHaveBeenCalledWith({ top: 770, behavior: reducedMotion ? "instant" : "smooth" });
    expect(scrollAncestors).not.toHaveBeenCalled();
    expect(scrollWindow).not.toHaveBeenCalled();
  });

  it.each([false, true])("opens notification settings in its content and consumes each navigation request (reduced motion: %s)", async (reducedMotion) => {
    vi.stubGlobal("matchMedia", vi.fn((query: string) => ({
      matches: reducedMotion && query === "(prefers-reduced-motion: reduce)",
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })));
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: /^Notifications/ }));

    const originalScrollTo = HTMLElement.prototype.scrollTo;
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    const originalClientTop = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientTop");
    const originalBounds = HTMLElement.prototype.getBoundingClientRect;
    const scrollContent = vi.fn();
    const scrollAncestors = vi.fn();
    const scrollWindow = vi.spyOn(window, "scrollTo");
    const margin = document.createElement("style");
    margin.textContent = "#notification-settings { scroll-margin-top: 8px; }";
    document.head.append(margin);
    Object.defineProperty(HTMLElement.prototype, "scrollTo", { configurable: true, value: scrollContent });
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: scrollAncestors });
    Object.defineProperty(HTMLElement.prototype, "clientTop", {
      configurable: true,
      get() { return this.id === "settings-content" ? 2 : originalClientTop?.get?.call(this) ?? 0; },
    });
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      if (this.id === "settings-content") return new DOMRect(0, 120, 500, 400);
      if (this.id === "notification-settings") return new DOMRect(0, 820, 500, 160);
      return originalBounds.call(this);
    });

    try {
      fireEvent.click(screen.getByRole("button", { name: "Open settings" }));
      await waitFor(() => expect(scrollContent).toHaveBeenCalledTimes(1));
      expect(scrollContent).toHaveBeenLastCalledWith({ top: 690, behavior: reducedMotion ? "instant" : "smooth" });
      expect(scrollContent.mock.contexts[0]).toBe(document.getElementById("settings-content"));
      expect(scrollAncestors).not.toHaveBeenCalled();
      expect(scrollWindow).not.toHaveBeenCalled();
      expect(screen.getByRole("link", { name: "Notifications" })).toHaveAttribute("aria-current", "location");

      fireEvent.click(screen.getByRole("button", { name: /^Notifications/ }));
      fireEvent.click(screen.getByRole("button", { name: "Open settings" }));
      await waitFor(() => expect(scrollContent).toHaveBeenCalledTimes(2));

      fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
      fireEvent.click(screen.getByRole("button", { name: "Settings" }));
      expect(scrollContent).toHaveBeenCalledTimes(2);
      expect(screen.getByRole("link", { name: "Server" })).toHaveAttribute("aria-current", "location");

      fireEvent.change(screen.getByLabelText("Settings category"), { target: { value: "notifications" } });
      expect(scrollContent).toHaveBeenCalledTimes(3);
      expect(scrollContent.mock.contexts[2]).toBe(document.getElementById("settings-content"));
      expect(screen.getByRole("link", { name: "Notifications" })).toHaveAttribute("aria-current", "location");
      expect(scrollAncestors).not.toHaveBeenCalled();
      expect(scrollWindow).not.toHaveBeenCalled();
    } finally {
      margin.remove();
      Object.defineProperty(HTMLElement.prototype, "scrollTo", { configurable: true, value: originalScrollTo });
      Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: originalScrollIntoView });
      if (originalClientTop) Object.defineProperty(HTMLElement.prototype, "clientTop", originalClientTop);
      else delete (HTMLElement.prototype as { clientTop?: number }).clientTop;
    }
  });

  it("keeps a titlebar compose fallback when the thread list is hidden by another view", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));

    fireEvent.click(screen.getByRole("button", { name: "New conversation" }));

    await waitFor(() => expect(document.getElementById("thread-list")).toBeVisible());
    expect(document.getElementById("draft-recipients")).toBeInTheDocument();
  });

  it("keeps a titlebar compose fallback when the conversation list is collapsed", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Conversations" }));

    expect(document.getElementById("thread-list")).not.toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "New conversation" }));

    await waitFor(() => expect(document.getElementById("thread-list")).toBeVisible());
    expect(document.getElementById("draft-recipients")).toBeInTheDocument();
  });

  it("resets the active settings category when a conditional section is removed", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.click(screen.getByRole("link", { name: "Startup" }));
    expect(screen.getByRole("link", { name: "Startup" })).toHaveAttribute("aria-current", "location");

    host.desktop = { ...host.desktop, startupSupported: false };
    await act(async () => hint?.());

    await waitFor(() => expect(screen.queryByRole("link", { name: "Startup" })).not.toBeInTheDocument());
    expect(screen.getByRole("link", { name: "Server" })).toHaveAttribute("aria-current", "location");
  });

  it("rolls back and re-enables start at login after a failed change", async () => {
    const pending = deferred<void>();
    vi.mocked(bridge.set_start_at_login).mockImplementation(() => pending.promise);
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const startup = screen.getByRole("checkbox", { name: "Start at login" });
    fireEvent.click(startup);
    expect(startup).toBeDisabled();
    expect(startup).not.toBeChecked();
    pending.reject({ message: "Registration denied." });
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    await waitFor(() => expect(startup).toBeEnabled());
    expect(startup).not.toBeChecked();
  });

  it("shows the public URL returned by publish_attachment and reports cancellation", async () => {
    vi.mocked(bridge.publish_attachment)
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce({
        url: "https://example.test/file/mms-usercontent/token/photo.png",
        expiresInSeconds: 7200,
      });
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(
      screen.getByRole("button", { name: "Create public link for photo.png" }),
    );
    expect(
      await screen.findByText(/Public link not created/),
    ).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", { name: "Create public link for photo.png" }),
    );
    expect(
      await screen.findByLabelText("Public link for photo.png"),
    ).toHaveValue("https://example.test/file/mms-usercontent/token/photo.png");
    expect(screen.getByText("Expires in 2 h.")).toBeInTheDocument();
    expect(bridge.publish_attachment).toHaveBeenCalledWith("photo-1");
  });

  it("removes remote-only file-card actions and transfer state after storage policy removal", async () => {
    host.conversations[0].messages[0].attachments = [{
      id: "removed-file",
      name: "archive.pdf",
      mediaType: "application/pdf",
      byteSize: 10,
      state: "failed",
      transfer: "download",
      retryable: true,
      remoteState: "removed_by_storage_policy",
      localAvailable: false,
    }];
    render(<App />);

    await screen.findByText("archive.pdf");
    const card = document.querySelector<HTMLElement>('[data-attachment-id="removed-file"]')!;
    expect(card).toHaveTextContent("This attachment was removed from hosted storage to make room. The message remains.");
    expect(card).not.toHaveTextContent("Failed");
    expect(within(card).queryByRole("button", { name: /save|public link|retry/i })).not.toBeInTheDocument();
  });

  it("keeps a verified local policy-removed file-card save action", async () => {
    host.conversations[0].messages[0].attachments = [{
      id: "local-file",
      name: "archive.pdf",
      mediaType: "application/pdf",
      byteSize: 10,
      state: "ready",
      remoteState: "removed_by_storage_policy",
      localAvailable: true,
    }];
    render(<App />);

    await screen.findByText("archive.pdf");
    const card = document.querySelector<HTMLElement>('[data-attachment-id="local-file"]')!;
    expect(card).toHaveTextContent("Hosted copy removed. Available on this device.");
    expect(within(card).getByRole("button", { name: "Save archive.pdf" })).toBeInTheDocument();
  });

  it("shows the native connection error code", async () => {
    host.connection = {
      state: "connected",
      origin: "https://example.test",
      errorCode: "outbox-rejected",
    };
    render(<App />);
    const trigger = await screen.findByRole("button", { name: /status:/i });
    fireEvent.click(trigger);
    const statusDetails = screen.getByRole("region", { name: "Status details" });
    expect(within(statusDetails).getByText(/the server rejected queued messages/)).toBeVisible();
    expect(statusDetails.querySelector("#connection-status")).toHaveAttribute("data-error-code", "outbox-rejected");
    host.connection = {
      state: "offline",
      origin: "https://example.test",
      errorCode: "live-x",
    };
    await act(async () => hint?.());
    await waitFor(() => expect(within(statusDetails).getByText("Offline — live-x")).toBeVisible());
    expect(statusDetails.querySelector("#connection-status")).toHaveAttribute("data-error-code", "live-x");
  });

  it("binds status details to the rail popover instead of the main titlebar", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    const titlebar = document.getElementById("desktop-titlebar")!;
    expect(titlebar.querySelector("#connection-status")).not.toBeInTheDocument();
    const trigger = document.querySelector("[data-status-trigger]") as HTMLButtonElement;
    const panel = document.querySelector("[data-status-panel]") as HTMLElement;
    expect(panel).toHaveAttribute("hidden");
    fireEvent.click(trigger);
    expect(panel).not.toHaveAttribute("hidden");
    expect(panel.querySelector("[data-disclosure=\"sync-state\"]")).toHaveAttribute("data-sync-state", "unlocked");
    expect(panel.querySelector("[data-disclosure=\"carrier-sms\"]")).toBeInTheDocument();
    expect(panel.querySelector("#connection-status")).toHaveAttribute("data-connection-state", "connected");
  });

  it("does not repeat a connection state used as its error code", async () => {
    host.connection = {
      state: "offline",
      origin: "https://example.test",
      errorCode: "offline",
    };
    render(<App />);
    expect(await screen.findByText("Offline")).toBeInTheDocument();
    expect(screen.queryByText("Offline — offline")).not.toBeInTheDocument();
  });

  it("retries a rejected visible-row mark after native state refresh without a refresh alert", async () => {
    const mark = vi.mocked(bridge.mark_seen);
    mark.mockRejectedValueOnce({ message: "Window is hidden." });
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    const observed: ((
      entries: { target: Element; isIntersecting: boolean; intersectionRect?: Pick<DOMRectReadOnly, "width" | "height"> }[],
    ) => void)[] = [];
    const targets: Element[] = [];
    class Observer {
      constructor(
        private callback: (
          entries: { target: Element; isIntersecting: boolean; intersectionRect?: Pick<DOMRectReadOnly, "width" | "height"> }[],
        ) => void,
      ) {
        observed.push(callback);
      }
      observe(target: Element) {
        targets.push(target);
        this.callback([{ target, isIntersecting: false }]);
      }
      disconnect() {}
      unobserve() {}
      takeRecords() {
        return [];
      }
      root = null;
      rootMargin = "";
      thresholds = [];
    }
    vi.stubGlobal("IntersectionObserver", Observer);
    render(<App />);
    await screen.findByText("Hello from Aurora");
    expect(mark).not.toHaveBeenCalled();
    const row = targets.find(
      (target) => target.getAttribute("data-message-id") === "m-aurora",
    )!;
    act(() => observed.at(-1)!([{ target: row, isIntersecting: true, intersectionRect: { width: 240, height: 48 } }]));
    await waitFor(() => expect(mark).toHaveBeenCalledTimes(1));
    expect(document.querySelector('[data-app-notice-source="refresh"]')).not.toBeInTheDocument();
    await act(async () => hint?.());
    await waitFor(() => expect(mark).toHaveBeenCalledTimes(2));
    expect(mark).toHaveBeenLastCalledWith(["m-aurora"]);
  });
});

describe("desktop presentation controls", () => {
  const stubNarrowBrowserMedia = () => {
    let matches = true;
    const listeners = new Set<(event: MediaQueryListEvent) => void>();
    vi.stubGlobal("matchMedia", vi.fn(() => ({
      get matches() { return matches; },
      media: "(max-width: 640px)",
      addEventListener: (_type: string, listener: (event: MediaQueryListEvent) => void) => listeners.add(listener),
      removeEventListener: (_type: string, listener: (event: MediaQueryListEvent) => void) => listeners.delete(listener),
      addListener: (listener: (event: MediaQueryListEvent) => void) => listeners.add(listener),
      removeListener: (listener: (event: MediaQueryListEvent) => void) => listeners.delete(listener),
      dispatchEvent: () => true,
    })));
    return (next: boolean) => {
      matches = next;
      listeners.forEach((listener) => listener({ matches, media: "(max-width: 640px)" } as MediaQueryListEvent));
    };
  };

  it("uses one browser pane on a narrow viewport without changing the saved desktop layout", async () => {
    localStorage.setItem("peppy.layout.v1", JSON.stringify({ listWidth: 342, listCollapsed: false }));
    const setNarrow = stubNarrowBrowserMedia();
    render(<App hostKind="browser" fixedOrigin="https://example.test" />);

    await screen.findByText("Hello from Aurora");
    await waitFor(() => expect(document.getElementById("thread-list")).not.toBeVisible());
    expect(document.getElementById("desktop-shell")).toHaveAttribute("data-narrow-browser", "true");
    expect(document.getElementById("handle-h1")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Toggle conversation list" }));
    expect(document.getElementById("thread-list")).toBeVisible();
    expect(document.getElementById("conversation-pane")).toHaveAttribute("data-mobile-list", "true");
    fireEvent.click(screen.getByRole("button", { name: /River/ }));
    await waitFor(() => expect(document.getElementById("thread-list")).not.toBeVisible());
    expect(document.getElementById("conversation-pane")).not.toHaveAttribute("data-mobile-list");

    setNarrow(false);
    await waitFor(() => expect(document.getElementById("thread-list")).toBeVisible());
    expect(document.getElementById("thread-list")).toHaveStyle({ width: "342px" });
    expect(document.getElementById("handle-h1")).toBeInTheDocument();
    expect(localStorage.getItem("peppy.layout.v1")).toBe(JSON.stringify({ listWidth: 342, listCollapsed: false }));
  });

  it("leaves a narrow native desktop layout unchanged", async () => {
    stubNarrowBrowserMedia();
    render(<App />);

    await screen.findByText("Hello from Aurora");
    expect(document.getElementById("desktop-shell")).not.toHaveAttribute("data-narrow-browser");
    expect(document.getElementById("thread-list")).toBeVisible();
    expect(document.getElementById("handle-h1")).toBeInTheDocument();
  });

  it("does not treat a released geometry correction as an upward history scroll", async () => {
    const geometry = { overlay: 100, viewport: 300 };
    const restoreGeometry = mockMessageListGeometry(geometry);
    host.conversations[0].hasOlderMessages = true;
    const loadHistory = vi.spyOn(bridge, "load_history").mockResolvedValue({ conversationId: "aurora", messages: [], hasMore: false });
    TestResizeObserver.reset();
    vi.stubGlobal("ResizeObserver", TestResizeObserver);
    try {
      render(<App />);
      const list = await screen.findByRole("log", { name: "Messages" });
      await screen.findByRole("button", { name: "Load older messages" });
      await act(async () => TestResizeObserver.resize(document.getElementById("composer-area")!));
      list.scrollTop = 400;
      fireEvent.scroll(list);
      geometry.overlay = 150;
      await act(async () => TestResizeObserver.resize(document.getElementById("composer-area")!));
      await waitFor(() => expect(list).not.toHaveAttribute("data-scroll-programmatic"));
      fireEvent.scroll(list);

      expect(loadHistory).not.toHaveBeenCalled();
    } finally {
      restoreGeometry();
    }
  });

  describe("navigation position", () => {
    it("defaults to the side rail without writing layout storage", async () => {
      const storedLayout = localStorage.getItem("peppy.layout.v1");
      render(<App />);

      await screen.findByText("Hello from Aurora");
      expect(document.getElementById("desktop-rail")).toBeInTheDocument();
      expect(document.getElementById("desktop-shell")).toHaveAttribute(
        "data-navigation",
        "side-rail",
      );
      expect(document.getElementById("titlebar-navigation")).not.toBeInTheDocument();
      expect(localStorage.getItem("peppy.layout.v1")).toBe(storedLayout);
    });

    it("restores title-bar navigation from layout storage", async () => {
      localStorage.setItem(
        "peppy.layout.v1",
        JSON.stringify({ navigationPosition: "title-bar" }),
      );
      render(<App />);

      await screen.findByText("Hello from Aurora");
      const titlebar = document.getElementById("desktop-titlebar")!;
      const navigation = document.getElementById("titlebar-navigation")!;
      expect(document.getElementById("desktop-rail")).not.toBeInTheDocument();
      expect(document.getElementById("desktop-shell")).toHaveAttribute(
        "data-navigation",
        "title-bar",
      );
      expect(titlebar).toContainElement(navigation);
      expect(navigation).toHaveAttribute("aria-label", "Main navigation");
      expect(document.getElementById("thread-list")).toBeVisible();
      expect(navigation.querySelectorAll("[data-rail-item]")).toHaveLength(4);
    });

    it.each(["side-rail", "title-bar"])("keeps compose in the main navbar with %s navigation", async navigationPosition => {
      localStorage.setItem("peppy.layout.v1", JSON.stringify({ navigationPosition }));
      render(<App />);
      await screen.findByText("Hello from Aurora");
      const action = within(document.getElementById("desktop-titlebar")!).getByRole("button", { name: "New conversation" });
      expect(document.querySelector("#thread-list-toolbar")).not.toBeInTheDocument();
      fireEvent.click(action);
      expect(document.getElementById("draft-recipients")).toBeInTheDocument();
    });

    it.each(["Settings", "Notifications", "Contacts"])("navbar compose returns from %s to a new conversation", async view => {
      render(<App />);
      await screen.findByText("Hello from Aurora");
      fireEvent.click(screen.getByRole("button", { name: new RegExp(`^${view}`) }));
      fireEvent.click(within(document.getElementById("desktop-titlebar")!).getByRole("button", { name: "New conversation" }));
      await waitFor(() => expect(document.getElementById("conversation-pane")).toHaveAttribute("data-view", "conversations"));
      expect(document.getElementById("draft-recipients")).toBeInTheDocument();
    });

    it.each([
      ["an invalid navigation value", JSON.stringify({ navigationPosition: "sidebar" })],
      ["a numeric navigation value", JSON.stringify({ navigationPosition: 42 })],
      ["corrupt layout JSON", "not-json"],
      ["a non-object layout", "42"],
    ])("falls back to the side rail for %s", async (_description, storedLayout) => {
      localStorage.setItem("peppy.layout.v1", storedLayout);
      expect(() => render(<App />)).not.toThrow();

      await screen.findByText("Hello from Aurora");
      expect(document.getElementById("desktop-rail")).toBeInTheDocument();
      expect(document.getElementById("desktop-shell")).toHaveAttribute(
        "data-navigation",
        "side-rail",
      );
    });

    it("places title-bar navigation by platform", async () => {
      const originalPlatform = Object.getOwnPropertyDescriptor(navigator, "platform");
      const assertPlacement = async (
        platform: string,
        placement: "leading" | "trailing",
      ) => {
        Object.defineProperty(navigator, "platform", {
          configurable: true,
          value: platform,
        });
        localStorage.setItem(
          "peppy.layout.v1",
          JSON.stringify({ navigationPosition: "title-bar" }),
        );
        const { unmount } = render(<App />);
        await screen.findByText("Hello from Aurora");
        const titlebar = document.getElementById("desktop-titlebar")!;
        const navigation = document.getElementById("titlebar-navigation")!;
        expect(titlebar).toHaveAttribute("data-navigation-placement", placement);
        expect(
          placement === "leading"
            ? titlebar.firstElementChild
            : titlebar.lastElementChild,
        ).toBe(navigation);
        unmount();
        localStorage.removeItem("peppy.layout.v1");
      };

      try {
        await assertPlacement("MacIntel", "trailing");
        await assertPlacement("Win32", "leading");
        await assertPlacement("Linux x86_64", "leading");
      } finally {
        if (originalPlatform)
          Object.defineProperty(navigator, "platform", originalPlatform);
        else delete (navigator as { platform?: string }).platform;
      }
    });

    it("keeps title-bar navigation interactive with badges and list toggling", async () => {
      host.notifications = [{
        target: { sourceDeviceId: "phone-1", notificationKey: "k1", lifetime: "1" },
        packageName: "com.example.chat",
        appName: "Chat",
        title: "Morgan",
        text: "Hello",
        postedAt: Date.now(),
        dismissible: true,
        seen: false,
        dismissalPending: false,
      }];
      vi.mocked(bridge.load_state).mockImplementation(async id => ({
        ...host.load(id),
        contactsPendingCount: 2,
      }));
      localStorage.setItem(
        "peppy.layout.v1",
        JSON.stringify({ navigationPosition: "title-bar" }),
      );
      render(<App />);

      await screen.findByText("Hello from Aurora");
      const navigation = document.getElementById("titlebar-navigation")!;
      const conversations = within(navigation).getByRole("button", {
        name: "Conversations",
      });
      const contacts = within(navigation).getByRole("button", {
        name: "Contacts, 2 pending",
      });
      const notifications = within(navigation).getByRole("button", {
        name: "Notifications, 1 unread",
      });
      const settings = within(navigation).getByRole("button", { name: "Settings" });

      fireEvent.click(contacts);
      expect(document.getElementById("conversation-pane")).toHaveAttribute(
        "aria-label",
        "Contacts pane",
      );
      expect(contacts).toHaveAttribute("aria-current", "page");

      fireEvent.click(notifications);
      expect(document.getElementById("conversation-pane")).toHaveAttribute(
        "aria-label",
        "Notifications pane",
      );
      expect(notifications).toHaveAttribute("aria-current", "page");

      fireEvent.click(settings);
      expect(screen.getByRole("region", { name: "Settings" })).toBeInTheDocument();
      expect(settings).toHaveAttribute("aria-current", "page");

      fireEvent.click(conversations);
      expect(document.getElementById("thread-list")).toBeVisible();
      expect(conversations).toHaveAttribute("aria-current", "page");
      fireEvent.click(conversations);
      expect(document.getElementById("thread-list")).toHaveAttribute(
        "data-collapsed",
        "true",
      );
    });

    it("persists navigation changes from Settings without changing the active view", async () => {
      localStorage.setItem(
        "peppy.layout.v1",
        JSON.stringify({ listWidth: 320, composerHeight: 128 }),
      );
      render(<App />);

      await screen.findByText("Hello from Aurora");
      fireEvent.click(screen.getByRole("button", { name: "Settings" }));
      const position = document.getElementById("settings-navigation-position")!;
      expect(within(position).getByRole("radio", { name: "Side rail" })).toBeChecked();
      const titleBar = within(position).getByRole("radio", { name: "Title bar" });
      fireEvent.click(titleBar);

      expect(document.getElementById("desktop-rail")).not.toBeInTheDocument();
      expect(screen.getByRole("region", { name: "Settings" })).toBeInTheDocument();
      expect(
        within(document.getElementById("titlebar-navigation")!).getByRole("button", {
          name: "Settings",
        }),
      ).toHaveAttribute("aria-current", "page");
      expect(JSON.parse(localStorage.getItem("peppy.layout.v1")!)).toMatchObject({
        listWidth: 320,
        composerHeight: 128,
        navigationPosition: "title-bar",
      });

      fireEvent.click(screen.getByRole("radio", { name: "Side rail" }));
      expect(document.getElementById("desktop-rail")).toBeInTheDocument();
      expect(screen.getByRole("region", { name: "Settings" })).toBeInTheDocument();
      expect(JSON.parse(localStorage.getItem("peppy.layout.v1")!)).toMatchObject({
        navigationPosition: "side-rail",
      });
    });

    it("moves the status trigger between the titlebar and rail", async () => {
      localStorage.setItem(
        "peppy.layout.v1",
        JSON.stringify({ navigationPosition: "title-bar" }),
      );
      const { unmount } = render(<App />);

      await screen.findByText("Hello from Aurora");
      expect(document.getElementById("titlebar-status")).toContainElement(
        document.querySelector("[data-status-trigger]"),
      );
      expect(document.getElementById("desktop-rail")).not.toBeInTheDocument();
      unmount();

      localStorage.removeItem("peppy.layout.v1");
      render(<App />);
      await screen.findByText("Hello from Aurora");
      expect(document.getElementById("desktop-rail")).toContainElement(
        document.querySelector("[data-status-trigger]"),
      );
    });

    it("increases the thread-list resize maximum by the removed rail width", async () => {
      const originalInnerWidth = Object.getOwnPropertyDescriptor(window, "innerWidth");
      Object.defineProperty(window, "innerWidth", {
        configurable: true,
        value: 800,
      });
      try {
        const { unmount } = render(<App />);
        await screen.findByText("Hello from Aurora");
        const sideRailMaximum = Number(
          document.getElementById("handle-h1")!.getAttribute("aria-valuemax"),
        );
        unmount();

        localStorage.setItem(
          "peppy.layout.v1",
          JSON.stringify({ navigationPosition: "title-bar" }),
        );
        render(<App />);
        await screen.findByText("Hello from Aurora");
        const titleBarMaximum = Number(
          document.getElementById("handle-h1")!.getAttribute("aria-valuemax"),
        );
        expect(titleBarMaximum - sideRailMaximum).toBe(48);
      } finally {
        if (originalInnerWidth)
          Object.defineProperty(window, "innerWidth", originalInnerWidth);
      }
    });

  });

  it("uses titlebar conversation actions to restore the list before starting a draft", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    const toggle = screen.getByRole("button", { name: "Toggle conversation list" });
    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(document.getElementById("thread-list")).not.toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "New conversation" }));
    await waitFor(() => expect(toggle).toHaveAttribute("aria-expanded", "true"));
    expect(document.getElementById("draft-recipients")).toBeInTheDocument();
    await waitFor(() => expect(bridge.save_draft).toHaveBeenCalledWith(
      expect.objectContaining({ recipientIds: [] }),
    ));
    expect(vi.mocked(bridge.save_draft).mock.calls.some(([input]) => input.recipientIds.includes(""))).toBe(false);
  });

  it("shows the overlay scrollbar while the message list is scrolled", async () => {
    render(<App />);
    const list = await screen.findByRole("log", { name: "Messages" });
    await waitFor(() =>
      expect(list).not.toHaveAttribute("data-scroll-programmatic"),
    );
    fireEvent.scroll(list);
    expect(list).toHaveAttribute("data-scrolling", "true");
  });

  it("scrolls the message list to the bottom when a conversation opens", async () => {
    const scrollHeight = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "scrollHeight",
    );
    const clientHeight = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "clientHeight",
    );
    Object.defineProperty(HTMLElement.prototype, "scrollHeight", {
      configurable: true,
      get: () => 400,
    });
    Object.defineProperty(HTMLElement.prototype, "clientHeight", {
      configurable: true,
      get: () => 100,
    });
    try {
      render(<App />);
      const list = await screen.findByRole("log", { name: "Messages" });
      expect(list.scrollTop).toBe(400);
    } finally {
      if (scrollHeight)
        Object.defineProperty(HTMLElement.prototype, "scrollHeight", scrollHeight);
      else delete (HTMLElement.prototype as { scrollHeight?: number }).scrollHeight;
      if (clientHeight)
        Object.defineProperty(HTMLElement.prototype, "clientHeight", clientHeight);
      else delete (HTMLElement.prototype as { clientHeight?: number }).clientHeight;
    }
  });

  it("keeps a reader anchored to the unobscured bottom when the composer grows", async () => {
    const geometry = { overlay: 100, viewport: 300 };
    const restoreGeometry = mockMessageListGeometry(geometry);
    TestResizeObserver.reset();
    vi.stubGlobal("ResizeObserver", TestResizeObserver);
    try {
      render(<App />);
      const list = await screen.findByRole("log", { name: "Messages" });
      await act(async () => TestResizeObserver.resize(document.getElementById("composer-area")!));
      await waitFor(() => expect(document.getElementById("conversation-stage")).toHaveStyle({ "--composer-overlay-height": "100px" }));
      list.scrollTop = 400;
      fireEvent.scroll(list);

      geometry.overlay = 150;
      await act(async () => TestResizeObserver.resize(document.getElementById("composer-area")!));

      expect(list.scrollTop).toBe(450);
      geometry.overlay = 100;
      await act(async () => TestResizeObserver.resize(document.getElementById("composer-area")!));

      expect(list.scrollTop).toBe(400);
      geometry.overlay = 400;
      await act(async () => TestResizeObserver.resize(document.getElementById("composer-area")!));

      expect(list.scrollTop).toBe(600);
    } finally {
      restoreGeometry();
    }
  });

  it("rebaselines after a hidden message list returns without treating it as a reader scroll", async () => {
    const geometry = { overlay: 100, viewport: 300 };
    const restoreGeometry = mockMessageListGeometry(geometry);
    const loadHistory = vi.spyOn(bridge, "load_history").mockResolvedValue({
      conversationId: "aurora",
      messages: [],
      hasMore: false,
    });
    host.conversations[0].hasOlderMessages = true;
    TestResizeObserver.reset();
    vi.stubGlobal("ResizeObserver", TestResizeObserver);
    try {
      render(<App />);
      const list = await screen.findByRole("log", { name: "Messages" });
      await waitFor(() => expect(document.getElementById("conversation-stage")).toHaveStyle({ "--composer-overlay-height": "100px" }));
      list.scrollTop = 400;
      fireEvent.scroll(list);

      fireEvent.click(screen.getByRole("tab", { name: "Media" }));
      geometry.viewport = 0;
      await act(async () => TestResizeObserver.resize(list));
      geometry.overlay = 150;
      await act(async () => TestResizeObserver.resize(document.getElementById("composer-area")!));
      fireEvent.click(screen.getByRole("tab", { name: "Messages" }));
      geometry.viewport = 300;
      await act(async () => TestResizeObserver.resize(list));

      expect(list.scrollTop).toBe(400);
      expect(loadHistory).not.toHaveBeenCalled();
      await waitFor(() =>
        expect(list).not.toHaveAttribute("data-scroll-programmatic"),
      );
      list.scrollTop = 300;
      fireEvent.scroll(list);
      await waitFor(() => expect(loadHistory).toHaveBeenCalledOnce());
    } finally {
      restoreGeometry();
    }
  });

  it("applies a combined list and composer change once while ignoring content-only resizes", async () => {
    const geometry = { overlay: 100, viewport: 300 };
    const restoreGeometry = mockMessageListGeometry(geometry);
    TestResizeObserver.reset();
    vi.stubGlobal("ResizeObserver", TestResizeObserver);
    try {
      render(<App />);
      const list = await screen.findByRole("log", { name: "Messages" });
      await waitFor(() => expect(document.getElementById("conversation-stage")).toHaveStyle({ "--composer-overlay-height": "100px" }));
      list.scrollTop = 400;
      fireEvent.scroll(list);

      await act(async () => TestResizeObserver.resize(document.getElementById("message-list-content")!));
      expect(list.scrollTop).toBe(400);

      geometry.overlay = 150;
      geometry.viewport = 220;
      await act(async () => TestResizeObserver.resize(document.getElementById("composer-area")!, list));
      expect(list.scrollTop).toBe(530);
      await act(async () => TestResizeObserver.resize(list));
      expect(list.scrollTop).toBe(530);
    } finally {
      restoreGeometry();
    }
  });

  it("pins a near-bottom reader when the composer geometry changes", async () => {
    const geometry = { overlay: 100, viewport: 300 };
    const restoreGeometry = mockMessageListGeometry(geometry);
    TestResizeObserver.reset();
    vi.stubGlobal("ResizeObserver", TestResizeObserver);
    try {
      render(<App />);
      const list = await screen.findByRole("log", { name: "Messages" });
      await waitFor(() => expect(document.getElementById("conversation-stage")).toHaveStyle({ "--composer-overlay-height": "100px" }));
      list.scrollTop = 652;
      fireEvent.scroll(list);

      geometry.overlay = 150;
      await act(async () => TestResizeObserver.resize(document.getElementById("composer-area")!));

      expect(list.scrollTop).toBe(1000);
      list.scrollTop = 651;
      fireEvent.scroll(list);
      geometry.overlay = 100;
      await act(async () => TestResizeObserver.resize(document.getElementById("composer-area")!));

      expect(list.scrollTop).toBe(601);
    } finally {
      restoreGeometry();
    }
  });

  it("reattaches geometry observation after returning from Settings", async () => {
    const geometry = { overlay: 100, viewport: 478, scrollHeight: 2000 };
    const restoreGeometry = mockMessageListGeometry(geometry);
    TestResizeObserver.reset();
    vi.stubGlobal("ResizeObserver", TestResizeObserver);
    try {
      render(<App />);
      await screen.findByRole("log", { name: "Messages" });
      fireEvent.click(screen.getByRole("button", { name: "Settings" }));
      fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
      const list = await screen.findByRole("log", { name: "Messages" });
      await act(async () => TestResizeObserver.resize(list));
      list.scrollTop = 500;
      fireEvent.scroll(list);

      geometry.viewport = 378;
      await act(async () => TestResizeObserver.resize(list));

      expect(list.scrollTop).toBe(600);
    } finally {
      restoreGeometry();
    }
  });

  it("renders a narrow list without persisting its window clamp", async () => {
    localStorage.setItem(
      "peppy.layout.v1",
      JSON.stringify({ listWidth: 480, listCollapsed: false }),
    );
    const innerWidth = Object.getOwnPropertyDescriptor(window, "innerWidth");
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      value: 760,
    });
    try {
      render(<App />);
      await screen.findByText("Hello from Aurora");
      fireEvent.resize(window);
      const list = document.getElementById("thread-list")!;
      expect(Number.parseFloat(list.style.width)).toBeLessThanOrEqual(351);
      fireEvent.keyDown(
        screen.getByRole("separator", { name: "Resize composer" }),
        { key: "ArrowDown" },
      );
      expect(JSON.parse(localStorage.getItem("peppy.layout.v1")!)).toMatchObject({
        listWidth: 480,
      });
    } finally {
      if (innerWidth) Object.defineProperty(window, "innerWidth", innerWidth);
    }
  });

  it("falls back safely from corrupt persisted layout", async () => {
    localStorage.setItem("peppy.layout.v1", "not-json");
    expect(() => render(<App />)).not.toThrow();
    await screen.findByText("Hello from Aurora");
    expect(document.getElementById("thread-list")).toBeVisible();
  });

  it("clamps a too-small saved composer height without persisting the display clamp", async () => {
    localStorage.setItem(
      "peppy.layout.v1",
      JSON.stringify({ listWidth: 280, listCollapsed: false, composerHeight: 72 }),
    );
    render(<App />);
    await screen.findByText("Hello from Aurora");
    expect(document.getElementById("handle-h2")).toHaveAttribute(
      "aria-valuenow",
      "96",
    );
    expect(localStorage.getItem("peppy.layout.v1")).toContain(
      '"composerHeight":72',
    );
  });

  it("puts the composer resize grip on the floating card over the message list", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    const stage = document.getElementById("conversation-stage")!;
    const area = document.getElementById("composer-area")!;
    expect(stage).toContainElement(screen.getByRole("log", { name: "Messages" }));
    expect(stage).toContainElement(area);
    expect(area).toContainElement(
      screen.getByRole("separator", { name: "Resize composer" }),
    );
  });

  it("renders the recipient panel in the safe rail layer between messages and composer", async () => {
    render(<App />);
    fireEvent.click(await findNewConversationAction());
    const picker = await screen.findByLabelText("Recipients");
    fireEvent.change(picker, { target: { value: "+1 202 555 0100" } });
    fireEvent.keyDown(picker, { key: "Enter" });
    await waitFor(() =>
      expect(bridge.load_state).toHaveBeenLastCalledWith("conv-new-1"),
    );
    const panel = screen.getByRole("region", { name: "Message recipients" });
    const stage = document.getElementById("conversation-stage")!;
    const layer = document.getElementById("recipient-rail-layer")!;
    const composer = document.getElementById("composer-area")!;
    expect(layer.parentElement).toBe(stage);
    expect(panel.parentElement).toBe(layer);
    expect(layer.previousElementSibling).toBe(document.getElementById("tabpanel-messages"));
    expect(layer.nextElementSibling).toBe(composer);
    expect(composer).not.toContainElement(panel);
  });

  it("restores a valid persisted recipient panel position", async () => {
    localStorage.setItem(
      "peppy.layout.v1",
      JSON.stringify({ listWidth: 320, recipientPosition: { x: 37, y: 53 } }),
    );
    render(<App />);
    fireEvent.click(await findNewConversationAction());
    const picker = await screen.findByLabelText("Recipients");
    fireEvent.change(picker, { target: { value: "+1 202 555 0100" } });
    fireEvent.keyDown(picker, { key: "Enter" });
    await waitFor(() =>
      expect(bridge.load_state).toHaveBeenLastCalledWith("conv-new-1"),
    );
    const panel = screen.getByRole("region", { name: "Message recipients" });
    expect(panel).toHaveStyle({ left: "37px", top: "53px" });
    const savedLayout = localStorage.getItem("peppy.layout.v1");
    fireEvent.resize(window);
    expect(localStorage.getItem("peppy.layout.v1")).toBe(savedLayout);
  });

  it("defaults invalid recipient panel positions and preserves layout fields on keyboard movement", async () => {
    localStorage.setItem(
      "peppy.layout.v1",
      JSON.stringify({ listWidth: 320, recipientAnchor: "bottom-left", sibling: true, recipientPosition: { x: -1, y: 53 } }),
    );
    render(<App />);
    fireEvent.click(await findNewConversationAction());
    const picker = await screen.findByLabelText("Recipients");
    fireEvent.change(picker, { target: { value: "+1 202 555 0100" } });
    fireEvent.keyDown(picker, { key: "Enter" });
    await waitFor(() =>
      expect(bridge.load_state).toHaveBeenLastCalledWith("conv-new-1"),
    );
    const panel = screen.getByRole("region", { name: "Message recipients" });
    expect(panel.style.left).not.toBe("-1px");
    fireEvent.keyDown(
      screen.getByRole("button", { name: "Move recipients panel" }),
      { key: "ArrowDown" },
    );
    await waitFor(() => {
      expect(JSON.parse(localStorage.getItem("peppy.layout.v1")!)).toMatchObject({
        listWidth: 320,
        sibling: true,
        recipientPosition: expect.objectContaining({ x: expect.any(Number), y: expect.any(Number) }),
      });
      expect(JSON.parse(localStorage.getItem("peppy.layout.v1")!)).not.toHaveProperty("recipientAnchor");
    });
  });

  it("renders the recipient panel in the safe rail layer in a composer window", async () => {
    // jsdom does not supply PointerEvent coordinates without a constructor.
    vi.stubGlobal("PointerEvent", MouseEvent);
    openComposerWindow("aurora");
    host.conversations[0].messages = [];
    host.conversations[0].draftOnly = true;
    render(<App />);
    const panel = await screen.findByRole("region", { name: "Message recipients" });
    const stage = document.getElementById("conversation-stage")!;
    const layer = document.getElementById("recipient-rail-layer")!;
    expect(layer.parentElement).toBe(stage);
    expect(panel.parentElement).toBe(layer);
    expect(layer.previousElementSibling).toBe(document.getElementById("tabpanel-messages"));
    expect(layer.nextElementSibling).toBe(document.getElementById("composer-area"));
    const grip = screen.getByRole("button", { name: "Move recipients panel" });
    const before = { left: panel.style.left, top: panel.style.top };
    const saved = localStorage.getItem("peppy.layout.v1");
    fireEvent.pointerDown(grip, {
      pointerId: 1,
      clientX: 10,
      clientY: 10,
    });
    fireEvent.pointerMove(grip, { pointerId: 1, clientX: 80, clientY: 60 });
    expect(document.querySelector("[data-recipient-anchor], #recipient-drop-targets, .recipient-drop-target")).not.toBeInTheDocument();
    fireEvent.pointerCancel(grip, { pointerId: 1 });
    expect(panel.style.left).toBe(before.left);
    expect(panel.style.top).toBe(before.top);
    expect(localStorage.getItem("peppy.layout.v1")).toBe(saved);
  });

  it("collapses and restores the thread list with Enter and persists it", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    const handle = document.getElementById("handle-h1")!;
    fireEvent.keyDown(handle, { key: "Enter" });
    expect(document.getElementById("thread-list")).toHaveAttribute(
      "data-collapsed",
      "true",
    );
    expect(localStorage.getItem("peppy.layout.v1")).toContain(
      '"listCollapsed":true',
    );
    fireEvent.keyDown(handle, { key: "Enter" });
    expect(document.getElementById("thread-list")).not.toHaveAttribute(
      "data-collapsed",
    );
  });

  it("uses the Conversations rail button to toggle the list", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    const conversations = screen.getByRole("button", { name: "Conversations" });
    expect(conversations).toHaveAttribute("aria-expanded", "true");
    fireEvent.click(conversations);
    expect(conversations).toHaveAttribute("aria-expanded", "false");
    expect(document.getElementById("thread-list")).toHaveAttribute(
      "data-collapsed",
      "true",
    );
  });
});

describe("development fixture bridge", () => {
  beforeEach(() => vi.restoreAllMocks());

  it("assigns stable draft and conversation IDs for an empty-ID save and enforces revisions like the host", async () => {
    const created = await fixtureBridge.save_draft({
      id: "",
      conversationId: "",
      text: "",
      recipientIds: ["+12025550100"],
      attachmentIds: [],
      expectedRevision: "0",
    });
    expect(created.id).not.toBe("");
    expect(created.conversationId).not.toBe("");
    expect(created).toMatchObject({
      recipientIds: ["+12025550100"],
      revision: "1",
    });

    const updated = await fixtureBridge.save_draft({
      id: created.id,
      conversationId: created.conversationId,
      text: "hi",
      recipientIds: [],
      attachmentIds: [],
      expectedRevision: "1",
    });
    expect(updated).toMatchObject({
      id: created.id,
      conversationId: created.conversationId,
      text: "hi",
      recipientIds: [],
      revision: "2",
    });
    await expect(
      fixtureBridge.save_draft({
        id: created.id,
        conversationId: created.conversationId,
        text: "late",
        recipientIds: [],
        attachmentIds: [],
        expectedRevision: "1",
      }),
    ).rejects.toMatchObject({ code: "stale-draft" });
    await expect(
      fixtureBridge.save_draft({
        id: "draft-unknown",
        conversationId: "",
        text: "",
        recipientIds: [],
        attachmentIds: [],
        expectedRevision: "0",
      }),
    ).rejects.toMatchObject({ code: "not-found" });

    const reattached = await fixtureBridge.save_draft({
      id: "",
      conversationId: created.conversationId,
      text: "again",
      recipientIds: [],
      attachmentIds: [],
      expectedRevision: "2",
    });
    expect(reattached.id).toBe(created.id);

    const state = await fixtureBridge.load_state(created.conversationId);
    expect(state.mode).toBe("fixture");
    expect(state.activeConversationId).toBe(created.conversationId);
    expect(state.draft).toMatchObject({
      id: created.id,
      text: "again",
      revision: "3",
    });
    expect(
      state.conversations.some(
        (conversation) => conversation.id === created.conversationId,
      ),
    ).toBe(true);
  });

  it("runs the new-recipient flow in fixture mode without debug labels", async () => {
    const save = vi.spyOn(fixtureBridge, "save_draft");
    render(<App />);
    const omni = await screen.findByRole("combobox", { name: "Search Peppy" });
    for (const debugText of [
      "SIMULATED UI",
      "DEVELOPMENT FIXTURE",
      "Browser fixture",
      "sim-fixture",
    ])
      expect(document.body.textContent).not.toContain(debugText);
    fireEvent.change(omni, { target: { value: "+1 202 555 0199" } });
    fireEvent.click(await screen.findByText("Start conversation with (202) 555-0199"));
    await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
    const assigned = await save.mock.results[0].value;
    expect(assigned.conversationId).not.toBe("");
    type("fixture text");
    await waitFor(() => expect(save).toHaveBeenCalledTimes(2));
    expect(save.mock.calls[1][0]).toMatchObject({
      id: assigned.id,
      conversationId: assigned.conversationId,
      expectedRevision: "1",
      text: "fixture text",
    });
    await waitFor(() =>
      expect(screen.queryByRole("alert")).not.toBeInTheDocument(),
    );
    expect(
      within(document.getElementById("thread-list")!).getByRole("button", {
        name: /\(202\) 555-0199/,
      }),
    ).toHaveAttribute("data-conversation-id", assigned.conversationId);
  });
});

describe("display-only contact names", () => {
  it("shows resolved names and photos without changing stored names or recipient IDs", async () => {
    host.conversations.push({
      id: "ada",
      name: "+12025550100",
      preview: "Ping",
      unread: 0,
      participants: ["+12025550100"],
      messages: [{ id: "m-ada", revision: "1", sender: "other", body: "Ping", timestamp: "now", attachments: [] }],
    });
    host.contactResolution = {
      "+12025550100": { contactId: "c1", bookId: "b1", displayName: "Ada Lovelace", photoDataUrl: "data:image/jpeg;base64,AA==" },
    };
    render(<App />);
    const row = await screen.findByText("Ada Lovelace");
    const avatar = row.closest("[data-conversation-row]")?.querySelector("[data-avatar-color]");
    expect(avatar?.querySelector("img")).toHaveAttribute("src", "data:image/jpeg;base64,AA==");
    expect(avatar).toHaveAttribute("data-avatar-color", String(paletteIndex("+12025550100")));
    expect(host.conversations.find((c) => c.id === "ada")?.name).toBe("+12025550100");
    // Unresolved conversations keep their stored names.
    expect(document.querySelector('[data-conversation-id="aurora"]')).toHaveTextContent("Aurora");
  });

  it("uses person and group identities from raw conversation addresses", async () => {
    host.conversations.push(
      { id: "short-code", name: "5556", preview: "Ping", unread: 0, messages: [] },
      { id: "unresolved-number", name: "+18015551234", preview: "Ping", unread: 0, participants: ["+18015551234"], messages: [] },
      {
        id: "group-addresses",
        name: "+18015551234, +15555550199",
        preview: "Group ping",
        unread: 0,
        participants: ["+18015551234", "+15555550199"],
        messages: [],
      },
    );
    render(<App />);
    await waitFor(() => expect(document.querySelector('[data-conversation-id="short-code"] [data-avatar-color]')).toBeInTheDocument());
    expect(document.querySelector('[data-conversation-id="short-code"] [data-avatar-color]')).toHaveAttribute("data-avatar-kind", "person");
    expect(document.querySelector('[data-conversation-id="unresolved-number"] [data-avatar-color]')).toHaveAttribute("data-avatar-kind", "person");
    const groupAvatar = document.querySelector('[data-conversation-id="group-addresses"] [data-avatar-color]');
    expect(groupAvatar).toHaveAttribute("data-avatar-kind", "group");
    expect(groupAvatar).toHaveAttribute(
      "data-avatar-color",
      String(paletteIndex(conversationAvatar({ id: "group-addresses", name: "+18015551234, +15555550199", participants: ["+18015551234", "+15555550199"] }).seed)),
    );

    fireEvent.click(document.querySelector('[data-conversation-id="group-addresses"]')!);
    await waitFor(() => expect(document.querySelector("#thread-contact-avatar [data-avatar-color]")).toHaveAttribute("data-avatar-color", groupAvatar?.getAttribute("data-avatar-color")));
  });

  it("starts a message to a contact phone that has no conversation, using the phone address", async () => {
    vi.spyOn(bridge, "search_contact_recipients").mockResolvedValue([
      { address: "+12025550160", bookId: "book-phone", displayName: "Sol Rivera", number: "+12025550160", label: "mobile", normalized: true, contactId: "c-sol", phoneId: "p1" },
      { address: "+12025550161", bookId: "book-phone", displayName: "Sol Rivera", number: "+12025550161", label: "work", normalized: true, contactId: "c-sol", phoneId: "p2" },
    ]);
    const save = vi.mocked(bridge.save_draft);
    render(<App />);
    fireEvent.click(await findNewConversationAction());
    const picker = await screen.findByLabelText("Recipients");
    await waitFor(() => expect(save).toHaveBeenCalledOnce());
    save.mockClear();
    fireEvent.change(picker, { target: { value: "Sol" } });
    const work = await screen.findByText("work · (202) 555-0161");
    expect(screen.getByText("mobile · (202) 555-0160")).toBeInTheDocument();
    fireEvent.mouseDown(work.closest("li")!);
    await waitFor(() => expect(save).toHaveBeenCalled());
    expect(save.mock.calls[0][0].recipientIds).toEqual(["+12025550161"]);
  });

  it("maps each contact result to its book, contact, and phone row identity", async () => {
    vi.spyOn(bridge, "search_contact_recipients").mockResolvedValue([
      { address: "+12025550160", bookId: "book-a", displayName: "Ada", number: "+12025550160", normalized: true, contactId: "contact-a", phoneId: "phone-a" },
      { address: "+12025550160", bookId: "book-b", displayName: "Ada", number: "+12025550160", normalized: true, contactId: "contact-b", phoneId: "phone-b" },
    ]);

    await expect(searchRecipientSuggestions("Ada")).resolves.toEqual([
      expect.objectContaining({ id: "+12025550160", rowId: '["book-a","contact-a","phone-a"]' }),
      expect.objectContaining({ id: "+12025550160", rowId: '["book-b","contact-b","phone-b"]' }),
    ]);
  });

  it("resolves phone-number notification titles in the feed only for display", async () => {
    host.notifications = [{
      target: { sourceDeviceId: "phone-1", notificationKey: "k1", lifetime: "1" },
      packageName: "com.example.chat", appName: "Chat", title: "+12025550100", text: "Lunch?",
      postedAt: Date.now(), dismissible: true, seen: false, dismissalPending: false,
    }];
    host.contactResolution = { "+12025550100": { contactId: "c1", bookId: "b1", displayName: "Ada Lovelace" } };
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /Notifications/ }));
    expect(await screen.findByLabelText("Chat on Phone 1: Ada Lovelace: Lunch?")).toBeInTheDocument();
    expect(host.notifications[0].title).toBe("+12025550100");
  });
});

describe("notification bulk dismissal", () => {
  const notification = (index: number, overrides: Partial<DesktopSnapshot["notifications"][number]> = {}) => ({
    target: { sourceDeviceId: "phone-1", notificationKey: `notification-${index}`, lifetime: "1" },
    packageName: "com.example.chat",
    appName: "Chat",
    title: `Message ${index}`,
    text: "Body",
    postedAt: Date.now() - index,
    dismissible: true,
    seen: false,
    dismissalPending: false,
    ...overrides,
  });

  it("disables bulk dismissal for locked or ineligible notifications", async () => {
    host.encryption = { state: "locked" };
    host.notifications = [notification(1)];
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /Notifications/ }));
    expect(screen.getByRole("button", { name: "Dismiss all" })).toBeDisabled();
  });

  it("disables bulk dismissal when unlocked notifications are all pending, nondismissible, or muted", async () => {
    host.notifications = [
      notification(1, { dismissalPending: true }),
      notification(2, { dismissible: false }),
      notification(3, { packageName: "com.example.mail" }),
    ];
    host.appFilters = [{ sourceDeviceId: "phone-1", packageName: "com.example.mail", appName: "Mail", muted: true }];
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /Notifications/ }));
    expect(screen.getByRole("button", { name: "Dismiss all" })).toBeDisabled();
  });

  it("labels more than one hundred eligible notifications and suppresses same-turn duplicate dismissal", async () => {
    host.notifications = Array.from({ length: 101 }, (_, index) => notification(index));
    const request = deferred<void>();
    const dismissAll = vi.spyOn(bridge, "dismiss_all_notifications").mockReturnValue(request.promise);
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /Notifications/ }));
    const bulk = screen.getByRole("button", { name: "Dismiss up to 100" });
    fireEvent.click(bulk);
    fireEvent.click(bulk);
    expect(dismissAll).toHaveBeenCalledTimes(1);
    expect(bulk).toBeDisabled();
    request.resolve();
    await waitFor(() => expect(bulk).toBeEnabled());
  });

  it("labels exactly one hundred eligible notifications as dismiss all", async () => {
    host.notifications = Array.from({ length: 100 }, (_, index) => notification(index));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /Notifications/ }));

    expect(screen.getByRole("button", { name: "Dismiss all" })).toBeEnabled();
  });

  it("calls, refreshes, and positions the eligible bulk action in the header", async () => {
    host.notifications = [notification(1)];
    const dismissAll = vi.spyOn(bridge, "dismiss_all_notifications").mockResolvedValue();
    const load = vi.mocked(bridge.load_state);
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /Notifications/ }));
    const bulk = screen.getByRole("button", { name: "Dismiss all" });
    expect(bulk).toHaveClass("btn");
    expect(bulk.closest("#thread-pane-header")).not.toBeNull();
    expect(bulk.closest(".header-actions")).toBeNull();
    const initialLoads = load.mock.calls.length;
    fireEvent.click(bulk);
    await waitFor(() => expect(dismissAll).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(load.mock.calls.length).toBeGreaterThan(initialLoads));
  });

  it("shows bulk dismissal failures", async () => {
    host.notifications = [notification(1)];
    vi.spyOn(bridge, "dismiss_all_notifications").mockRejectedValue(new Error("offline"));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /Notifications/ }));
    fireEvent.click(screen.getByRole("button", { name: "Dismiss all" }));
    expect(await screen.findByText("Could not dismiss notifications. offline")).toBeVisible();
  });

  it("clears a bulk dismissal error before a successful retry", async () => {
    host.notifications = [notification(1)];
    const dismissAll = vi.spyOn(bridge, "dismiss_all_notifications")
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce();
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /Notifications/ }));
    const bulk = screen.getByRole("button", { name: "Dismiss all" });
    fireEvent.click(bulk);
    expect(await screen.findByText("Could not dismiss notifications. offline")).toBeVisible();

    fireEvent.click(bulk);
    await waitFor(() => expect(dismissAll).toHaveBeenCalledTimes(2));
    expect(screen.queryByText("Could not dismiss notifications. offline")).not.toBeInTheDocument();
  });
});

describe("desktop app lock", () => {
  const lockedSnapshot = (errorCode: string): DesktopSnapshot => ({
    ...host.load(),
    connection: { state: "offline", origin: "https://example.test", errorCode },
  });

  it("defaults the password requirement off and never renders a password field", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const checkbox = await screen.findByRole("checkbox", { name: "Require a password when Peppy starts" });
    expect(checkbox).not.toBeChecked();
    expect(document.querySelector("input[type=\"password\"]")).toBeNull();
    expect(screen.getByText(/Changes apply the next time Peppy starts/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Lock now" })).not.toBeInTheDocument();
  });

  it("enables the requirement only through the native prompt and reflects the fresh status", async () => {
    const setLock = vi.spyOn(bridge, "set_app_lock").mockResolvedValue(true);
    vi.spyOn(bridge, "get_app_lock_status")
      .mockResolvedValueOnce({ enabled: false, locked: false, storageAvailable: true })
      .mockResolvedValueOnce({ enabled: true, locked: false, storageAvailable: true });
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const checkbox = await screen.findByRole("checkbox", { name: "Require a password when Peppy starts" });
    fireEvent.click(checkbox);
    await waitFor(() => expect(setLock).toHaveBeenCalledWith(true));
    await waitFor(() => expect(screen.getByRole("checkbox", { name: "Require a password when Peppy starts" })).toBeChecked());
    expect(document.querySelector("input[type=\"password\"]")).toBeNull();
  });

  it("keeps the prior setting when the native prompt is cancelled", async () => {
    const setLock = vi.spyOn(bridge, "set_app_lock").mockResolvedValue(false);
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const checkbox = await screen.findByRole("checkbox", { name: "Require a password when Peppy starts" });
    fireEvent.click(checkbox);
    await waitFor(() => expect(setLock).toHaveBeenCalledWith(true));
    expect(await screen.findByText("The password requirement was not changed.")).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "Require a password when Peppy starts" })).not.toBeChecked();
  });

  it("disables the control and explains storage failure when secure storage is unavailable", async () => {
    vi.spyOn(bridge, "get_app_lock_status").mockResolvedValue({ enabled: false, locked: false, storageAvailable: false });
    render(<App />);
    await screen.findByText("Hello from Aurora");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(await screen.findByRole("checkbox", { name: "Require a password when Peppy starts" })).toBeDisabled();
    expect(screen.getByText("Secure storage is unavailable, so this setting cannot be changed.")).toBeInTheDocument();
  });

  it("replaces the shell with a plain lock screen that shows no host data until unlocked", async () => {
    vi.mocked(bridge.load_state).mockResolvedValueOnce(lockedSnapshot("app-locked"));
    const unlock = vi.spyOn(bridge, "unlock_app").mockResolvedValue(true);
    render(<App />);
    expect(await screen.findByRole("heading", { name: "Peppy is locked" })).toBeInTheDocument();
    expect(screen.queryByText("Hello from Aurora")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Settings" })).not.toBeInTheDocument();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Unlock" }));
    expect(await screen.findByText("Hello from Aurora")).toBeInTheDocument();
    expect(unlock).toHaveBeenCalledOnce();
    expect(screen.queryByRole("heading", { name: "Peppy is locked" })).not.toBeInTheDocument();
  });

  it("stays locked when the native unlock is cancelled or rejected", async () => {
    vi.mocked(bridge.load_state).mockResolvedValue(lockedSnapshot("app-locked"));
    const unlock = vi.spyOn(bridge, "unlock_app").mockResolvedValue(false);
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Unlock" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Peppy is still locked.");
    expect(unlock).toHaveBeenCalledOnce();
    expect(bridge.load_state).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("heading", { name: "Peppy is locked" })).toBeInTheDocument();
  });

  it("shows a storage recovery message instead of an unlock action when secure storage fails", async () => {
    vi.mocked(bridge.load_state).mockResolvedValue(lockedSnapshot("secure-store-unavailable"));
    const unlock = vi.spyOn(bridge, "unlock_app").mockResolvedValue(true);
    render(<App />);
    expect(await screen.findByRole("heading", { name: "Peppy cannot check its lock" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Unlock" })).not.toBeInTheDocument();
    expect(screen.queryByText(/wrong password|incorrect password|server/i)).not.toBeInTheDocument();
    expect(screen.queryByText("Hello from Aurora")).not.toBeInTheDocument();
    expect(unlock).not.toHaveBeenCalled();
  });

  it("never applies the local app lock in the browser host", async () => {
    vi.mocked(bridge.load_state).mockResolvedValue({ ...lockedSnapshot("app-locked"), mode: "browser", desktop: undefined });
    const status = vi.spyOn(bridge, "get_app_lock_status");
    render(<App hostKind="browser" fixedOrigin="https://community.example" />);
    await screen.findByRole("button", { name: "Settings" });
    expect(screen.queryByRole("heading", { name: "Peppy is locked" })).not.toBeInTheDocument();
    expect(status).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(screen.queryByRole("checkbox", { name: "Require a password when Peppy starts" })).not.toBeInTheDocument();
  });
});

describe("vault shared secret reveal", () => {
  it("shows the generic fallback, not the raw native message, for an unknown reveal error", async () => {
    vi.mocked(bridge.load_state).mockResolvedValue({
      ...host.load(),
      deviceRole: "owner",
      connection: { state: "connected", origin: "https://example.test" },
    });
    const reveal = vi.spyOn(bridge, "reveal_vault_secret").mockRejectedValue(
      Object.assign(new Error("raw keychain detail /private/secret"), { code: "keychain-unexpected" }),
    );
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Settings" }));
    fireEvent.click(screen.getByRole("button", { name: "Show vault shared secret" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The native operation failed. Your edits remain in this window.",
    );
    expect(reveal).toHaveBeenCalledOnce();
    expect(screen.queryByText(/raw keychain detail/)).not.toBeInTheDocument();
  });
});

describe("theme persistence and window sync (M26)", () => {
  const LAYOUT_KEY = "peppy.layout.v1";
  const shell = () => document.getElementById("desktop-shell");
  const chooseTheme = (theme: "light" | "dark" | "system") => {
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.change(screen.getByLabelText("Theme"), { target: { value: theme } });
  };
  const storedLayout = () => JSON.parse(localStorage.getItem(LAYOUT_KEY) ?? "{}") as Record<string, unknown>;
  const otherWindowWrites = (newValue: string | null) => act(() => {
    window.dispatchEvent(new StorageEvent("storage", { key: LAYOUT_KEY, newValue }));
  });

  it("keeps a chosen Light theme, its body portal class, and sibling layout keys across remount", async () => {
    localStorage.setItem(LAYOUT_KEY, JSON.stringify({ composerHeight: 128 }));
    const first = render(<App />);
    await screen.findByText("Hello from Aurora");

    chooseTheme("light");

    expect(shell()).toHaveClass("theme-light");
    expect(document.body).toHaveClass("theme-light");
    expect(storedLayout()).toMatchObject({ theme: "light", composerHeight: 128 });
    first.unmount();
    expect(document.body).not.toHaveClass("theme-light");

    render(<App />);
    await screen.findByText("Hello from Aurora");
    expect(shell()).toHaveClass("theme-light");
    expect(document.body).toHaveClass("theme-light");
  });

  it("follows another window's theme change without writing it back", async () => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    const setItem = vi.spyOn(localStorage, "setItem");

    otherWindowWrites(JSON.stringify({ theme: "dark", composerHeight: 140 }));

    expect(shell()).toHaveClass("theme-dark");
    expect(document.body).toHaveClass("theme-dark");
    expect(setItem.mock.calls.filter(([key]) => key === LAYOUT_KEY)).toEqual([]);
  });

  it.each([
    ["an unknown theme", JSON.stringify({ theme: "purple" })],
    ["a non-string theme", JSON.stringify({ theme: 1 })],
    ["corrupt layout JSON", "not-json"],
  ])("defaults to the system theme for %s", async (_description, layout) => {
    localStorage.setItem(LAYOUT_KEY, layout);
    render(<App />);
    await screen.findByText("Hello from Aurora");

    expect(shell()).toHaveClass("theme-system");
    expect(document.body).toHaveClass("theme-system");
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(screen.getByLabelText("Theme")).toHaveValue("system");
  });

  it.each([
    ["an unknown theme", JSON.stringify({ theme: "neon" })],
    ["corrupt layout JSON", "{"],
    ["a cleared layout", null],
  ])("keeps the current theme when another window writes %s", async (_description, newValue) => {
    render(<App />);
    await screen.findByText("Hello from Aurora");
    chooseTheme("dark");

    otherWindowWrites(newValue);

    expect(shell()).toHaveClass("theme-dark");
    expect(screen.getByLabelText("Theme")).toHaveValue("dark");
  });
});
