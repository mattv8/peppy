import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { NotificationSettings } from "./NotificationSettings";
import { notificationPersistenceSettings, notificationQuickReplySettings } from "../App";
import { StrictMode } from "react";
import type { AppFilter, MirroredNotification, NotificationCheck, NotificationPreferences } from "../bridge/bridge";
import type { NotificationPersistenceSettings } from "@peppy/desktop-ui/bridge-contracts";

afterEach(cleanup);

const CHECK = "Send test notification";
const MUTE = "Mute while a chat head is open";
const OPEN = "Open notification settings";
const preferences: NotificationPreferences = { messageBanners: true, mirroredBanners: true, preview: "full" };
const filter: AppFilter = { sourceDeviceId: "one", packageName: "chat", appName: "Chat", muted: true };
const notification: MirroredNotification = {
  target: { sourceDeviceId: "one", notificationKey: "key", lifetime: "life" },
  packageName: "chat", appName: "Chat", title: "Hello", text: "Text", postedAt: 0,
  dismissible: true, seen: false, dismissalPending: false,
};
const check = (status: NotificationCheck["status"], extra: Partial<NotificationCheck> = {}): NotificationCheck =>
  ({ status, testSent: status !== "denied", canOpenSettings: true, ...extra });
const props = () => ({
  notifications: [notification], filters: [filter], sources: [{ id: "one", name: "Pixel" }],
  preferences, headsSupported: true, locked: false, onPreferences: vi.fn().mockResolvedValue(undefined),
  onMute: vi.fn().mockResolvedValue(undefined), onPermission: vi.fn().mockResolvedValue(undefined),
  onOpenSettings: vi.fn().mockResolvedValue(undefined),
});
const LOCKED_NOTE = "Unlock Peppy to change banner settings.";
const SYNC_NOTE = /Banner choices apply to every computer and browser in this vault/;
const persistenceSettings = (value: NotificationPersistenceSettings = { seconds: 5, supported: true }) => ({
  load: vi.fn().mockResolvedValue(value),
  save: vi.fn().mockResolvedValue(undefined),
});
const quickReplySettings = (value = { enabled: false, supported: true }) => ({
  load: vi.fn().mockResolvedValue(value),
  save: vi.fn().mockResolvedValue(undefined),
});

it("hides quick reply without a supported adapter and saves only after loading", async () => {
  const input = props();
  const settings = quickReplySettings();
  const { rerender } = render(<NotificationSettings {...input} />);
  expect(screen.queryByRole("switch", { name: "Quick reply in notifications" })).not.toBeInTheDocument();
  rerender(<NotificationSettings {...input} quickReplySettings={settings} />);
  const toggle = await screen.findByRole("switch", { name: "Quick reply in notifications" });
  expect(toggle).toBeEnabled();
  fireEvent.click(toggle);
  await waitFor(() => expect(settings.save).toHaveBeenCalledWith(true));
});

it("preserves quick reply's applied state after a failed save", async () => {
  const settings = quickReplySettings({ enabled: true, supported: true });
  settings.save.mockRejectedValue(new Error("disk"));
  render(<NotificationSettings {...props()} quickReplySettings={settings} />);
  const toggle = await screen.findByRole("switch", { name: "Quick reply in notifications" });
  expect(toggle).toBeChecked();
  fireEvent.click(toggle);
  expect(await screen.findByRole("alert")).toHaveTextContent("Could not save quick reply settings. Try again.");
  expect(toggle).toBeChecked();
});

it("binds quick reply adapter methods to class-based bridges", async () => {
  class NativeBridge {
    enabled = false;
    async get_notification_quick_reply() { return { enabled: this.enabled, supported: true }; }
    async set_notification_quick_reply(enabled: boolean) { this.enabled = enabled; }
  }
  const bridge = new NativeBridge();
  const adapter = notificationQuickReplySettings(bridge);
  expect(await adapter?.load()).toEqual({ enabled: false, supported: true });
  await adapter?.save(true);
  expect(bridge.enabled).toBe(true);
});

it("binds persistence adapter methods to class-based bridges", async () => {
  class BrowserBridge {
    value = 45;

    async get_notification_persistence() {
      return { seconds: this.value, supported: true };
    }

    async set_notification_persistence(value: { seconds: number | null }) {
      this.value = value.seconds ?? 0;
    }
  }
  const bridge = new BrowserBridge();
  const adapter = notificationPersistenceSettings(bridge);

  expect(await adapter?.load()).toEqual({ seconds: 45, supported: true });
  await adapter?.save({ seconds: 30 });
  expect(bridge.value).toBe(30);
});

it("saves sender-or-app-only previews as hidden previews with source visibility", async () => {
  const value = props();
  render(<NotificationSettings {...value} />);

  fireEvent.change(screen.getByLabelText("Banner preview"), { target: { value: "source" } });

  await waitFor(() => expect(value.onPreferences).toHaveBeenCalledWith({ ...preferences, preview: "hidden", showSource: true }));
});

it("keeps the confirmed source-only selection through transitions and rejected saves", async () => {
  const sourcePreferences = { ...preferences, preview: "hidden" as const, showSource: true };
  const value = { ...props(), preferences: sourcePreferences, onPreferences: vi.fn().mockRejectedValue(new Error("rejected")) };
  render(<NotificationSettings {...value} />);

  expect(screen.getByLabelText("Banner preview")).toHaveValue("source");
  fireEvent.change(screen.getByLabelText("Banner preview"), { target: { value: "full" } });
  await waitFor(() => expect(value.onPreferences).toHaveBeenCalledWith({ ...sourcePreferences, preview: "full", showSource: false }));
  expect(screen.getByLabelText("Banner preview")).toHaveValue("source");

  fireEvent.click(screen.getByRole("switch", { name: "SMS/MMS banners" }));
  await waitFor(() => expect(value.onPreferences).toHaveBeenCalledWith({ ...sourcePreferences, messageBanners: false }));
});

it("maps source-only transitions to full and generic values", async () => {
  const sourcePreferences = { ...preferences, preview: "hidden" as const, showSource: true };
  const value = { ...props(), preferences: sourcePreferences };
  render(<NotificationSettings {...value} />);

  fireEvent.change(screen.getByLabelText("Banner preview"), { target: { value: "hidden" } });
  await waitFor(() => expect(value.onPreferences).toHaveBeenCalledWith({ ...sourcePreferences, preview: "hidden", showSource: false }));
});

it("keeps the full selection authoritative and pending saves non-optimistic", async () => {
  let resolve!: () => void;
  const pending = new Promise<void>(done => { resolve = done; });
  const value = { ...props(), preferences: { ...preferences, showSource: true }, onPreferences: vi.fn().mockReturnValue(pending) };
  render(<NotificationSettings {...value} />);

  expect(screen.getByLabelText("Banner preview")).toHaveValue("full");
  fireEvent.change(screen.getByLabelText("Banner preview"), { target: { value: "source" } });
  expect(screen.getByLabelText("Banner preview")).toHaveValue("full");
  expect(screen.getByLabelText("Banner preview")).toBeDisabled();
  resolve();
});

it("loads local notification sound and saves only after the user toggles it", async () => {
  const soundSettings = { load: vi.fn().mockResolvedValue(false), save: vi.fn().mockResolvedValue(undefined) };
  const input = props();
  render(<NotificationSettings {...input} soundSettings={soundSettings} />);
  const toggle = screen.getByRole("switch", { name: "Notification sounds" });
  await waitFor(() => expect(toggle).toBeEnabled());
  expect(toggle).not.toBeChecked();
  fireEvent.click(toggle);
  await waitFor(() => expect(toggle).toBeChecked());
  expect(soundSettings.save).toHaveBeenCalledWith(true);
  expect(input.onPreferences).not.toHaveBeenCalled();
});

it("keeps sound off and reports a failed local save", async () => {
  const soundSettings = { load: vi.fn().mockResolvedValue(false), save: vi.fn().mockRejectedValue(new Error("disk")) };
  render(<NotificationSettings {...props()} soundSettings={soundSettings} />);
  const toggle = screen.getByRole("switch", { name: "Notification sounds" });
  await waitFor(() => expect(toggle).toBeEnabled());
  fireEvent.click(toggle);
  await screen.findByRole("alert");
  expect(toggle).not.toBeChecked();
});

it("does not expose native sound settings without a native adapter or mutate them while locked", () => {
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} />);
  expect(screen.queryByRole("switch", { name: "Notification sounds" })).not.toBeInTheDocument();
  const soundSettings = { load: vi.fn().mockResolvedValue(true), save: vi.fn() };
  rerender(<NotificationSettings {...input} locked soundSettings={soundSettings} />);
  const toggle = screen.getByRole("switch", { name: "Notification sounds" });
  expect(toggle).toBeDisabled();
  fireEvent.click(toggle);
  expect(soundSettings.save).not.toHaveBeenCalled();
});

it("explains that banner choices sync to the vault and that permission stays per device", () => {
  render(<NotificationSettings {...props()} />);
  expect(screen.getByText(SYNC_NOTE)).toHaveAttribute("id", "notification-banner-sync-note");
  expect(screen.getByText(/System notification permission is set on each device/)).toBeVisible();
  expect(screen.queryByText(LOCKED_NOTE)).not.toBeInTheDocument();
});

it("keeps the locked explanation as the only persistent heads-toggle description", () => {
  render(<NotificationSettings {...props()} />);
  expect(screen.getByRole("switch", { name: MUTE })).not.toHaveAttribute("aria-describedby");
  expect(screen.getByRole("switch", { name: "SMS/MMS banners" })).not.toHaveAttribute("aria-describedby");
  expect(screen.getByLabelText("Banner preview")).toBeEnabled();
});

it("disables vault-synced banner controls while locked and explains why", () => {
  const input = props();
  render(<NotificationSettings {...input} locked />);

  expect(screen.getByText(SYNC_NOTE)).toBeVisible();
  expect(screen.getByText(LOCKED_NOTE)).toHaveAttribute("id", "notification-banner-locked-note");
  expect(screen.getByRole("switch", { name: "SMS/MMS banners" })).toBeDisabled();
  expect(screen.getByRole("switch", { name: MUTE })).toBeDisabled();
  expect(screen.getByRole("switch", { name: "App notification banners" })).toBeDisabled();
  expect(screen.getByLabelText("Banner preview")).toBeDisabled();
  expect(screen.getByRole("switch", { name: "SMS/MMS banners" })).toHaveAttribute("aria-describedby", "notification-banner-locked-note");
  expect(screen.getByRole("switch", { name: "App notification banners" })).toHaveAttribute("aria-describedby", "notification-banner-locked-note");
  expect(screen.getByLabelText("Banner preview")).toHaveAttribute("aria-describedby", "notification-banner-locked-note");
  expect(screen.getByRole("switch", { name: MUTE })).toHaveAttribute("aria-describedby", "notification-banner-locked-note");
});

it("keeps local permission checks and per-app mute controls available while locked", async () => {
  const input = props();
  input.onPermission.mockResolvedValue(check("granted"));
  render(<NotificationSettings {...input} locked />);

  expect(screen.getByRole("button", { name: CHECK })).toBeEnabled();
  fireEvent.click(screen.getByRole("button", { name: CHECK }));
  expect(await screen.findByRole("status")).toHaveTextContent(/allowed to show notifications/i);
  fireEvent.click(screen.getByText("Per-app mirroring"));
  const control = screen.getByRole("switch", { name: "Mirror Chat from Pixel" });
  expect(control).toBeEnabled();
  fireEvent.click(control);
  await waitFor(() => expect(input.onMute).toHaveBeenCalledWith({ ...filter, muted: false }));
});

it("never saves a banner preference while locked", () => {
  const input = props();
  render(<NotificationSettings {...input} locked />);
  fireEvent.click(screen.getByRole("switch", { name: "SMS/MMS banners" }));
  fireEvent.click(screen.getByRole("switch", { name: MUTE }));
  fireEvent.click(screen.getByRole("switch", { name: "App notification banners" }));
  fireEvent.change(screen.getByLabelText("Banner preview"), { target: { value: "hidden" } });

  expect(input.onPreferences).not.toHaveBeenCalled();
});

it("re-enables banner controls once unlocked", () => {
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} locked />);
  rerender(<NotificationSettings {...input} locked={false} />);

  expect(screen.queryByText(LOCKED_NOTE)).not.toBeInTheDocument();
  expect(screen.getByRole("switch", { name: "SMS/MMS banners" })).toBeEnabled();
  expect(screen.getByLabelText("Banner preview")).toBeEnabled();
});

it("shows banner controls when either desktop banner preference is enabled", () => {
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} preferences={{ ...preferences, messageBanners: false, mirroredBanners: false }} />);
  expect(screen.queryByLabelText("Banner preview")).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: CHECK })).not.toBeInTheDocument();

  rerender(<NotificationSettings {...input} preferences={{ ...preferences, messageBanners: true, mirroredBanners: false }} />);
  expect(screen.getByLabelText("Banner preview")).toBeVisible();
  expect(screen.getByRole("button", { name: CHECK })).toBeVisible();
});

it("shows the chat-head mute toggle only for SMS/MMS banners on platforms with heads", () => {
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} />);
  expect(screen.getByRole("switch", { name: MUTE })).toBeChecked();
  expect(screen.getByRole("switch", { name: MUTE })).not.toHaveAttribute("aria-describedby");
  rerender(<NotificationSettings {...input} preferences={{ ...preferences, messageBanners: false }} />);
  expect(screen.queryByRole("switch", { name: MUTE })).not.toBeInTheDocument();
  rerender(<NotificationSettings {...input} headsSupported={false} />);
  expect(screen.queryByRole("switch", { name: MUTE })).not.toBeInTheDocument();
});

it("defaults missing muteWithHeads to on and persists a changed choice while saving", async () => {
  let resolve!: () => void;
  const input = props();
  input.onPreferences.mockImplementation(() => new Promise<void>(done => { resolve = done; }));
  render(<NotificationSettings {...input} />);

  const toggle = screen.getByRole("switch", { name: MUTE });
  expect(toggle).toBeChecked();
  fireEvent.click(toggle);
  expect(input.onPreferences).toHaveBeenCalledWith({ ...preferences, muteWithHeads: false });
  expect(toggle).toBeDisabled();
  expect(screen.getByRole("button", { name: CHECK })).toBeDisabled();
  await act(async () => resolve());
  expect(toggle).toBeEnabled();
});

it.each([
  ["granted", true, /test notification sent.*allowed/i],
  ["granted", false, /could not be sent/i],
  ["quiet", true, /deliver quietly.*no banner may pop up/i],
  ["quiet", false, /deliver quietly.*could not be sent/i],
  ["denied", true, /turned off for peppy/i],
  ["unknown", true, /test notification sent.*doesn't report/i],
  ["unknown", false, /doesn't report.*could not be sent/i],
] as const)("reports %s notification status honestly", async (status, testSent, expectedStatus) => {
  const input = props();
  input.onPermission.mockResolvedValue(check(status, { testSent }));
  render(<NotificationSettings {...input} />);
  fireEvent.click(screen.getByRole("button", { name: CHECK }));

  expect(await screen.findByText(expectedStatus)).toBeVisible();
  expect(screen.getByRole("button", { name: OPEN })).toBeVisible();
});

it("hides the settings button when the platform has no settings page", async () => {
  const input = props();
  input.onPermission.mockResolvedValue(check("unknown", { canOpenSettings: false }));
  render(<NotificationSettings {...input} />);
  fireEvent.click(screen.getByRole("button", { name: CHECK }));
  await screen.findByRole("status");
  expect(screen.queryByRole("button", { name: OPEN })).not.toBeInTheDocument();
});

it("shows pending check status and hides the result button until the request resolves", async () => {
  let resolve!: (result: NotificationCheck) => void;
  const input = props();
  input.onPermission.mockImplementation(() => new Promise<NotificationCheck>(done => { resolve = done; }));
  render(<NotificationSettings {...input} />);
  fireEvent.click(screen.getByRole("button", { name: CHECK }));

  expect(screen.getByRole("status")).toHaveTextContent("Sending test notification");
  expect(screen.queryByRole("button", { name: OPEN })).not.toBeInTheDocument();
  await act(async () => resolve(check("granted")));
  expect(await screen.findByRole("status")).toHaveTextContent(/allowed to show notifications/i);
  expect(screen.getByRole("button", { name: OPEN })).toBeVisible();
});

it("reports check and settings failures while preserving a retryable result", async () => {
  const input = props();
  input.onPermission.mockRejectedValueOnce(new Error("failure"));
  render(<NotificationSettings {...input} />);
  fireEvent.click(screen.getByRole("button", { name: CHECK }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Could not send test notification. Try again.");

  input.onPermission.mockResolvedValue(check("denied"));
  fireEvent.click(screen.getByRole("button", { name: CHECK }));
  await screen.findByRole("status");
  input.onOpenSettings.mockRejectedValueOnce(new Error("failure"));
  fireEvent.click(screen.getByRole("button", { name: OPEN }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Could not open notification settings.");
  expect(screen.getByRole("button", { name: OPEN })).toBeEnabled();
  fireEvent.click(screen.getByRole("button", { name: OPEN }));
  await waitFor(() => expect(input.onOpenSettings).toHaveBeenCalledTimes(2));
});

it("keeps the stored mute choice over observed defaults and allows unmuting that phone while desktop banners are off", async () => {
  const input = props();
  render(<NotificationSettings {...input} preferences={{ ...preferences, messageBanners: false, mirroredBanners: false }} />);
  fireEvent.click(screen.getByText("Per-app mirroring"));
  const control = screen.getByRole("switch", { name: "Mirror Chat from Pixel" });
  expect(control).not.toBeChecked();
  fireEvent.click(control);
  await waitFor(() => expect(input.onMute).toHaveBeenCalledWith({ ...filter, muted: false }));
});

it("hides the saved preview without changing it when both desktop banners are off", () => {
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} />);
  rerender(<NotificationSettings {...input} preferences={{ ...preferences, messageBanners: false, mirroredBanners: false, preview: "hidden" }} />);

  expect(screen.queryByLabelText("Banner preview")).not.toBeInTheDocument();
  expect(input.onPreferences).not.toHaveBeenCalled();
  expect(input.onPermission).not.toHaveBeenCalled();

  rerender(<NotificationSettings {...input} preferences={{ ...preferences, messageBanners: true, mirroredBanners: false, preview: "hidden" }} />);
  expect(screen.getByLabelText("Banner preview")).toHaveValue("hidden");
});

it("uses friendly names for legacy package labels without changing mute payloads", async () => {
  const input = props();
  const legacyFilter = { ...filter, packageName: "com.example.chat", appName: "com.example.chat" };
  render(<NotificationSettings {...input} filters={[legacyFilter]} notifications={[{ ...notification, appName: legacyFilter.appName }]} />);
  fireEvent.click(screen.getByText("Per-app mirroring"));

  expect(screen.getByText("Chat")).toBeVisible();
  expect(screen.getByRole("switch", { name: "Mirror Chat from Pixel" })).toBeInTheDocument();
  fireEvent.click(screen.getByRole("switch", { name: "Mirror Chat from Pixel" }));
  await waitFor(() => expect(input.onMute).toHaveBeenCalledWith({ ...legacyFilter, muted: false }));
});

it("disables preferences while saving so a second click cannot submit a stale snapshot", async () => {
  let resolve!: () => void;
  const input = props();
  input.onPreferences.mockImplementation(() => new Promise<void>(done => { resolve = done; }));
  const { rerender } = render(<NotificationSettings {...input} />);
  fireEvent.click(screen.getByRole("switch", { name: "SMS/MMS banners" }));
  expect(input.onPreferences).toHaveBeenCalledWith({ ...preferences, messageBanners: false });
  expect(screen.getByRole("switch", { name: "App notification banners" })).toBeDisabled();
  expect(screen.getByLabelText("Banner preview")).toBeDisabled();
  rerender(<NotificationSettings {...input} preferences={{ ...preferences, messageBanners: false }} />);
  await act(async () => resolve());
  expect(screen.getByRole("switch", { name: "SMS/MMS banners" })).not.toBeChecked();
  expect(screen.getByLabelText("Banner preview")).toBeEnabled();
});

it("keeps unknown phones distinguishable and keeps save errors visible after app rules close", async () => {
  const input = props();
  input.onMute.mockRejectedValue(new Error("failure"));
  render(<NotificationSettings {...input} sources={[]} filters={[filter, { ...filter, sourceDeviceId: "two" }]} />);
  fireEvent.click(screen.getByText("Per-app mirroring"));
  expect(screen.getByRole("switch", { name: "Mirror Chat from Phone 2" })).not.toBeChecked();
  fireEvent.click(screen.getByRole("switch", { name: "Mirror Chat from Phone 1" }));
  expect(await screen.findByRole("alert")).toBeVisible();
  fireEvent.click(screen.getByText("Per-app mirroring"));
  expect(screen.getByRole("alert")).toBeVisible();
  expect(screen.getByRole("switch", { name: "Mirror Chat from Phone 1" })).toBeEnabled();
});

it("does not save, request permission, or change muted app controls when app rules are disclosed", () => {
  const input = props();
  render(<NotificationSettings {...input} />);
  const disclosure = screen.getByText("Per-app mirroring");
  fireEvent.click(disclosure);
  fireEvent.click(disclosure);

  expect(input.onPreferences).not.toHaveBeenCalled();
  expect(input.onMute).not.toHaveBeenCalled();
  expect(input.onPermission).not.toHaveBeenCalled();
  expect(input.onOpenSettings).not.toHaveBeenCalled();
});

it("does not render an empty app rules disclosure", () => {
  const input = props();
  render(<NotificationSettings {...input} notifications={[]} filters={[]} />);

  expect(screen.queryByText("Per-app mirroring")).not.toBeInTheDocument();
  expect(screen.queryByRole("list", { name: "App mirroring filters" })).not.toBeInTheDocument();
});

it("hides the local group when no local notification setting is available", async () => {
  const unsupported = quickReplySettings({ enabled: false, supported: false });
  const { rerender } = render(<NotificationSettings {...props()} />);

  expect(document.querySelector("#notification-local-group")).not.toBeInTheDocument();
  rerender(<NotificationSettings {...props()} quickReplySettings={unsupported} />);
  await waitFor(() => expect(document.querySelector("#notification-local-group")).not.toBeInTheDocument());
});

it("keeps a local load error visible without exposing off-banner local controls", async () => {
  const input = props();
  const soundSettings = { load: vi.fn().mockResolvedValue(true), save: vi.fn().mockResolvedValue(undefined) };
  const durationSettings = persistenceSettings({ seconds: 45, supported: true });
  const replySettings = quickReplySettings();
  replySettings.load.mockRejectedValue(new Error("disk"));
  render(<NotificationSettings {...input} preferences={{ ...preferences, messageBanners: false, mirroredBanners: false }} soundSettings={soundSettings} persistenceSettings={durationSettings} quickReplySettings={replySettings} />);

  const error = await screen.findByRole("alert");
  const localGroup = document.querySelector("#notification-local-group");

  expect(localGroup).toContainElement(error);
  expect(screen.queryByRole("switch", { name: "Notification sounds" })).not.toBeInTheDocument();
  expect(screen.queryByLabelText("Banner duration")).not.toBeInTheDocument();
});


it.each([
  ["windows", 5, "5"],
  ["windows", 30, "25"],
  ["windows", 60, "25"],
  ["windows", 45, "25"],
  ["windows", 120, "25"],
  ["windows", null, "indefinite"],
  ["linux", 5, "5"],
  ["linux", 30, "30"],
  ["linux", 60, "60"],
  ["linux", 45, "45"],
  ["linux", 120, "120"],
  ["linux", null, "indefinite"],
] as const)("renders %s persistence for %s seconds", async (platform, seconds, selection) => {
  const settings = persistenceSettings({ seconds, supported: true, platform });
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);

  expect(await screen.findByLabelText("Banner duration")).toHaveValue(selection);
  if (platform === "linux" && seconds !== null && ![5, 15, 30].includes(seconds)) {
    expect(screen.getByRole("option", { name: /\(current\)$/ })).toBeInTheDocument();
  }
});

it.each([
  ["macos", 5],
  ["macos", 30],
  ["macos", 60],
  ["macos", 45],
  ["macos", 120],
  ["macos", null],
] as const)("renders macOS status for %s seconds", async (_platform, seconds) => {
  const settings = persistenceSettings({ seconds, supported: true, platform: "macos", macosStyle: "banners" });
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);

  expect(await screen.findByRole("status")).toHaveTextContent("Current style: Banners. They fade on their own.");
  expect(screen.getByRole("button", { name: "Open System Settings" })).toBeEnabled();
});

it.each([
  ["browser", 5],
  ["browser", 30],
  ["browser", 60],
  ["browser", 45],
  ["browser", 120],
  ["browser", null],
] as const)("renders browser persistence for %s seconds", async (_platform, seconds) => {
  const settings = persistenceSettings({ seconds, supported: true, platform: "browser" });
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);

  const toggle = await screen.findByRole("switch", { name: "Keep banners until dismissed" });
  if (seconds === null) expect(toggle).toBeChecked();
  else expect(toggle).not.toBeChecked();
});

it("saves Windows and Linux selections without rewriting their displayed legacy values", async () => {
  const windows = persistenceSettings({ seconds: 45, supported: true, platform: "windows" });
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} persistenceSettings={windows} />);
  const windowsControl = await screen.findByLabelText("Banner duration");
  expect(windowsControl).toHaveValue("25");
  fireEvent.change(windowsControl, { target: { value: "5" } });
  await waitFor(() => expect(windows.save).toHaveBeenCalledWith({ seconds: 5 }));

  const linux = persistenceSettings({ seconds: 45, supported: true, platform: "linux" });
  rerender(<NotificationSettings {...input} persistenceSettings={linux} />);
  const linuxControl = await screen.findByLabelText("Banner duration");
  fireEvent.change(linuxControl, { target: { value: "15" } });
  await waitFor(() => expect(linux.save).toHaveBeenCalledWith({ seconds: 15 }));
});

it("removes Linux legacy options after saving and restores them after a failed save", async () => {
  const settings = persistenceSettings({ seconds: 45, supported: true, platform: "linux" });
  const { rerender } = render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const control = await screen.findByLabelText("Banner duration");
  expect(screen.getByRole("option", { name: "45 seconds (current)" })).toBeInTheDocument();
  fireEvent.change(control, { target: { value: "15" } });
  await waitFor(() => expect(screen.queryByRole("option", { name: "45 seconds (current)" })).not.toBeInTheDocument());

  const failing = persistenceSettings({ seconds: 45, supported: true, platform: "linux" });
  failing.save.mockRejectedValue(new Error("disk"));
  rerender(<NotificationSettings {...props()} persistenceSettings={failing} />);
  const failingControl = await screen.findByLabelText("Banner duration");
  fireEvent.change(failingControl, { target: { value: "15" } });
  await screen.findByRole("alert");
  expect(screen.getByRole("option", { name: "45 seconds (current)" })).toBeInTheDocument();
});

it.each([
  ["banners", "Current style: Banners. They fade on their own."],
  ["alerts", "Current style: Alerts. They stay until you dismiss them."],
  ["off", "Notifications are off for Peppy in macOS."],
  ["unknown", "Peppy can't read this setting. Check Peppy's page in System Settings."],
] as const)("shows the macOS %s status and refreshes it on focus", async (macosStyle, status) => {
  const settings = persistenceSettings({ seconds: 5, supported: true, platform: "macos", macosStyle });
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  expect(await screen.findByRole("status")).toHaveTextContent(status);
  fireEvent.focus(window);
  await waitFor(() => expect(settings.load).toHaveBeenCalledTimes(2));
});

it("uses the browser switch to save indefinite and the last finite persistence", async () => {
  const settings = persistenceSettings({ seconds: 45, supported: true, platform: "browser" });
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const toggle = await screen.findByRole("switch", { name: "Keep banners until dismissed" });
  fireEvent.click(toggle);
  await waitFor(() => expect(settings.save).toHaveBeenCalledWith({ seconds: null }));
  fireEvent.click(toggle);
  await waitFor(() => expect(settings.save).toHaveBeenCalledWith({ seconds: 45 }));
});

it("disables mutable platform controls while locked", async () => {
  const settings = persistenceSettings({ seconds: 5, supported: true, platform: "linux" });
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} persistenceSettings={settings} />);
  await screen.findByLabelText("Banner duration");
  rerender(<NotificationSettings {...input} locked persistenceSettings={settings} />);
  expect(screen.getByLabelText("Banner duration")).toBeDisabled();
});

it("keeps the macOS status after a rejected refresh", async () => {
  const settings = persistenceSettings({ seconds: 5, supported: true, platform: "macos", macosStyle: "banners" });
  settings.load.mockResolvedValueOnce({ seconds: 5, supported: true, platform: "macos", macosStyle: "banners" });
  settings.load.mockRejectedValueOnce(new Error("System Settings closed"));
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);

  expect(await screen.findByRole("status")).toHaveTextContent("Current style: Banners. They fade on their own.");
  fireEvent.focus(window);
  await waitFor(() => expect(settings.load).toHaveBeenCalledTimes(2));
  expect(screen.getByRole("status")).toHaveTextContent("Current style: Banners. They fade on their own.");
});

it("updates the macOS status after a focus refresh", async () => {
  const settings = persistenceSettings({ seconds: 5, supported: true, platform: "macos", macosStyle: "alerts" });
  settings.load.mockResolvedValueOnce({ seconds: 5, supported: true, platform: "macos", macosStyle: "banners" });
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);

  expect(await screen.findByRole("status")).toHaveTextContent("Current style: Banners. They fade on their own.");
  // A focus that lands before the initial load settles is ignored, so keep focusing until a refresh runs.
  await waitFor(() => {
    fireEvent.focus(window);
    expect(screen.getByRole("status")).toHaveTextContent("Current style: Alerts. They stay until you dismiss them.");
  });
});

it("keeps the quick-reply preview consequence with the preview decision", async () => {
  const settings = quickReplySettings({ enabled: true, supported: true });
  render(<NotificationSettings {...props()} quickReplySettings={settings} />);

  await screen.findByRole("switch", { name: "Quick reply in notifications" });
  const hint = document.querySelector<HTMLElement>("#notification-quick-reply-preview-hint");
  expect(screen.getByLabelText("Banner preview")).toHaveAttribute("aria-describedby", "notification-quick-reply-preview-hint");
  expect(document.querySelector<HTMLElement>("#notification-banner-group")).toContainElement(hint);
  expect(document.querySelector<HTMLElement>("#notification-local-group")).not.toContainElement(hint);
});

it("hides the quick-reply preview consequence when the adapter reports unsupported", async () => {
  const settings = quickReplySettings({ enabled: true, supported: false });
  render(<NotificationSettings {...props()} quickReplySettings={settings} />);

  await waitFor(() => expect(settings.load).toHaveBeenCalled());
  expect(screen.queryByRole("switch", { name: "Quick reply in notifications" })).not.toBeInTheDocument();
  expect(document.querySelector("#notification-quick-reply-preview-hint")).not.toBeInTheDocument();
  expect(screen.getByLabelText("Banner preview")).not.toHaveAttribute("aria-describedby");
});

it("opens locked sound help without changing notification adapters", () => {
  const input = props();
  const soundSettings = { load: vi.fn().mockResolvedValue(true), save: vi.fn().mockResolvedValue(undefined) };
  render(<NotificationSettings {...input} locked soundSettings={soundSettings} />);

  const trigger = screen.getByRole("button", { name: "About notification sounds" });
  fireEvent.click(trigger);
  expect(screen.getByRole("dialog", { name: "Notification sounds" })).toBeVisible();
  expect(input.onPreferences).not.toHaveBeenCalled();
  expect(input.onMute).not.toHaveBeenCalled();
  expect(input.onPermission).not.toHaveBeenCalled();
  expect(soundSettings.save).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  expect(trigger).toHaveFocus();
});

it("hides a saved permission result with banner controls without changing preferences or app rules", async () => {
  const input = props();
  input.onPermission.mockResolvedValue(check("granted"));
  const { rerender } = render(<NotificationSettings {...input} />);
  fireEvent.click(screen.getByRole("button", { name: CHECK }));
  expect(await screen.findByRole("status")).toHaveTextContent(/allowed to show notifications/i);

  rerender(<NotificationSettings {...input} preferences={{ ...preferences, messageBanners: false, mirroredBanners: false }} />);
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  fireEvent.click(screen.getByText("Per-app mirroring"));
  fireEvent.click(screen.getByText("Per-app mirroring"));
  expect(input.onPreferences).not.toHaveBeenCalled();
  expect(input.onPermission).toHaveBeenCalledTimes(1);
  expect(input.onMute).not.toHaveBeenCalled();
});

it("loads persistence inside StrictMode", async () => {
  const settings = persistenceSettings();
  render(<StrictMode><NotificationSettings {...props()} persistenceSettings={settings} /></StrictMode>);
  expect(await screen.findByLabelText("Banner duration")).toBeEnabled();
});

it("loads persistence after a lock interrupts its first load", async () => {
  let resolve!: (value: NotificationPersistenceSettings) => void;
  const settings = persistenceSettings();
  settings.load.mockImplementationOnce(() => new Promise(done => { resolve = done; }));
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} persistenceSettings={settings} />);

  rerender(<NotificationSettings {...input} locked persistenceSettings={settings} />);
  await act(async () => resolve({ seconds: 5, supported: true }));
  rerender(<NotificationSettings {...input} persistenceSettings={settings} />);
  expect(await screen.findByLabelText("Banner duration")).toBeEnabled();
});

it("does not apply a stale persistence save after its adapter changes", async () => {
  let resolve!: () => void;
  const initial = persistenceSettings({ seconds: 45, supported: true, platform: "linux" });
  initial.save.mockImplementation(() => new Promise<void>(done => { resolve = done; }));
  const replacement = persistenceSettings({ seconds: 45, supported: true, platform: "linux" });
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} persistenceSettings={initial} />);
  const control = await screen.findByLabelText("Banner duration");
  fireEvent.change(control, { target: { value: "15" } });
  rerender(<NotificationSettings {...input} persistenceSettings={replacement} />);
  expect(await screen.findByLabelText("Banner duration")).toHaveValue("45");
  await act(async () => resolve());
  expect(screen.getByLabelText("Banner duration")).toHaveValue("45");
});

it("allows a replacement adapter save while an old persistence save is unresolved", async () => {
  let resolveOld!: () => void;
  let resolveReplacement!: () => void;
  const initial = persistenceSettings({ seconds: 45, supported: true, platform: "linux" });
  initial.save.mockImplementation(() => new Promise<void>(done => { resolveOld = done; }));
  const replacement = persistenceSettings({ seconds: 45, supported: true, platform: "linux" });
  replacement.save.mockImplementation(() => new Promise<void>(done => { resolveReplacement = done; }));
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} persistenceSettings={initial} />);
  fireEvent.change(await screen.findByLabelText("Banner duration"), { target: { value: "15" } });
  rerender(<NotificationSettings {...input} persistenceSettings={replacement} />);
  const replacementControl = await screen.findByLabelText("Banner duration");
  fireEvent.change(replacementControl, { target: { value: "15" } });
  await waitFor(() => expect(replacement.save).toHaveBeenCalledWith({ seconds: 15 }));
  await act(async () => resolveOld());
  await act(async () => resolveReplacement());
  expect(replacementControl).toHaveValue("15");
});

it("keeps focus on the duration select after a preset save", async () => {
  let resolve!: () => void;
  const settings = persistenceSettings({ seconds: 30, supported: true, platform: "linux" });
  settings.save.mockImplementation(() => new Promise<void>(done => { resolve = done; }));
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const control = await screen.findByLabelText("Banner duration");

  control.focus();
  fireEvent.change(control, { target: { value: "15" } });
  expect(control).toBeEnabled();
  await act(async () => resolve());
  expect(control).toHaveFocus();
});

it("does not expose banner duration without a supported persistence adapter", async () => {
  const input = props();
  const unsupported = persistenceSettings({ seconds: 5, supported: false, platform: "linux" });
  const { rerender } = render(<NotificationSettings {...input} />);
  expect(screen.queryByLabelText("Banner duration")).not.toBeInTheDocument();

  rerender(<NotificationSettings {...input} persistenceSettings={unsupported} />);
  await waitFor(() => expect(unsupported.load).toHaveBeenCalled());
  expect(screen.queryByLabelText("Banner duration")).not.toBeInTheDocument();
});

it("falls back to the default duration when the bridge loads an invalid value", async () => {
  const settings = persistenceSettings({ seconds: 0, supported: true, platform: "linux" });
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  expect(await screen.findByLabelText("Banner duration")).toHaveValue("5");
});

it("hides banner duration and reports a failed persistence load", async () => {
  const settings = persistenceSettings();
  settings.load.mockRejectedValue(new Error("disk"));
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);

  expect(screen.queryByLabelText("Banner duration")).not.toBeInTheDocument();
  expect(await screen.findByRole("alert")).toHaveTextContent("Could not load banner settings. Reopen Settings to try again.");
});

it("disables loaded persistence and defers a new adapter load while locked", async () => {
  const settings = persistenceSettings();
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} persistenceSettings={settings} />);
  expect(await screen.findByLabelText("Banner duration")).toBeEnabled();

  rerender(<NotificationSettings {...input} locked persistenceSettings={settings} />);
  expect(screen.getByLabelText("Banner duration")).toBeDisabled();
  expect(settings.load).toHaveBeenCalledTimes(1);
  const deferred = persistenceSettings();
  rerender(<NotificationSettings {...input} locked persistenceSettings={deferred} />);
  expect(deferred.load).not.toHaveBeenCalled();
});

it("describes locked local persistence controls with the banner lock note", async () => {
  const soundSettings = { load: vi.fn().mockResolvedValue(true), save: vi.fn().mockResolvedValue(undefined) };
  const settings = persistenceSettings({ seconds: 45, supported: true, platform: "linux" });
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} soundSettings={soundSettings} persistenceSettings={settings} />);
  await screen.findByLabelText("Banner duration");

  rerender(<NotificationSettings {...input} locked soundSettings={soundSettings} persistenceSettings={settings} />);
  expect(screen.getByRole("switch", { name: "Notification sounds" })).toHaveAttribute("aria-describedby", "notification-banner-locked-note");
  expect(screen.getByLabelText("Banner duration")).toHaveAttribute("aria-describedby", expect.stringContaining("notification-banner-locked-note"));
});

it("keeps the persistence load failure through unrelated notification actions", async () => {
  const input = props();
  const settings = persistenceSettings();
  settings.load.mockRejectedValue(new Error("disk"));
  input.onPermission.mockResolvedValue(check("granted"));
  render(<NotificationSettings {...input} persistenceSettings={settings} />);

  await screen.findByRole("alert");
  fireEvent.click(screen.getByRole("button", { name: CHECK }));
  await screen.findByRole("status");
  expect(screen.getByRole("alert")).toHaveAttribute("id", "notification-persistence-load-error");
});

it("hides a persistence load failure when its adapter is removed", async () => {
  const input = props();
  const settings = persistenceSettings();
  settings.load.mockRejectedValue(new Error("disk"));
  const { rerender } = render(<NotificationSettings {...input} persistenceSettings={settings} />);

  await screen.findByRole("alert");
  rerender(<NotificationSettings {...input} />);
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("clears a persistence load failure when a replacement adapter is locked", async () => {
  const input = props();
  const failed = persistenceSettings();
  failed.load.mockRejectedValue(new Error("disk"));
  const replacement = persistenceSettings();
  const { rerender } = render(<NotificationSettings {...input} persistenceSettings={failed} />);

  await screen.findByRole("alert");
  rerender(<NotificationSettings {...input} locked persistenceSettings={replacement} />);
  expect(replacement.load).not.toHaveBeenCalled();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});
