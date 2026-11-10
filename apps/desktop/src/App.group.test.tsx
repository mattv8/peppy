import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ConversationView, GroupView, MessageView, SendResultView, TypingSignal } from "@peppy/desktop-ui/bridge-contracts";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge/bridge";
import { peppyCopy } from "./generated/peppyCopy";

const GROUP_ID = "conv-group-test";
const NEW_ID = "conv-new-group";
const MEMBERS = ["+15555550142", "+15555550199"];

let base: DesktopSnapshot;
let snapshot: DesktopSnapshot;

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
  base = structuredClone(await fixtureBridge.load_state());
  vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId) => ({
    ...structuredClone(snapshot),
    activeConversationId: conversationId ?? snapshot.activeConversationId,
  }));
  vi.spyOn(bridge, "subscribe").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  vi.spyOn(bridge, "save_draft").mockImplementation(async (input) => ({
    id: input.id || "draft-created",
    conversationId: input.conversationId || "conv-created",
    text: input.text,
    recipientIds: input.recipientIds,
    attachmentIds: input.attachmentIds,
    groupName: input.groupName,
    gatewayId: input.gatewayId,
    simId: input.simId,
    revision: String(Number(input.expectedRevision) + 1),
  }));
  snapshot = { ...base };
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const group = (overrides: Partial<GroupView> = {}): GroupView => ({
  members: MEMBERS,
  state: "live",
  memberRevision: "3",
  canCreate: false,
  createNeedsConfirmation: false,
  ...overrides,
});

const groupConversation = (overrides: Partial<ConversationView> = {}): ConversationView => ({
  id: GROUP_ID,
  name: "Weekend crew",
  preview: "",
  unread: 0,
  participants: MEMBERS,
  messages: [],
  group: group({ name: "Weekend crew" }),
  ...overrides,
});

const message = (overrides: Partial<MessageView> & Pick<MessageView, "id">): MessageView => ({
  revision: "1",
  sender: "other",
  senderAddress: MEMBERS[0],
  body: "hello",
  timestamp: "1000",
  transport: "rcs",
  attachments: [],
  ...overrides,
});

/** A snapshot whose active conversation is `conversations[0]`, with RCS eligibility for it. */
function useSnapshot(conversations: ConversationView[], draft: Partial<NonNullable<DesktopSnapshot["draft"]>> = {}) {
  const gateway = { ...base.gateways[0], simulated: false };
  const expiresAtMs = String(Date.now() + 60_000);
  snapshot = {
    ...base,
    mode: "native",
    gateways: [gateway],
    conversations,
    activeConversationId: conversations[0].id,
    draft: { id: "draft-group", conversationId: conversations[0].id, text: "", recipientIds: [], attachmentIds: [], gatewayId: gateway.id, simId: gateway.simId, revision: "1", ...draft },
    rcsStatus: {
      gateways: [{ gatewayId: gateway.id, state: "linked", current: true, available: true, healthy: true, phoneResponsive: true, observedAtMs: "0", capabilityRevision: "1", expiresAtMs }],
      eligibilities: [{ conversationId: conversations[0].id, gatewayId: gateway.id, eligibility: "eligible", observedAtMs: "0", mappingRevision: "1", expiresAtMs }],
    },
  };
}

/** A two-recipient draft that has no conversation yet, as the new-conversation composer uses. */
const newGroupConversation = (): ConversationView => ({ id: NEW_ID, name: "", preview: "", unread: 0, participants: [], messages: [], draftOnly: true });

async function typeText(text: string) {
  const textarea = await screen.findByRole<HTMLTextAreaElement>("textbox", { name: "Message" });
  fireEvent.change(textarea, { target: { value: text } });
  return textarea;
}

const sentAt = (sendDraft: { mock: { calls: unknown[][] } }, call: number) => sendDraft.mock.calls[call]?.[0] as Record<string, unknown>;

/** The main Send button, once the draft can be sent. */
async function sendButton() {
  return waitFor(() => {
    const button = screen.getByRole("button", { name: "Send" });
    expect(button).toBeEnabled();
    return button;
  });
}

describe("group header and details", () => {
  it("opens the details dialog from the header and returns focus to the button", async () => {
    useSnapshot([groupConversation()]);
    render(<App />);

    const button = await waitFor(() => {
      const found = document.getElementById("header-group-details");
      expect(found).toBeInTheDocument();
      return found!;
    });
    expect(button).toHaveAttribute("aria-haspopup", "dialog");
    fireEvent.click(button);

    expect(document.getElementById("group-details-dialog")).toBeInTheDocument();
    expect(document.getElementById(`group-member-${MEMBERS[0]}`)).toBeInTheDocument();
    fireEvent.click(document.getElementById("group-details-close")!);
    expect(document.getElementById("group-details-dialog")).toBeNull();
  });

  it("titles a named group with its name and an unnamed group with its member count", async () => {
    useSnapshot([groupConversation()]);
    const { unmount } = render(<App />);
    await waitFor(() => expect(document.querySelector("[data-header-title]")).toHaveTextContent("Weekend crew"));
    expect(document.querySelector("[data-conversation-participants]")!.textContent).not.toContain("people");
    unmount();

    useSnapshot([groupConversation({ name: "", group: group() })]);
    render(<App />);
    await waitFor(() => expect(document.querySelector("[data-conversation-participants]")).toHaveTextContent(peppyCopy.group_header_count.replace("{count}", "3")));
  });

  it("shows the RCS badge marked paused for a paused group, and the MMS badge for an unlinked group", async () => {
    useSnapshot([groupConversation({ group: group({ state: "paused", name: "Weekend crew" }) })]);
    const { unmount } = render(<App />);
    await waitFor(() => expect(document.getElementById("conversation-rcs-badge")).toHaveAttribute("data-paused", "true"));
    expect(document.getElementById("conversation-rcs-notice")).toHaveAttribute("data-rcs-notice", "group-paused");
    unmount();

    useSnapshot([groupConversation({ group: group({ state: "unlinked", name: "Weekend crew" }) })]);
    render(<App />);
    await waitFor(() => expect(document.getElementById("conversation-mms-badge")).toBeInTheDocument());
    expect(document.getElementById("conversation-rcs-badge")).toBeNull();
  });
});

describe("group name and send", () => {
  it("shows the group name field only for two or more recipients", async () => {
    useSnapshot([newGroupConversation()], { recipientIds: [MEMBERS[0]] });
    const { unmount } = render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    expect(document.getElementById("new-group-name")).toBeNull();
    unmount();

    useSnapshot([newGroupConversation()], { recipientIds: MEMBERS });
    render(<App />);
    await waitFor(() => expect(document.getElementById("new-group-name")).toBeInTheDocument());
    expect(document.getElementById("new-group-help")).toHaveAttribute("data-group-name-help");
  });

  it("sends the trimmed group name with the new group's draft", async () => {
    useSnapshot([newGroupConversation()], { recipientIds: MEMBERS });
    const sendDraft = vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "submitted", revision: "2" });
    render(<App />);
    const name = await waitFor(() => {
      const field = document.getElementById("new-group-name") as HTMLInputElement | null;
      expect(field).toBeInTheDocument();
      return field!;
    });
    fireEvent.change(name, { target: { value: "  Sunday   crew " } });
    await typeText("See you");
    fireEvent.click(await sendButton());

    await waitFor(() => expect(sendDraft).toHaveBeenCalledTimes(1));
    expect(sentAt(sendDraft, 0)).toMatchObject({ groupName: "Sunday crew", recipientIds: MEMBERS });
    expect(sentAt(sendDraft, 0)).not.toHaveProperty("expectedMemberRevision");
  });

  it("sends the composer's member revision on every group send", async () => {
    useSnapshot([groupConversation()]);
    const sendDraft = vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "submitted", revision: "2" });
    render(<App />);
    await typeText("Hi all");
    fireEvent.click(await sendButton());

    await waitFor(() => expect(sendDraft).toHaveBeenCalledTimes(1));
    expect(sentAt(sendDraft, 0)).toMatchObject({ expectedMemberRevision: "3" });
  });
});

describe("send refusals", () => {
  it("keeps a members-changed draft, shows the new list, and resends only when asked", async () => {
    useSnapshot([groupConversation()]);
    const refusal: SendResultView = { accepted: false, status: "failed-before-submit", reason: "members_changed", group: { members: [...MEMBERS, "+15555550123"], memberRevision: "4" } };
    const sendDraft = vi.spyOn(bridge, "send_draft").mockResolvedValueOnce(refusal).mockResolvedValue({ accepted: true, status: "submitted", revision: "2" });
    render(<App />);
    const textarea = await typeText("Keep me");
    fireEvent.click(await sendButton());

    const alert = await waitFor(() => {
      const found = document.getElementById("composer-group-changed");
      expect(found).toHaveAttribute("role", "alert");
      return found!;
    });
    expect(alert).toHaveTextContent(peppyCopy.group_send_members_changed);
    expect(alert).toHaveTextContent("Now in this group:");
    expect(textarea).toHaveValue("Keep me");
    expect(sendDraft).toHaveBeenCalledTimes(1);

    fireEvent.click(document.getElementById("group-send-updated")!);
    await waitFor(() => expect(sendDraft).toHaveBeenCalledTimes(2));
    expect(sentAt(sendDraft, 1)).toMatchObject({ expectedMemberRevision: "4" });
  });

  it("uses generic text and does not invent a revision when the refusal carries no metadata yet", async () => {
    useSnapshot([groupConversation()]);
    vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: false, status: "failed-before-submit", reason: "members_changed" });
    render(<App />);
    await typeText("Generic");
    fireEvent.click(await sendButton());

    const alert = await waitFor(() => {
      const found = document.getElementById("composer-group-changed");
      expect(found).toBeInTheDocument();
      return found!;
    });
    expect(alert).toHaveTextContent(peppyCopy.group_send_members_changed);
    expect(document.getElementById("composer-group-changed-members")).toBeNull();
  });

  it("asks before creating the group again after an unknown create outcome", async () => {
    useSnapshot([newGroupConversation()], { recipientIds: MEMBERS });
    const sendDraft = vi.spyOn(bridge, "send_draft")
      .mockResolvedValueOnce({ accepted: false, status: "unknown", reason: "create_unconfirmed" })
      .mockResolvedValue({ accepted: true, status: "submitted", revision: "2" });
    render(<App />);
    await typeText("Create this");
    fireEvent.click(await sendButton());

    fireEvent.click(await waitFor(() => {
      const again = document.getElementById("group-create-again");
      expect(again).toBeInTheDocument();
      return again!;
    }));
    expect(sentAt(sendDraft, 0)).not.toHaveProperty("groupCreateConfirmDuplicate");
    await waitFor(() => expect(sendDraft).toHaveBeenCalledTimes(2));
    expect(sentAt(sendDraft, 1)).toMatchObject({ groupCreateConfirmDuplicate: true });
  });

  it("tells the user once that the group was sent as MMS when no RCS group could be made", async () => {
    useSnapshot([newGroupConversation()], { recipientIds: MEMBERS });
    vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "submitted", reason: "create_not_rcs", revision: "2" });
    render(<App />);
    await typeText("Hello");
    fireEvent.click(await sendButton());
    await waitFor(() => expect(document.getElementById("composer-group-create-not-rcs")).toBeInTheDocument());
    expect(document.getElementById("composer-group-create-not-rcs")).toHaveAttribute("data-group-create", "not-rcs");
  });

  it("shows the update-required notice and keeps the draft", async () => {
    useSnapshot([groupConversation()]);
    vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: false, status: "failed-before-submit", reason: "update_required" });
    render(<App />);
    const textarea = await typeText("Update first");
    fireEvent.click(await sendButton());
    await waitFor(() => expect(document.getElementById("composer-group-update-required")).toBeInTheDocument());
    expect(textarea).toHaveValue("Update first");
  });

  it("copies a refused timeline message back into the composer without sending it", async () => {
    useSnapshot([groupConversation({ messages: [message({ id: "message-refused", sender: "self", senderAddress: undefined, body: "Please resend", status: "failed-before-submit", groupHint: "members_changed" })] })]);
    const sendDraft = vi.spyOn(bridge, "send_draft");
    render(<App />);
    fireEvent.click(await waitFor(() => {
      const edit = document.getElementById("group-message-edit-message-refused");
      expect(edit).toBeInTheDocument();
      return edit!;
    }));

    expect(await screen.findByRole<HTMLTextAreaElement>("textbox", { name: "Message" })).toHaveValue("Please resend");
    expect(sendDraft).not.toHaveBeenCalled();
    expect(document.getElementById("composer-group-changed")).toBeInTheDocument();
  });

  it("puts an unconfirmed create back in the composer behind its confirmation, and sends only on that confirmation", async () => {
    useSnapshot([groupConversation({ messages: [message({ id: "message-unknown", sender: "self", senderAddress: undefined, body: "Hello team", status: "unknown", groupHint: "create_unknown" })] })]);
    const sendDraft = vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "queued-local", revision: "2" });
    render(<App />);
    fireEvent.click(await waitFor(() => {
      const again = document.getElementById("group-message-create-again-message-unknown");
      expect(again).toBeInTheDocument();
      return again!;
    }));

    expect(await screen.findByRole<HTMLTextAreaElement>("textbox", { name: "Message" })).toHaveValue("Hello team");
    expect(sendDraft).not.toHaveBeenCalled();
    fireEvent.click(document.getElementById("group-create-again")!);
    await waitFor(() => expect(sendDraft).toHaveBeenCalledWith(expect.objectContaining({ groupCreateConfirmDuplicate: true })));
  });

  it("shows the update-required and withdrawn message hints as informational text", async () => {
    useSnapshot([groupConversation({ messages: [
      message({ id: "message-update", sender: "self", senderAddress: undefined, body: "Old", status: "failed-before-submit", groupHint: "update_required" }),
      message({ id: "message-withdrawn", sender: "self", senderAddress: undefined, body: "Gone", status: "failed-before-submit", groupHint: "withdrawn" }),
    ] })]);
    render(<App />);
    await waitFor(() => expect(document.querySelector("[data-hint='group-update-required']")).toHaveTextContent(peppyCopy.group_update_required));
    expect(document.querySelector("[data-hint='group-withdrawn']")).toHaveTextContent(peppyCopy.group_withdrawn_notice);
  });
});

describe("withdrawn groups", () => {
  it("is read-only and offers a new group MMS draft to the same people", async () => {
    useSnapshot([groupConversation({ group: group({ state: "withdrawn", name: "Old crew" }) })]);
    const sendDraft = vi.spyOn(bridge, "send_draft");
    render(<App />);
    const textarea = await screen.findByRole<HTMLTextAreaElement>("textbox", { name: "Message" });
    expect(textarea).toHaveAttribute("readonly");
    expect(document.getElementById("unavailable-hint")).toHaveTextContent(`Sending unavailable: ${peppyCopy.group_composer_reason_left}`);
    expect(document.getElementById("conversation-rcs-badge")).toBeNull();
    expect(document.getElementById("conversation-mms-badge")).toBeNull();
    expect(document.getElementById("conversation-rcs-notice")).toHaveAttribute("data-rcs-notice", "group-left");
    expect(document.getElementById("composer-group-withdrawn")).toBeInTheDocument();

    fireEvent.click(document.getElementById("group-start-new-mms")!);
    await waitFor(() => expect(document.getElementById("new-group-name")).toBeInTheDocument());
    expect(sendDraft).not.toHaveBeenCalled();
  });
});

describe("timeline notices", () => {
  it("places core notices between the messages around their time", async () => {
    useSnapshot([groupConversation({
      messages: [
        message({ id: "message-before", body: "before", timestamp: "1000" }),
        message({ id: "message-after", body: "after", timestamp: "3000" }),
      ],
      group: group({ name: "Weekend crew", notices: [{ kind: "added", member: MEMBERS[1], atMs: "2000" }, { kind: "renamed", name: "Weekend crew 2", atMs: "2500" }] }),
    })]);
    render(<App />);

    const notice = await waitFor(() => {
      const found = document.querySelector<HTMLElement>(`[data-group-notice="added"]`);
      expect(found).toBeInTheDocument();
      return found!;
    });
    expect(notice).toHaveClass("timeline-notice");
    expect(notice).toHaveTextContent(" was added");
    const before = document.querySelector('[data-message-id="message-before"]')!;
    const after = document.querySelector('[data-message-id="message-after"]')!;
    expect(before.compareDocumentPosition(notice) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(notice.compareDocumentPosition(after) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(document.querySelector('[data-group-notice="name"]')).toHaveTextContent("Name changed to Weekend crew 2");
  });
});

describe("typing in groups", () => {
  it("does not send typing for a paused group", async () => {
    useSnapshot([groupConversation({ group: group({ state: "paused", name: "Weekend crew" }) })]);
    const sendTyping = vi.spyOn(bridge, "send_typing").mockResolvedValue();
    render(<App />);
    const textarea = await screen.findByRole("textbox", { name: "Message" });
    sendTyping.mockClear();
    fireEvent.change(textarea, { target: { value: "hello" } });
    expect(sendTyping).not.toHaveBeenCalled();
  });

  it("sends typing for a live group", async () => {
    useSnapshot([groupConversation()]);
    const sendTyping = vi.spyOn(bridge, "send_typing").mockResolvedValue();
    render(<App />);
    const textarea = await screen.findByRole("textbox", { name: "Message" });
    await waitFor(() => expect(document.getElementById("conversation-rcs-badge")).toBeInTheDocument());
    sendTyping.mockClear();
    fireEvent.change(textarea, { target: { value: "hello" } });
    expect(sendTyping).toHaveBeenCalledWith({ conversationId: GROUP_ID, state: "started" });
  });

  it.each([
    [[MEMBERS[0]], / is typing$/],
    [MEMBERS, / are typing$/],
  ])("attributes typing to the members who are typing (%#)", async (participants, pattern) => {
    useSnapshot([groupConversation()]);
    let receive: (signal: TypingSignal) => void = () => undefined;
    vi.spyOn(bridge, "subscribe_typing").mockImplementation((listener) => {
      receive = listener;
      return () => undefined;
    });
    render(<App />);
    await waitFor(() => expect(document.getElementById("conversation-rcs-badge")).toBeInTheDocument());
    for (const participant of participants) act(() => receive({ conversationId: GROUP_ID, state: "started", participant }));

    expect(document.getElementById("typing-indicator")).toHaveAttribute("data-typing-visible", "true");
    expect(document.getElementById("typing-indicator-label")!.textContent).toMatch(pattern);
  });
});
