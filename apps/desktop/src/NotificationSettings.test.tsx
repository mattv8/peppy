import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { NotificationSettings } from "./NotificationSettings";
import type { AppFilter, MirroredNotification, NotificationCheck, NotificationPreferences } from "./bridge";

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
