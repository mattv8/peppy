import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { GatewayView, MessageView, SendRecovery } from "@peppy/desktop-ui/bridge-contracts";
import { bridge } from "../bridge/bridge";
import { formatCopy, peppyCopy } from "../generated/peppyCopy";
import { SendAgainHost, type SendAgainHostProps } from "./SendAgainHost";
import { SendRecoveryNotice } from "./SendAgain";
import type { SendAgainRouteLabeler } from "./SendAgainRouteList";
import { formatComposerReason } from "@peppy/desktop-ui";
import { useContext } from "react";
import { SendAgainStoreContext } from "./SendAgain";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.useRealTimers();
  document.body.innerHTML = "";
});

const routeLabel: SendAgainRouteLabeler = (gateway) => ({ simLabel: gateway.simId ? "SIM 1" : undefined, statusLabel: gateway.settingUp ? "Setting up" : "Connected" });

const phone = (overrides: Partial<GatewayView> = {}): GatewayView => ({
  id: "pixel",
  name: "Pixel 8",
  simId: "sim-1",
  online: true,
  simulated: false,
  supportsSms: true,
  supportsMms: true,
  mmsContentVersion: 4,
  availability: "connected",
  ...overrides,
});

const galaxy = (overrides: Partial<GatewayView> = {}): GatewayView => phone({ id: "galaxy", name: "Galaxy S24", simId: "sim-2", ...overrides });

const original = (sendRecovery: SendRecovery = { kind: "not-sent", canSendAgain: true }, overrides: Partial<MessageView> = {}): MessageView => ({
  id: "m1",
  revision: "1",
  sender: "self",
  body: "Are you there?",
  timestamp: "1788434580000",
  status: "failed-before-submit",
  statusAt: "1788434640000",
  attachments: [],
  transport: "sms",
  sendRecovery,
  ...overrides,
});

type HostOptions = Partial<Pick<SendAgainHostProps, "gateways" | "serverOffline" | "onGoToLatest" | "writesPaused">> & { messages?: MessageView[] };

/** Renders the real flow: the host, the bubble notices, and the popover it opens. Rerender through the returned helper. */
function renderHost(initial: HostOptions = {}) {
  const state: HostOptions & { version: number } = { messages: [original()], gateways: [phone()], serverOffline: false, onGoToLatest: vi.fn(), version: 0, ...initial };
  const build = () => {
    const messages = state.messages ?? [];
    return <SendAgainHost conversation={{ id: "c1", name: "Ana", preview: "", unread: 0, messages }} gateways={state.gateways ?? []} routeLabel={routeLabel} serverOffline={state.serverOffline ?? false} writesPaused={state.writesPaused} onGoToLatest={state.onGoToLatest ?? (() => {})}>
      {messages.map((message) => <SendRecoveryNotice key={`${message.id}-${state.version}`} message={message} />)}
    </SendAgainHost>;
  };
  const view = render(build());
  return {
    ...view,
    update(next: HostOptions) {
      Object.assign(state, next);
      view.rerender(build());
    },
    remount() {
      state.version += 1;
      view.rerender(build());
    },
  };
}

const openSendAgain = (id = "m1") => fireEvent.click(document.getElementById(`message-send-again-${id}`)!);
it("disables send again while writes are paused", () => {
  const host = renderHost({ writesPaused: true });
  expect(document.getElementById("message-send-again-m1")).toBeDisabled();
  host.update({ writesPaused: false });
  expect(document.getElementById("message-send-again-m1")).toBeEnabled();
});
const popover = (id = "m1") => document.getElementById(`send-again-popover-${id}`);
const confirmButton = (id = "m1") => document.getElementById(`send-again-confirm-${id}`) as HTMLButtonElement;
const cancelButton = (id = "m1") => document.getElementById(`send-again-cancel-${id}`) as HTMLButtonElement;
const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((next, fail) => { resolve = next; reject = fail; });
  return { promise, resolve, reject };
};

describe("SendAgainPopover: surface and content", () => {
  it("is a non-modal dialog labelled by its title and body, and takes focus to the title", () => {
    renderHost();
    openSendAgain();
    const dialog = popover()!;
    expect(dialog).toHaveAttribute("role", "dialog");
    expect(dialog).toHaveAttribute("aria-modal", "false");
    expect(dialog).toHaveAttribute("aria-labelledby", "send-again-title-m1");
    expect(dialog).toHaveAttribute("aria-describedby", "send-again-body-m1");
    expect(document.getElementById("send-again-title-m1")).toHaveTextContent(peppyCopy.message_send_again_title);
    expect(document.getElementById("send-again-title-m1")).toHaveFocus();
  });

  it("uses the not-sent body, and the unconfirmed body for a removed-phone message", () => {
    renderHost();
    openSendAgain();
    expect(document.getElementById("send-again-body-m1")).toHaveTextContent(peppyCopy.message_send_again_body_not_sent);
    cleanup();
    renderHost({ messages: [original({ kind: "unconfirmed-removed", canSendAgain: true }, { status: "gateway-persisted" })] });
    openSendAgain();
    expect(document.getElementById("send-again-body-m1")).toHaveTextContent(peppyCopy.message_send_again_body_unconfirmed);
  });

  it("previews the first 120 characters by code point, never splitting an emoji, with the full text in title", () => {
    renderHost({ messages: [original({ kind: "not-sent", canSendAgain: true }, { body: "😀".repeat(130) })] });
    openSendAgain();
    const preview = document.querySelector("[data-send-again-preview]")!;
    expect(preview.querySelector("bdi")).toHaveAttribute("title", "😀".repeat(130));
    expect(preview.textContent).toBe(`${"😀".repeat(120)}…`);
  });

  it("omits the preview for an attachment-only message and states the attachment count", () => {
    const file = { id: "a1", name: "one.jpg", mediaType: "image/jpeg", byteSize: 1, state: "ready" as const };
    renderHost({ messages: [original({ kind: "not-sent", canSendAgain: true }, { body: "", transport: "mms", attachments: [file, { ...file, id: "a2" }, { ...file, id: "a3" }] })] });
    openSendAgain();
    expect(document.querySelector("[data-send-again-preview]")).toBeNull();
    expect(document.querySelector("[data-send-again-attachments]")).toHaveTextContent(formatCopy(peppyCopy.message_send_again_attachments_many, "3"));
  });

  it("states the recipient count when more than one recipient", () => {
    renderHost({ messages: [original({ kind: "not-sent", canSendAgain: true }, { transport: "mms", participants: ["+15550001", "+15550002"] })] });
    openSendAgain();
    expect(document.querySelector("[data-send-again-recipients]")).toHaveTextContent(peppyCopy.group_header_count.replace("{count}", "2"));
  });

  it("lists one eligible route as a single preselected read-only row without a radio, and confirm sends to it", async () => {
    const send = vi.spyOn(bridge, "send_again").mockResolvedValue({ messageId: "m1-sent-again" });
    renderHost();
    openSendAgain();
    expect(document.querySelector('[data-send-again-route="pixel:sim-1"]')).toHaveAttribute("data-send-again-route-state", "only");
    expect(document.querySelector('[data-send-again-route="pixel:sim-1"] input')).toBeNull();
    expect(confirmButton()).not.toHaveAttribute("aria-disabled", "true");
    fireEvent.click(confirmButton());
    await waitFor(() => expect(send).toHaveBeenCalledWith({ messageId: "m1", gatewayId: "pixel", simId: "sim-1" }));
  });

  it("lists many eligible routes as radios with none preselected, and blocks confirm until one is chosen", async () => {
    const send = vi.spyOn(bridge, "send_again").mockResolvedValue({ messageId: "m1-sent-again" });
    renderHost({ gateways: [phone(), galaxy()] });
    openSendAgain();
    const radios = Array.from(document.querySelectorAll<HTMLInputElement>('input[type="radio"]'));
    expect(radios).toHaveLength(2);
    expect(radios.every((radio) => !radio.checked)).toBe(true);
    expect(confirmButton()).toHaveAttribute("aria-disabled", "true");
    expect(confirmButton()).toHaveAttribute("aria-describedby", "send-again-hint-m1");
    expect(document.querySelector("[data-send-again-hint]")).toHaveTextContent(peppyCopy.message_send_again_route_choose);
    fireEvent.click(confirmButton());
    expect(send).not.toHaveBeenCalled();

    fireEvent.click(document.querySelector<HTMLInputElement>('[data-send-again-route="galaxy:sim-2"] input')!);
    expect(confirmButton()).not.toHaveAttribute("aria-disabled", "true");
    fireEvent.click(confirmButton());
    await waitFor(() => expect(send).toHaveBeenCalledWith({ messageId: "m1", gatewayId: "galaxy", simId: "sim-2" }));
  });

  it("shows the no-route state with each phone's reason as a sentence, and a described, blocked confirm", () => {
    renderHost({ gateways: [phone({ supportsSms: false, supportsMms: false })] });
    openSendAgain();
    const none = document.querySelector("[data-send-again-none]")!;
    expect(none).toHaveTextContent(peppyCopy.message_send_again_none);
    expect(none).toHaveTextContent(peppyCopy.message_send_again_none_hint);
    expect(none.querySelector('[data-send-again-reason="pixel:sim-1"]')).toHaveTextContent(formatComposerReason("this phone/SIM does not support SMS"));
    expect(none.querySelector('[data-send-again-reason="pixel:sim-1"] [data-route-badge="device"]')).toHaveTextContent("Pixel 8");
    expect(confirmButton()).toHaveAttribute("aria-disabled", "true");
    expect(confirmButton()).toHaveAttribute("aria-describedby", "send-again-none-m1");
  });

  it("lists a setting-up phone disabled, with the plain reason (never the choose-another variant)", () => {
    renderHost({ gateways: [phone(), galaxy({ supportsSms: false, supportsMms: false, availability: "setting_up", settingUp: true, simId: "" })] });
    openSendAgain();
    const radio = document.querySelector<HTMLInputElement>('[data-send-again-route="galaxy:"] input')!;
    expect(radio).toBeDisabled();
    expect(radio).toHaveAccessibleDescription(formatComposerReason(formatCopy(peppyCopy.gateway_problem_setting_up, "Galaxy S24")));
  });
});

describe("SendAgainPopover: selection rules", () => {
  it("clears a chosen route that stops being eligible and never moves the choice to another phone", () => {
    const send = vi.spyOn(bridge, "send_again").mockResolvedValue({ messageId: "x" });
    const { update } = renderHost({ gateways: [phone(), galaxy()] });
    openSendAgain();
    fireEvent.click(document.querySelector<HTMLInputElement>('[data-send-again-route="pixel:sim-1"] input')!);
    expect(confirmButton()).not.toHaveAttribute("aria-disabled", "true");
    update({ gateways: [phone({ availability: "offline" }), galaxy()] });
    expect(document.querySelector<HTMLInputElement>('[data-send-again-route="pixel:sim-1"] input')).toBeDisabled();
    expect(document.querySelector<HTMLInputElement>('[data-send-again-route="galaxy:sim-2"] input')).not.toBeChecked();
    expect(confirmButton()).toHaveAttribute("aria-disabled", "true");
    fireEvent.click(confirmButton());
    expect(send).not.toHaveBeenCalled();
  });

  it("a sole preselected route that goes unavailable and returns is a selectable radio, never re-selected, and sends once", async () => {
    const send = vi.spyOn(bridge, "send_again").mockResolvedValue({ messageId: "m1-sent-again" });
    const { update } = renderHost({ gateways: [phone()] });
    openSendAgain();
    // 1. Opened with exactly one eligible route: preselected, read-only, confirm enabled.
    expect(document.querySelector('[data-send-again-route="pixel:sim-1"]')).toHaveAttribute("data-send-again-route-state", "only");
    expect(confirmButton()).not.toHaveAttribute("aria-disabled", "true");

    // 2. Unavailable: the selection is cleared, confirm is blocked, and the reason is shown.
    update({ gateways: [phone({ availability: "offline" })] });
    expect(document.querySelector("[data-send-again-none]")).toBeInTheDocument();
    expect(document.querySelector('[data-send-again-reason="pixel:sim-1"]')).toHaveTextContent("isn't responding");
    expect(confirmButton()).toHaveAttribute("aria-disabled", "true");

    // 3. Eligible again: a selectable radio, nothing selected, confirm still blocked.
    update({ gateways: [phone()] });
    expect(document.querySelector('[data-send-again-route="pixel:sim-1"]')).toHaveAttribute("data-send-again-route-state", "eligible");
    const radio = document.querySelector<HTMLInputElement>('[data-send-again-route="pixel:sim-1"] input')!;
    expect(radio).not.toBeDisabled();
    expect(radio).not.toBeChecked();
    expect(confirmButton()).toHaveAttribute("aria-disabled", "true");
    expect(send).not.toHaveBeenCalled();

    // 4. The user chooses it: it stays a radio (now checked) and confirm sends through it exactly once.
    fireEvent.click(radio);
    expect(document.querySelector<HTMLInputElement>('[data-send-again-route="pixel:sim-1"] input')).toBeChecked();
    expect(document.querySelector('[data-send-again-route="pixel:sim-1"]')).toHaveAttribute("data-send-again-route-state", "eligible");
    expect(confirmButton()).not.toHaveAttribute("aria-disabled", "true");
    fireEvent.click(confirmButton());
    await waitFor(() => expect(popover()).toBeNull());
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith({ messageId: "m1", gatewayId: "pixel", simId: "sim-1" });
  });

  it("moves a preselected single route to the no-route state when it stops being eligible", () => {
    const send = vi.spyOn(bridge, "send_again").mockResolvedValue({ messageId: "x" });
    const { update } = renderHost({ gateways: [phone()] });
    openSendAgain();
    update({ gateways: [phone({ availability: "offline" })] });
    expect(document.querySelector("[data-send-again-none]")).toBeInTheDocument();
    expect(confirmButton()).toHaveAttribute("aria-disabled", "true");
    fireEvent.click(confirmButton());
    expect(send).not.toHaveBeenCalled();
  });

  it("does not preselect a route when the popover opened with two eligible routes, even if one drops out", () => {
    const { update } = renderHost({ gateways: [phone(), galaxy()] });
    openSendAgain();
    update({ gateways: [phone(), galaxy({ availability: "offline" })] });
    expect(document.querySelector<HTMLInputElement>('[data-send-again-route="pixel:sim-1"] input')).not.toBeChecked();
    expect(document.querySelector('[data-send-again-route="pixel:sim-1"]')).toHaveAttribute("data-send-again-route-state", "eligible");
    expect(confirmButton()).toHaveAttribute("aria-disabled", "true");
  });

});

describe("SendAgainPopover: errors and blocks", () => {
  it("does not claim the original is unchanged when the call outcome is unknown", async () => {
    vi.spyOn(bridge, "send_again").mockRejectedValueOnce(new Error("connection lost"));
    renderHost();
    openSendAgain();
    fireEvent.click(confirmButton());
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't confirm it was sent again. Check the conversation before trying again.");
    expect(confirmButton()).not.toHaveAttribute("aria-disabled", "true");
  });
  it.each([
    ["an unrecognised code", "unexpected", peppyCopy.message_send_again_unconfirmed_call],
    ["offline at call time", "offline", peppyCopy.message_send_again_offline],
  ])("shows the copy for %s and keeps the popover open", async (_label, code, text) => {
    vi.spyOn(bridge, "send_again").mockRejectedValueOnce({ code, message: "raw" });
    renderHost();
    openSendAgain();
    fireEvent.click(confirmButton());
    expect(await screen.findByRole("alert")).toHaveTextContent(text);
    expect(popover()).toBeInTheDocument();
  });

  it("keeps confirm usable after an offline failure that happened at call time (not latched)", async () => {
    const send = vi.spyOn(bridge, "send_again").mockRejectedValueOnce({ code: "offline", message: "raw" }).mockResolvedValueOnce({ messageId: "m1-sent-again" });
    renderHost();
    openSendAgain();
    fireEvent.click(confirmButton());
    await screen.findByRole("alert");
    expect(confirmButton()).not.toHaveAttribute("aria-disabled", "true");
    fireEvent.click(confirmButton());
    await waitFor(() => expect(send).toHaveBeenCalledTimes(2));
  });

  it("blocks confirm and shows the offline copy while the server is unreachable, and clears when it returns", () => {
    const { update } = renderHost({ serverOffline: true });
    openSendAgain();
    expect(screen.getByRole("alert")).toHaveTextContent(peppyCopy.message_send_again_offline);
    expect(confirmButton()).toHaveAttribute("aria-disabled", "true");
    expect(confirmButton()).toHaveAttribute("aria-describedby", "send-again-error-m1");
    update({ serverOffline: false });
    expect(confirmButton()).not.toHaveAttribute("aria-disabled", "true");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("clears the route selection on route-unavailable and marks that phone unavailable with its reason", async () => {
    const send = vi.spyOn(bridge, "send_again").mockRejectedValueOnce({ code: "route-unavailable", message: "raw" });
    renderHost({ gateways: [phone(), galaxy()] });
    openSendAgain();
    fireEvent.click(document.querySelector<HTMLInputElement>('[data-send-again-route="pixel:sim-1"] input')!);
    fireEvent.click(confirmButton());
    expect(await screen.findByRole("alert")).toHaveTextContent(peppyCopy.message_send_again_route_gone);
    expect(document.querySelector('[data-send-again-route="pixel:sim-1"]')).toHaveAttribute("data-send-again-route-state", "unavailable");
    expect(document.querySelector<HTMLInputElement>('[data-send-again-route="pixel:sim-1"] input')).toBeDisabled();
    expect(document.querySelector<HTMLInputElement>('[data-send-again-route="galaxy:sim-2"] input')).not.toBeChecked();
    expect(confirmButton()).toHaveAttribute("aria-disabled", "true");
    expect(send).toHaveBeenCalledTimes(1);
  });

  it("moves a single-route popover to the no-route state after route-unavailable instead of looping on the same phone", async () => {
    const send = vi.spyOn(bridge, "send_again").mockRejectedValue({ code: "route-unavailable", message: "raw" });
    renderHost({ gateways: [phone()] });
    openSendAgain();
    fireEvent.click(confirmButton());
    await waitFor(() => expect(document.querySelector("[data-send-again-none]")).toBeInTheDocument());
    expect(confirmButton()).toHaveAttribute("aria-disabled", "true");
    fireEvent.click(confirmButton());
    expect(send).toHaveBeenCalledTimes(1);
  });

  it("keeps confirm blocked for attachments-unavailable whatever route is chosen afterwards", async () => {
    vi.spyOn(bridge, "send_again").mockRejectedValueOnce({ code: "attachments-unavailable", message: "raw" });
    renderHost({ gateways: [phone(), galaxy()] });
    openSendAgain();
    fireEvent.click(document.querySelector<HTMLInputElement>('[data-send-again-route="pixel:sim-1"] input')!);
    fireEvent.click(confirmButton());
    expect(await screen.findByRole("alert")).toHaveTextContent(peppyCopy.message_send_again_attachments_missing);
    fireEvent.click(document.querySelector<HTMLInputElement>('[data-send-again-route="galaxy:sim-2"] input')!);
    expect(confirmButton()).toHaveAttribute("aria-disabled", "true");
  });

  it("pre-blocks a message whose attachments are not on this device, before any call", () => {
    const send = vi.spyOn(bridge, "send_again");
    renderHost({ messages: [original({ kind: "not-sent", canSendAgain: true }, { transport: "mms", attachments: [{ id: "a1", name: "x.jpg", mediaType: "image/jpeg", byteSize: 1, state: "ready", localAvailable: false }] })] });
    openSendAgain();
    expect(screen.getByRole("alert")).toHaveTextContent(peppyCopy.message_send_again_attachments_missing);
    expect(confirmButton()).toHaveAttribute("aria-disabled", "true");
    fireEvent.click(confirmButton());
    expect(send).not.toHaveBeenCalled();
  });

  it("moves focus back to confirm after a failure, once the button is enabled again (I5)", async () => {
    vi.spyOn(bridge, "send_again").mockRejectedValueOnce({ code: "unexpected", message: "raw" });
    renderHost();
    openSendAgain();
    fireEvent.click(confirmButton());
    await screen.findByRole("alert");
    await waitFor(() => expect(confirmButton()).toHaveFocus());
  });

  it("closes without an error when this device already holds its copy (already-sent-again)", async () => {
    vi.spyOn(bridge, "send_again").mockRejectedValueOnce({ code: "already-sent-again", message: "raw" });
    renderHost();
    openSendAgain();
    fireEvent.click(confirmButton());
    await waitFor(() => expect(popover()).toBeNull());
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("shows the sent-again row at once after success, before any snapshot confirms it (I4)", async () => {
    vi.spyOn(bridge, "send_again").mockResolvedValue({ messageId: "m1-sent-again" });
    renderHost();
    openSendAgain();
    fireEvent.click(confirmButton());
    await waitFor(() => expect(popover()).toBeNull());
    expect(document.querySelector('[data-message-send-again-done="m1"]')).toBeInTheDocument();
    expect(document.getElementById("message-send-again-m1")).toBeNull();
  });
});

describe("SendAgainPopover: pending, in-flight guard and host", () => {
  it("clears the transient link after a top-level sentAgain snapshot", async () => {
    vi.spyOn(bridge, "send_again").mockResolvedValue({ messageId: "copy" });
    const Probe = () => {
      const store = useContext(SendAgainStoreContext);
      return <output data-testid="transient-link">{store?.sentAgainFor("m1") ?? ""}</output>;
    };
    const build = (message: MessageView) => <SendAgainHost conversation={{ id: "c", name: "Ana", preview: "", unread: 0, messages: [message] }} gateways={[phone()]} routeLabel={routeLabel} serverOffline={false} onGoToLatest={() => {}}>
      <SendRecoveryNotice message={message} /><Probe />
    </SendAgainHost>;
    const view = render(build(original()));
    openSendAgain();
    fireEvent.click(confirmButton());
    await waitFor(() => expect(screen.getByTestId("transient-link")).toHaveTextContent("copy"));
    view.rerender(build(original(undefined, { sentAgain: { messageId: "copy" } })));
    await waitFor(() => expect(screen.getByTestId("transient-link")).toBeEmptyDOMElement());
  });
  it("locks the dialog while the send is pending: Escape, Cancel and outside presses are ignored, and both buttons are disabled", async () => {
    const pending = deferred<{ messageId: string }>();
    vi.spyOn(bridge, "send_again").mockReturnValueOnce(pending.promise);
    renderHost();
    openSendAgain();
    fireEvent.click(confirmButton());
    expect(popover()).toHaveAttribute("aria-busy", "true");
    expect(confirmButton()).toBeDisabled();
    expect(confirmButton()).toHaveTextContent(peppyCopy.message_send_again_pending);
    expect(cancelButton()).toBeDisabled();
    fireEvent.keyDown(popover()!, { key: "Escape" });
    fireEvent.pointerDown(document.body);
    expect(popover()).toBeInTheDocument();
    await act(async () => pending.resolve({ messageId: "m1-sent-again" }));
    await waitFor(() => expect(popover()).toBeNull());
  });

  it("creates one message when confirm is activated twice, with the button forced enabled so the in-flight guard is what holds", () => {
    const send = vi.spyOn(bridge, "send_again").mockReturnValue(new Promise(() => {}));
    renderHost();
    openSendAgain();
    fireEvent.click(confirmButton());
    confirmButton().disabled = false;
    fireEvent.click(confirmButton());
    expect(send).toHaveBeenCalledTimes(1);
  });

  it("does not open another message's popover while a call is in flight", () => {
    vi.spyOn(bridge, "send_again").mockReturnValue(new Promise(() => {}));
    renderHost({ messages: [original(), original({ kind: "not-sent", canSendAgain: true }, { id: "m2", body: "Second" })] });
    openSendAgain("m1");
    fireEvent.click(confirmButton("m1"));
    openSendAgain("m2");
    expect(popover("m2")).toBeNull();
    expect(popover("m1")).toBeInTheDocument();
  });

  it("keeps the popover while a call is pending even when its message leaves the conversation, then closes on settle", async () => {
    const pending = deferred<{ messageId: string }>();
    vi.spyOn(bridge, "send_again").mockReturnValueOnce(pending.promise);
    const { update } = renderHost();
    openSendAgain();
    fireEvent.click(confirmButton());
    update({ messages: [] });
    expect(popover()).toBeInTheDocument();
    await act(async () => pending.resolve({ messageId: "m1-sent-again" }));
    await waitFor(() => expect(popover()).toBeNull());
  });

  it("keeps the last error for a message after the popover closes, and shows it again on reopen", async () => {
    vi.spyOn(bridge, "send_again").mockRejectedValueOnce({ code: "unexpected", message: "raw" });
    renderHost();
    openSendAgain();
    fireEvent.click(confirmButton());
    await screen.findByRole("alert");
    fireEvent.click(cancelButton());
    expect(popover()).toBeNull();
    openSendAgain();
    expect(screen.getByRole("alert")).toHaveTextContent(peppyCopy.message_send_again_unconfirmed_call);
  });

  it("toggles closed when the Send again button is pressed while its popover is open", () => {
    renderHost();
    openSendAgain();
    expect(popover()).toBeInTheDocument();
    openSendAgain();
    expect(popover()).toBeNull();
  });

  it("shows the sent-again row, not the button, after its popover closes for a sentAgain the local snapshot carries", () => {
    const { update } = renderHost();
    openSendAgain();
    update({ messages: [original({ kind: "not-sent", canSendAgain: false, sentAgain: { messageId: "m1-sent-again" } })] });
    expect(popover()).toBeNull();
    expect(document.querySelector('[data-message-send-again-done="m1"]')).toBeInTheDocument();
  });
});

describe("SendAgainPopover: close, focus and keyboard", () => {
  it("closes on Escape and returns focus to the Send again button", () => {
    renderHost();
    openSendAgain();
    fireEvent.keyDown(popover()!, { key: "Escape" });
    expect(popover()).toBeNull();
    expect(document.getElementById("message-send-again-m1")).toHaveFocus();
  });

  it("returns focus to the opener found at dismiss time, not the element that opened it", () => {
    const { remount } = renderHost();
    openSendAgain();
    remount();
    expect(popover()).toBeInTheDocument();
    fireEvent.keyDown(popover()!, { key: "Escape" });
    expect(document.getElementById("message-send-again-m1")).toHaveFocus();
  });

  it("closes on an outside press without taking focus back from the page", () => {
    renderHost();
    openSendAgain();
    const elsewhere = document.createElement("button");
    document.body.appendChild(elsewhere);
    elsewhere.focus();
    fireEvent.pointerDown(elsewhere);
    expect(popover()).toBeNull();
    expect(elsewhere).toHaveFocus();
  });

  it("does not close when the Send again button itself is pressed (the host toggles it instead)", () => {
    renderHost();
    openSendAgain();
    fireEvent.pointerDown(document.getElementById("message-send-again-m1")!);
    expect(popover()).toBeInTheDocument();
  });

  it("closes on Tab past the last control and returns focus to the opener", () => {
    renderHost();
    openSendAgain();
    fireEvent.keyDown(confirmButton(), { key: "Tab" });
    expect(popover()).toBeNull();
    expect(document.getElementById("message-send-again-m1")).toHaveFocus();
  });

  it("closes on Shift+Tab from the first control and returns focus to the opener", () => {
    renderHost({ gateways: [phone(), galaxy()] });
    openSendAgain();
    const first = document.querySelector<HTMLInputElement>('[data-send-again-route="pixel:sim-1"] input')!;
    fireEvent.keyDown(first, { key: "Tab", shiftKey: true });
    expect(popover()).toBeNull();
    expect(document.getElementById("message-send-again-m1")).toHaveFocus();
  });

  it("moves focus to the done row after success when focus was inside the popover", async () => {
    vi.spyOn(bridge, "send_again").mockResolvedValue({ messageId: "m1-sent-again" });
    renderHost();
    openSendAgain();
    fireEvent.click(confirmButton());
    await waitFor(() => expect(popover()).toBeNull());
    await waitFor(() => expect(document.querySelector('[data-message-send-again-done="m1"]')).toHaveFocus());
  });

  it("does not steal focus after success when the user had moved elsewhere", async () => {
    const pending = deferred<{ messageId: string }>();
    vi.spyOn(bridge, "send_again").mockReturnValueOnce(pending.promise);
    renderHost();
    openSendAgain();
    fireEvent.click(confirmButton());
    const elsewhere = document.createElement("input");
    document.body.appendChild(elsewhere);
    elsewhere.focus();
    await act(async () => pending.resolve({ messageId: "m1-sent-again" }));
    await waitFor(() => expect(popover()).toBeNull());
    expect(elsewhere).toHaveFocus();
  });

  it("closes when this device's snapshot carries sentAgain while the popover is open, and does not steal focus from the page", () => {
    const { update } = renderHost();
    openSendAgain();
    const elsewhere = document.createElement("input");
    document.body.appendChild(elsewhere);
    elsewhere.focus();
    update({ messages: [original({ kind: "not-sent", canSendAgain: false, sentAgain: { messageId: "m1-sent-again" } })] });
    expect(popover()).toBeNull();
    expect(elsewhere).toHaveFocus();
  });

  it("closes when canSendAgain stops being true while the popover is open (I3)", () => {
    const { update } = renderHost();
    openSendAgain();
    update({ messages: [original({ kind: "unconfirmed-removed-cancel", canSendAgain: false })] });
    expect(popover()).toBeNull();
  });

  it("forgets a done-row focus request after its 3 second window, so a later row mount does not steal focus", async () => {
    vi.useFakeTimers();
    const pending = deferred<{ messageId: string }>();
    vi.spyOn(bridge, "send_again").mockReturnValueOnce(pending.promise);
    const { update } = renderHost();
    openSendAgain();
    fireEvent.click(confirmButton());
    // The bubble is gone while the call runs (the list changed), so nothing consumes the request at once.
    update({ messages: [] });
    await act(async () => pending.resolve({ messageId: "m1-sent-again" }));
    expect(popover()).toBeNull();
    await act(async () => { await vi.advanceTimersByTimeAsync(3500); });
    const elsewhere = document.createElement("input");
    document.body.appendChild(elsewhere);
    elsewhere.focus();
    update({ messages: [original({ kind: "not-sent", canSendAgain: false, sentAgain: { messageId: "m1-sent-again" } })] });
    expect(document.querySelector('[data-message-send-again-done="m1"]')).toBeInTheDocument();
    expect(elsewhere).toHaveFocus();
  });
});

describe("SendAgainPopover: placement", () => {
  const anchorRect = (top: number, bottom: number) => ({ top, bottom, left: 40, right: 120, width: 80, height: bottom - top, x: 40, y: top, toJSON: () => ({}) });

  it("repositions when the window resizes", () => {
    renderHost();
    const opener = document.getElementById("message-send-again-m1")!;
    const spy = vi.spyOn(opener, "getBoundingClientRect").mockReturnValue(anchorRect(400, 420) as DOMRect);
    openSendAgain();
    expect(popover()).toHaveStyle({ bottom: `${window.innerHeight - 400 + 6}px` });
    spy.mockReturnValue(anchorRect(200, 220) as DOMRect);
    fireEvent(window, new Event("resize"));
    expect(popover()).toHaveStyle({ top: "226px" });
  });

  it("repositions when something outside the popover scrolls, and ignores scrolling inside it", () => {
    renderHost();
    const opener = document.getElementById("message-send-again-m1")!;
    const spy = vi.spyOn(opener, "getBoundingClientRect").mockReturnValue(anchorRect(400, 420) as DOMRect);
    openSendAgain();
    const above400 = `${window.innerHeight - 400 + 6}px`;
    expect(popover()).toHaveStyle({ bottom: above400 });
    spy.mockReturnValue(anchorRect(300, 320) as DOMRect);
    fireEvent.scroll(popover()!);
    expect(popover()).toHaveStyle({ bottom: above400 });
    fireEvent.scroll(document);
    expect(popover()).toHaveStyle({ bottom: `${window.innerHeight - 300 + 6}px` });
    spy.mockReturnValue(anchorRect(100, 120) as DOMRect);
    fireEvent.scroll(document);
    expect(popover()).toHaveStyle({ top: "126px" });
  });

  it("closes without moving focus when the opener scrolls out of the viewport", () => {
    renderHost();
    const opener = document.getElementById("message-send-again-m1")!;
    const spy = vi.spyOn(opener, "getBoundingClientRect").mockReturnValue(anchorRect(400, 420) as DOMRect);
    openSendAgain();
    spy.mockReturnValue(anchorRect(-200, -180) as DOMRect);
    fireEvent(window, new Event("resize"));
    expect(popover()).toBeNull();
  });
});

describe("SendAgainPopover: done row and Show", () => {
  it("Show finds an attempt represented by a grouped row", () => {
    const onGoToLatest = vi.fn();
    renderHost({ messages: [original({ kind: "not-sent", canSendAgain: false, sentAgain: { messageId: "hidden-copy" } })], onGoToLatest });
    const row = document.createElement("article");
    row.dataset.resendAttempts = "hidden-copy latest";
    document.body.appendChild(row);
    fireEvent.click(screen.getByRole("button", { name: peppyCopy.message_send_again_show_label }));
    expect(row).toHaveFocus();
    expect(onGoToLatest).not.toHaveBeenCalled();
  });
  it("Show focuses the new message row when it is rendered", () => {
    const onGoToLatest = vi.fn();
    renderHost({ messages: [original({ kind: "not-sent", canSendAgain: false, sentAgain: { messageId: "m1-sent-again" } })], onGoToLatest });
    const row = document.createElement("article");
    row.setAttribute("data-message-id", "m1-sent-again");
    row.tabIndex = -1;
    document.body.appendChild(row);
    fireEvent.click(screen.getByRole("button", { name: peppyCopy.message_send_again_show_label }));
    expect(row).toHaveFocus();
    expect(onGoToLatest).not.toHaveBeenCalled();
  });

  it("Show scrolls to the live tail when the new row is not rendered (the jump-to-latest mechanism)", () => {
    const onGoToLatest = vi.fn();
    renderHost({ messages: [original({ kind: "not-sent", canSendAgain: false, sentAgain: { messageId: "m1-sent-again" } })], onGoToLatest });
    fireEvent.click(screen.getByRole("button", { name: peppyCopy.message_send_again_show_label }));
    expect(onGoToLatest).toHaveBeenCalledTimes(1);
  });
});
