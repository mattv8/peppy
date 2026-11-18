import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { GatewayView, MessageView, SendRecovery } from "@peppy/desktop-ui/bridge-contracts";
import { App, MessageMeta, sendAgainRouteLabel } from "./App";
import { bridge } from "./bridge/bridge";
import { peppyCopy } from "./generated/peppyCopy";
import { SendAgainStoreContext, type SendAgainStore } from "./messages/SendAgain";
import { SendAgainHost } from "./messages/SendAgainHost";

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
  it("moves focus from the sent-again row to history when the copy arrives", async () => {
    const first = outgoing({ kind: "not-sent", canSendAgain: true });
    const latest = outgoing(undefined, { id: "copy", status: "server-accepted" });
    const gateway: GatewayView = { id: "phone", name: "Phone", simId: "sim", online: true, simulated: false, supportsSms: true, supportsMms: true };
    vi.spyOn(bridge, "send_again").mockResolvedValue({ messageId: "copy" });
    const build = (grouped: boolean) => <SendAgainHost conversation={{ id: "c", name: "Test", preview: "", unread: 0, messages: grouped ? [first, latest] : [first] }} gateways={[gateway]} routeLabel={() => ({ statusLabel: "Connected" })} serverOffline={false} onGoToLatest={() => {}}>
      {grouped ? <MessageMeta message={latest} resend={{ attempts: [first, latest], earlierRisk: "none" }} /> : <MessageMeta message={first} />}
    </SendAgainHost>;
    const view = render(build(false));
    fireEvent.click(screen.getByRole("button", { name: "Send again" }));
    fireEvent.click(document.getElementById("send-again-confirm-m1")!);
    await waitFor(() => expect(document.getElementById("message-send-again-done-m1")).toHaveFocus());
    view.rerender(build(true));
    expect(document.getElementById("message-send-history-copy")).toHaveFocus();
  });

  it("focuses history in the timeline when a sent-again snapshot arrives", async () => {
    const snapshot = await bridge.load_state("conv-aurora");
    const first = outgoing({ kind: "not-sent", canSendAgain: true }, { id: "chain-a" });
    const latest = outgoing(undefined, { id: "chain-b", status: "server-accepted" });
    const gateway: GatewayView = { id: "phone", name: "Phone", simId: "sim", online: true, simulated: false, supportsSms: true, supportsMms: true };
    let grouped = false;
    const listeners = new Set<() => void>();
    vi.spyOn(bridge, "subscribe").mockImplementation((listener) => { listeners.add(listener); return () => { listeners.delete(listener); }; });
    vi.spyOn(bridge, "load_state").mockImplementation(async () => {
      return { ...snapshot, gateways: [gateway], conversations: snapshot.conversations.map((conversation, index) => index === 0 ? { ...conversation, messages: grouped ? [{ ...first, sentAgain: { messageId: "chain-b" } }, latest] : [first] } : conversation) };
    });
    vi.spyOn(bridge, "send_again").mockResolvedValue({ messageId: "chain-b" });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Send again" }));
    fireEvent.click(document.getElementById("send-again-confirm-chain-a")!);
    await waitFor(() => expect(document.getElementById("message-send-again-done-chain-a")).toHaveFocus());
    grouped = true;
    act(() => listeners.forEach((notify) => notify()));
    expect(await screen.findByRole("button", { name: "Send history, 2 attempts" })).toHaveFocus();
  });
  it("shows history and duplicate risk, then restores focus after closing", () => {
    const first = outgoing({ kind: "unconfirmed-removed", canSendAgain: false }, { id: "first", status: "unknown" });
    const latest = outgoing(undefined, { id: "latest", status: "server-accepted" });
    render(<MessageMeta message={latest} resend={{ attempts: [first, latest], earlierRisk: "possible" }} />);
    expect(screen.getByText(peppyCopy.message_send_earlier_may_have_sent)).toBeInTheDocument();
    const button = screen.getByRole("button", { name: "Send history, 2 attempts" });
    button.focus();
    fireEvent.click(button);
    const dialog = screen.getByRole("dialog", { name: "Send history" });
    expect([...dialog.querySelectorAll("[data-send-attempt]")].map((item) => item.getAttribute("data-send-attempt"))).toEqual(["latest", "first"]);
    expect(dialog).toHaveTextContent(peppyCopy.message_unconfirmed_removed_hint);
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(button).toHaveFocus();
  });

  it("shows only the delivery history button for a delivered chain", () => {
    const first = outgoing(undefined, { id: "first" });
    const latest = outgoing(undefined, { id: "latest", status: "delivery-confirmed" });
    render(<MessageMeta message={latest} resend={{ attempts: [first, latest], earlierRisk: "sent" }} />);
    const footer = document.querySelector('[data-message-meta="latest"]')!;
    expect(footer.children).toHaveLength(1);
    expect(screen.getByRole("button", { name: "Delivery confirmed. Send history, 2 attempts" })).toHaveAttribute("title", "Delivery confirmed");
  });

  it("numbers earlier history attempts and shows their status tones", () => {
    const first = outgoing({ kind: "not-sent", canSendAgain: false }, { id: "first" });
    const middle = outgoing({ kind: "unconfirmed-removed", canSendAgain: false }, { id: "middle", status: "unknown" });
    const latest = outgoing(undefined, { id: "latest", status: "server-accepted" });
    render(<MessageMeta message={latest} resend={{ attempts: [first, middle, latest], earlierRisk: "possible" }} />);
    fireEvent.click(screen.getByRole("button", { name: "Send history, 3 attempts" }));
    expect(document.querySelector('[data-send-attempt="first"]')).toHaveTextContent("Attempt 1");
    expect(document.querySelector('[data-send-attempt="middle"]')).toHaveTextContent("Attempt 2");
    expect(document.querySelector('[data-send-attempt="first"] [data-recovery-tone="error"]')).toBeInTheDocument();
    expect(document.querySelector('[data-send-attempt="middle"] [data-recovery-tone="warning"]')).toBeInTheDocument();
  });

  it("puts the resend action after history for a grouped latest recovery", () => {
    const first = outgoing(undefined, { id: "first" });
    const latest = outgoing({ kind: "not-sent", canSendAgain: true }, { id: "latest" });
    render(<MessageMeta message={latest} resend={{ attempts: [first, latest], earlierRisk: "none" }} />);
    const history = screen.getByRole("button", { name: "Send history, 2 attempts" });
    const send = screen.getByRole("button", { name: "Send again" });
    expect(history.compareDocumentPosition(send) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });
  it("shows Not sent, the never-sent reason and Send again for a not-sent SMS", () => {
    render(<MessageMeta message={outgoing({ kind: "not-sent", canSendAgain: true })} />);
    const recovery = document.querySelector('[data-message-recovery="m1"]');
    expect(recovery).toHaveAttribute("data-recovery-kind", "not-sent");
    expect(screen.getByText(peppyCopy.message_status_not_sent)).toBeInTheDocument();
    expect(screen.queryByText(peppyCopy.message_not_sent_hint)).not.toBeInTheDocument();
    const details = screen.getByRole("button", { name: "Details for Not sent" });
    details.focus();
    fireEvent.click(details);
    expect(screen.getByRole("dialog", { name: "Not sent" })).toHaveTextContent(peppyCopy.message_not_sent_hint);
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(details).toHaveFocus();
    expect(screen.getByRole("button", { name: peppyCopy.message_send_again_action })).toHaveAttribute("id", "message-send-again-m1");
    expect(document.querySelector('[data-message-meta="m1"]')).toHaveAttribute("data-status", "failed-before-submit");
  });

  it("shows Couldn't confirm with the removed-phone reason and Send again for an unconfirmed-removed SMS", () => {
    render(<MessageMeta message={outgoing({ kind: "unconfirmed-removed", canSendAgain: true }, { status: "gateway-persisted" })} />);
    expect(document.querySelector('[data-message-recovery="m1"]')).toHaveAttribute("data-recovery-kind", "unconfirmed-removed");
    expect(screen.getByText(peppyCopy.message_status_unconfirmed)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Details for Couldn't confirm" }));
    expect(screen.getByRole("dialog")).toHaveTextContent(peppyCopy.message_unconfirmed_removed_hint);
    expect(screen.getByRole("button", { name: peppyCopy.message_send_again_action })).toBeInTheDocument();
  });

  it("shows the cancel reason and no button for unconfirmed-removed-cancel", () => {
    render(<MessageMeta message={outgoing({ kind: "unconfirmed-removed-cancel", canSendAgain: false }, { status: "server-accepted" })} />);
    expect(document.querySelector('[data-message-recovery="m1"]')).toHaveAttribute("data-recovery-kind", "unconfirmed-removed-cancel");
    expect(screen.getByText(peppyCopy.message_status_unconfirmed)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Details for Couldn't confirm" }));
    expect(screen.getByRole("dialog")).toHaveTextContent(peppyCopy.message_unconfirmed_cancel_hint);
    expect(screen.queryByRole("button", { name: peppyCopy.message_send_again_action })).not.toBeInTheDocument();
  });

  it("shows the reason without a button when canSendAgain is false", () => {
    render(<MessageMeta message={outgoing({ kind: "not-sent", canSendAgain: false })} />);
    expect(screen.getByRole("button", { name: "Details for Not sent" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: peppyCopy.message_send_again_action })).not.toBeInTheDocument();
  });

  it("replaces the reason and button with the done row and a Show button once sentAgain is set", () => {
    render(<MessageMeta message={outgoing({ kind: "not-sent", canSendAgain: false, sentAgain: { messageId: "m1-sent-again" } })} />);
    const done = document.querySelector('[data-message-send-again-done="m1"]');
    expect(done).toHaveAttribute("id", "message-send-again-done-m1");
    expect(done).toHaveTextContent(peppyCopy.message_send_again_sent);
    expect(screen.getByRole("button", { name: peppyCopy.message_send_again_show_label })).toHaveAttribute("id", "message-send-again-show-m1");
    expect(screen.queryByText(peppyCopy.message_not_sent_hint)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: peppyCopy.message_send_again_action })).not.toBeInTheDocument();
    expect(screen.getByText(peppyCopy.message_status_not_sent)).toBeInTheDocument();
  });

  it("uses the unconfirmed done copy for a removed-phone message", () => {
    render(<MessageMeta message={outgoing({ kind: "unconfirmed-removed", canSendAgain: false, sentAgain: { messageId: "m1-sent-again" } }, { status: "queued-local" })} />);
    expect(document.querySelector('[data-message-send-again-done="m1"]')).toHaveTextContent(peppyCopy.message_send_again_sent);
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

  it("provides the reason through the contextual details button", () => {
    render(<MessageMeta message={outgoing({ kind: "not-sent", canSendAgain: true })} />);
    expect(screen.getByRole("button", { name: "Details for Not sent" })).toHaveAttribute("aria-haspopup", "dialog");
  });

  it("keeps the recovery hook off the RCS route-hint attribute (non-RCS rows assert it is absent)", () => {
    render(<MessageMeta message={outgoing({ kind: "not-sent", canSendAgain: true })} />);
    expect(document.querySelector("[data-message-route-hint]")).toBeNull();
    expect(document.querySelector('#message-send-details-m1')).toBeInTheDocument();
  });

  it("puts the notice after the time, so the time stays on the status line", () => {
    render(<MessageMeta message={outgoing({ kind: "unconfirmed-removed", canSendAgain: true }, { status: "gateway-persisted", statusAt: "1788434640000" })} />);
    const meta = document.querySelector('[data-message-meta="m1"]')!;
    expect(meta.lastElementChild).toHaveAttribute("data-message-recovery", "m1");
    const time = meta.querySelector("time");
    expect(time && time.compareDocumentPosition(meta.lastElementChild!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(meta.querySelector(".message-meta-separator")).toBeNull();
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
    expect(await screen.findByRole("button", { name: "Details for Couldn't confirm" })).toBeInTheDocument();
    const row = document.querySelector('[data-message-row="message-aurora-2"]')!;
    expect(row.querySelector("[data-message-cancel]")).toBeNull();
    expect(row.querySelector("[data-message-cancellation]")).toBeNull();
    expect(row.querySelector("[data-message-cancellation-notice]")).toBeNull();
    expect(row.querySelector("[data-message-recovery]")).toHaveAttribute("data-recovery-kind", "unconfirmed-removed-cancel");
  });

  it("collapses adjacent explicitly linked attempts into one row", async () => {
    const load = bridge.load_state.bind(bridge);
    const first = outgoing({ kind: "unconfirmed-removed", canSendAgain: false }, { id: "chain-a", status: "unknown", sentAgain: { messageId: "chain-b" } });
    const latest = outgoing(undefined, { id: "chain-b", status: "server-accepted" });
    vi.spyOn(bridge, "load_state").mockImplementation(async (...args) => {
      const snapshot = await load(...args);
      return { ...snapshot, conversations: snapshot.conversations.map((conversation, index) => index === 0 ? { ...conversation, messages: [first, latest] } : conversation) };
    });
    render(<App />);
    expect(await screen.findByRole("button", { name: "Send history, 2 attempts" })).toBeInTheDocument();
    expect(document.querySelector('[data-message-id="chain-a"]')).toBeNull();
    expect(document.querySelector('[data-message-id="chain-b"]')).toHaveAttribute("data-resend-attempts", "chain-a chain-b");
    expect(screen.getAllByText("Are you there?")).toHaveLength(1);
  });

  it("leaves a nonadjacent resend visible with Show navigation", async () => {
    const load = bridge.load_state.bind(bridge);
    const first = outgoing({ kind: "not-sent", canSendAgain: false }, { id: "chain-a", sentAgain: { messageId: "chain-b" } });
    const reply = outgoing(undefined, { id: "reply", sender: "other", body: "Reply", status: undefined });
    const latest = outgoing(undefined, { id: "chain-b", status: "server-accepted" });
    vi.spyOn(bridge, "load_state").mockImplementation(async (...args) => {
      const snapshot = await load(...args);
      return { ...snapshot, conversations: snapshot.conversations.map((conversation, index) => index === 0 ? { ...conversation, messages: [first, reply, latest] } : conversation) };
    });
    render(<App />);
    const show = await screen.findByRole("button", { name: peppyCopy.message_send_again_show_label });
    expect(document.querySelector('[data-message-id="chain-a"]')).toBeInTheDocument();
    const latestRow = document.querySelector<HTMLElement>('[data-message-id="chain-b"]')!;
    fireEvent.click(show);
    expect(latestRow).toHaveFocus();
  });

  it("keeps an earlier attempt visible when it has reactions", async () => {
    const load = bridge.load_state.bind(bridge);
    const first = outgoing({ kind: "not-sent", canSendAgain: false }, { id: "chain-a", sentAgain: { messageId: "chain-b" }, reactions: [{ emoji: "👍", actors: [{ self: true }] }] });
    const latest = outgoing(undefined, { id: "chain-b", status: "server-accepted" });
    vi.spyOn(bridge, "load_state").mockImplementation(async (...args) => {
      const snapshot = await load(...args);
      return { ...snapshot, conversations: snapshot.conversations.map((conversation, index) => index === 0 ? { ...conversation, messages: [first, latest] } : conversation) };
    });
    render(<App />);
    expect(await screen.findByRole("button", { name: peppyCopy.message_send_again_show_label })).toBeInTheDocument();
    expect(document.querySelector('[data-message-id="chain-a"]')).toBeInTheDocument();
    expect(document.querySelector('[data-message-id="chain-b"]')).toBeInTheDocument();
  });

  it("keeps an earlier attempt with cancellation state visible", async () => {
    const load = bridge.load_state.bind(bridge);
    const first = outgoing(undefined, { id: "chain-a", status: "unknown", sentAgain: { messageId: "chain-b" }, sendCancellation: { state: "unknown", canCancel: false, requiresGatewayUpdate: false } });
    const latest = outgoing(undefined, { id: "chain-b", status: "server-accepted" });
    vi.spyOn(bridge, "load_state").mockImplementation(async (...args) => {
      const snapshot = await load(...args);
      return { ...snapshot, conversations: snapshot.conversations.map((conversation, index) => index === 0 ? { ...conversation, messages: [first, latest] } : conversation) };
    });
    render(<App />);
    expect(await screen.findByText("Cancellation status is unknown.")).toBeInTheDocument();
    expect(document.querySelector('[data-message-id="chain-a"]')).toBeInTheDocument();
    expect(document.querySelector('[data-message-id="chain-b"]')).toBeInTheDocument();
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
