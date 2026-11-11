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

it("keeps the existing mute hint as the only description on unlocked heads toggle", () => {
  render(<NotificationSettings {...props()} />);
  expect(screen.getByRole("switch", { name: MUTE })).toHaveAttribute("aria-describedby", "notification-mute-with-heads-hint");
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
  expect(screen.getByRole("switch", { name: MUTE })).toHaveAttribute("aria-describedby", "notification-mute-with-heads-hint notification-banner-locked-note");
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
  expect(screen.getByRole("switch", { name: MUTE })).toHaveAttribute("aria-describedby", "notification-mute-with-heads-hint");
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

it("does not expose banner duration without a supported persistence adapter", async () => {
  const input = props();
  const unsupported = persistenceSettings({ seconds: 5, supported: false });
  const { rerender } = render(<NotificationSettings {...input} />);
  expect(screen.queryByLabelText("Banner duration")).not.toBeInTheDocument();

  rerender(<NotificationSettings {...input} persistenceSettings={unsupported} />);
  await waitFor(() => expect(unsupported.load).toHaveBeenCalled());
  await act(async () => {});
  expect(screen.queryByLabelText("Banner duration")).not.toBeInTheDocument();
});

it.each([
  ["5", { seconds: 5 }],
  ["30", { seconds: 30 }],
  ["60", { seconds: 60 }],
  ["indefinite", { seconds: null }],
] as const)("saves the selected banner duration", async (selection, expected) => {
  const settings = persistenceSettings({ seconds: 45, supported: true });
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const control = await screen.findByLabelText("Banner duration");

  fireEvent.change(control, { target: { value: selection } });

  await waitFor(() => expect(settings.save).toHaveBeenCalledWith(expected));
});

it("falls back to the default duration when the bridge loads an invalid value", async () => {
  const settings = persistenceSettings({ seconds: 0, supported: true });
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);

  expect(await screen.findByLabelText("Banner duration")).toHaveValue("5");
});

it("shows a stored non-preset duration as an editable custom value", async () => {
  const settings = persistenceSettings({ seconds: 45, supported: true });
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);

  expect(await screen.findByLabelText("Banner duration")).toHaveValue("custom");
  expect(screen.getByLabelText("Custom banner duration in seconds")).toHaveValue(45);
});

it("commits valid custom durations on Enter and blur", async () => {
  const settings = persistenceSettings();
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const control = await screen.findByLabelText("Banner duration");
  fireEvent.change(control, { target: { value: "custom" } });
  const custom = screen.getByLabelText("Custom banner duration in seconds");

  fireEvent.change(custom, { target: { value: "45" } });
  fireEvent.keyDown(custom, { key: "Enter" });
  await waitFor(() => expect(settings.save).toHaveBeenCalledWith({ seconds: 45 }));
  fireEvent.change(custom, { target: { value: "46" } });
  fireEvent.blur(custom);
  await waitFor(() => expect(settings.save).toHaveBeenCalledWith({ seconds: 46 }));
});

it.each(["0", "3601", "1.5", ""])("rejects invalid custom duration %s without saving", async (duration) => {
  const settings = persistenceSettings();
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  fireEvent.change(await screen.findByLabelText("Banner duration"), { target: { value: "custom" } });
  const custom = screen.getByLabelText("Custom banner duration in seconds");

  fireEvent.change(custom, { target: { value: duration } });
  fireEvent.keyDown(custom, { key: "Enter" });

  expect(settings.save).not.toHaveBeenCalled();
  expect(custom).toHaveAttribute("aria-invalid", "true");
  expect(screen.getByRole("alert")).toHaveTextContent("Enter whole seconds from 1 to 3600.");
});

it("restores the confirmed custom duration on Escape and after a rejected save", async () => {
  const settings = persistenceSettings({ seconds: 45, supported: true });
  settings.save.mockRejectedValue(new Error("disk"));
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const custom = await screen.findByLabelText("Custom banner duration in seconds");

  fireEvent.change(custom, { target: { value: "46" } });
  fireEvent.keyDown(custom, { key: "Escape" });
  expect(custom).toHaveValue(45);
  fireEvent.change(custom, { target: { value: "46" } });
  fireEvent.keyDown(custom, { key: "Enter" });
  await screen.findByRole("alert");
  expect(custom).toHaveValue(45);
});

it("hides banner duration and reports a failed persistence load", async () => {
  const settings = persistenceSettings();
  settings.load.mockRejectedValue(new Error("disk"));
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);

  expect(screen.queryByLabelText("Banner duration")).not.toBeInTheDocument();
  expect(await screen.findByRole("alert")).toHaveTextContent("Could not load banner duration settings. Reopen Settings to try again.");
});

it("disables a loaded banner duration and defers its load while initially locked", async () => {
  const settings = persistenceSettings();
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} persistenceSettings={settings} />);
  expect(await screen.findByLabelText("Banner duration")).toBeEnabled();

  rerender(<NotificationSettings {...input} locked persistenceSettings={settings} />);
  expect(screen.getByLabelText("Banner duration")).toBeDisabled();
  expect(settings.load).toHaveBeenCalledTimes(1);
  rerender(<NotificationSettings {...input} persistenceSettings={settings} />);
  expect(screen.getByLabelText("Banner duration")).toBeEnabled();
  expect(settings.load).toHaveBeenCalledTimes(1);

  const deferred = persistenceSettings();
  rerender(<NotificationSettings {...input} locked persistenceSettings={deferred} />);

  expect(deferred.load).not.toHaveBeenCalled();
});

it("keeps Custom read-only and does not save while locked", async () => {
  const settings = persistenceSettings({ seconds: 45, supported: true });
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} persistenceSettings={settings} />);
  const custom = await screen.findByLabelText("Custom banner duration in seconds");

  rerender(<NotificationSettings {...input} locked persistenceSettings={settings} />);
  expect(custom).toHaveAttribute("readonly");
  fireEvent.keyDown(custom, { key: "Enter" });
  expect(settings.save).not.toHaveBeenCalled();
});

it("keeps other notification controls available while a custom duration is saving", async () => {
  let resolve!: () => void;
  const settings = persistenceSettings({ seconds: 45, supported: true });
  settings.save.mockImplementation(() => new Promise<void>(done => { resolve = done; }));
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const custom = await screen.findByLabelText("Custom banner duration in seconds");

  fireEvent.change(custom, { target: { value: "46" } });
  fireEvent.blur(custom);
  expect(screen.getByRole("button", { name: CHECK })).toBeEnabled();
  expect(screen.getByLabelText("Banner preview")).toBeEnabled();
  await act(async () => resolve());
});

it("saves a Custom prefill from Indefinitely and restores Indefinitely when it fails", async () => {
  const settings = persistenceSettings({ seconds: null, supported: true });
  settings.save.mockRejectedValue(new Error("disk"));
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const control = await screen.findByLabelText("Banner duration");

  fireEvent.change(control, { target: { value: "custom" } });
  await waitFor(() => expect(settings.save).toHaveBeenCalledWith({ seconds: 5 }));
  expect(control).toHaveValue("indefinite");
});

it("restores the confirmed preset after a rejected preset save", async () => {
  const settings = persistenceSettings({ seconds: 30, supported: true });
  settings.save.mockRejectedValue(new Error("disk"));
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const control = await screen.findByLabelText("Banner duration");

  fireEvent.change(control, { target: { value: "60" } });
  await screen.findByRole("alert");
  expect(control).toHaveValue("30");
});

it.each(["30", "indefinite"])("keeps confirmed Custom mode until a %s save succeeds", async (selection) => {
  let resolve!: () => void;
  const settings = persistenceSettings({ seconds: 45, supported: true });
  settings.save.mockImplementation(() => new Promise<void>(done => { resolve = done; }));
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const control = await screen.findByLabelText("Banner duration");
  await screen.findByLabelText("Custom banner duration in seconds");

  fireEvent.change(control, { target: { value: selection } });

  expect(control).toHaveValue("custom");
  expect(screen.getByLabelText("Custom banner duration in seconds")).toHaveValue(45);
  await act(async () => resolve());
  expect(control).toHaveValue(selection);
  expect(screen.queryByLabelText("Custom banner duration in seconds")).not.toBeInTheDocument();
});

it("keeps Custom mode and its draft after a rejected preset save", async () => {
  const settings = persistenceSettings({ seconds: 45, supported: true });
  settings.save.mockRejectedValue(new Error("disk"));
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const control = await screen.findByLabelText("Banner duration");
  await screen.findByLabelText("Custom banner duration in seconds");

  fireEvent.change(control, { target: { value: "30" } });

  await screen.findByRole("alert");
  expect(control).toHaveValue("custom");
  expect(screen.getByLabelText("Custom banner duration in seconds")).toHaveValue(45);
});

it("does not apply a stale duration save after its adapter changes", async () => {
  let resolve!: () => void;
  const initial = persistenceSettings({ seconds: 45, supported: true });
  initial.save.mockImplementation(() => new Promise<void>(done => { resolve = done; }));
  const replacement = persistenceSettings({ seconds: 45, supported: true });
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} persistenceSettings={initial} />);
  const control = await screen.findByLabelText("Banner duration");
  await screen.findByLabelText("Custom banner duration in seconds");

  fireEvent.change(control, { target: { value: "30" } });
  rerender(<NotificationSettings {...input} persistenceSettings={replacement} />);
  await screen.findByLabelText("Custom banner duration in seconds");
  await act(async () => resolve());

  expect(screen.getByLabelText("Banner duration")).toHaveValue("custom");
  expect(screen.getByLabelText("Custom banner duration in seconds")).toHaveValue(45);
});

it("allows a replacement adapter save while an old duration save is unresolved", async () => {
  let resolveOld!: () => void;
  let resolveReplacement!: () => void;
  const initial = persistenceSettings({ seconds: 45, supported: true });
  initial.save.mockImplementation(() => new Promise<void>(done => { resolveOld = done; }));
  const replacement = persistenceSettings({ seconds: 45, supported: true });
  replacement.save.mockImplementation(() => new Promise<void>(done => { resolveReplacement = done; }));
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} persistenceSettings={initial} />);
  const control = await screen.findByLabelText("Banner duration");
  await screen.findByLabelText("Custom banner duration in seconds");

  fireEvent.change(control, { target: { value: "30" } });
  rerender(<NotificationSettings {...input} persistenceSettings={replacement} />);
  const replacementControl = await screen.findByLabelText("Banner duration");
  const replacementCustom = await screen.findByLabelText("Custom banner duration in seconds");
  expect(replacementCustom).not.toHaveAttribute("readonly");
  fireEvent.change(replacementControl, { target: { value: "30" } });
  await waitFor(() => expect(replacement.save).toHaveBeenCalledWith({ seconds: 30 }));
  expect(replacementCustom).toHaveAttribute("readonly");

  await act(async () => resolveOld());
  expect(replacementCustom).toHaveAttribute("readonly");
  await act(async () => resolveReplacement());
  expect(replacementControl).toHaveValue("30");
});

it("keeps an invalid custom draft marked invalid after blur", async () => {
  const settings = persistenceSettings({ seconds: 45, supported: true });
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const custom = await screen.findByLabelText("Custom banner duration in seconds");

  fireEvent.change(custom, { target: { value: "0" } });
  fireEvent.blur(custom);
  expect(custom).toHaveAttribute("aria-invalid", "true");
  expect(settings.save).not.toHaveBeenCalled();
});

it("keeps invalid duration feedback consistent after desktop banners toggle", async () => {
  const input = props();
  const settings = persistenceSettings({ seconds: 45, supported: true });
  const { rerender } = render(<NotificationSettings {...input} persistenceSettings={settings} />);
  const custom = await screen.findByLabelText("Custom banner duration in seconds");

  fireEvent.change(custom, { target: { value: "0" } });
  fireEvent.blur(custom);
  expect(custom).toHaveAttribute("aria-invalid", "true");
  expect(screen.getByRole("alert")).toBeVisible();
  rerender(<NotificationSettings {...input} preferences={{ ...preferences, messageBanners: false, mirroredBanners: false }} persistenceSettings={settings} />);
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  rerender(<NotificationSettings {...input} persistenceSettings={settings} />);

  expect(screen.getByLabelText("Custom banner duration in seconds")).toHaveAttribute("aria-invalid", "true");
  expect(screen.getByRole("alert")).toBeVisible();
});

it("keeps validation visible with a failed notification action", async () => {
  const input = props();
  input.onPermission.mockRejectedValue(new Error("failure"));
  const settings = persistenceSettings({ seconds: 45, supported: true });
  render(<NotificationSettings {...input} persistenceSettings={settings} />);
  const custom = await screen.findByLabelText("Custom banner duration in seconds");

  fireEvent.change(custom, { target: { value: "0" } });
  fireEvent.blur(custom);
  fireEvent.click(screen.getByRole("button", { name: CHECK }));

  await waitFor(() => expect(screen.getAllByRole("alert")).toHaveLength(2));
  expect(screen.getByRole("alert", { name: "Invalid custom banner duration" })).toBeVisible();
  expect(screen.getByRole("alert", { name: "Notification settings action failed" })).toHaveTextContent("Could not send test notification");
});

it("resets an invalid custom draft when a preset save is rejected", async () => {
  const settings = persistenceSettings({ seconds: 45, supported: true });
  settings.save.mockRejectedValue(new Error("disk"));
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const control = await screen.findByLabelText("Banner duration");
  const custom = await screen.findByLabelText("Custom banner duration in seconds");

  fireEvent.change(custom, { target: { value: "0" } });
  fireEvent.blur(custom);
  fireEvent.change(control, { target: { value: "30" } });

  await screen.findByRole("alert");
  expect(control).toHaveValue("custom");
  expect(custom).toHaveValue(45);
  expect(custom).not.toHaveAttribute("aria-invalid");
  expect(screen.getByRole("alert", { name: "Notification settings action failed" })).toHaveTextContent("Could not save notification settings");
});

it.each(["30", "indefinite"])("clears invalid Custom feedback after a successful %s save", async (selection) => {
  const settings = persistenceSettings({ seconds: 45, supported: true });
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const control = await screen.findByLabelText("Banner duration");
  const custom = await screen.findByLabelText("Custom banner duration in seconds");

  fireEvent.change(custom, { target: { value: "0" } });
  fireEvent.blur(custom);
  fireEvent.change(control, { target: { value: selection } });

  await waitFor(() => expect(screen.queryByLabelText("Custom banner duration in seconds")).not.toBeInTheDocument());
  expect(control).toHaveValue(selection);
  expect(screen.queryByRole("alert", { name: "Invalid custom banner duration" })).not.toBeInTheDocument();
});

it("loads banner duration inside StrictMode", async () => {
  const settings = persistenceSettings();
  render(<StrictMode><NotificationSettings {...props()} persistenceSettings={settings} /></StrictMode>);

  expect(await screen.findByLabelText("Banner duration")).toBeEnabled();
});

it("loads banner duration after a lock interrupts its first load", async () => {
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

it("keeps the banner-duration load failure through unrelated notification actions", async () => {
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

it("hides a duration load failure when the persistence adapter is removed", async () => {
  const input = props();
  const settings = persistenceSettings();
  settings.load.mockRejectedValue(new Error("disk"));
  const { rerender } = render(<NotificationSettings {...input} persistenceSettings={settings} />);

  await screen.findByRole("alert");
  rerender(<NotificationSettings {...input} />);

  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("clears a duration load failure when a replacement adapter is locked", async () => {
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

it("keeps focus on the duration select after a preset save", async () => {
  let resolve!: () => void;
  const settings = persistenceSettings({ seconds: 30, supported: true });
  settings.save.mockImplementation(() => new Promise<void>(done => { resolve = done; }));
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const control = await screen.findByLabelText("Banner duration");

  control.focus();
  fireEvent.change(control, { target: { value: "60" } });
  expect(control).toBeEnabled();
  await act(async () => resolve());
  expect(control).toHaveFocus();
});

it("makes the Custom input read-only while its save is pending", async () => {
  let resolve!: () => void;
  const settings = persistenceSettings({ seconds: 45, supported: true });
  settings.save.mockImplementation(() => new Promise<void>(done => { resolve = done; }));
  render(<NotificationSettings {...props()} persistenceSettings={settings} />);
  const custom = await screen.findByLabelText("Custom banner duration in seconds");

  fireEvent.change(custom, { target: { value: "46" } });
  fireEvent.blur(custom);
  expect(custom).toHaveAttribute("readonly");
  await act(async () => resolve());
});
