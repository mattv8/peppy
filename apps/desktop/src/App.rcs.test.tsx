import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge/bridge";
import { peppyCopy } from "./generated/peppyCopy";

let snapshot: DesktopSnapshot;

const currentSnapshot = async () => {
  const base = structuredClone(await fixtureBridge.load_state());
  const conversationId = base.activeConversationId!;
  const gatewayId = base.gateways[0].id;
  const expiresAtMs = String(Date.now() + 10_000);
  return {
    ...base,
    mode: "native" as const,
    gateways: [{ ...base.gateways[0], simulated: false }],
    draft: {
      id: "draft-current",
      conversationId,
      text: "x".repeat(161),
      recipientIds: [],
      attachmentIds: [],
      gatewayId,
      simId: base.gateways[0].simId,
      revision: "1",
    },
    rcsStatus: {
      gateways: [{ gatewayId, state: "linked" as const, current: true, available: true, healthy: true, phoneResponsive: true, observedAtMs: "0", capabilityRevision: "1", expiresAtMs }],
      eligibilities: [{ conversationId, gatewayId, eligibility: "eligible" as const, observedAtMs: "0", mappingRevision: "1", expiresAtMs }],
    },
  };
};

beforeEach(async () => {
  window.history.replaceState({}, "", "/");
  vi.restoreAllMocks();
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
  snapshot = await currentSnapshot();
  vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId) => ({
    ...structuredClone(snapshot),
    activeConversationId: conversationId ?? snapshot.activeConversationId,
  }));
  vi.spyOn(bridge, "subscribe").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const expectBadge = async () => {
  await waitFor(() => expect(document.getElementById("conversation-rcs-badge")).toBeInTheDocument());
  return document.getElementById("conversation-rcs-badge")!;
};
const expectNoBadge = () => expect(document.getElementById("conversation-rcs-badge")).toBeNull();

describe("RCS conversation route", () => {
  it("renders a badge only for the current eligible 1:1 chat and hides the SMS counter", async () => {
    render(<App />);

    const badge = await expectBadge();
    expect(badge).toHaveTextContent("RCS");
    expect(badge).toHaveAttribute("title", expect.stringContaining("RCS"));
    expect(badge?.querySelector(".visually-hidden")).toHaveTextContent("RCS");
    expect(document.getElementById("sms-counter")).toBeNull();
  });

  it("does not apply MMS attachment gating to a current RCS route", async () => {
    snapshot = {
      ...snapshot,
      gateways: [{ ...snapshot.gateways[0], supportsMms: false, mmsContentVersion: undefined }],
      draft: {
        ...snapshot.draft!,
        attachmentIds: ["rcs-image"],
        attachments: [{ id: "rcs-image", name: "photo.jpg", mediaType: "image/jpeg", byteSize: 32 * 1024 * 1024, state: "ready", draftState: "local" }],
      },
    };
    render(<App />);

    await screen.findByRole("textbox", { name: "Message" });
    expect(screen.getByRole("button", { name: "Send" })).toBeEnabled();
    expect(document.getElementById("attachment-tray-region")).toHaveTextContent("Up to 32 MiB each");
    expect(document.getElementById("unavailable-hint")).not.toHaveTextContent(/MMS/);
  });

  it("restores only a refused RCS attachment after the send is rejected", async () => {
    const attachment = { id: "rcs-image", name: "photo.jpg", mediaType: "image/jpeg", byteSize: 1, state: "ready" as const, draftState: "local" as const };
    snapshot = {
      ...snapshot,
      draft: { ...snapshot.draft!, text: "Keep this", attachmentIds: [attachment.id], attachments: [attachment] },
    };
    const sendDraft = vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: false, status: "failed-confirmed", reason: "rejected", refusedAttachmentIds: [attachment.id] });
    render(<App />);

    await screen.findByRole("textbox", { name: "Message" });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => expect(sendDraft).toHaveBeenCalledOnce());
    await waitFor(() => expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Message" })).toHaveValue("Keep this"));
    expect(document.querySelector(`[data-attachment-id="${attachment.id}"]`)).toBeInTheDocument();
  });

  it.each([
    ["locked encryption", (next: DesktopSnapshot) => ({ ...next, encryption: { state: "locked" as const } })],
    ["an ineligible conversation", (next: DesktopSnapshot) => ({ ...next, rcsStatus: { ...next.rcsStatus!, eligibilities: [{ ...next.rcsStatus!.eligibilities[0], eligibility: "not_eligible" as const }] } })],
    ["a stale route", (next: DesktopSnapshot) => ({ ...next, rcsStatus: { ...next.rcsStatus!, eligibilities: [{ ...next.rcsStatus!.eligibilities[0], expiresAtMs: "0" }] } })],
    ["a group", (next: DesktopSnapshot) => ({ ...next, conversations: next.conversations.map((conversation) => conversation.id === next.activeConversationId ? { ...conversation, participants: ["one", "two"] } : conversation) })],
    ["a draft", (next: DesktopSnapshot) => ({ ...next, conversations: next.conversations.map((conversation) => conversation.id === next.activeConversationId ? { ...conversation, draftOnly: true } : conversation) })],
  ])("omits the badge for %s", async (_label, change) => {
    snapshot = change(snapshot);
    render(<App />);

    await screen.findByRole("textbox", { name: "Message" });
    expectNoBadge();
    expect(document.getElementById("sms-counter")).toBeInTheDocument();
  });

  it("omits the badge and shows the recovery title for a revoked connection with a stale RCS route", async () => {
    snapshot = { ...snapshot, connection: { state: "error", origin: "https://example.test", errorCode: "revoked" } };
    render(<App />);

    await waitFor(() => expect(document.querySelector("[data-header-title]")?.textContent).toBe("Set up Peppy"));
    expectNoBadge();
  });

  it("omits the badge while the app is locked", async () => {
    vi.spyOn(bridge, "get_app_lock_status").mockResolvedValue({ enabled: true, locked: true, storageAvailable: true, pending: false });
    render(<App />);

    await waitFor(() => expect(bridge.get_app_lock_status).toHaveBeenCalled());
    expectNoBadge();
  });

  it("removes the badge at expiry without another snapshot", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1000);
    snapshot.rcsStatus = {
      gateways: [{ ...snapshot.rcsStatus!.gateways[0], expiresAtMs: "2000" }],
      eligibilities: [{ ...snapshot.rcsStatus!.eligibilities[0], expiresAtMs: "2000" }],
    };
    render(<App />);
    await act(async () => {});
    expect(document.getElementById("conversation-rcs-badge")).toBeInTheDocument();

    await act(async () => vi.advanceTimersByTimeAsync(1000));

    expectNoBadge();
  });

  it("removes the badge in the same render when switching conversations", async () => {
    render(<App />);
    await expectBadge();

    fireEvent.click(screen.getByRole("button", { name: /River Park/ }));

    await screen.findByText("Attachment received");
    expectNoBadge();
  });

  it.each(["settings", "contacts", "notifications"] as const)('omits the badge on the %s screen', async (view) => {
    render(<App />);
    await expectBadge();

    fireEvent.click(document.querySelector<HTMLButtonElement>(`[data-rail-item="${view}"]`)!);

    await waitFor(() => expect(document.getElementById("conversation-pane")).toHaveAttribute("data-view", view));
    expectNoBadge();
  });

  it("omits the badge in detached composer and head panel windows", async () => {
    window.history.replaceState({}, "", "?window=composer&conversationId=conv-aurora");
    const { unmount } = render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    expectNoBadge();
    unmount();

    window.history.replaceState({}, "", "?window=composer&conversationId=conv-aurora&head=1");
    render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    expectNoBadge();
  });

  it("blocks an unproven known RCS chat without clearing its draft or attachment", async () => {
    const sendDraft = vi.spyOn(bridge, "send_draft");
    const attachmentId = snapshot.conversations[0].messages[0].attachments[0]?.id;
    snapshot = {
      ...snapshot,
      conversations: snapshot.conversations.map((conversation) => conversation.id === snapshot.activeConversationId ? { ...conversation, rcsEncryption: true } : conversation),
      draft: { ...snapshot.draft!, text: "Keep this draft", attachmentIds: attachmentId ? [attachmentId] : [] },
      rcsStatus: { ...snapshot.rcsStatus!, eligibilities: [{ ...snapshot.rcsStatus!.eligibilities[0], eligibility: "unknown" }] },
    };
    render(<App />);

    const textarea = await screen.findByRole<HTMLTextAreaElement>("textbox", { name: "Message" });
    const send = screen.getByRole("button", { name: "Send" });
    const reason = peppyCopy.rcs_composer_reason_unproven;
    expect(send).toBeDisabled();
    expect(textarea).toHaveValue("Keep this draft");
    expect(document.getElementById("unavailable-hint")).toHaveTextContent(`Sending unavailable: ${reason}`);
    expect(document.getElementById("composer-route-notice")).toHaveAttribute("role", "status");
    expect(document.getElementById("composer-route-help")).toHaveTextContent(peppyCopy.rcs_composer_draft_kept);
    expect(document.getElementById("sms-counter")).toBeNull();
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(sendDraft).not.toHaveBeenCalled();
  });

  it("keeps a carrier problem ahead of the RCS block reason", async () => {
    snapshot = {
      ...snapshot,
      conversations: snapshot.conversations.map((conversation) => conversation.id === snapshot.activeConversationId ? { ...conversation, rcsEncryption: true } : conversation),
      rcsStatus: { ...snapshot.rcsStatus!, eligibilities: [{ ...snapshot.rcsStatus!.eligibilities[0], eligibility: "unknown" }] },
      gateways: [{ ...snapshot.gateways[0], availability: "offline" }],
    };
    render(<App />);

    await screen.findByRole("textbox", { name: "Message" });
    expect(document.getElementById("unavailable-hint")).not.toHaveTextContent(peppyCopy.rcs_composer_reason_unproven);
  });

  it("keeps the draft when a route becomes blocked while sending", async () => {
    const sendDraft = vi.spyOn(bridge, "send_draft");
    let notify: () => void = () => {};
    vi.mocked(bridge.subscribe).mockImplementation((listener) => {
      notify = listener;
      return () => undefined;
    });
    let releaseSave: (() => void) | undefined;
    const savePending = new Promise<void>((resolve) => { releaseSave = resolve; });
    vi.spyOn(bridge, "save_draft").mockImplementation(async (input) => {
      await savePending;
      return { ...input, id: "draft-current", revision: "2" };
    });
    render(<App />);

    const textarea = await screen.findByRole<HTMLTextAreaElement>("textbox", { name: "Message" });
    fireEvent.change(textarea, { target: { value: "Keep this draft" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    snapshot = {
      ...snapshot,
      conversations: snapshot.conversations.map((conversation) => conversation.id === snapshot.activeConversationId ? { ...conversation, rcsEncryption: true } : conversation),
      rcsStatus: { ...snapshot.rcsStatus!, eligibilities: [{ ...snapshot.rcsStatus!.eligibilities[0], eligibility: "unknown" }] },
    };
    await act(async () => {
      notify();
      releaseSave?.();
    });

    await waitFor(() => expect(document.getElementById("composer-inline-status")).toHaveTextContent(`Not sent: ${peppyCopy.rcs_composer_reason_unproven}.`));
    expect(textarea).toHaveValue("Keep this draft");
    expect(sendDraft).not.toHaveBeenCalled();
  });
});

describe("RCS conversation metadata", () => {
  beforeEach(() => window.history.replaceState({}, "", "/"));

  const withActive = (change: Partial<DesktopSnapshot["conversations"][number]>): DesktopSnapshot => ({
    ...snapshot,
    conversations: snapshot.conversations.map((conversation) => conversation.id === snapshot.activeConversationId ? { ...conversation, ...change } : conversation),
  });

  it("shows the attributed encryption pill only when Google reports encryption for the current chat", async () => {
    snapshot = withActive({ rcsEncryption: true });
    render(<App />);

    await expectBadge();
    const pill = document.getElementById("conversation-rcs-e2ee-badge");
    expect(pill).toHaveAttribute("data-rcs-e2ee-badge", "reported");
    expect(pill).toHaveTextContent(peppyCopy.rcs_e2ee_badge_label);
    expect(peppyCopy.rcs_e2ee_badge_label).toBe("Google: end-to-end encrypted");
    expect(pill?.querySelector(".visually-hidden")).toHaveTextContent(peppyCopy.rcs_e2ee_badge_description);
  });

  it.each([
    ["no encryption fact", {}],
    ["a false encryption fact", { rcsEncryption: false }],
  ])("omits the encryption pill for %s", async (_label, change) => {
    snapshot = withActive(change);
    render(<App />);

    await expectBadge();
    expect(document.getElementById("conversation-rcs-e2ee-badge")).toBeNull();
  });

  it("omits the encryption pill when the route is not current", async () => {
    snapshot = { ...withActive({ rcsEncryption: true }), encryption: { state: "locked" as const } };
    render(<App />);

    await screen.findByRole("textbox", { name: "Message" });
    expectNoBadge();
    expect(document.getElementById("conversation-rcs-e2ee-badge")).toBeNull();
  });

  it("shows the history-gap notice only when core reports a gap for the current chat", async () => {
    snapshot = withActive({ rcsHistoryGap: true });
    render(<App />);

    const notice = await waitFor(() => {
      const element = document.getElementById("conversation-rcs-notice");
      expect(element).toHaveAttribute("data-rcs-notice", "history-gap");
      return element!;
    });
    expect(notice).toHaveAttribute("role", "status");
    expect(notice).toHaveTextContent(peppyCopy.rcs_history_gap_notice);
  });

  it("keeps the history-gap notice empty and collapsed when there is no gap", async () => {
    render(<App />);

    await expectBadge();
    const notice = document.getElementById("conversation-rcs-notice");
    expect(notice).toHaveAttribute("role", "status");
    expect(notice).not.toHaveAttribute("data-rcs-notice");
    expect(notice).toBeEmptyDOMElement();
  });

  it("hides the history-gap notice on the settings screen", async () => {
    snapshot = withActive({ rcsHistoryGap: true });
    render(<App />);

    await waitFor(() => expect(document.getElementById("conversation-rcs-notice")).toHaveAttribute("data-rcs-notice", "history-gap"));
    fireEvent.click(document.querySelector<HTMLButtonElement>('[data-rail-item="settings"]')!);

    await waitFor(() => expect(document.getElementById("conversation-pane")).toHaveAttribute("data-view", "settings"));
    expect(document.getElementById("conversation-rcs-notice")).toBeNull();
  });
});

describe("member-change media recovery", () => {
  it.each(["application/pdf", "audio/mpeg", "video/mp4"])("does not restore or resend a definitive attempted %s failure at index zero", async (mediaType) => {
    snapshot.draft = { ...snapshot.draft!, text: "Existing draft", attachmentIds: [] };
    snapshot.conversations = snapshot.conversations.map((conversation) => conversation.id === snapshot.activeConversationId ? {
      ...conversation,
      messages: [{ id: "attempted-failure", revision: "1", sender: "self", body: "Attempted caption", timestamp: "2026-01-01", transport: "rcs", attachments: [{ id: "attempted-file", name: "attempted.file", mediaType, byteSize: 1, state: "ready", index: 0, rcsSendStatus: "send_failed" }] }],
    } : conversation);
    const sendDraft = vi.spyOn(bridge, "send_draft");
    render(<App />);

    expect(await screen.findByText("Send failed")).toBeInTheDocument();
    expect(document.querySelector('[data-rcs-media-status]')).toHaveAttribute("data-rcs-media-terminal-failure", "true");
    expect(screen.queryByRole("button", { name: peppyCopy.group_message_edit_resend })).toBeNull();
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue("Existing draft");
    expect(sendDraft).not.toHaveBeenCalled();
  });

  it("keeps an attachment with no view pending on the current RCS route", async () => {
    snapshot.draft = { ...snapshot.draft!, attachmentIds: ["metadata-pending"] };
    let notify: () => void = () => undefined;
    vi.mocked(bridge.subscribe).mockImplementation((listener) => {
      notify = listener;
      return () => undefined;
    });
    render(<App />);

    const chip = await screen.findByRole("button", { name: "Remove Attached file" });
    expect(chip.closest("[data-attachment-id]")).toHaveAttribute("data-attachment-id", "metadata-pending");
    snapshot.draft = { ...snapshot.draft, attachments: [{ id: "metadata-pending", name: "later.pdf", mediaType: "application/pdf", byteSize: 1, state: "ready" }] };
    await act(async () => { notify(); });
    await waitFor(() => expect(screen.getByRole("button", { name: "Remove later.pdf" })).toBeInTheDocument());
  });

  it("does not treat a sparse nonzero attachment as the caption carrier", async () => {
    snapshot.draft = { ...snapshot.draft!, text: "My reply", attachmentIds: [] };
    snapshot.conversations = snapshot.conversations.map((conversation) => conversation.id === snapshot.activeConversationId ? {
      ...conversation,
      messages: [{ id: "sparse-media", revision: "1", sender: "self", body: "Photo caption", timestamp: "2026-01-01", transport: "rcs", attachments: [{ id: "sparse-file", name: "sparse.pdf", mediaType: "application/pdf", byteSize: 1, state: "ready", index: 3, rcsSendStatus: "not_sent" }] }],
    } : conversation);
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: peppyCopy.group_message_edit_resend }));
    await waitFor(() => expect(document.querySelector('[data-attachment-id="sparse-file"]')).toBeInTheDocument());
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue("My reply");
  });

  it.each([false, true])("restores definitely unsent original indexes only (mixed=%s), without sending", async (mixed) => {
    const attachments = [
      ...(mixed ? [{ id: "already-sent", name: "attempted.jpg", mediaType: "image/jpeg", byteSize: 67, state: "ready" as const, index: 0, rcsSendStatus: "send_failed" as const }, { id: "unknown", name: "unknown.pdf", mediaType: "application/pdf", byteSize: 80, state: "ready" as const, index: 1, rcsSendStatus: "outcome_unknown" as const }] : []),
      { id: "member-file", name: "review.pdf", mediaType: "application/pdf", byteSize: 2048, state: "ready" as const, index: mixed ? 2 : 0, rcsSendStatus: "members_changed" as const },
      ...(mixed ? [{ id: "never-sent", name: "rest.pdf", mediaType: "application/pdf", byteSize: 1024, state: "ready" as const, index: 3, rcsSendStatus: "not_sent" as const }] : []),
    ];
    snapshot.draft = { ...snapshot.draft!, text: "", attachmentIds: [] };
    snapshot.conversations = snapshot.conversations.map(c => c.id === snapshot.activeConversationId ? { ...c, participants: ["+15555550101", "+15555550102"], group: { members: ["+15555550101", "+15555550102"], memberRevision: "reviewed-7", state: "live", canCreate: false, createNeedsConfirmation: false }, messages: [{ id: "review-media", revision: "1", sender: "self", body: "Original caption", timestamp: "2026-01-01", transport: "rcs", attachments }] } : c);
    const sendDraft = vi.spyOn(bridge, "send_draft");
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: peppyCopy.group_message_edit_resend }));
    fireEvent.click(screen.getByRole("button", { name: peppyCopy.group_message_edit_resend }));
    await waitFor(() => expect(document.querySelector('#attachment-tray-region [data-attachment-id="member-file"]')).toBeInTheDocument());
    const tray = document.getElementById("attachment-tray-region")!;
    expect(tray).toHaveTextContent("review.pdf");
    expect(tray.querySelector('[data-attachment-id="member-file"] svg.lucide-file-text')).toBeInTheDocument();
    expect(tray.querySelector('[data-attachment-id="already-sent"]')).toBeNull();
    expect(tray.querySelector('[data-attachment-id="unknown"]')).toBeNull();
    if (mixed) expect(tray.querySelector('[data-attachment-id="never-sent"]')).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue(mixed ? "" : "Original caption");
    expect(document.getElementById("composer-group-changed")).toHaveAttribute("data-member-revision", "reviewed-7");
    expect(document.getElementById("composer-group-changed-members")).toHaveTextContent("+15555550101");
    expect(sendDraft).not.toHaveBeenCalled();
  });

  it("keeps intentional matching caption text and recovers equal captions from distinct sources", async () => {
    snapshot.draft = { ...snapshot.draft!, text: "Hello there", attachmentIds: [] };
    snapshot.conversations = snapshot.conversations.map(c => c.id === snapshot.activeConversationId ? {
      ...c,
      participants: ["+15555550101", "+15555550102"],
      group: { members: ["+15555550101", "+15555550102"], memberRevision: "reviewed-7", state: "live", canCreate: false, createNeedsConfirmation: false },
      messages: ["source-one", "source-two"].map((id, index) => ({
        id,
        revision: "1",
        sender: "self",
        body: "Hello",
        timestamp: "2026-01-01",
        transport: "rcs" as const,
        attachments: [{ id: `file-${index}`, name: `file-${index}.pdf`, mediaType: "application/pdf", byteSize: 1, state: "ready" as const, index: 0, rcsSendStatus: "not_sent" as const }],
      })),
    } : c);
    render(<App />);

    const recover = await screen.findAllByRole("button", { name: peppyCopy.group_message_edit_resend });
    fireEvent.click(recover[0]);
    await waitFor(() => expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue("Hello\nHello there"));
    fireEvent.click(recover[1]);

    await waitFor(() => expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue("Hello\nHello\nHello there"));
    expect(document.querySelectorAll('#attachment-tray-region [data-attachment-id]')).toHaveLength(2);
  });
});

describe("incoming RCS attachment replacement focus", () => {
  it.each([
    ["image", false, false], ["image", true, false], ["file", false, false], ["file", true, false],
    ["image", false, true], ["image", true, true], ["file", false, true], ["file", true, true],
  ] as const)("keeps slot identity for %s, intermediate=%s, user leaves=%s", async (kind, intermediate, userLeaves) => {
    const messageId = "focus-recovery";
    const placeholder = { id: "placeholder", index: 3, name: kind === "image" ? "photo.jpg" : "report.pdf", mediaType: kind === "image" ? "image/jpeg" : "application/pdf", byteSize: 128, state: "pending" as const, rcsState: "quota_full" as const };
    const sibling = { id: "sibling", index: 4, name: "second.jpg", mediaType: "image/jpeg", byteSize: 67, state: "ready" as const, rcsState: "stored" as const, previewUrl: "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==" };
    snapshot.conversations = snapshot.conversations.map(c => c.id === snapshot.activeConversationId ? { ...c, messages: [{ id: messageId, revision: "1", sender: "other", body: "Focus recovery", timestamp: "2026-01-01", transport: "rcs", attachments: kind === "image" ? [placeholder, sibling] : [placeholder] }] } : c);
    render(<App />);
    const retry = await screen.findByRole("button", { name: /Retry download for/ });
    retry.focus();
    const root = retry.closest(kind === "image" ? "[data-media-tile]" : "[data-rcs-attachment-slot]");
    const transition = async (rcsState: "downloading" | "stored") => {
      const value = { ...placeholder, rcsState, ...(rcsState === "stored" ? { id: "hosted-original", state: "ready" as const, previewUrl: kind === "image" ? sibling.previewUrl : undefined } : {}) };
      snapshot.conversations = snapshot.conversations.map(c => c.id === snapshot.activeConversationId ? { ...c, messages: c.messages.map(m => m.id === messageId ? { ...m, revision: String(Number(m.revision) + 1), attachments: kind === "image" ? [value, sibling] : [value] } : m) } : c);
      await act(async () => { vi.mocked(bridge.subscribe).mock.calls.at(-1)![0](); });
      await waitFor(() => expect(document.querySelector(`[data-attachment-id="${value.id}"]`) ?? document.querySelector(`[data-media-attachment="${value.id}"]`)).toBeInTheDocument());
      await waitFor(() => expect(document.querySelector(`[data-rcs-media-state="${rcsState}"]`)).toBeInTheDocument());
    };
    if (intermediate) {
      await transition("downloading");
      expect(document.activeElement).toBe(root);
    }
    const outside = screen.getByRole("textbox", { name: "Message" });
    if (userLeaves) outside.focus();
    await transition("stored");
    const replacement = document.querySelector(kind === "image" ? '[data-media-tile][data-media-attachment="hosted-original"]' : '[data-rcs-attachment-slot]');
    expect(replacement).toBe(root);
    if (userLeaves) expect(document.activeElement).toBe(outside);
    else expect(document.activeElement).toBe(root?.querySelector("button"));
  });
});
