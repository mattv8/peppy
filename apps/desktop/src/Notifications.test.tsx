import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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

afterEach(cleanup);

describe("NotificationsView", () => {
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
