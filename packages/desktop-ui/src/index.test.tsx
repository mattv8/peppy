import "@testing-library/jest-dom/vitest";
import type { ComponentProps } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { PairingStatus } from "./PairPhone";
import {
  AppTitlebar,
  Composer,
  installOverlayScrollbars,
  isRecipientPosition,
  NavButtons,
  Panel,
  RecipientPanel,
  RecipientPicker,
  ResizeHandle,
  StatusPopover,
  ConnectionDot,
  PairPhone,
  pairingQrPayload,
} from "./index";

const people = [{ id: "conv-a", name: "Aurora", preview: "Hi", unread: 0 }];
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("StatusPopover", () => {
  it("opens after pointer entry and closes after pointer departure", () => {
    vi.useFakeTimers();
    render(<StatusPopover tone="warning" summary="Connection: offline"><p>Status detail</p></StatusPopover>);
    const trigger = screen.getByRole("button", { name: "Status: Connection: offline" });
    fireEvent.pointerEnter(trigger);
    act(() => vi.advanceTimersByTime(120));
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    fireEvent.pointerLeave(trigger);
    act(() => vi.advanceTimersByTime(200));
    expect(trigger).toHaveAttribute("aria-expanded", "false");
  });

  it("opens on focus, pins on click, and dismisses on Escape without stale reopening", () => {
    render(<StatusPopover tone="error" summary="Connection: unavailable"><p>Status detail</p></StatusPopover>);
    const trigger = screen.getByRole("button", { name: "Status: Connection: unavailable" });
    act(() => trigger.focus());
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    fireEvent.click(trigger);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    expect(trigger).toHaveFocus();
  });

  it("reopens when focus re-enters after Escape dismissal", () => {
    render(<><StatusPopover tone="error" summary="Connection: unavailable"><p>Status detail</p></StatusPopover><button type="button">Elsewhere</button></>);
    const trigger = screen.getByRole("button", { name: "Status: Connection: unavailable" });
    fireEvent.focus(trigger);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    fireEvent.focus(screen.getByRole("button", { name: "Elsewhere" }));
    fireEvent.focus(trigger);
    expect(trigger).toHaveAttribute("aria-expanded", "true");
  });

  it("retains a click-pinned card after its scheduled hover close elapses", () => {
    vi.useFakeTimers();
    render(<StatusPopover tone="neutral" summary="Connection: offline"><p>Status detail</p></StatusPopover>);
    const trigger = screen.getByRole("button", { name: "Status: Connection: offline" });
    fireEvent.pointerEnter(trigger);
    act(() => vi.advanceTimersByTime(120));
    fireEvent.pointerLeave(trigger);
    fireEvent.click(trigger);
    act(() => vi.advanceTimersByTime(200));
    expect(trigger).toHaveAttribute("aria-expanded", "true");
  });

  it("pins open after dismissing a pending hover open", () => {
    vi.useFakeTimers();
    render(<StatusPopover tone="neutral" summary="Connection: offline"><p>Status detail</p></StatusPopover>);
    const trigger = screen.getByRole("button", { name: "Status: Connection: offline" });
    fireEvent.pointerEnter(trigger);
    fireEvent.keyDown(document, { key: "Escape" });
    fireEvent.click(trigger);
    act(() => vi.advanceTimersByTime(120));
    expect(trigger).toHaveAttribute("aria-expanded", "true");
  });

  it("keeps the card open while crossing from the trigger and dismisses pinned state outside", () => {
    vi.useFakeTimers();
    render(<StatusPopover tone="neutral" summary="Connection: offline"><p>Status detail</p></StatusPopover>);
    const trigger = screen.getByRole("button", { name: "Status: Connection: offline" });
    const wrapper = trigger.closest("[data-status-popover]")!;
    fireEvent.pointerEnter(wrapper);
    act(() => vi.advanceTimersByTime(120));
    fireEvent.click(trigger);
    fireEvent.pointerLeave(trigger, { relatedTarget: document.querySelector("[data-status-panel]") });
    act(() => vi.advanceTimersByTime(200));
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    fireEvent.pointerDown(document.body);
    expect(trigger).toHaveAttribute("aria-expanded", "false");
  });

  it("unpins on a second click and closes on blur outside", () => {
    render(<><StatusPopover tone="neutral" summary="Connection: offline"><p>Status detail</p></StatusPopover><button type="button">Elsewhere</button></>);
    const trigger = screen.getByRole("button", { name: "Status: Connection: offline" });
    fireEvent.click(trigger);
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    fireEvent.click(trigger);
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    act(() => trigger.focus());
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    fireEvent.blur(trigger, { relatedTarget: screen.getByRole("button", { name: "Elsewhere" }) });
    expect(trigger).toHaveAttribute("aria-expanded", "false");
  });

  it("cancels a pending hover open during unmount", () => {
    vi.useFakeTimers();
    const { unmount } = render(<StatusPopover tone="neutral" summary="Connection: offline"><p>Status detail</p></StatusPopover>);
    fireEvent.pointerEnter(screen.getByRole("button", { name: "Status: Connection: offline" }));
    unmount();
    expect(() => act(() => vi.advanceTimersByTime(120))).not.toThrow();
  });

  it("keeps accessible details mounted and binds generic tone without connection selectors", () => {
    render(<StatusPopover tone="ok" summary="Connection: connected"><p>Status detail</p></StatusPopover>);
    const trigger = screen.getByRole("button", { name: "Status: Connection: connected" });
    const panel = document.querySelector("[data-status-panel]");
    expect(panel).toHaveAttribute("id", trigger.getAttribute("aria-controls"));
    expect(panel).toHaveAttribute("hidden");
    expect(trigger).toHaveAttribute("data-status-tone", "ok");
    expect(trigger).not.toHaveAttribute("data-connection-state");
  });
});

describe("ConnectionDot", () => {
  it("uses the supplied accessible label verbatim and exposes its state", () => {
    render(<ConnectionDot state="missing-native-host" label="Connection: Native host unavailable" />);
    const dot = screen.getByRole("status", { name: "Connection: Native host unavailable" });
    expect(dot).toHaveAttribute("title", "Connection: Native host unavailable");
    expect(dot).toHaveAttribute("data-connection-state", "missing-native-host");
  });
});

function renderRecipientPanel(props: Partial<ComponentProps<typeof RecipientPanel>> = {}, withLayer = false) {
  const rect = (width: number, height: number) => ({
    width, height, top: 0, left: 0, right: width, bottom: height, x: 0, y: 0,
    toJSON: () => ({}),
  }) as DOMRect;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    if (this.id === "conversation-stage") return rect(400, 300);
    if (this.id === "recipient-rail-layer") return rect(400, 220);
    return rect(160, 40);
  });
  const panel = <RecipientPanel
    recipients={[]}
    onCommit={() => {}}
    position={{ x: 20, y: 30 }}
    onPositionChange={() => {}}
    {...props}
  />;
  return render(<div id="conversation-stage">{withLayer ? <div id="recipient-rail-layer">{panel}</div> : panel}</div>);
}

function firePointer(target: Element, type: string, pointerId: number, clientX: number, clientY: number) {
  const event = new Event(type, { bubbles: true });
  Object.defineProperties(event, {
    pointerId: { value: pointerId },
    button: { value: 0 },
    clientX: { value: clientX },
    clientY: { value: clientY },
  });
  fireEvent(target, event);
}

describe("desktop UI controls", () => {
  it("emits the exact cross-surface JSON QR payload", () => {
    expect(pairingQrPayload({ httpsOrigin: "https://vault.example", intentToken: "A".repeat(43), expiresInSeconds: 300 }))
      .toBe(`{"https_origin":"https://vault.example","intent_token":"${"A".repeat(43)}"}`);
  });

  it("requires SAS confirmation before approving a claimed phone", async () => {
    const approve = vi.fn().mockResolvedValue(undefined);
    render(<PairPhone
      createIntent={async () => ({ httpsOrigin: "https://example.test", intentToken: "A".repeat(43), expiresInSeconds: 300 })}
      getStatus={async () => ({ claimed: true, approved: false, keyDigest: "b".repeat(64), sas: "123456", expiresInSeconds: 300 })}
      approveIntent={approve}
    />);
    fireEvent.click(screen.getByTestId("pairing-new-qr-button"));
    expect(await screen.findByTestId("pairing-qr-image")).toHaveAttribute("src", expect.stringMatching(/^data:image\//));
    const approval = await screen.findByTestId("pairing-approve-prompt");
    expect(within(approval).getByTestId("pairing-sas-code")).toHaveTextContent("123456");
    const button = within(approval).getByRole("button", { name: "Approve pairing" });
    expect(button).toBeDisabled();
    fireEvent.click(within(approval).getByRole("checkbox"));
    fireEvent.click(button);
    await vi.waitFor(() => expect(approve).toHaveBeenCalledWith("A".repeat(43), "b".repeat(64)));
  });

  it("starts pairing for callers that omit availability props", async () => {
    const createIntent = vi.fn().mockResolvedValue({ httpsOrigin: "https://example.test", intentToken: "A".repeat(43), expiresInSeconds: 300 });
    render(<PairPhone
      createIntent={createIntent}
      getStatus={async () => ({ claimed: false, approved: false, expiresInSeconds: 300 })}
      approveIntent={async () => undefined}
    />);

    fireEvent.click(screen.getByRole("button", { name: "Generate QR code" }));

    expect(await screen.findByTestId("pairing-qr-image")).toBeInTheDocument();
    expect(createIntent).toHaveBeenCalledOnce();
  });

  it("blocks new pairing requests with an accessible unavailable reason", () => {
    const createIntent = vi.fn();
    render(<PairPhone
      canStart={false}
      unavailableReason="Only the owner can pair a phone."
      createIntent={createIntent}
      getStatus={async () => ({ claimed: false, approved: false, expiresInSeconds: 300 })}
      approveIntent={async () => undefined}
    />);

    const generate = screen.getByRole("button", { name: "Generate QR code" });
    const reason = document.getElementById("pairing-availability-note");
    fireEvent.click(generate);

    expect(generate).toBeDisabled();
    expect(generate).toHaveAttribute("aria-describedby", "pairing-availability-note");
    expect(reason).toHaveTextContent("Only the owner can pair a phone.");
    expect(createIntent).not.toHaveBeenCalled();
  });

  it("retains an active pairing session and local cancellation when availability changes", async () => {
    const waitingForStatus = new Promise<PairingStatus>(() => {});
    const { rerender } = render(<PairPhone
      createIntent={async () => ({ httpsOrigin: "https://example.test", intentToken: "A".repeat(43), expiresInSeconds: 300 })}
      getStatus={async () => waitingForStatus}
      approveIntent={async () => undefined}
    />);

    fireEvent.click(screen.getByRole("button", { name: "Generate QR code" }));
    expect(await screen.findByTestId("pairing-qr-image")).toBeInTheDocument();

    rerender(<PairPhone
      canStart={false}
      unavailableReason="Pairing is temporarily unavailable."
      createIntent={async () => ({ httpsOrigin: "https://example.test", intentToken: "A".repeat(43), expiresInSeconds: 300 })}
      getStatus={async () => waitingForStatus}
      approveIntent={async () => undefined}
    />);

    expect(screen.getByTestId("pairing-qr-image")).toBeInTheDocument();
    expect(screen.getByTestId("pairing-waiting")).toBeInTheDocument();
    const cancel = screen.getByRole("button", { name: "Cancel pairing" });
    expect(cancel).toBeEnabled();
    fireEvent.click(cancel);
    expect(screen.queryByTestId("pairing-qr-image")).not.toBeInTheDocument();
  });

  it("retains claimed verification details and cancellation when availability changes", async () => {
    const getStatus = async () => ({ claimed: true, approved: false, keyDigest: "b".repeat(64), sas: "123456", expiresInSeconds: 300 });
    const createIntent = async () => ({ httpsOrigin: "https://example.test", intentToken: "A".repeat(43), expiresInSeconds: 300 });
    const { rerender } = render(<PairPhone createIntent={createIntent} getStatus={getStatus} approveIntent={async () => undefined} />);

    fireEvent.click(screen.getByRole("button", { name: "Generate QR code" }));
    expect(await screen.findByTestId("pairing-approve-prompt")).toBeInTheDocument();

    rerender(<PairPhone
      canStart={false}
      unavailableReason="Pairing is temporarily unavailable."
      createIntent={createIntent}
      getStatus={getStatus}
      approveIntent={async () => undefined}
    />);

    expect(screen.getByTestId("pairing-qr-image")).toBeInTheDocument();
    expect(screen.getByTestId("pairing-sas-code")).toHaveTextContent("123456");
    expect(screen.getByRole("button", { name: "Cancel pairing" })).toBeEnabled();
  });

  it("never enables approval for a malformed claimed SAS", async () => {
    render(<PairPhone
      createIntent={async () => ({ httpsOrigin: "https://example.test", intentToken: "A".repeat(43), expiresInSeconds: 300 })}
      getStatus={async () => ({ claimed: true, approved: false, keyDigest: "b".repeat(64), sas: "invalid", expiresInSeconds: 300 })}
      approveIntent={async () => undefined}
    />);
    fireEvent.click(screen.getByTestId("pairing-new-qr-button"));
    const approval = await screen.findByTestId("pairing-approve-prompt");
    fireEvent.click(within(approval).getByRole("checkbox"));
    expect(within(approval).getByRole("button", { name: "Approve pairing" })).toBeDisabled();
  });

  it("selects recipients with an ARIA combobox keyboard flow", () => {
    const changed = vi.fn();
    render(<RecipientPicker recipients={people} onChange={changed} />);
    const input = screen.getByRole("combobox");
    fireEvent.change(input, { target: { value: "Aur" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(changed).toHaveBeenCalledWith(["conv-a"]);
  });

  it("selects an existing formatted phone conversation for canonical-equivalent queries", () => {
    const changed = vi.fn();
    const phoneConversation = [{ id: "phone-conversation", name: "(202) 555-0100", preview: "Hi", unread: 0 }];
    render(<RecipientPicker recipients={phoneConversation} onChange={changed} onNewRecipient={() => {}} />);
    const input = screen.getByRole("combobox");
    fireEvent.change(input, { target: { value: "2025550100" } });
    expect(screen.getByRole("option", { name: "(202) 555-0100" })).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(changed).toHaveBeenCalledWith(["phone-conversation"]);

    fireEvent.click(screen.getByRole("button", { name: "Remove (202) 555-0100" }));
    fireEvent.change(input, { target: { value: "+12025550100" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(changed).toHaveBeenLastCalledWith(["phone-conversation"]);
  });

  it("keeps named-contact matching as free text", () => {
    const changed = vi.fn();
    render(<RecipientPicker recipients={people} onChange={changed} />);
    const input = screen.getByRole("combobox");
    fireEvent.change(input, { target: { value: "Aur" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(changed).toHaveBeenCalledWith(["conv-a"]);
  });

  it("offers a formatted, canonical new-recipient option for a valid number without a conversation", () => {
    const changed = vi.fn(),
      started = vi.fn();
    render(
      <RecipientPicker
        recipients={people}
        onChange={changed}
        onNewRecipient={started}
      />,
    );
    const input = screen.getByRole("combobox");
    fireEvent.change(input, { target: { value: " +1 202 555 0100 " } });
    expect(
      screen.getByRole("option", { name: "Message (202) 555-0100" }),
    ).toBeInTheDocument();
    fireEvent.keyDown(input, { key: "Enter" });
    expect(started).toHaveBeenCalledWith("+12025550100");
    expect(changed).not.toHaveBeenCalled();
  });

  it("retains an invalid new-recipient search and exposes its validation error", () => {
    const started = vi.fn();
    render(<RecipientPicker recipients={people} onChange={() => {}} onNewRecipient={started} />);
    const input = screen.getByRole("combobox");
    fireEvent.change(input, { target: { value: "202 555" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(started).not.toHaveBeenCalled();
    expect(input).toHaveValue("202 555");
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByRole("alert")).toBeInTheDocument();
    fireEvent.change(input, { target: { value: "2025550100" } });
    expect(input).not.toHaveAttribute("aria-invalid");
  });

  it("selects an option after recipients appear following empty-result navigation", () => {
    const changed = vi.fn();
    const { rerender } = render(<RecipientPicker recipients={[]} onChange={changed} />);
    const input = screen.getByRole("combobox");
    fireEvent.change(input, { target: { value: "Aur" } });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    rerender(<RecipientPicker recipients={people} onChange={changed} />);
    fireEvent.keyDown(input, { key: "Enter" });
    expect(changed).toHaveBeenCalledWith(["conv-a"]);
  });

  it("does not commit a recipient during IME composition", () => {
    const changed = vi.fn();
    render(<RecipientPicker recipients={people} onChange={changed} />);
    const input = screen.getByRole("combobox");
    fireEvent.change(input, { target: { value: "Aur" } });
    fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    expect(changed).not.toHaveBeenCalled();
  });

  it("does not send Enter during IME composition", () => {
    const sent = vi.fn();
    render(
      <Composer
        draft="こんにちは"
        attachments={[]}
        sendSupported
        onDraftChange={() => {}}
        onSend={sent}
      />,
    );
    fireEvent.keyDown(screen.getByLabelText("Message"), {
      key: "Enter",
      isComposing: true,
    });
    expect(sent).not.toHaveBeenCalled();
  });

  it("allows attachment-only sends and explains why sending is unavailable", () => {
    const sent = vi.fn();
    const { rerender } = render(
      <Composer
        draft=""
        attachments={[{ id: "a", name: "a.png", state: "ready" }]}
        sendSupported
        onDraftChange={() => {}}
        onSend={sent}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(sent).toHaveBeenCalledTimes(1);
    rerender(
      <Composer
        draft="hi"
        attachments={[]}
        sendSupported={false}
        unavailableReason="choose a gateway and SIM"
        onDraftChange={() => {}}
        onSend={sent}
      />,
    );
    expect(document.querySelectorAll("#unavailable-hint")).toHaveLength(1);
    expect(screen.getByText("Sending unavailable: choose a gateway and SIM")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
    rerender(
      <Composer
        draft="hi"
        attachments={[]}
        sendSupported={false}
        gatewaySlot={<span id="unavailable-hint">Gateway unavailable</span>}
        onDraftChange={() => {}}
        onSend={sent}
      />,
    );
    expect(document.querySelectorAll("#unavailable-hint")).toHaveLength(1);
  });

  it("never renders the simulated badge", () => {
    const noop = () => {};
    const { rerender } = render(
      <AppTitlebar onMinimize={noop} onMaximize={noop} onClose={noop} />,
    );
    expect(screen.queryByText("SIMULATED UI")).not.toBeInTheDocument();
    rerender(
      <AppTitlebar
        onMinimize={noop}
        onMaximize={noop}
        onClose={noop}
        simulated
      />,
    );
    expect(screen.queryByText("SIMULATED UI")).not.toBeInTheDocument();
  });

  it("renders titlebar status only when provided", () => {
    const noop = () => {};
    const { rerender } = render(
      <AppTitlebar onMinimize={noop} onMaximize={noop} onClose={noop} status={<span>Status node</span>} />,
    );
    expect(document.getElementById("titlebar-status")).toHaveTextContent("Status node");
    rerender(<AppTitlebar onMinimize={noop} onMaximize={noop} onClose={noop} />);
    expect(document.getElementById("titlebar-status")).not.toBeInTheDocument();
  });

  it("omits the window-control group when the host supplies no window handlers", () => {
    render(<AppTitlebar />);
    expect(document.getElementById("window-controls")).not.toBeInTheDocument();
  });

  it("provides macOS traffic lights and keyboard resizing", () => {
    const resize = vi.fn();
    const resizeTo = vi.fn();
    render(<main data-platform="macos"><AppTitlebar platform="macos" onMinimize={() => {}} onMaximize={() => {}} onClose={() => {}} /><ResizeHandle direction="horizontal" ariaLabel="Resize list" value={280} min={200} max={480} valueUnit="pixels" onResize={resize} onResizeTo={resizeTo} collapsible={{ side: "before", restoreValue: 280 }} /></main>);
    expect(screen.getByRole("button", { name: "Close window" })).toHaveClass("traffic-light");
    const handle = screen.getByRole("separator");
    fireEvent.keyDown(handle, { key: "ArrowLeft" });
    fireEvent.keyDown(handle, { key: "ArrowRight", shiftKey: true });
    fireEvent.keyDown(handle, { key: "Home" });
    fireEvent.keyDown(handle, { key: "Enter" });
    expect(resize).toHaveBeenNthCalledWith(1, -8);
    expect(resize).toHaveBeenNthCalledWith(2, 32);
    expect(resizeTo).toHaveBeenCalledWith(200);
    expect(resizeTo).toHaveBeenCalledWith(0);
  });

  it("renders main-window conversation controls and invokes their callbacks", () => {
    const toggleSidebar = vi.fn();
    const newMessage = vi.fn();
    render(<AppTitlebar
      onMinimize={() => {}}
      onMaximize={() => {}}
      onClose={() => {}}
      onToggleSidebar={toggleSidebar}
      sidebarExpanded
      sidebarControls="thread-list"
      onNewMessage={newMessage}
    />);
    const toggle = screen.getByRole("button", { name: "Toggle conversation list" });
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(toggle).toHaveAttribute("aria-controls", "thread-list");
    fireEvent.click(toggle);
    fireEvent.click(screen.getByRole("button", { name: "New conversation" }));
    expect(toggleSidebar).toHaveBeenCalledOnce();
    expect(newMessage).toHaveBeenCalledOnce();
  });

  it("does not render main-window conversation controls in the composer", () => {
    render(<AppTitlebar isComposer onMinimize={() => {}} onMaximize={() => {}} onClose={() => {}} onToggleSidebar={() => {}} onNewMessage={() => {}} />);
    expect(document.getElementById("titlebar-actions")).not.toBeInTheDocument();
  });

  it.each(["rail", "titlebar"] as const)("renders %s navigation buttons with badges and view behavior", (orientation) => {
    const onView = vi.fn();
    const onToggleList = vi.fn();
    render(<NavButtons
      orientation={orientation}
      activeView="conversations"
      onView={onView}
      onToggleList={onToggleList}
      listCollapsed={false}
      threadListId="thread-list"
      notificationUnread={100}
      contactsPending={100}
    />);
    const navigation = screen.getByRole("navigation", { name: "Main navigation" });
    const conversations = screen.getByRole("button", { name: "Conversations" });
    expect(navigation).toHaveAttribute("data-orientation", orientation);
    expect(conversations).toHaveAttribute("aria-current", "page");
    expect(conversations).toHaveAttribute("aria-expanded", "true");
    expect(conversations).toHaveAttribute("aria-controls", "thread-list");
    expect(screen.getByRole("button", { name: "Contacts, 100 pending" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Notifications, 100 unread" })).toBeInTheDocument();
    expect(screen.getAllByText("99+")).toHaveLength(2);
    fireEvent.click(conversations);
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(onToggleList).toHaveBeenCalledOnce();
    expect(onView).toHaveBeenCalledWith("settings");
    if (orientation === "titlebar") expect(navigation).toHaveAttribute("id", "titlebar-navigation");
    else expect(navigation).not.toHaveAttribute("id");
  });

  it("places titlebar navigation after status on macOS and first on Windows and Linux", () => {
    const navigation = <NavButtons orientation="titlebar" activeView="conversations" onView={() => {}} />;
    const { rerender } = render(<AppTitlebar platform="macos" status={<span>Status node</span>} navigation={navigation} />);
    const titlebar = document.getElementById("desktop-titlebar")!;
    const titlebarNavigation = document.getElementById("titlebar-navigation")!;
    expect(titlebar).toHaveAttribute("data-navigation-placement", "trailing");
    expect(titlebar.lastElementChild).toBe(titlebarNavigation);
    expect(titlebarNavigation.previousElementSibling).toHaveAttribute("id", "titlebar-status");
    expect(titlebarNavigation).not.toHaveAttribute("data-tauri-drag-region");

    rerender(<AppTitlebar platform="windows" navigation={navigation} />);
    expect(titlebar).toHaveAttribute("data-navigation-placement", "leading");
    expect(titlebar.firstElementChild).toBe(document.getElementById("titlebar-navigation"));

    rerender(<AppTitlebar platform="linux" navigation={navigation} />);
    expect(titlebar).toHaveAttribute("data-navigation-placement", "leading");
    expect(titlebar.firstElementChild).toBe(document.getElementById("titlebar-navigation"));
  });

  it("ignores titlebar navigation in the composer", () => {
    render(<AppTitlebar isComposer platform="macos" navigation={<NavButtons orientation="titlebar" activeView="conversations" onView={() => {}} />} />);
    expect(document.getElementById("titlebar-navigation")).not.toBeInTheDocument();
    expect(document.getElementById("desktop-titlebar")).not.toHaveAttribute("data-navigation-placement");
  });

  it("keeps the desktop rail navigation when Panel uses shared nav buttons", () => {
    render(<Panel activeView="conversations" onView={() => {}} threadListId="thread-list" />);
    const rail = document.getElementById("desktop-rail");
    expect(rail).toBeInTheDocument();
    expect(within(rail!).getByRole("navigation", { name: "Main navigation" })).toHaveAttribute("data-orientation", "rail");
  });

  it("does not end a drag when its parent rerenders", () => {
    const ended = vi.fn();
    const resize = vi.fn();
    const { rerender } = render(
      <ResizeHandle
        direction="horizontal"
        ariaLabel="Resize list"
        value={280}
        min={200}
        max={480}
        valueUnit="pixels"
        onResize={resize}
        onResizeTo={() => {}}
        onResizeEnd={ended}
      />,
    );
    const handle = screen.getByRole("separator") as HTMLDivElement;
    handle.setPointerCapture = vi.fn();
    handle.hasPointerCapture = vi.fn(() => true);
    handle.releasePointerCapture = vi.fn();
    fireEvent.pointerDown(handle, { pointerId: 1, clientX: 100 });
    rerender(
      <ResizeHandle
        direction="horizontal"
        ariaLabel="Resize list"
        value={281}
        min={200}
        max={480}
        valueUnit="pixels"
        onResize={resize}
        onResizeTo={() => {}}
        onResizeEnd={() => ended()}
      />,
    );
    expect(ended).not.toHaveBeenCalled();
    fireEvent.pointerUp(handle, { pointerId: 1, clientX: 101 });
    expect(ended).toHaveBeenCalledOnce();
  });

  it("counts SMS characters only as text approaches the limit", () => {
    const { rerender } = render(<Composer draft={"a".repeat(135)} attachments={[]} sendSupported onDraftChange={() => {}} onSend={() => {}} />);
    expect(screen.queryByLabelText("SMS character count")).not.toBeInTheDocument();
    rerender(<Composer draft={"a".repeat(161)} attachments={[]} sendSupported onDraftChange={() => {}} onSend={() => {}} />);
    expect(screen.getByLabelText("SMS character count")).toHaveTextContent("2 SMS · 145 left");
    rerender(<Composer draft={"😀".repeat(60)} attachments={[]} sendSupported onDraftChange={() => {}} onSend={() => {}} />);
    expect(screen.getByLabelText("SMS character count")).toHaveTextContent("2 SMS · 14 left");
    rerender(<Composer draft={"a".repeat(160)} attachments={[{ id: "a", name: "a.png", state: "ready" }]} sendSupported onDraftChange={() => {}} onSend={() => {}} />);
    expect(screen.queryByLabelText("SMS character count")).not.toBeInTheDocument();
  });

  it("caps composer auto-grow at the configured height and defaults to 176px", () => {
    Object.defineProperty(HTMLTextAreaElement.prototype, "scrollHeight", {
      configurable: true,
      get: () => 240,
    });
    const props = {
      draft: "Message",
      attachments: [],
      sendSupported: true,
      onDraftChange: () => {},
      onSend: () => {},
    };
    const { rerender } = render(<Composer {...props} maxAutoGrowHeight={100} />);
    expect(screen.getByLabelText("Message")).toHaveStyle({ height: "100px" });

    rerender(<Composer {...props} />);
    expect(screen.getByLabelText("Message")).toHaveStyle({ height: "176px" });
  });

  it("formats a pending recipient without committing until a commit action", () => {
    const committed = vi.fn();
    renderRecipientPanel({ onCommit: committed });
    const input = screen.getByLabelText("Recipients");
    fireEvent.change(input, { target: { value: "2025550100" } });
    expect(committed).not.toHaveBeenCalled();
    expect(input).toHaveValue("(202) 555-0100");
    fireEvent.keyDown(input, { key: "," });
    expect(committed).toHaveBeenCalledWith(["+12025550100"]);
  });

  it("commits canonical recipients on Enter, blur, and separated paste without duplicates", () => {
    const committed = vi.fn();
    const { rerender } = renderRecipientPanel({ recipients: [{ id: "one", label: "One" }], onCommit: committed });
    const input = screen.getByLabelText("Recipients");
    fireEvent.change(input, { target: { value: "2025550100;+12025550100" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(committed).toHaveBeenLastCalledWith(["one", "+12025550100"]);
    rerender(<div id="conversation-stage"><RecipientPanel recipients={[{ id: "one", label: "One" }, { id: "+12025550100", label: "+12025550100" }]} onCommit={committed} position={{ x: 20, y: 30 }} onPositionChange={() => {}} /></div>);
    fireEvent.change(input, { target: { value: "2025550101" } });
    fireEvent.blur(input);
    expect(committed).toHaveBeenLastCalledWith(["one", "+12025550100", "+12025550101"]);
    fireEvent.paste(input, { clipboardData: { getData: () => "2025550102, 2025550103" } });
    expect(committed).toHaveBeenLastCalledWith(["one", "+12025550100", "+12025550102"]);
    expect(input).toHaveValue("2025550103");
  });

  it("retains an invalid recipient batch without committing a valid subset", () => {
    const committed = vi.fn();
    renderRecipientPanel({ onCommit: committed });
    const input = screen.getByLabelText("Recipients");
    fireEvent.paste(input, { clipboardData: { getData: () => "2025550100, invalid" } });
    expect(committed).not.toHaveBeenCalled();
    expect(input).toHaveValue("2025550100, invalid");
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByRole("alert")).toBeInTheDocument();
    fireEvent.change(input, { target: { value: "2025550100" } });
    expect(input).not.toHaveAttribute("aria-invalid");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(committed).toHaveBeenCalledWith(["+12025550100"]);
  });

  it("retains a rejected delimiter-terminated recipient paste as pending input", () => {
    const committed = vi.fn();
    const pending = vi.fn();
    renderRecipientPanel({
      recipients: [{ id: "+12025550199", label: "+12025550199" }],
      onCommit: committed,
      onPendingChange: pending,
    });
    const input = screen.getByLabelText("Recipients");
    fireEvent.paste(input, { clipboardData: { getData: () => "2025550123,not-a-phone," } });
    expect(committed).not.toHaveBeenCalled();
    expect(input).toHaveValue("2025550123,not-a-phone,");
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(pending).toHaveBeenLastCalledWith(true);
  });

  it("deduplicates new recipient formats against existing canonical and legacy ids", () => {
    const committed = vi.fn();
    renderRecipientPanel({
      recipients: [
        { id: "+12025550100", label: "+12025550100" },
        { id: "2025550101", label: "Legacy" },
      ],
      onCommit: committed,
    });
    const input = screen.getByLabelText("Recipients");
    expect(screen.getByText("(202) 555-0100")).toBeInTheDocument();
    expect(screen.getByText("Legacy")).toBeInTheDocument();
    fireEvent.change(input, { target: { value: "(202) 555-0100; +1 202 555 0101" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(committed).not.toHaveBeenCalled();
  });

  it("preserves equivalent loaded recipient ids while appending a new canonical id", () => {
    const committed = vi.fn();
    renderRecipientPanel({
      recipients: [
        { id: "2025550100", label: "Legacy" },
        { id: "+12025550100", label: "Canonical" },
      ],
      onCommit: committed,
    });
    const input = screen.getByLabelText("Recipients");
    fireEvent.blur(input);
    expect(committed).not.toHaveBeenCalled();
    fireEvent.change(input, { target: { value: "2025550102" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(committed).toHaveBeenCalledWith(["2025550100", "+12025550100", "+12025550102"]);
  });

  it("does not trap Backspace at leading phone-number formatting characters", () => {
    renderRecipientPanel();
    const input = screen.getByLabelText("Recipients") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "2025550100" } });
    input.setSelectionRange(1, 1);
    fireEvent.keyDown(input, { key: "Backspace" });
    expect(input).toHaveValue("(202) 555-0100");

    fireEvent.change(input, { target: { value: "+12025550100" } });
    input.setSelectionRange(1, 1);
    fireEvent.keyDown(input, { key: "Backspace" });
    expect(input).toHaveValue("+1 202 555 0100");
  });

  it("removes the preceding digit when Backspace follows phone-number punctuation", () => {
    renderRecipientPanel();
    const input = screen.getByLabelText("Recipients") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "2025550100" } });
    input.setSelectionRange(6, 6);
    fireEvent.keyDown(input, { key: "Backspace" });
    expect((input.value.match(/\d/g) ?? [])).toHaveLength(9);
    expect(input).not.toHaveValue("(202) 555-0100");
  });

  it("reports pending recipient text without emitting a cleanup callback", () => {
    const pending = vi.fn();
    const { unmount } = renderRecipientPanel({ onPendingChange: pending });
    const input = screen.getByLabelText("Recipients");
    fireEvent.change(input, { target: { value: "202" } });
    expect(pending).toHaveBeenLastCalledWith(true);
    fireEvent.change(input, { target: { value: "" } });
    expect(pending).toHaveBeenLastCalledWith(false);
    const calls = pending.mock.calls.length;
    unmount();
    expect(pending).toHaveBeenCalledTimes(calls);
  });

  it("removes recipient chips by Backspace and their remove button", () => {
    const committed = vi.fn();
    renderRecipientPanel({ recipients: [{ id: "one", label: "One" }, { id: "two", label: "Two" }], onCommit: committed });
    const input = screen.getByLabelText("Recipients");
    fireEvent.keyDown(input, { key: "Backspace" });
    expect(committed).toHaveBeenLastCalledWith(["one"]);
    fireEvent.click(screen.getByRole("button", { name: "Remove One" }));
    expect(committed).toHaveBeenLastCalledWith(["two"]);
    expect(input).toHaveFocus();
    expect(screen.getByText("Group · MMS")).toBeInTheDocument();
  });

  it("removes the last chip by its stored id with an empty recipient commit", () => {
    const committed = vi.fn();
    renderRecipientPanel({ recipients: [{ id: "legacy-id", label: "(202) 555-0100" }], onCommit: committed });
    fireEvent.click(screen.getByRole("button", { name: "Remove (202) 555-0100" }));
    expect(committed).toHaveBeenCalledWith([]);
  });

  it("keeps pending recipient input when removing a chip", () => {
    const committed = vi.fn();
    renderRecipientPanel({ recipients: [{ id: "legacy-id", label: "Aurora" }], onCommit: committed });
    const input = screen.getByLabelText("Recipients");
    input.focus();
    fireEvent.change(input, { target: { value: "202" } });
    fireEvent.pointerDown(screen.getByRole("button", { name: "Remove Aurora" }));
    fireEvent.blur(input, { relatedTarget: screen.getByRole("button", { name: "Remove Aurora" }) });
    fireEvent.click(screen.getByRole("button", { name: "Remove Aurora" }));
    expect(committed).toHaveBeenCalledWith([]);
    expect(input).toHaveValue("(202)");
  });

  it("moves recipients by exact pixels with arrow keys while preserving focus and pending input", () => {
    const changed = vi.fn();
    renderRecipientPanel({ onPositionChange: changed });
    const input = screen.getByLabelText("Recipients");
    const grip = screen.getByRole("button", { name: "Move recipients panel" });
    input.focus();
    fireEvent.change(input, { target: { value: "uncommitted" } });
    fireEvent.keyDown(grip, { key: "ArrowDown", shiftKey: true });
    expect(changed).toHaveBeenCalledWith({ x: 20, y: 62 });
    expect(input).toHaveFocus();
    expect(input).toHaveValue("uncommitted");
  });

  it("previews exact free pointer positions, persists on release, and reverts cancellation", () => {
    const changed = vi.fn();
    renderRecipientPanel({ onPositionChange: changed });
    const grip = screen.getByRole("button", { name: "Move recipients panel" });
    firePointer(grip, "pointerdown", 1, 100, 100);
    firePointer(grip, "pointermove", 1, 137, 153);
    expect(document.getElementById("draft-recipients")).toHaveStyle({ left: "57px", top: "83px" });
    firePointer(grip, "pointerup", 1, 137, 153);
    expect(changed).toHaveBeenLastCalledWith({ x: 57, y: 83 });
    firePointer(grip, "pointerdown", 2, 100, 100);
    firePointer(grip, "pointermove", 2, 180, 180);
    firePointer(grip, "pointercancel", 2, 180, 180);
    expect(changed).toHaveBeenCalledTimes(1);
    expect(document.getElementById("draft-recipients")).toHaveStyle({ left: "20px", top: "30px" });
  });

  it("clamps rendered and keyboard positions to the stage bounds", () => {
    const changed = vi.fn();
    renderRecipientPanel({ position: { x: 900, y: 900 }, onPositionChange: changed });
    const panel = document.getElementById("draft-recipients");
    expect(panel).toHaveStyle({ left: "240px", top: "260px" });
    fireEvent.keyDown(screen.getByRole("button", { name: "Move recipients panel" }), { key: "ArrowRight" });
    expect(changed).toHaveBeenCalledWith({ x: 240, y: 260 });
  });

  it("uses the rail layer as a hard bottom stop for pointer and keyboard movement", () => {
    const changed = vi.fn();
    renderRecipientPanel({ onPositionChange: changed }, true);
    const grip = screen.getByRole("button", { name: "Move recipients panel" });
    firePointer(grip, "pointerdown", 1, 100, 100);
    firePointer(grip, "pointermove", 1, 100, 500);
    expect(document.getElementById("draft-recipients")).toHaveStyle({ top: "180px" });
    firePointer(grip, "pointerup", 1, 100, 500);
    expect(changed).toHaveBeenLastCalledWith({ x: 20, y: 180 });
    fireEvent.keyDown(grip, { key: "ArrowDown" });
    expect(changed).toHaveBeenLastCalledWith({ x: 20, y: 180 });
  });

  it("clamps to layer and panel resize geometry without persisting", () => {
    let layerHeight = 220;
    let panelHeight = 40;
    const rect = (width: number, height: number) => ({
      width, height, top: 0, left: 0, right: width, bottom: height, x: 0, y: 0,
      toJSON: () => ({}),
    }) as DOMRect;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      if (this.id === "conversation-stage") return rect(400, 300);
      if (this.id === "recipient-rail-layer") return rect(400, layerHeight);
      return rect(160, panelHeight);
    });
    const changed = vi.fn();
    const { rerender } = render(<div id="conversation-stage"><div id="recipient-rail-layer"><RecipientPanel recipients={[]} onCommit={() => {}} position={{ x: 20, y: 180 }} onPositionChange={changed} /></div></div>);
    layerHeight = 120;
    panelHeight = 80;
    rerender(<div id="conversation-stage"><div id="recipient-rail-layer"><RecipientPanel recipients={[{ id: "one", label: "One" }]} onCommit={() => {}} position={{ x: 20, y: 180 }} onPositionChange={changed} /></div></div>);
    expect(document.getElementById("draft-recipients")).toHaveStyle({ top: "40px" });
    expect(changed).not.toHaveBeenCalled();
  });

  it("binds recipient input sizing to its compact and empty placeholders", () => {
    const { rerender } = renderRecipientPanel();
    const input = screen.getByLabelText("Recipients");
    expect(input).toHaveAttribute("size", "16");
    rerender(<div id="conversation-stage"><RecipientPanel recipients={[{ id: "one", label: "One" }]} onCommit={() => {}} position={{ x: 20, y: 30 }} onPositionChange={() => {}} /></div>);
    expect(screen.getByLabelText("Recipients")).toHaveAttribute("size", "3");
  });

  it("preserves a supplied position until geometry is measurable", () => {
    render(<div id="conversation-stage"><RecipientPanel
      recipients={[]}
      onCommit={() => {}}
      position={{ x: 37, y: 53 }}
      onPositionChange={() => {}}
    /></div>);
    expect(document.getElementById("draft-recipients")).toHaveStyle({ left: "37px", top: "53px" });
  });

  it("uses controlled keyboard updates and clamps later resizes without persisting them", () => {
    let stageWidth = 400;
    let stageHeight = 300;
    const rect = (width: number, height: number) => ({
      width, height, top: 0, left: 0, right: width, bottom: height, x: 0, y: 0,
      toJSON: () => ({}),
    }) as DOMRect;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      return this.id === "conversation-stage" ? rect(stageWidth, stageHeight) : rect(160, 40);
    });
    const changed = vi.fn();
    const { rerender } = render(<div id="conversation-stage"><RecipientPanel recipients={[]} onCommit={() => {}} position={{ x: 20, y: 30 }} onPositionChange={changed} /></div>);
    fireEvent.keyDown(screen.getByRole("button", { name: "Move recipients panel" }), { key: "ArrowRight" });
    expect(changed).toHaveBeenCalledWith({ x: 28, y: 30 });
    stageWidth = 200;
    stageHeight = 120;
    rerender(<div id="conversation-stage"><RecipientPanel recipients={[{ id: "one", label: "One" }]} onCommit={() => {}} position={{ x: 300, y: 200 }} onPositionChange={changed} /></div>);
    expect(document.getElementById("draft-recipients")).toHaveStyle({ left: "40px", top: "80px" });
    expect(changed).toHaveBeenCalledTimes(1);
  });

  it("accumulates batched keyboard movements before a controlled rerender", () => {
    const changed = vi.fn();
    renderRecipientPanel({ onPositionChange: changed });
    const grip = screen.getByRole("button", { name: "Move recipients panel" });
    act(() => {
      fireEvent.keyDown(grip, { key: "ArrowRight" });
      fireEvent.keyDown(grip, { key: "ArrowRight" });
    });
    expect(changed).toHaveBeenNthCalledWith(1, { x: 28, y: 30 });
    expect(changed).toHaveBeenNthCalledWith(2, { x: 36, y: 30 });
  });

  it("moves from the released pointer position before its controlled rerender", () => {
    const changed = vi.fn();
    renderRecipientPanel({ onPositionChange: changed });
    const grip = screen.getByRole("button", { name: "Move recipients panel" });
    act(() => {
      firePointer(grip, "pointerdown", 1, 100, 100);
      firePointer(grip, "pointermove", 1, 137, 153);
      firePointer(grip, "pointerup", 1, 137, 153);
      fireEvent.keyDown(grip, { key: "ArrowRight" });
    });
    expect(changed).toHaveBeenNthCalledWith(1, { x: 57, y: 83 });
    expect(changed).toHaveBeenNthCalledWith(2, { x: 65, y: 83 });
  });

  it("validates finite nonnegative recipient positions", () => {
    expect(isRecipientPosition({ x: 0, y: 12 })).toBe(true);
    expect(isRecipientPosition({ x: Number.NaN, y: 0 })).toBe(false);
    expect(isRecipientPosition({ x: -1, y: 0 })).toBe(false);
    expect(isRecipientPosition({ x: 0, y: Infinity })).toBe(false);
  });
});

describe("installOverlayScrollbars", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("shows a thumb while scrolling and hides it after the final scroll", () => {
    vi.useFakeTimers();
    const target = document.createElement("div");
    document.body.append(target);
    const cleanup = installOverlayScrollbars(document);

    fireEvent.scroll(target);
    expect(target).toHaveAttribute("data-scrolling", "true");
    vi.advanceTimersByTime(999);
    expect(target).toHaveAttribute("data-scrolling", "true");
    vi.advanceTimersByTime(1);
    expect(target).not.toHaveAttribute("data-scrolling");

    cleanup();
    target.remove();
  });

  it("resets each target's hide timer after another scroll", () => {
    vi.useFakeTimers();
    const target = document.createElement("div");
    document.body.append(target);
    const cleanup = installOverlayScrollbars(document);

    fireEvent.scroll(target);
    vi.advanceTimersByTime(750);
    fireEvent.scroll(target);
    vi.advanceTimersByTime(250);
    expect(target).toHaveAttribute("data-scrolling", "true");
    vi.advanceTimersByTime(750);
    expect(target).not.toHaveAttribute("data-scrolling");

    cleanup();
    target.remove();
  });

  it("ignores programmatic scrolling", () => {
    const target = document.createElement("div");
    target.setAttribute("data-scroll-programmatic", "");
    document.body.append(target);
    const cleanup = installOverlayScrollbars(document);

    fireEvent.scroll(target);
    expect(target).not.toHaveAttribute("data-scrolling");

    cleanup();
    target.remove();
  });

  it("removes its listener, timers, and active attributes during cleanup", () => {
    vi.useFakeTimers();
    const target = document.createElement("div");
    document.body.append(target);
    const cleanup = installOverlayScrollbars(document);

    fireEvent.scroll(target);
    cleanup();
    expect(target).not.toHaveAttribute("data-scrolling");
    fireEvent.scroll(target);
    expect(target).not.toHaveAttribute("data-scrolling");

    target.remove();
  });
});

describe("contact recipient discovery", () => {
  const sol = [
    { id: "+12025550160", label: "Sol Rivera", detail: "mobile · (202) 555-0160" },
    { id: "+12025550161", label: "Sol Rivera", detail: "work · (202) 555-0161" },
  ];

  it("offers every phone of a matching contact and commits the chosen phone address", async () => {
    const committed = vi.fn();
    const chosen = vi.fn();
    const search = vi.fn(async (query: string) => (query.toLowerCase().startsWith("sol") ? sol : []));
    renderRecipientPanel({ onCommit: committed, searchContacts: search, onSuggestionChosen: chosen });
    const input = screen.getByLabelText("Recipients");
    fireEvent.change(input, { target: { value: "Sol" } });
    const options = await screen.findAllByRole("option");
    expect(options).toHaveLength(2);
    expect(screen.getByRole("listbox", { name: "Contact suggestions" })).toBeInTheDocument();
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(committed).toHaveBeenCalledWith(["+12025550161"]);
    expect(chosen).toHaveBeenCalledWith(sol[1]);
    expect(input).toHaveValue("");
  });

  it("keeps a complete typed number as the recipient even when contacts match", async () => {
    const committed = vi.fn();
    renderRecipientPanel({ onCommit: committed, searchContacts: async () => sol });
    const input = screen.getByLabelText("Recipients");
    fireEvent.change(input, { target: { value: "2025550199" } });
    await screen.findAllByRole("option");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(committed).toHaveBeenCalledWith(["+12025550199"]);
  });

  it("ignores a stale search response that resolves after a newer query", async () => {
    let resolveOld: (value: typeof sol) => void = () => {};
    const search = vi.fn((query: string) =>
      query === "So" ? new Promise<typeof sol>((resolve) => { resolveOld = resolve; }) : Promise.resolve([sol[0]]),
    );
    renderRecipientPanel({ searchContacts: search });
    const input = screen.getByLabelText("Recipients");
    fireEvent.change(input, { target: { value: "So" } });
    await vi.waitFor(() => expect(search).toHaveBeenCalledWith("So"));
    fireEvent.change(input, { target: { value: "Sol" } });
    expect(await screen.findAllByRole("option")).toHaveLength(1);
    await act(async () => resolveOld(sol));
    expect(screen.getAllByRole("option")).toHaveLength(1);
  });

  it("starts a new message from a contact phone without an existing conversation", async () => {
    const started = vi.fn();
    render(<RecipientPicker recipients={people} onChange={() => {}} onNewRecipient={started} searchContacts={async () => sol} />);
    const picker = screen.getByRole("combobox", { name: "Search recipients" });
    fireEvent.change(picker, { target: { value: "Sol" } });
    const option = await screen.findByText("work · (202) 555-0161");
    fireEvent.mouseDown(option.closest("li")!);
    expect(started).toHaveBeenCalledWith("+12025550161");
  });
});
