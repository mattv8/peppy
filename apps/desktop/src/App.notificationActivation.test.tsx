import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { NotificationActivation } from "@peppy/generated/notification-activation";
import type { ComponentProps } from "react";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge";
import { nativeNotificationActivation } from "./notificationActivation";

const native = vi.hoisted(() => ({ pending: null as NotificationActivation | null, hint: () => {}, ack: vi.fn() }));
const feed = vi.hoisted(() => ({ result: undefined as ((requestId: string, found: boolean) => void) | undefined }));
vi.mock("./Notifications", async importOriginal => {
  const original = await importOriginal<typeof import("./Notifications")>();
  return { ...original, NotificationsView: (props: ComponentProps<typeof original.NotificationsView>) => {
    feed.result = props.onFocusResult;
    return <original.NotificationsView {...props} />;
  } };
});
vi.mock("./notificationActivation", async importOriginal => {
  const original = await importOriginal<typeof import("./notificationActivation")>();
  return { ...original, nativeNotificationActivation: {
    subscribe: async (listener: () => void) => { native.hint = listener; return () => {}; },
    peek: async () => native.pending,
    ack: async (value: NotificationActivation) => { native.ack(value); if (native.pending?.requestId === value.requestId) native.pending = null; return true; },
  } };
});
let snapshot: DesktopSnapshot;
let stateHint = () => {};
beforeEach(async () => {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key) });
  native.pending = null;
  native.ack.mockClear();
  snapshot = structuredClone(await fixtureBridge.load_state());
  snapshot.encryption.profileFingerprint = "profile-a";
  vi.spyOn(bridge, "load_state").mockImplementation(async id => ({ ...structuredClone(snapshot), activeConversationId: id ?? snapshot.activeConversationId }));
  const listeners = new Set<() => void>();
  stateHint = () => { for (const listener of [...listeners]) listener(); };
  vi.spyOn(bridge, "subscribe").mockImplementation(listener => { listeners.add(listener); return () => { listeners.delete(listener); }; });
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => {});
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => {});
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  Object.defineProperty(Element.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
async function clickBanner(destination: NotificationActivation["destination"]) {
  await act(async () => {
    native.pending = { requestId: native.pending ? "click-2" : "click-1", sessionGeneration: "native-1", destination };
    native.hint();
  });
}

it("opens the selected conversation even when Notifications is open", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  fireEvent.click(screen.getByRole("button", { name: /^Notifications/ }));
  await clickBanner({ kind: "conversation", conversationId: "conv-aurora" });
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  expect(document.querySelector("#notification-feed")).not.toBeInTheDocument();
  expect(screen.getByRole("textbox", { name: "Message" })).toBeInTheDocument();
});

it("keeps a replacement lifetime unfocused and acknowledges the unavailable target", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  await clickBanner({ kind: "mirrored", target: { ...snapshot.notifications[0].target, lifetime: "obsolete" } });
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  expect(document.activeElement).not.toHaveAttribute("data-notification-target");
  expect(document.querySelector("#notifications-action-notice")?.textContent).toBeTruthy();
});

it("retains pending activation while locked and drains after the unlocked snapshot", async () => {
  snapshot.encryption.state = "locked";
  render(<App />);
  await waitFor(() => expect(screen.getByRole("combobox", { name: "Search Peppy" })).toBeDisabled());
  await clickBanner({ kind: "conversation", conversationId: "conv-river" });
  expect(native.ack).not.toHaveBeenCalled();
  snapshot.encryption.state = "unlocked";
  await act(async () => stateHint());
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  expect(document.querySelector('[data-conversation-id="conv-river"]')).toHaveAttribute("aria-current", "location");
});

it("does not navigate or ACK when draft flushing fails", async () => {
  vi.spyOn(bridge, "save_draft").mockRejectedValue(new Error("disk unavailable"));
  render(<App />);
  const composer = await screen.findByRole("textbox", { name: "Message" });
  fireEvent.change(composer, { target: { value: "keep my draft" } });
  await clickBanner({ kind: "conversation", conversationId: "conv-river" });
  await waitFor(() => expect(bridge.save_draft).toHaveBeenCalled());
  expect(native.ack).not.toHaveBeenCalled();
  expect(composer).toHaveValue("keep my draft");
  expect(document.querySelector('[data-conversation-id="conv-aurora"]')).toHaveAttribute("aria-current", "location");
});

it("allows only the newest activation after a contact guard approves", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  fireEvent.click(screen.getByRole("button", { name: /^Contacts/ }));
  fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
  fireEvent.click(screen.getByLabelText("Edit contact"));
  fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Unsaved" } });
  await clickBanner({ kind: "conversation", conversationId: "conv-aurora" });
  await screen.findByRole("button", { name: "Keep editing" });
  expect(native.ack).not.toHaveBeenCalled();
  await clickBanner({ kind: "conversation", conversationId: "conv-river" });
  fireEvent.click(screen.getByRole("button", { name: "Discard" }));
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  expect(native.ack.mock.calls[0][0].requestId).toBe("click-2");
  expect(document.querySelector('[data-conversation-id="conv-river"]')).toHaveAttribute("aria-current", "location");
});

it("ACKs guard cancellation and does not replay it on remount", async () => {
  const view = render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  fireEvent.click(screen.getByRole("button", { name: /^Contacts/ }));
  fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
  fireEvent.click(screen.getByLabelText("Edit contact"));
  fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Unsaved" } });
  await clickBanner({ kind: "mirrored", target: snapshot.notifications[0].target });
  fireEvent.click(await screen.findByRole("button", { name: "Keep editing" }));
  expect(screen.getByLabelText("Given name")).toHaveValue("Unsaved");
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  expect(native.pending).toBeNull();
  view.unmount();
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  expect(document.querySelector('[data-view="notifications"]')).not.toBeInTheDocument();
});

it("drops an older delayed refresh when a newer banner arrives", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  let release!: (value: DesktopSnapshot) => void;
  const delayed = new Promise<DesktopSnapshot>(resolve => { release = resolve; });
  vi.mocked(bridge.load_state).mockReturnValueOnce(delayed);
  await clickBanner({ kind: "conversation", conversationId: "conv-river" });
  await clickBanner({ kind: "mirrored", target: snapshot.notifications[0].target });
  await act(async () => release({ ...snapshot, activeConversationId: "conv-river" }));
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  expect(native.ack.mock.calls[0][0].requestId).toBe("click-2");
  expect(document.activeElement).toHaveAttribute("data-notification-key", snapshot.notifications[0].target.notificationKey);
  expect(document.querySelector('[data-conversation-id="conv-river"]')).not.toHaveAttribute("aria-current");
});

it("does not classify a failed refresh as a missing target and retries the hint", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  vi.mocked(bridge.load_state).mockRejectedValueOnce(new Error("offline"));
  await clickBanner({ kind: "conversation", conversationId: "conv-river" });
  expect(native.ack).not.toHaveBeenCalled();
  await act(async () => native.hint());
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  expect(document.querySelector('[data-conversation-id="conv-river"]')).toHaveAttribute("aria-current", "location");
});

it("lands on the conversation list when the authoritative target is missing", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  await clickBanner({ kind: "conversation", conversationId: "gone" });
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  expect(document.querySelector('[data-conversation-id][aria-current="location"]')).toBeNull();
  expect(document.querySelector("#app-notice-banner")).toBeInTheDocument();
});

it("leaves the current destination intact for a summary banner", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  fireEvent.click(screen.getByRole("button", { name: /^Notifications/ }));
  await clickBanner({ kind: "main" });
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  expect(document.querySelector('[data-view="notifications"]')).toBeInTheDocument();
});

it("does not consume native activations in the browser host", async () => {
  native.hint = () => {};
  render(<App hostKind="browser" />);
  await screen.findByRole("textbox", { name: "Message" });
  await clickBanner({ kind: "conversation", conversationId: "conv-river" });
  expect(native.ack).not.toHaveBeenCalled();
  expect(document.querySelector('[data-conversation-id="conv-aurora"]')).toHaveAttribute("aria-current", "location");
});

it("drops an unlocked response that finishes after the session locks", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  const beforeLock = structuredClone(snapshot);
  let release!: (value: DesktopSnapshot) => void;
  vi.mocked(bridge.load_state).mockReturnValueOnce(new Promise(resolve => { release = resolve; }));
  await clickBanner({ kind: "conversation", conversationId: "conv-river" });
  snapshot.encryption.state = "locked";
  await act(async () => stateHint());
  await waitFor(() => expect(screen.getByRole("combobox", { name: "Search Peppy" })).toBeDisabled());
  await act(async () => release(beforeLock));
  expect(native.ack).not.toHaveBeenCalled();
  expect(document.querySelector('[data-view="notifications"]')).not.toBeInTheDocument();
  snapshot.encryption.state = "unlocked";
  await act(async () => stateHint());
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  expect(document.querySelector('[data-conversation-id="conv-river"]')).toHaveAttribute("aria-current", "location");
});

it("acknowledges a mirrored destination after the exact row is focused", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  await clickBanner({ kind: "mirrored", target: snapshot.notifications[0].target });
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  expect(document.activeElement).toHaveAttribute("data-notification-key", snapshot.notifications[0].target.notificationKey);
});

it("does not replay handled focus after leaving and returning to Notifications", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  await clickBanner({ kind: "mirrored", target: snapshot.notifications[0].target });
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
  fireEvent.click(screen.getByRole("button", { name: /^Notifications/ }));
  expect(document.activeElement).not.toHaveAttribute("data-notification-target");
});

it("preserves the selected non-first conversation through mirrored activation", async () => {
  snapshot.conversations.find(item => item.id === "conv-river")!.messages = [{ id: "message-river-1", revision: "1", sender: "other", body: "River message", timestamp: "1788435180000", attachments: [] }];
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  fireEvent.click(document.querySelector('[data-conversation-id="conv-river"]')!);
  await waitFor(() => expect(screen.getByRole("textbox", { name: "Message" })).toHaveAttribute("placeholder", "Message River Park"));
  await clickBanner({ kind: "mirrored", target: snapshot.notifications[0].target });
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
  expect(screen.getByRole("textbox", { name: "Message" })).toHaveAttribute("placeholder", "Message River Park");
  expect(document.querySelector('[data-message-id="message-river-1"]')).toBeInTheDocument();
});

it.each(["contacts", "conversation", "search"])("does not override later user %s navigation after a delayed load", async destination => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  let release!: (value: DesktopSnapshot) => void;
  vi.mocked(bridge.load_state).mockReturnValueOnce(new Promise(resolve => { release = resolve; }));
  await clickBanner({ kind: "conversation", conversationId: "conv-river" });
  if (destination === "contacts") {
    fireEvent.click(screen.getByRole("button", { name: /^Contacts/ }));
    fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
    fireEvent.click(screen.getByLabelText("Edit contact"));
    fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Unsaved" } });
  } else if (destination === "search") {
    vi.spyOn(bridge, "search_local").mockImplementation(async ({ query }) => ({ query, items: [{ kind: "conversation", conversationId: "conv-short-code", title: "5556", subtitle: "Short code" }], indexState: "ready", hasMore: { conversation: false, message: false, contact: false } }));
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "5556" } });
    await waitFor(() => expect(document.querySelector("[data-search-hit]")).not.toBeNull());
    fireEvent.click(document.querySelector("[data-search-hit]")!);
  } else fireEvent.click(document.querySelector('[data-conversation-id="conv-short-code"]')!);
  await act(async () => release({ ...snapshot, activeConversationId: "conv-river" }));
  if (destination === "contacts") expect(screen.getByLabelText("Given name")).toHaveValue("Unsaved");
  else expect(document.querySelector('[data-conversation-id="conv-short-code"]')).toHaveAttribute("aria-current", "location");
});

it("reports unguarded load failure and retries after a successful data hint", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  vi.mocked(bridge.load_state).mockRejectedValueOnce(new Error("offline"));
  await clickBanner({ kind: "conversation", conversationId: "conv-river" });
  await waitFor(() => expect(document.querySelector("#app-notice-banner")).toHaveTextContent("offline"));
  expect(native.ack).not.toHaveBeenCalled();
  await act(async () => stateHint());
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
});

it("retries a guarded refresh failure after a data hint", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  fireEvent.click(screen.getByRole("button", { name: /^Contacts/ }));
  fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
  fireEvent.click(screen.getByLabelText("Edit contact"));
  fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Unsaved" } });
  await clickBanner({ kind: "conversation", conversationId: "conv-river" });
  vi.mocked(bridge.load_state).mockRejectedValueOnce(new Error("offline"));
  fireEvent.click(await screen.findByRole("button", { name: "Discard" }));
  await waitFor(() => expect(document.querySelector("#app-notice-banner")).toHaveTextContent("offline"));
  expect(native.ack).not.toHaveBeenCalled();
  await act(async () => stateHint());
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
});

it("retries failed draft flushing after recovery without losing the draft", async () => {
  const save = vi.spyOn(bridge, "save_draft").mockRejectedValue(new Error("disk unavailable"));
  render(<App />);
  const composer = await screen.findByRole("textbox", { name: "Message" });
  fireEvent.change(composer, { target: { value: "preserved draft" } });
  await clickBanner({ kind: "conversation", conversationId: "conv-river" });
  await waitFor(() => expect(document.querySelector("#app-notice-banner")).toHaveTextContent("disk unavailable"));
  expect(native.ack).not.toHaveBeenCalled();
  save.mockRestore();
  await act(async () => stateHint());
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  expect(document.querySelector('[data-conversation-id="conv-river"]')).toHaveAttribute("aria-current", "location");
});

it("preserves detached history when refreshing the mirrored feed", async () => {
  const target = { id: "old-target", revision: "1", sender: "other" as const, body: "Old target message", timestamp: "1788435180000", attachments: [] };
  vi.spyOn(bridge, "search_local").mockImplementation(async ({ query }) => ({ query, items: [{ kind: "message", conversationId: "conv-aurora", messageId: target.id, title: "Aurora", snippet: target.body, timestamp: target.timestamp }], indexState: "ready", hasMore: { conversation: false, message: false, contact: false } }));
  vi.spyOn(bridge, "load_message_window").mockResolvedValue({ conversationId: "conv-aurora", messages: [target], hasOlder: true, hasNewer: true });
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "old target" } });
  await waitFor(() => expect(document.querySelector("[data-search-hit]")).not.toBeNull());
  fireEvent.click(document.querySelector("[data-search-hit]")!);
  await screen.findByText(target.body);
  await clickBanner({ kind: "mirrored", target: snapshot.notifications[0].target });
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
  expect(screen.getByText(target.body)).toBeInTheDocument();
  expect(bridge.load_message_window).toHaveBeenLastCalledWith({ direction: "refresh", conversationId: "conv-aurora", messageIds: ["old-target"] });
});

it("keeps the acknowledged mirrored highlight until its 1600ms expiry", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  vi.useFakeTimers({ shouldAdvanceTime: true });
  try {
    await clickBanner({ kind: "mirrored", target: snapshot.notifications[0].target });
    await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
    const row = document.activeElement!;
    expect(row).toHaveAttribute("data-activation-highlight", "true");
    await act(async () => { feed.result?.("click-1", true); feed.result?.("click-1", true); });
    expect(native.ack).toHaveBeenCalledOnce();
    expect(row).toHaveAttribute("data-activation-highlight", "true");
    await act(async () => { await vi.advanceTimersByTimeAsync(500); });
    // Native emits the availability hint for ordinary STATE_EVENT updates too.
    await act(async () => { stateHint(); native.hint(); });
    expect(native.pending).toBeNull();
    expect(row).toHaveAttribute("data-activation-highlight", "true");
    await act(async () => { await vi.advanceTimersByTimeAsync(1_150); });
    expect(row).not.toHaveAttribute("data-activation-highlight");
    expect(native.ack).toHaveBeenCalledOnce();
  } finally {
    vi.useRealTimers();
  }
});

it("supersedes acknowledged mirrored focus only when the hint reveals a newer activation", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  await clickBanner({ kind: "mirrored", target: snapshot.notifications[0].target });
  await waitFor(() => expect(native.ack).toHaveBeenCalledOnce());
  const firstRow = document.activeElement!;
  const secondTarget = snapshot.notifications[1].target;
  await act(async () => {
    native.pending = { requestId: "next-click", sessionGeneration: "native-1", destination: { kind: "mirrored", target: secondTarget } };
    native.hint();
  });
  await waitFor(() => expect(native.ack).toHaveBeenCalledTimes(2));
  expect(firstRow).not.toHaveAttribute("data-activation-highlight");
  expect(document.activeElement).toHaveAttribute("data-notification-key", secondTarget.notificationKey);
  expect(document.activeElement).toHaveAttribute("data-activation-highlight", "true");
});

it("drops an old native peek when the current identity changes", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  let release!: (value: NotificationActivation | null) => void;
  vi.spyOn(nativeNotificationActivation, "peek").mockImplementationOnce(() => new Promise(resolve => { release = resolve; }));
  await clickBanner({ kind: "conversation", conversationId: "conv-river" });
  const obsolete = native.pending;
  native.pending = null;
  snapshot.encryption.profileFingerprint = "profile-b";
  await act(async () => stateHint());
  await act(async () => release(obsolete));
  expect(native.ack).not.toHaveBeenCalled();
  expect(document.querySelector('[data-conversation-id="conv-river"]')).not.toHaveAttribute("aria-current", "location");
});

it("respects a new dirty contact guard while an initially approved load is pending", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  fireEvent.click(screen.getByRole("button", { name: /^Contacts/ }));
  fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
  let release!: (value: DesktopSnapshot) => void;
  vi.mocked(bridge.load_state).mockReturnValueOnce(new Promise(resolve => { release = resolve; }));
  await clickBanner({ kind: "conversation", conversationId: "conv-river" });
  fireEvent.click(screen.getByLabelText("Edit contact"));
  fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "New edit" } });
  await act(async () => release({ ...snapshot, activeConversationId: "conv-river" }));
  expect(screen.getByLabelText("Given name")).toHaveValue("New edit");
});

it("preserves a post-Discard contact re-edit while the approved activation load is pending", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  fireEvent.click(screen.getByRole("button", { name: /^Contacts/ }));
  fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
  fireEvent.click(screen.getByLabelText("Edit contact"));
  fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "First edit" } });
  await clickBanner({ kind: "conversation", conversationId: "conv-river" });
  let release!: (value: DesktopSnapshot) => void;
  vi.mocked(bridge.load_state).mockReturnValueOnce(new Promise(resolve => { release = resolve; }));
  fireEvent.click(await screen.findByRole("button", { name: "Discard" }));
  await waitFor(() => expect(release).toBeDefined());
  fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Second edit" } });
  await act(async () => release({ ...snapshot, activeConversationId: "conv-river" }));
  expect(screen.getByLabelText("Given name")).toHaveValue("Second edit");
  expect(document.querySelector('[data-conversation-id="conv-river"]')).not.toHaveAttribute("aria-current", "location");
});

it("finishes a pending message search across a Notifications view switch", async () => {
  const target = { id: "delayed-search", revision: "1", sender: "other" as const, body: "Delayed search message", timestamp: "1788435180000", attachments: [] };
  vi.spyOn(bridge, "search_local").mockImplementation(async ({ query }) => ({ query, items: [{ kind: "message", conversationId: "conv-aurora", messageId: target.id, title: "Aurora", snippet: target.body, timestamp: target.timestamp }], indexState: "ready", hasMore: { conversation: false, message: false, contact: false } }));
  let release!: (value: Awaited<ReturnType<typeof bridge.load_message_window>>) => void;
  vi.spyOn(bridge, "load_message_window").mockReturnValue(new Promise(resolve => { release = resolve; }));
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "delayed" } });
  await waitFor(() => expect(document.querySelector("[data-search-hit]")).not.toBeNull());
  fireEvent.click(document.querySelector("[data-search-hit]")!);
  await waitFor(() => expect(bridge.load_message_window).toHaveBeenCalled());
  fireEvent.click(screen.getByRole("button", { name: /^Notifications/ }));
  await act(async () => release({ conversationId: "conv-aurora", messages: [target], hasOlder: true, hasNewer: true }));
  fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
  expect(screen.getByText(target.body)).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Go to latest" })).toBeEnabled();
});
