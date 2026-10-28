import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { dismissibleNotifications, NotificationsView } from "./Notifications";
import type { MirroredNotification } from "./bridge";

const notification = (sourceDeviceId: string, packageName = "chat", overrides: Partial<MirroredNotification> = {}): MirroredNotification => ({
  target: { sourceDeviceId, notificationKey: `key-${sourceDeviceId}`, lifetime: "one" },
  packageName,
  appName: "Chat",
  title: `Title ${sourceDeviceId}`,
  text: "A readable notification body",
  postedAt: Date.now(),
  dismissible: true,
  seen: false,
  dismissalPending: false,
  ...overrides,
});

const props = (notifications: MirroredNotification[]) => ({
  notifications,
  filters: [],
  sources: [{ id: "phone-a", name: "Pixel" }, { id: "phone-b", name: "Tablet" }],
  locked: false,
  onDismiss: vi.fn(),
  onMute: vi.fn(),
  onSeen: vi.fn().mockResolvedValue(undefined),
  onSettings: vi.fn(),
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  Reflect.deleteProperty(HTMLElement.prototype, "scrollIntoView");
});

describe("NotificationsView", () => {
  it("focuses and highlights only the exact tuple before reporting success without marking it seen", () => {
    vi.useFakeTimers();
    const scroll = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: scroll });
    const target = { sourceDeviceId: "phone-a", notificationKey: "shared", lifetime: "one" };
    const view = props([
      notification("phone-b", "chat", { target: { ...target, sourceDeviceId: "phone-b" } }),
      notification("phone-a", "chat", { target: { ...target, lifetime: "two" } }),
      notification("phone-a", "chat", { target }),
    ]);
    const onFocusResult = vi.fn(() => {
      expect(screen.getAllByRole("article")[2]).toHaveFocus();
      expect(screen.getAllByRole("article")[2]).toHaveAttribute("data-activation-highlight", "true");
    });
    render(<NotificationsView {...view} focusRequest={{ requestId: "first", target }} onFocusResult={onFocusResult} />);
    expect(onFocusResult).toHaveBeenCalledWith("first", true);
    expect(scroll).toHaveBeenCalledWith({ block: "nearest", behavior: "instant" });
    expect(scroll.mock.contexts[0]).toBe(screen.getAllByRole("article")[2]);
    expect(view.onSeen).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(1600));
    expect(screen.getAllByRole("article")[2]).not.toHaveAttribute("data-activation-highlight");
  });

  it("waits while locked or no ready request is supplied and rejects replacement lifetimes and muted rows", () => {
    const item = notification("phone-a");
    const view = props([item]);
    const onFocusResult = vi.fn();
    const request = { requestId: "missing", target: { ...item.target, lifetime: "obsolete" } };
    const { rerender } = render(<NotificationsView {...view} onFocusResult={onFocusResult} />);
    expect(onFocusResult).not.toHaveBeenCalled();
    rerender(<NotificationsView {...view} locked focusRequest={request} onFocusResult={onFocusResult} />);
    expect(onFocusResult).not.toHaveBeenCalled();
    rerender(<NotificationsView {...view} focusRequest={request} onFocusResult={onFocusResult} />);
    expect(onFocusResult).toHaveBeenLastCalledWith("missing", false);
    rerender(<NotificationsView {...view} filters={[{ sourceDeviceId: "phone-a", packageName: "chat", appName: "Chat", muted: true }]} focusRequest={{ requestId: "muted", target: item.target }} onFocusResult={onFocusResult} />);
    expect(onFocusResult).toHaveBeenLastCalledWith("muted", false);
  });

  it("restarts repeated activations and clears highlights on supersession and unmount", () => {
    vi.useFakeTimers();
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
    const first = notification("phone-a");
    const second = notification("phone-b");
    const view = props([first, second]);
    const { rerender, unmount } = render(<NotificationsView {...view} focusRequest={{ requestId: "one", target: first.target }} />);
    act(() => vi.advanceTimersByTime(1000));
    rerender(<NotificationsView {...view} focusRequest={{ requestId: "two", target: first.target }} />);
    act(() => vi.advanceTimersByTime(600));
    expect(screen.getAllByRole("article")[0]).toHaveAttribute("data-activation-highlight", "true");
    rerender(<NotificationsView {...view} focusRequest={{ requestId: "three", target: second.target }} />);
    expect(screen.getAllByRole("article")[0]).not.toHaveAttribute("data-activation-highlight");
    expect(screen.getAllByRole("article")[1]).toHaveFocus();
    const activeRow = screen.getAllByRole("article")[1];
    unmount();
    expect(activeRow).not.toHaveAttribute("data-activation-highlight");
    act(() => vi.advanceTimersByTime(1600));
    expect(activeRow).not.toHaveAttribute("data-activation-highlight");
  });

  it("defers a valid locked request and focuses its exact row after unlock", () => {
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
    const item = notification("phone-a");
    const view = props([notification("phone-b"), item]);
    const focusRequest = { requestId: "unlock", target: item.target };
    const onFocusResult = vi.fn();
    const { rerender } = render(<NotificationsView {...view} locked focusRequest={focusRequest} onFocusResult={onFocusResult} />);
    expect(screen.queryByRole("article")).not.toBeInTheDocument();
    expect(onFocusResult).not.toHaveBeenCalled();

    rerender(<NotificationsView {...view} focusRequest={focusRequest} onFocusResult={onFocusResult} />);
    const row = screen.getByRole("article", { name: /Chat on Pixel/ });
    expect(row).toHaveFocus();
    expect(row).toHaveAttribute("data-activation-highlight", "true");
    expect(onFocusResult).toHaveBeenCalledExactlyOnceWith("unlock", true);
  });

  it.each(["lock", "withdrawal", "removal", "replacement", "mute"] as const)(
    "clears an active highlight on %s without focusing a replacement",
    (transition) => {
      vi.useFakeTimers();
      const scroll = vi.fn();
      Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: scroll });
      const item = notification("phone-a");
      const view = props([item]);
      const focusRequest = { requestId: "active", target: item.target };
      const onFocusResult = vi.fn();
      const { rerender } = render(<NotificationsView {...view} focusRequest={focusRequest} onFocusResult={onFocusResult} />);
      const row = screen.getByRole("article");
      expect(row).toHaveAttribute("data-activation-highlight", "true");
      act(() => vi.advanceTimersByTime(500));

      rerender(<NotificationsView
        {...view}
        locked={transition === "lock"}
        focusRequest={transition === "withdrawal" ? undefined : focusRequest}
        notifications={transition === "removal" ? [] : transition === "replacement"
          ? [notification("phone-a", "chat", { target: { ...item.target, lifetime: "replacement" } })]
          : view.notifications}
        filters={transition === "mute" ? [{ sourceDeviceId: "phone-a", packageName: "chat", appName: "Chat", muted: true }] : []}
        onFocusResult={onFocusResult}
      />);

      expect(row).not.toHaveAttribute("data-activation-highlight");
      expect(scroll).toHaveBeenCalledTimes(1);
      if (transition === "removal" || transition === "replacement" || transition === "mute") {
        expect(onFocusResult).toHaveBeenLastCalledWith("active", false);
      } else {
        expect(onFocusResult).toHaveBeenCalledExactlyOnceWith("active", true);
      }
      if (transition === "replacement") {
        const replacement = screen.getByRole("article");
        expect(replacement).not.toBe(row);
        expect(replacement).not.toHaveFocus();
        expect(replacement).not.toHaveAttribute("data-activation-highlight");
      }
      act(() => vi.advanceTimersByTime(1600));
      expect(row).not.toHaveAttribute("data-activation-highlight");
      expect(document.querySelector('[data-activation-highlight="true"]')).toBeNull();
    },
  );

  it("does not refocus or extend the highlight when only the result callback identity changes", () => {
    vi.useFakeTimers();
    const scroll = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: scroll });
    const item = notification("phone-a");
    const view = props([item]);
    const focusRequest = { requestId: "stable", target: item.target };
    const onFocusResult = vi.fn();
    const { rerender } = render(<NotificationsView {...view} focusRequest={focusRequest} onFocusResult={onFocusResult} />);
    const row = screen.getByRole("article");
    act(() => vi.advanceTimersByTime(1000));
    const mute = screen.getByRole("button", { name: "Mute Chat on Pixel" });
    mute.focus();
    const nextResult = vi.fn();
    rerender(<NotificationsView {...view} focusRequest={focusRequest} onFocusResult={nextResult} />);

    expect(mute).toHaveFocus();
    expect(scroll).toHaveBeenCalledTimes(1);
    expect(onFocusResult).toHaveBeenCalledExactlyOnceWith("stable", true);
    expect(nextResult).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(599));
    expect(row).toHaveAttribute("data-activation-highlight", "true");
    act(() => vi.advanceTimersByTime(1));
    expect(row).not.toHaveAttribute("data-activation-highlight");
  });

  it("keeps same-package apps isolated by phone and hides only the muted source", () => {
    const view = props([notification("phone-a"), notification("phone-b")]);
    const { rerender } = render(<NotificationsView {...view} />);
    expect(screen.getByRole("article", { name: /Chat on Pixel/ })).toBeInTheDocument();
    expect(screen.getByRole("article", { name: /Chat on Tablet/ })).toBeInTheDocument();
    rerender(<NotificationsView {...view} filters={[{ sourceDeviceId: "phone-a", packageName: "chat", appName: "Chat", muted: true }]} />);
    expect(screen.queryByRole("article", { name: /Chat on Pixel/ })).not.toBeInTheDocument();
    expect(screen.getByRole("article", { name: /Chat on Tablet/ })).toBeInTheDocument();
  });

  it("keeps pending and non-dismissible notifications visible while disabling dismissal", () => {
    render(<NotificationsView {...props([notification("phone-a", "chat", { dismissalPending: true }), notification("phone-b", "mail", { dismissible: false })])} />);
    expect(screen.getByText(/Dismissal pending/)).toBeVisible();
    expect(screen.getByRole("button", { name: /Dismiss Title phone-a from Chat on Pixel/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: /Dismiss Title phone-b from Chat on Tablet/ })).toBeDisabled();
  });

  it("binds the synced icon and derives a readable legacy package name", () => {
    render(<NotificationsView {...props([
      notification("phone-a", "com.example.inbox", { appName: "com.example.inbox", appIconPngBase64: "fixture-icon" }),
    ])} />);

    expect(document.querySelector('[data-notification-app-icon="image"]')).toHaveAttribute("src", "data:image/png;base64,fixture-icon");
    expect(screen.getByText("Inbox")).toBeVisible();
  });

  it("falls back to the app initial when the synced icon fails to load", () => {
    render(<NotificationsView {...props([
      notification("phone-a", "com.example.chat", { appIconPngBase64: "broken-icon" }),
    ])} />);

    fireEvent.error(document.querySelector('[data-notification-app-icon="image"]')!);
    expect(screen.getByText("C", { selector: ".notif-app-icon-fallback" })).toBeVisible();
  });

  it("recovers a failed icon when the same notification receives a replacement", () => {
    const view = props([notification("phone-a", "com.example.chat", { appIconPngBase64: "broken-icon" })]);
    const { rerender } = render(<NotificationsView {...view} />);

    fireEvent.error(document.querySelector('[data-notification-app-icon="image"]')!);
    rerender(<NotificationsView {...view} notifications={[notification("phone-a", "com.example.chat", { appIconPngBase64: "replacement-icon" })]} />);

    expect(document.querySelector('[data-notification-app-icon="image"]')).toHaveAttribute("src", "data:image/png;base64,replacement-icon");
  });

  it("preserves interleaved snapshot order and mutes the exact app source", () => {
    const view = props([
      notification("phone-b", "mail", { appName: "Mail" }),
      notification("phone-a", "chat", { appName: "Chat", target: { sourceDeviceId: "phone-a", notificationKey: "chat", lifetime: "one" } }),
      notification("phone-b", "chat", { appName: "Chat", target: { sourceDeviceId: "phone-b", notificationKey: "chat", lifetime: "one" } }),
    ]);
    render(<NotificationsView {...view} />);

    expect(screen.getAllByRole("article").map(row => row.dataset.notificationKey)).toEqual(["key-phone-b", "chat", "chat"]);
    fireEvent.click(screen.getByRole("button", { name: "Mute Chat on Pixel" }));
    expect(view.onMute).toHaveBeenCalledWith({ sourceDeviceId: "phone-a", packageName: "chat", appName: "Chat", muted: false });
  });

  it("excludes locked, muted, pending, and nondismissible notifications from bulk eligibility", () => {
    const notifications = [
      notification("phone-a", "chat"),
      notification("phone-a", "mail", { dismissalPending: true }),
      notification("phone-b", "calendar", { dismissible: false }),
    ];

    const eligible = notification("phone-b", "messages");
    notifications.push(eligible);
    expect(dismissibleNotifications(notifications, [{ sourceDeviceId: "phone-a", packageName: "chat", appName: "Chat", muted: true }], false)).toEqual([eligible]);
    expect(dismissibleNotifications(notifications, [], true)).toEqual([]);
  });

  it("shows an unknown time instead of an elapsed age for a nonpositive post time", () => {
    render(<NotificationsView {...props([notification("phone-a", "chat", { postedAt: 0 })])} />);
    expect(screen.getByText("Unknown time")).toBeVisible();
  });

  it("marks rows seen after focus when they were already intersecting", async () => {
    let callback!: IntersectionObserverCallback;
    vi.stubGlobal("IntersectionObserver", class {
      constructor(next: IntersectionObserverCallback) { callback = next; }
      observe() {}
      disconnect() {}
      unobserve() {}
      takeRecords() { return []; }
      root = null;
      rootMargin = "0px";
      thresholds = [];
    });
    const view = props([notification("phone-a")]);
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    render(<NotificationsView {...view} />);
    const row = screen.getByRole("article");
    callback([{ target: row, isIntersecting: true } as unknown as IntersectionObserverEntry], {} as IntersectionObserver);
    expect(view.onSeen).not.toHaveBeenCalled();
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    fireEvent.focus(window);
    await waitFor(() => expect(view.onSeen).toHaveBeenCalledTimes(1));
  });
});
