import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
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

it("dismisses app-stack targets in one bulk call", async () => {
  localStorage.setItem("peppy.notifications.v1", JSON.stringify({ arrangement: "app" }));
  snapshot.notifications = [notification(1), notification(2), notification(3)];
  const dismiss = vi.spyOn(bridge, "dismiss_notifications").mockImplementation(async targets => targets.map(target => ({ target, outcome: "recorded" as const })));
  render(<App />);
  await openNotifications();

  fireEvent.click(screen.getByRole("button", { name: "Dismiss 3" }));

  await waitFor(() => expect(dismiss).toHaveBeenCalledTimes(1));
  expect(dismiss.mock.calls[0][0].map(target => target.notificationKey)).toEqual(["notification-1", "notification-2", "notification-3"]);
  await waitFor(() => expect(bridge.load_state).toHaveBeenCalledTimes(2));
});

it("reports a failed app-stack bulk dismissal and returns its rows with Try again", async () => {
  localStorage.setItem("peppy.notifications.v1", JSON.stringify({ arrangement: "app" }));
  snapshot.notifications = [notification(1), notification(2), notification(3)];
  vi.spyOn(bridge, "dismiss_notifications").mockRejectedValue(new Error("offline"));
  render(<App />);
  await openNotifications();

  fireEvent.click(screen.getByRole("button", { name: "Dismiss 3" }));

  expect(await screen.findByText("Couldn't dismiss 3 notifications. Peppy couldn't save this.")).toBeVisible();
  // A collapsed app stack shows its first two rows; the third returns behind "Show 1 more".
  expect(screen.getAllByRole("article")).toHaveLength(2);
  expect(screen.getByRole("button", { name: "Dismiss 3" })).toBeEnabled();
});

it("hides app-stack rows at once, disables Dismiss all, and sends no second dispatch", async () => {
  localStorage.setItem("peppy.notifications.v1", JSON.stringify({ arrangement: "app" }));
  snapshot.notifications = [notification(1), notification(2)];
  const dismissal = deferred<{ target: DesktopSnapshot["notifications"][number]["target"]; outcome: "recorded" }[]>();
  const dismiss = vi.spyOn(bridge, "dismiss_notifications").mockReturnValue(dismissal.promise);
  render(<App />);
  await openNotifications();

  fireEvent.click(screen.getByRole("button", { name: "Dismiss 2" }));
  expect(screen.queryAllByRole("article")).toHaveLength(0);
  expect(document.getElementById("notifications-dismiss-all")).toBeDisabled();
  expect(dismiss).toHaveBeenCalledTimes(1);

  await act(async () => dismissal.resolve(dismiss.mock.calls[0][0].map(target => ({ target, outcome: "recorded" as const }))));
  await waitFor(() => expect(bridge.load_state).toHaveBeenCalledTimes(2));
  expect(dismiss).toHaveBeenCalledTimes(1);
  expect(document.getElementById("notifications-dismiss-all")).toBeDisabled();
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

describe("notification dismissal integration", () => {
  const recorded = (targets: { sourceDeviceId: string; notificationKey: string; lifetime: string }[]) =>
    targets.map(target => ({ target, outcome: "recorded" as const }));

  it("hides a dismissed row before the bridge resolves and announces it", async () => {
    snapshot.notifications = [notification(1)];
    const dismissal = deferred<ReturnType<typeof recorded>>();
    vi.spyOn(bridge, "dismiss_notifications").mockReturnValue(dismissal.promise);
    render(<App />);
    await openNotifications();

    fireEvent.click(screen.getByRole("button", { name: /^Dismiss Message 1/ }));

    expect(screen.queryAllByRole("article")).toHaveLength(0);
    await waitFor(() => expect(document.getElementById("notifications-action-announcer")).toHaveTextContent("Dismissed Message 1 from Chat."));
    expect(document.getElementById("notifications-announcer")).toBeEmptyDOMElement();
    await act(async () => dismissal.resolve(recorded([notification(1).target])));
  });

  it("returns a row after a failed dismissal and retries it from Try again", async () => {
    snapshot.notifications = [notification(1)];
    const dismiss = vi.spyOn(bridge, "dismiss_notifications")
      .mockRejectedValueOnce(new Error("offline"))
      .mockImplementationOnce(async targets => recorded(targets));
    render(<App />);
    await openNotifications();

    fireEvent.click(screen.getByRole("button", { name: /^Dismiss Message 1/ }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't dismiss. Peppy couldn't save this.");
    const row = screen.getByRole("article");
    expect(row).toHaveAttribute("data-dismiss-state", "failed");
    fireEvent.click(screen.getByRole("button", { name: "Try again to dismiss Message 1 from Chat" }));
    await waitFor(() => expect(dismiss).toHaveBeenCalledTimes(2));
    expect(screen.queryAllByRole("article")).toHaveLength(0);
  });

  it("keeps the row hidden while an update in progress is retried", async () => {
    snapshot.notifications = [notification(1)];
    const busy = Object.assign(new Error("busy"), { code: "update-in-progress" });
    const dismiss = vi.spyOn(bridge, "dismiss_notifications")
      .mockRejectedValueOnce(busy)
      .mockImplementationOnce(async targets => recorded(targets));
    render(<App />);
    await openNotifications();

    fireEvent.click(screen.getByRole("button", { name: /^Dismiss Message 1/ }));

    expect(screen.queryAllByRole("article")).toHaveLength(0);
    await waitFor(() => expect(dismiss).toHaveBeenCalledTimes(2), { timeout: 4000 });
    expect(screen.queryAllByRole("article")).toHaveLength(0);
  });

  it("announces that a dismissed row of a blocked phone cannot clear yet", async () => {
    const phone = snapshot.gateways[0].id;
    snapshot.notifications = [notification(1, { target: { sourceDeviceId: phone, notificationKey: "blocked-1", lifetime: "1" } })];
    snapshot.notificationDismissalBlocks = [{ sourceDeviceId: phone, reason: "mirroringOff", observedAtMs: Date.now() }];
    vi.spyOn(bridge, "dismiss_notifications").mockImplementation(async targets => recorded(targets));
    render(<App />);
    await openNotifications();

    fireEvent.click(screen.getByRole("button", { name: /^Dismiss Message 1/ }));

    await waitFor(() => expect(document.getElementById("notifications-action-announcer")).toHaveTextContent("Dismissed Message 1 from Chat. Pixel 8 can't clear it until the notice above is resolved."));
  });

  it("dismiss all sends the 100 newest eligible rows, not the first 100 in list order", async () => {
    snapshot.notifications = Array.from({ length: 101 }, (_, index) => notification(index + 1));
    const dismiss = vi.spyOn(bridge, "dismiss_notifications").mockImplementation(async targets => recorded(targets));
    render(<App />);
    await openNotifications();

    fireEvent.click(document.getElementById("notifications-dismiss-all")!);

    await waitFor(() => expect(dismiss).toHaveBeenCalledOnce());
    const sent = dismiss.mock.calls[0][0].map(target => target.notificationKey);
    expect(sent).toHaveLength(100);
    expect(sent[0]).toBe("notification-1");
    expect(sent[99]).toBe("notification-100");
    expect(sent).not.toContain("notification-101");
  });

  it("disables every dismiss control while writes are paused", async () => {
    snapshot.notifications = [notification(1), notification(2)];
    snapshot.writesPaused = true;
    render(<App />);
    await openNotifications();

    for (const button of screen.getAllByRole("button", { name: /^Dismiss / })) expect(button).toBeDisabled();
    expect(document.getElementById("notifications-dismiss-all")).toBeDisabled();
  });

  it("shows a banner for a blocked phone from the snapshot and reveals its waiting rows", async () => {
    const phone = snapshot.gateways[0].id;
    snapshot.notifications = [notification(1, { target: { sourceDeviceId: phone, notificationKey: "waiting-1", lifetime: "1" }, dismissalPending: true })];
    snapshot.notificationDismissalBlocks = [{ sourceDeviceId: phone, reason: "notificationAccessOff", observedAtMs: Date.now() - 180_000 }];
    render(<App />);
    await openNotifications();

    const banner = document.querySelector<HTMLElement>("[data-phone-blocker]")!;
    expect(banner).toHaveAttribute("data-blocker-reason", "notificationAccessOff");
    expect(banner).toHaveTextContent("Notification access is off on Pixel 8");
    expect(banner).toHaveTextContent("Reported 3m ago.");
    fireEvent.click(within(banner).getByRole("button", { name: "Show 1 dismissed" }));
    expect(document.querySelector("#notifications-blocker-groups [data-hidden-group]")).toHaveAttribute("data-hidden-source", "blocker");
    expect(screen.queryByRole("button", { name: "1 waiting for phone" })).not.toBeInTheDocument();
  });

  it("reveals locked waiting rows from the waiting toggle", async () => {
    snapshot.notifications = [notification(1, { dismissalPending: true }), notification(2)];
    render(<App />);
    await openNotifications();

    fireEvent.click(screen.getByRole("button", { name: "1 waiting for phone" }));

    const waiting = document.querySelector<HTMLElement>("#notifications-waiting-groups article")!;
    expect(waiting).toHaveAttribute("data-dismiss-state", "pending");
    expect(within(waiting).queryByRole("button", { name: /^Dismiss / })).not.toBeInTheDocument();
    expect(screen.getAllByRole("article")).toHaveLength(2);
  });

  it("collapses three blocked phones into one grouped banner", async () => {
    const base = snapshot.gateways[0];
    const phones = ["phone-x", "phone-y", "phone-z"].map((id, index) => ({ ...base, id, name: `Phone ${index + 1}` }));
    snapshot.gateways = phones;
    snapshot.notifications = phones.map((phone, index) => notification(index + 1, { target: { sourceDeviceId: phone.id, notificationKey: `waiting-${index}`, lifetime: "1" }, dismissalPending: true }));
    snapshot.notificationDismissalBlocks = phones.map(phone => ({ sourceDeviceId: phone.id, reason: "mirroringOff", observedAtMs: Date.now() }));
    render(<App />);
    await openNotifications();

    expect(document.querySelectorAll("[data-phone-blocker-group]")).toHaveLength(1);
    expect(document.getElementById("phone-blockers-group")).toHaveAttribute("data-phone-blocker-group", "3");
    expect(document.getElementById("phone-blockers-group")).toHaveTextContent("3 phones can't clear notifications");
    expect(document.querySelectorAll("[data-phone-blocker]")).toHaveLength(0);
  });
});
