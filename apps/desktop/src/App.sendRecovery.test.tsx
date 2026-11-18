import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { GatewayView, MessageView, SendRecovery } from "@peppy/desktop-ui/bridge-contracts";
import { App, MessageMeta, sendAgainRouteLabel } from "./App";
import { bridge } from "./bridge/bridge";
import { peppyCopy } from "./generated/peppyCopy";
import { SendAgainStoreContext, type SendAgainStore } from "./messages/SendAgain";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const outgoing = (sendRecovery?: SendRecovery, overrides: Partial<MessageView> = {}): MessageView => ({
  id: "m1",
  revision: "1",
  sender: "self",
  body: "Are you there?",
  timestamp: "1788434580000",
  status: "failed-before-submit",
  statusAt: "1788434640000",
  attachments: [],
  transport: "sms",
  ...(sendRecovery ? { sendRecovery } : {}),
  ...overrides,
});

/** A store stub for the bubble alone: only the calls the notice makes. */
function storeStub(overrides: Partial<SendAgainStore> = {}): SendAgainStore {
  return {
    open: vi.fn(),
    close: vi.fn(),
    showSentAgain: vi.fn(),
    sentAgainFor: () => undefined,
    isPending: () => false,
    errorFor: () => undefined,
    startCall: () => true,
    settleCall: vi.fn(),
    requestDoneFocus: vi.fn(),
    consumeDoneFocus: () => false,
    ...overrides,
  };
}

describe("MessageMeta send recovery", () => {
  it("shows Not sent, the never-sent reason and Send again for a not-sent SMS", () => {
    render(<MessageMeta message={outgoing({ kind: "not-sent", canSendAgain: true })} />);
    const recovery = document.querySelector('[data-message-recovery="m1"]');
    expect(recovery).toHaveAttribute("data-recovery-kind", "not-sent");
    expect(screen.getByText(peppyCopy.message_status_not_sent)).toBeInTheDocument();
    expect(screen.getByText(peppyCopy.message_not_sent_hint)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: peppyCopy.message_send_again_action })).toHaveAttribute("id", "message-send-again-m1");
    expect(document.querySelector('[data-message-meta="m1"]')).toHaveAttribute("data-status", "failed-before-submit");
  });

  it("shows Couldn't confirm with the removed-phone reason and Send again for an unconfirmed-removed SMS", () => {
    render(<MessageMeta message={outgoing({ kind: "unconfirmed-removed", canSendAgain: true }, { status: "gateway-persisted" })} />);
    expect(document.querySelector('[data-message-recovery="m1"]')).toHaveAttribute("data-recovery-kind", "unconfirmed-removed");
    expect(screen.getByText(peppyCopy.message_status_unconfirmed)).toBeInTheDocument();
    expect(screen.getByText(peppyCopy.message_unconfirmed_removed_hint)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: peppyCopy.message_send_again_action })).toBeInTheDocument();
  });

  it("shows the cancel reason and no button for unconfirmed-removed-cancel", () => {
    render(<MessageMeta message={outgoing({ kind: "unconfirmed-removed-cancel", canSendAgain: false }, { status: "server-accepted" })} />);
    expect(document.querySelector('[data-message-recovery="m1"]')).toHaveAttribute("data-recovery-kind", "unconfirmed-removed-cancel");
    expect(screen.getByText(peppyCopy.message_status_unconfirmed)).toBeInTheDocument();
    expect(screen.getByText(peppyCopy.message_unconfirmed_cancel_hint)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: peppyCopy.message_send_again_action })).not.toBeInTheDocument();
  });

  it("shows the reason without a button when canSendAgain is false", () => {
    render(<MessageMeta message={outgoing({ kind: "not-sent", canSendAgain: false })} />);
    expect(screen.getByText(peppyCopy.message_not_sent_hint)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: peppyCopy.message_send_again_action })).not.toBeInTheDocument();
  });

  it("replaces the reason and button with the done row and a Show button once sentAgain is set", () => {
    render(<MessageMeta message={outgoing({ kind: "not-sent", canSendAgain: false, sentAgain: { messageId: "m1-sent-again" } })} />);
    const done = document.querySelector('[data-message-send-again-done="m1"]');
    expect(done).toHaveAttribute("id", "message-send-again-done-m1");
    expect(done).toHaveAttribute("role", "status");
    expect(done).toHaveTextContent(peppyCopy.message_send_again_done);
    expect(screen.getByRole("button", { name: peppyCopy.message_send_again_show_label })).toHaveAttribute("id", "message-send-again-show-m1");
    expect(screen.queryByText(peppyCopy.message_not_sent_hint)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: peppyCopy.message_send_again_action })).not.toBeInTheDocument();
    expect(screen.getByText(peppyCopy.message_status_not_sent)).toBeInTheDocument();
  });

  it("uses the unconfirmed done copy for a removed-phone message", () => {
    render(<MessageMeta message={outgoing({ kind: "unconfirmed-removed", canSendAgain: false, sentAgain: { messageId: "m1-sent-again" } }, { status: "queued-local" })} />);
    expect(document.querySelector('[data-message-send-again-done="m1"]')).toHaveTextContent(peppyCopy.message_send_again_done_unconfirmed);
    expect(screen.getByText(peppyCopy.message_status_unconfirmed)).toBeInTheDocument();
  });

  it("opens the popover through the store from the Send again button", () => {
    const store = storeStub();
    render(<SendAgainStoreContext.Provider value={store}>
      <MessageMeta message={outgoing({ kind: "not-sent", canSendAgain: true })} />
    </SendAgainStoreContext.Provider>);
    fireEvent.click(screen.getByRole("button", { name: peppyCopy.message_send_again_action }));
    expect(store.open).toHaveBeenCalledWith(expect.objectContaining({ id: "m1" }), expect.any(HTMLButtonElement));
  });

  it("calls the store's Show with the new message id", () => {
    const store = storeStub();
    render(<SendAgainStoreContext.Provider value={store}>
      <MessageMeta message={outgoing({ kind: "not-sent", canSendAgain: false, sentAgain: { messageId: "m1-sent-again" } })} />
    </SendAgainStoreContext.Provider>);
    fireEvent.click(screen.getByRole("button", { name: peppyCopy.message_send_again_show_label }));
    expect(store.showSentAgain).toHaveBeenCalledWith("m1-sent-again");
  });

  it("shows the done row from the store's sent-again result before the snapshot carries it", () => {
    render(<SendAgainStoreContext.Provider value={storeStub({ sentAgainFor: (id) => (id === "m1" ? "m1-sent-again" : undefined) })}>
      <MessageMeta message={outgoing({ kind: "not-sent", canSendAgain: true })} />
    </SendAgainStoreContext.Provider>);
    expect(document.querySelector('[data-message-send-again-done="m1"]')).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: peppyCopy.message_send_again_action })).not.toBeInTheDocument();
  });

  it("gives the Send again button the reason as its accessible description, so repeated buttons are told apart", () => {
    render(<MessageMeta message={outgoing({ kind: "not-sent", canSendAgain: true })} />);
    const button = screen.getByRole("button", { name: peppyCopy.message_send_again_action });
    expect(button).toHaveAccessibleDescription(peppyCopy.message_not_sent_hint);
  });

  it("keeps the recovery hook off the RCS route-hint attribute (non-RCS rows assert it is absent)", () => {
    render(<MessageMeta message={outgoing({ kind: "not-sent", canSendAgain: true })} />);
    expect(document.querySelector("[data-message-route-hint]")).toBeNull();
    expect(document.querySelector('[data-message-recovery="m1"] [data-recovery-reason="not-sent"]')).toBeInTheDocument();
  });

  it("puts the notice after the time, so the time stays on the status line", () => {
    render(<MessageMeta message={outgoing({ kind: "unconfirmed-removed", canSendAgain: true }, { status: "gateway-persisted", statusAt: "1788434640000" })} />);
    const meta = document.querySelector('[data-message-meta="m1"]')!;
    expect(meta.lastElementChild).toHaveAttribute("data-message-recovery", "m1");
    const separator = meta.querySelector(".message-meta-separator");
    const time = meta.querySelector("time");
    expect(separator && time && separator.compareDocumentPosition(time) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("renders no orphan separator for a timeless removed message (queued or accepted)", () => {
    const { unmount } = render(<MessageMeta message={outgoing({ kind: "unconfirmed-removed", canSendAgain: true }, { status: "queued-local" })} />);
    expect(document.querySelector(".message-meta-separator")).toBeNull();
    expect(document.querySelector('[data-message-recovery="m1"]')).toBeInTheDocument();
    unmount();
    render(<MessageMeta message={outgoing({ kind: "unconfirmed-removed-cancel", canSendAgain: false }, { status: "server-accepted" })} />);
    expect(document.querySelector(".message-meta-separator")).toBeNull();
  });

  it("keeps the RCS unknown hint and no recovery block for an RCS message without sendRecovery", () => {
    render(<MessageMeta message={{ id: "rcs1", revision: "1", sender: "self", body: "hi", timestamp: "1788434580000", status: "unknown", statusAt: "1788434640000", attachments: [], transport: "rcs" }} />);
    expect(document.querySelector('[data-message-route-hint="rcs1"]')).toHaveAttribute("data-hint", "unknown");
    expect(document.querySelector("[data-message-recovery]")).toBeNull();
    expect(screen.queryByRole("button", { name: peppyCopy.message_send_again_action })).not.toBeInTheDocument();
  });

  it("suppresses the cancel control and cancellation notice for a message in a recovery state", async () => {
    const load = bridge.load_state.bind(bridge);
    vi.spyOn(bridge, "load_state").mockImplementation(async (...args) => {
      const snapshot = await load(...args);
      return { ...snapshot, conversations: snapshot.conversations.map((conversation) => ({ ...conversation, messages: conversation.messages.map((message) => message.id === "message-aurora-2" ? { ...message, sendCancellation: { state: "requested" as const, canCancel: true, requiresGatewayUpdate: false }, sendRecovery: { kind: "unconfirmed-removed-cancel" as const, canSendAgain: false } } : message) })) };
    });
    render(<App />);
    expect(await screen.findByText(peppyCopy.message_unconfirmed_cancel_hint)).toBeInTheDocument();
    const row = document.querySelector('[data-message-row="message-aurora-2"]')!;
    expect(row.querySelector("[data-message-cancel]")).toBeNull();
    expect(row.querySelector("[data-message-cancellation]")).toBeNull();
    expect(row.querySelector("[data-message-cancellation-notice]")).toBeNull();
    expect(row.querySelector("[data-message-recovery]")).toHaveAttribute("data-recovery-kind", "unconfirmed-removed-cancel");
  });
});

describe("sendAgainRouteLabel", () => {
  const delayed: GatewayView = { id: "pixel", name: "Pixel 8", simId: "sim-1", online: true, simulated: false, supportsSms: true, supportsMms: false, availability: "delayed", lastSeenAgeSeconds: 720 };

  it("uses the composer's paused wording while the account is read-only", () => {
    expect(sendAgainRouteLabel([delayed], true)(delayed).statusLabel).toContain(peppyCopy.account_send_paused_status);
    expect(sendAgainRouteLabel([delayed], false)(delayed).statusLabel).not.toContain(peppyCopy.account_send_paused_status);
  });

  it("labels the SIM the way the composer's picker does, and omits it for a phone without a SIM", () => {
    expect(sendAgainRouteLabel([delayed])(delayed).simLabel).toBe("SIM 1");
    expect(sendAgainRouteLabel([{ ...delayed, simId: "" }])({ ...delayed, simId: "" }).simLabel).toBeUndefined();
  });
});
