import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge/bridge";

let snapshot: DesktopSnapshot;
let fixtureSnapshot: DesktopSnapshot | undefined;
let stateHint = () => {};

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
};

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

const openNotifications = async () => {
  fireEvent.click(screen.getByRole("button", { name: /^Notifications/ }));
  await screen.findByRole("region", { name: "Notifications" });
};

const muteChat = () => fireEvent.click(screen.getByRole("button", { name: "Mute Chat" }));
const showOnlyChat = () => { snapshot.notifications = [notification(1)]; };

beforeEach(async () => {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  });
  fixtureSnapshot ??= structuredClone(await fixtureBridge.load_state());
  snapshot = structuredClone(fixtureSnapshot);
  vi.spyOn(bridge, "load_state").mockImplementation(async id => ({
    ...structuredClone(snapshot),
    activeConversationId: id ?? snapshot.activeConversationId,
  }));
  const listeners = new Set<() => void>();
  stateHint = () => { for (const listener of [...listeners]) listener(); };
  vi.spyOn(bridge, "subscribe").mockImplementation(listener => {
    listeners.add(listener);
    return () => { listeners.delete(listener); };
  });
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => {});
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => {});
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  vi.spyOn(bridge, "mark_notifications_seen").mockResolvedValue();
  vi.spyOn(bridge, "set_app_muted").mockResolvedValue();
  Object.defineProperty(Element.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it("mutes a row through the bridge and shows an undo toast", async () => {
  showOnlyChat();
  render(<App />);
  await openNotifications();

  muteChat();

  await waitFor(() => expect(bridge.set_app_muted).toHaveBeenCalledWith("phone-1", "com.example.chat", "Chat", true));
  expect(document.getElementById("notifications-mute-notice")).toHaveTextContent("Muted Chat.");
  expect(screen.getByRole("button", { name: "Undo" })).toBeInTheDocument();
});

it("undoes a mute through the bridge and clears the toast", async () => {
  showOnlyChat();
  render(<App />);
  await openNotifications();
  muteChat();
  await screen.findByRole("button", { name: "Undo" });

  fireEvent.click(screen.getByRole("button", { name: "Undo" }));

  await waitFor(() => expect(bridge.set_app_muted).toHaveBeenLastCalledWith("phone-1", "com.example.chat", "Chat", false));
  await waitFor(() => expect(screen.queryByRole("button", { name: "Undo" })).not.toBeInTheDocument());
});

it("moves focus to the restored app's newest row after undo", async () => {
  snapshot.notifications = [notification(2, { title: "Older" }), notification(1, { title: "Newest" })];
  vi.mocked(bridge.set_app_muted).mockImplementation(async (sourceDeviceId, packageName, appName, muted) => {
    snapshot.appFilters = [{ sourceDeviceId, packageName, appName, muted }];
  });
  render(<App />);
  await openNotifications();
  fireEvent.click(screen.getAllByRole("button", { name: "Mute Chat" })[0]);
  const undo = await screen.findByRole("button", { name: "Undo" });

  undo.focus();
  fireEvent.click(undo);

  await waitFor(() => expect(document.activeElement).toHaveAttribute("data-notification-key", "notification-1"));
});

it("does not start a second mute while the first is pending", async () => {
  showOnlyChat();
  const pending = deferred<void>();
  vi.spyOn(bridge, "set_app_muted").mockReturnValue(pending.promise);
  render(<App />);
  await openNotifications();

  muteChat();
  muteChat();

  expect(bridge.set_app_muted).toHaveBeenCalledOnce();
  await act(async () => pending.resolve());
});

it("replaces the previous mute toast when muting another app", async () => {
  snapshot.notifications = [
    notification(1),
    notification(2, { packageName: "com.example.mail", appName: "Mail" }),
  ];
  render(<App />);
  await openNotifications();

  muteChat();
  await screen.findByText("Muted Chat.");
  fireEvent.click(screen.getByRole("button", { name: "Mute Mail" }));

  await waitFor(() => expect(document.getElementById("notifications-mute-notice")).toHaveTextContent("Muted Mail."));
  expect(document.getElementById("notifications-mute-notice")).not.toHaveTextContent("Muted Chat.");
});

it("offers retry when muting fails", async () => {
  showOnlyChat();
  vi.spyOn(bridge, "set_app_muted").mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce();
  render(<App />);
  await openNotifications();

  muteChat();

  const retry = await screen.findByRole("button", { name: "Retry" });
  expect(document.getElementById("notifications-mute-notice")).toHaveTextContent("Couldn't mute Chat.");
  fireEvent.click(retry);
  await waitFor(() => expect(bridge.set_app_muted).toHaveBeenCalledTimes(2));
  expect(bridge.set_app_muted).toHaveBeenLastCalledWith("phone-1", "com.example.chat", "Chat", true);
});

it("keeps a successful mute toast when its refresh fails", async () => {
  showOnlyChat();
  render(<App />);
  await openNotifications();
  vi.mocked(bridge.load_state).mockRejectedValueOnce(new Error("offline"));

  muteChat();

  await screen.findByRole("button", { name: "Undo" });
  expect(document.getElementById("notifications-mute-notice")).toHaveTextContent("Muted Chat.");
  expect(document.getElementById("notifications-mute-notice")).not.toHaveTextContent("Couldn't mute");
});

it("drops a stale mute completion after the session locks", async () => {
  showOnlyChat();
  const pending = deferred<void>();
  vi.spyOn(bridge, "set_app_muted").mockReturnValue(pending.promise);
  render(<App />);
  await openNotifications();
  muteChat();

  snapshot.encryption.state = "locked";
  await act(async () => stateHint());
  await act(async () => pending.resolve());

  await waitFor(() => expect(screen.getByRole("combobox", { name: "Search Peppy" })).toBeDisabled());
  expect(screen.queryByRole("button", { name: "Undo" })).not.toBeInTheDocument();
});

it("dismisses pinned app-stack targets sequentially and reports a partial failure", async () => {
  localStorage.setItem("peppy.notifications.v1", JSON.stringify({ arrangement: "app" }));
  snapshot.notifications = [notification(1), notification(2), notification(3)];
  const dismiss = vi.spyOn(bridge, "dismiss_notification")
    .mockResolvedValueOnce()
    .mockRejectedValueOnce(new Error("offline"));
  render(<App />);
  await openNotifications();

  fireEvent.click(screen.getByRole("button", { name: "Dismiss 3" }));

  await waitFor(() => expect(dismiss).toHaveBeenCalledTimes(2));
  expect(dismiss.mock.calls.map(([target]) => target.notificationKey)).toEqual(["notification-1", "notification-2"]);
  await waitFor(() => expect(document.getElementById("notifications-action-notice")).toHaveTextContent("Dismissed 1 of 3."));
  expect(bridge.load_state).toHaveBeenCalledTimes(2);
});

it("holds Dismiss all disabled until an app-stack dismissal and refresh finish", async () => {
  localStorage.setItem("peppy.notifications.v1", JSON.stringify({ arrangement: "app" }));
  snapshot.notifications = [notification(1), notification(2)];
  const dismissal = deferred<void>();
  const refresh = deferred<DesktopSnapshot>();
  vi.spyOn(bridge, "dismiss_notification").mockReturnValue(dismissal.promise);
  render(<App />);
  await openNotifications();
  vi.mocked(bridge.load_state).mockReturnValueOnce(refresh.promise);

  fireEvent.click(screen.getByRole("button", { name: "Dismiss 2" }));
  expect(document.getElementById("notifications-dismiss-all")).toBeDisabled();
  await act(async () => dismissal.resolve());
  await waitFor(() => expect(bridge.load_state).toHaveBeenCalledTimes(2));
  expect(document.getElementById("notifications-dismiss-all")).toBeDisabled();
  await act(async () => refresh.resolve(snapshot));
  await waitFor(() => expect(document.getElementById("notifications-dismiss-all")).toBeEnabled());
});

it("opens the mirrored-app settings group from the muted caught-up state", async () => {
  snapshot.notifications = [];
  snapshot.appFilters = [{ sourceDeviceId: "phone-1", packageName: "com.example.chat", appName: "Chat", muted: true }];
  render(<App />);
  await openNotifications();
  const scrollContent = vi.fn();
  Object.defineProperty(HTMLElement.prototype, "scrollTo", { configurable: true, value: scrollContent });
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    if (this.id === "settings-content") return new DOMRect(0, 120, 500, 400);
    if (this.id === "notification-apps-group") return new DOMRect(0, 820, 500, 160);
    return new DOMRect();
  });

  fireEvent.click(screen.getByRole("button", { name: "1 app muted" }));

  await waitFor(() => expect(scrollContent).toHaveBeenCalled());
  expect(screen.getByRole("link", { name: "Notifications" })).toHaveAttribute("aria-current", "location");
  expect(document.getElementById("notification-apps-group")).toBeInTheDocument();
});

it("clears an active app filter when dismissing all notifications", async () => {
  snapshot.notifications = [
    notification(1),
    notification(2, { packageName: "com.example.mail", appName: "Mail" }),
  ];
  render(<App />);
  await openNotifications();
  fireEvent.click(screen.getByRole("button", { name: /Chat, 1 notification/ }));
  expect(screen.getByRole("button", { name: "Chat, 1 notification" })).toHaveAttribute("aria-pressed", "true");

  fireEvent.click(document.getElementById("notifications-dismiss-all")!);

  await waitFor(() => expect(screen.getByRole("button", { name: "All" })).toHaveAttribute("aria-pressed", "true"));
});

it("uses a resolved contact display name in the notification row accessible name", async () => {
  snapshot.notifications = [notification(1, { title: "+12025550100" })];
  snapshot.contactResolution = {
    "+12025550100": { contactId: "c1", bookId: "b1", displayName: "Ada Lovelace" },
  };
  render(<App />);
  await openNotifications();

  expect(await screen.findByRole("article", { name: "Ada Lovelace" })).toBeInTheDocument();
});
