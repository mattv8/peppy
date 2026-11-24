import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge/bridge";

let snapshot: DesktopSnapshot;
let fixtureSnapshot: DesktopSnapshot | undefined;
let publishSnapshot = () => {};

const notification = (index: number) => ({
  target: { sourceDeviceId: "phone-1", notificationKey: `notification-${index}`, lifetime: "1" },
  packageName: "com.example.chat",
  appName: "Chat",
  title: `Message ${index}`,
  text: "Searchable body",
  postedAt: Date.now() - index,
  dismissible: true,
  seen: false,
  dismissalPending: false,
});

const openNotifications = async () => {
  fireEvent.click(screen.getByRole("button", { name: /^Notifications/ }));
  await screen.findByRole("region", { name: "Notifications" });
};

const leaveScopeAndSearch = async (query = "searchable") => {
  const input = screen.getByRole("combobox", { name: "Search Peppy" });
  await waitFor(() => expect(input).not.toBeDisabled());
  fireEvent.click(await screen.findByRole("button", { name: "Search everything, not just Conversations" }));
  fireEvent.change(input, { target: { value: query } });
  await screen.findByText(/Show all \d+ in Notifications/);
};

beforeEach(async () => {
  fixtureSnapshot ??= structuredClone(await fixtureBridge.load_state());
  snapshot = structuredClone(fixtureSnapshot);
  snapshot.notifications = [notification(1)];
  vi.spyOn(bridge, "load_state").mockImplementation(async id => ({
    ...structuredClone(snapshot),
    activeConversationId: id ?? snapshot.activeConversationId,
  }));
  const listeners = new Set<() => void>();
  publishSnapshot = () => { for (const listener of listeners) listener(); };
  vi.spyOn(bridge, "subscribe").mockImplementation(listener => {
    listeners.add(listener);
    return () => { listeners.delete(listener); };
  });
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => {});
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => {});
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  vi.spyOn(bridge, "mark_notifications_seen").mockResolvedValue();
  Object.defineProperty(Element.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
  vi.spyOn(bridge, "search_local").mockResolvedValue({
    query: "",
    items: [],
    hasMore: { conversation: false, message: false, contact: false },
    indexState: "ready",
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it("filters the notification feed through the scoped titlebar search without a local search", async () => {
  render(<App />);
  fireEvent.click(screen.getByRole("button", { name: /^Notifications/ }));
  await screen.findByRole("region", { name: "Notifications" });

  const input = screen.getByRole("searchbox", { name: "Search Peppy" });
  fireEvent.change(input, { target: { value: "searchable" } });

  expect(await screen.findByText(/1 notification matches/i)).toBeInTheDocument();
  expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  expect(bridge.search_local).not.toHaveBeenCalled();
});

it("exits scoped search into the global notification group", async () => {
  render(<App />);
  fireEvent.click(screen.getByRole("button", { name: /^Notifications/ }));
  await screen.findByRole("region", { name: "Notifications" });

  fireEvent.click(screen.getByRole("button", { name: "Search everything, not just Notifications" }));
  const input = screen.getByRole("combobox");
  fireEvent.change(input, { target: { value: "searchable" } });

  await waitFor(() => expect(document.querySelector(".omni-search-group-label")).toHaveTextContent("Notifications"));
  expect(screen.getByText(/Show all 1 in Notifications/)).toBeInTheDocument();
});

it("exits scope with Backspace on an empty input", async () => {
  render(<App />);
  await openNotifications();

  const input = screen.getByRole("searchbox");
  input.focus();
  fireEvent.keyDown(input, { key: "Backspace" });

  expect(screen.queryByRole("button", { name: "Search everything, not just Notifications" })).not.toBeInTheDocument();
  expect(screen.getByRole("combobox", { name: "Search Peppy" })).toHaveAttribute("aria-expanded", "true");
});

it("clears the scoped query when leaving notifications and re-arms scope on return", async () => {
  render(<App />);
  await openNotifications();
  fireEvent.change(screen.getByRole("searchbox"), { target: { value: "searchable" } });
  await screen.findByText(/1 notification matches/i);

  fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
  fireEvent.click(screen.getByRole("button", { name: /^Notifications/ }));
  await screen.findByRole("region", { name: "Notifications" });

  expect(screen.getByRole("button", { name: "Search everything, not just Notifications" })).toBeInTheDocument();
  expect(screen.getByRole("searchbox")).toHaveValue("");
});

it("shows up to three notification hits and Show all from Conversations", async () => {
  snapshot.notifications = Array.from({ length: 4 }, (_, index) => notification(index + 1));
  render(<App />);

  await leaveScopeAndSearch();

  await screen.findByText(/Show all 4 in Notifications/);
  expect(document.querySelectorAll("[data-omni-notification-hit]")).toHaveLength(3);
});

it("refreshes notification results when a matching snapshot update arrives", async () => {
  render(<App />);
  await leaveScopeAndSearch();
  expect(screen.getByText(/Show all 1 in Notifications/)).toBeInTheDocument();

  snapshot.notifications = [notification(1), notification(2)];
  publishSnapshot();

  await screen.findByText(/Show all 2 in Notifications/);
});

it("opens and focuses the original notification target after clicking a hit", async () => {
  snapshot.notifications = [{ ...notification(1), target: { sourceDeviceId: "phone-1", notificationKey: "notification-1", lifetime: "7" } }];
  render(<App />);
  await leaveScopeAndSearch();

  fireEvent.click(document.querySelector<HTMLElement>("[data-omni-notification-hit]")!);

  await waitFor(() => expect(document.activeElement).toHaveAttribute("data-notification-key", "notification-1"));
  expect(document.activeElement).toHaveAttribute("data-notification-target", expect.stringContaining('"lifetime":"7"'));
});

it("activates a notification hit with ArrowDown and Enter", async () => {
  render(<App />);
  await leaveScopeAndSearch();
  const input = screen.getByRole("combobox", { name: "Search Peppy" });

  fireEvent.keyDown(input, { key: "ArrowDown" });
  fireEvent.keyDown(input, { key: "Home" });
  fireEvent.keyDown(input, { key: "Enter" });

  await waitFor(() => expect(document.activeElement).toHaveAttribute("data-notification-key", "notification-1"));
});

it("opens Show all with the query applied in scoped feed search", async () => {
  render(<App />);
  await leaveScopeAndSearch();

  fireEvent.click(screen.getByText(/Show all 1 in Notifications/));

  await screen.findByRole("region", { name: "Notifications" });
  expect(screen.getByRole("searchbox")).toHaveValue("searchable");
  expect(screen.getByText(/1 notification matches/i)).toBeInTheDocument();
});

it("hides scoped and global notification search while encryption is locked", async () => {
  snapshot.encryption = { ...snapshot.encryption, state: "locked" };
  render(<App />);

  fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "searchable" } });
  expect(document.querySelector("[data-search-group='notifications']")).not.toBeInTheDocument();
  await openNotifications();
  expect(screen.queryByRole("button", { name: "Search everything, not just Notifications" })).not.toBeInTheDocument();
});

it("excludes muted apps from global notification results and counts", async () => {
  snapshot.notifications = [notification(1), { ...notification(2), packageName: "com.example.muted", appName: "Muted" }];
  snapshot.appFilters = [{ sourceDeviceId: "phone-1", packageName: "com.example.muted", appName: "Muted", muted: true }];
  render(<App />);

  await leaveScopeAndSearch();

  await screen.findByText(/Show all 1 in Notifications/);
  expect(screen.queryByText("Muted")).not.toBeInTheDocument();
});

it("clears a scoped query when dismissing all notifications", async () => {
  const dismiss = vi.spyOn(bridge, "dismiss_notifications").mockImplementation(async targets => targets.map(target => ({ target, outcome: "recorded" as const })));
  render(<App />);
  await openNotifications();
  fireEvent.change(screen.getByRole("searchbox"), { target: { value: "searchable" } });
  await screen.findByText(/1 notification matches/i);

  fireEvent.click(screen.getByRole("button", { name: /Dismiss all/ }));

  expect(screen.getByRole("searchbox")).toHaveValue("");
  expect(dismiss).toHaveBeenCalledWith([expect.objectContaining({ notificationKey: "notification-1" })]);
  expect(screen.queryAllByRole("article")).toHaveLength(0);
});

it("flushes an unsaved draft before activating a notification hit", async () => {
  const saveDraft = vi.spyOn(bridge, "save_draft").mockImplementation(async input => ({ ...input, id: "draft-notification", conversationId: "conv-aurora", revision: "2" }));
  render(<App />);
  await screen.findAllByText("Can you send over the estimate?");
  fireEvent.change(screen.getByRole("textbox", { name: "Message" }), { target: { value: "Unsaved notification draft" } });
  await waitFor(() => expect(saveDraft).toHaveBeenCalled());
  await leaveScopeAndSearch();

  fireEvent.click(document.querySelector<HTMLElement>("[data-omni-notification-hit]")!);

  await waitFor(() => expect(document.activeElement).toHaveAttribute("data-notification-key", "notification-1"));
  expect(saveDraft).toHaveBeenCalled();
});

it("reports a notification that disappears before activation", async () => {
  render(<App />);
  await leaveScopeAndSearch();
  const hit = document.querySelector<HTMLElement>("[data-omni-notification-hit]")!;
  snapshot.notifications = [];
  publishSnapshot();

  fireEvent.click(hit);

  await screen.findByText("This notification is no longer available.");
});

it("does not activate a hit that becomes muted before selection", async () => {
  render(<App />);
  await leaveScopeAndSearch();
  const hit = document.querySelector<HTMLElement>("[data-omni-notification-hit]")!;
  snapshot.appFilters = [{ sourceDeviceId: "phone-1", packageName: "com.example.chat", appName: "Chat", muted: true }];
  publishSnapshot();

  fireEvent.click(hit);

  await screen.findByText("This notification is no longer available.");
});
