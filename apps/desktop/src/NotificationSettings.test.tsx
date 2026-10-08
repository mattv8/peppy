import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { NotificationSettings } from "./NotificationSettings";
import type { AppFilter, MirroredNotification, NotificationPreferences } from "./bridge";

afterEach(cleanup);
const preferences: NotificationPreferences = { messageBanners: true, mirroredBanners: true, preview: "full" };
const filter: AppFilter = { sourceDeviceId: "one", packageName: "chat", appName: "Chat", muted: true };
const notification: MirroredNotification = {
  target: { sourceDeviceId: "one", notificationKey: "key", lifetime: "life" },
  packageName: "chat", appName: "Chat", title: "Hello", text: "Text", postedAt: 0,
  dismissible: true, seen: false, dismissalPending: false,
};
const props = () => ({
  notifications: [notification], filters: [filter], sources: [{ id: "one", name: "Pixel" }],
  preferences, onPreferences: vi.fn().mockResolvedValue(undefined),
  onMute: vi.fn().mockResolvedValue(undefined), onPermission: vi.fn().mockResolvedValue(undefined),
});

it("shows banner controls when either desktop banner preference is enabled", () => {
  const input = props();
  const { rerender } = render(<NotificationSettings {...input} preferences={{ ...preferences, messageBanners: false, mirroredBanners: false }} />);
  expect(screen.queryByLabelText("Banner preview")).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Check desktop notifications" })).not.toBeInTheDocument();

  rerender(<NotificationSettings {...input} preferences={{ ...preferences, messageBanners: true, mirroredBanners: false }} />);
  expect(screen.getByLabelText("Banner preview")).toBeVisible();
  expect(screen.getByRole("button", { name: "Check desktop notifications" })).toBeVisible();

  rerender(<NotificationSettings {...input} preferences={{ ...preferences, messageBanners: false, mirroredBanners: true }} />);
  expect(screen.getByLabelText("Banner preview")).toBeVisible();
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

it("keeps the stored mute choice over observed defaults and allows unmuting that phone while desktop banners are off", async () => {
  const input = props();
  render(<NotificationSettings {...input} preferences={{ ...preferences, messageBanners: false, mirroredBanners: false }} />);
  fireEvent.click(screen.getByText("Per-app mirroring"));
  const control = screen.getByRole("switch", { name: "Mirror Chat from Pixel" });
  expect(control).not.toBeChecked();
  fireEvent.click(control);
  await waitFor(() => expect(input.onMute).toHaveBeenCalledWith({ ...filter, muted: false }));
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
});

it("does not render an empty app rules disclosure", () => {
  const input = props();
  render(<NotificationSettings {...input} notifications={[]} filters={[]} />);

  expect(screen.queryByText("Per-app mirroring")).not.toBeInTheDocument();
  expect(screen.queryByRole("list", { name: "App mirroring filters" })).not.toBeInTheDocument();
});

it.each([
  ["granted", /granted/i],
  ["denied", /denied/i],
  ["unknown", /operating system notification settings/i],
] as const)("reports %s notification permission locally", async (permission, expectedStatus) => {
  const input = props();
  input.onPermission.mockResolvedValue(permission);
  render(<NotificationSettings {...input} />);
  fireEvent.click(screen.getByRole("button", { name: "Check desktop notifications" }));

  expect(await screen.findByRole("status")).toHaveTextContent(expectedStatus);
});

it("hides a saved permission result with banner controls without changing preferences or app rules", async () => {
  const input = props();
  input.onPermission.mockResolvedValue("granted");
  const { rerender } = render(<NotificationSettings {...input} />);
  fireEvent.click(screen.getByRole("button", { name: "Check desktop notifications" }));
  expect(await screen.findByRole("status")).toHaveTextContent(/granted/i);

  rerender(<NotificationSettings {...input} preferences={{ ...preferences, messageBanners: false, mirroredBanners: false }} />);
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  fireEvent.click(screen.getByText("Per-app mirroring"));
  fireEvent.click(screen.getByText("Per-app mirroring"));
  expect(input.onPreferences).not.toHaveBeenCalled();
  expect(input.onPermission).toHaveBeenCalledTimes(1);
  expect(input.onMute).not.toHaveBeenCalled();
});

it("reports a rejected notification permission check locally", async () => {
  const input = props();
  input.onPermission.mockRejectedValue(new Error("failure"));
  render(<NotificationSettings {...input} />);
  fireEvent.click(screen.getByRole("button", { name: "Check desktop notifications" }));

  expect(await screen.findByRole("alert")).toBeVisible();
});
