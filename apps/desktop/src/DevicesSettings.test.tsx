import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DevicesSettings } from "./DevicesSettings";
import { bridge, type DeviceRosterView } from "./bridge";

vi.mock("./bridge", { spy: true });

function deferredRoster() {
  let resolve!: (value: DeviceRosterView) => void;
  const promise = new Promise<DeviceRosterView>(complete => { resolve = complete; });
  return { promise, resolve };
}

describe("DevicesSettings", () => {
  afterEach(cleanup);
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(bridge.list_devices).mockReset().mockResolvedValue({ currentDeviceId: "self", devices: [
      { id: "self", name: "This Mac", kind: "desktop", role: "owner", revoked: false },
      { id: "phone", name: "Pixel 8", kind: "android", role: "owner", revoked: false },
      { id: "old", name: "Old phone", kind: "ios", role: "owner", revoked: true },
    ] });
    vi.mocked(bridge.revoke_device).mockReset().mockResolvedValue(undefined);
  });

  it("loads devices under StrictMode", async () => {
    render(<StrictMode><DevicesSettings /></StrictMode>);

    expect(await screen.findByText("This Mac")).toBeInTheDocument();
  });

  it("keeps the latest StrictMode load when responses resolve out of order", async () => {
    const firstLoad = deferredRoster();
    const secondLoad = deferredRoster();
    vi.mocked(bridge.list_devices)
      .mockReset()
      .mockReturnValueOnce(firstLoad.promise)
      .mockReturnValueOnce(secondLoad.promise);
    render(<StrictMode><DevicesSettings /></StrictMode>);
    await waitFor(() => expect(bridge.list_devices).toHaveBeenCalledTimes(2));

    await act(async () => secondLoad.resolve({ currentDeviceId: "new", devices: [
      { id: "new", name: "Latest device", kind: "desktop", role: "owner", revoked: false },
    ] }));
    expect(screen.getByText("Latest device")).toBeInTheDocument();

    await act(async () => firstLoad.resolve({ currentDeviceId: "old", devices: [
      { id: "old", name: "Stale device", kind: "desktop", role: "owner", revoked: false },
    ] }));
    expect(screen.getByText("Latest device")).toBeInTheDocument();
    expect(screen.queryByText("Stale device")).not.toBeInTheDocument();
  });

  it("ignores a load response after unmount", async () => {
    const pendingLoad = deferredRoster();
    vi.mocked(bridge.list_devices).mockReset().mockReturnValue(pendingLoad.promise);
    const { unmount } = render(<DevicesSettings />);
    unmount();

    await act(async () => pendingLoad.resolve({ currentDeviceId: "self", devices: [] }));

    expect(screen.queryByText("self")).not.toBeInTheDocument();
    expect(bridge.list_devices).toHaveBeenCalledOnce();
  });

  it("does not move focus when the settings section mounts", async () => {
    render(<><button autoFocus>Settings navigation</button><DevicesSettings /></>);

    expect(await screen.findByText("This Mac")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Settings navigation" })).toHaveFocus();
  });

  it("keeps current and last owner devices from removal", async () => {
    render(<DevicesSettings />);
    expect(await screen.findByText("This Mac")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Remove This Mac" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove Pixel 8" })).toBeEnabled();
  });

  it("cancels a removal dialog and restores focus", async () => {
    render(<DevicesSettings />);
    const remove = await screen.findByRole("button", { name: "Remove Pixel 8" });
    fireEvent.click(remove);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(document.activeElement).toBe(remove);
    expect(bridge.revoke_device).not.toHaveBeenCalled();
  });

  it("focuses the dialog heading, traps Tab, and closes on Escape", async () => {
    render(<DevicesSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove Pixel 8" }));
    const heading = screen.getByRole("heading", { name: "Remove Pixel 8?" });
    expect(heading).toHaveFocus();
    fireEvent.keyDown(heading, { key: "Tab", shiftKey: true });
    expect(screen.getByRole("button", { name: "Remove device" })).toHaveFocus();
    fireEvent.keyDown(screen.getByRole("button", { name: "Remove device" }), { key: "Tab" });
    expect(heading).toHaveFocus();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("reports an offline revoke response without immediately reconciling", async () => {
    vi.mocked(bridge.revoke_device).mockRejectedValue({ code: "offline", message: "Offline" });
    render(<DevicesSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove Pixel 8" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove device" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("The device may not have been removed because the server is offline. The list refreshes when Peppy reconnects, or you can retry now.");
    expect(bridge.list_devices).toHaveBeenCalledTimes(1);
  });

  it("mentions a failed roster reconciliation after an uncertain revoke response", async () => {
    vi.mocked(bridge.revoke_device).mockRejectedValue({ code: "server-response", message: "Lost response" });
    vi.mocked(bridge.list_devices)
      .mockResolvedValueOnce({ currentDeviceId: "self", devices: [
        { id: "self", name: "This Mac", kind: "desktop", role: "owner", revoked: false },
        { id: "phone", name: "Pixel 8", kind: "android", role: "owner", revoked: false },
      ] })
      .mockRejectedValueOnce(new Error("Offline"));
    render(<DevicesSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove Pixel 8" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove device" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("outcome is unknown. The roster was reloaded to check it. The latest roster could not be loaded.");
  });

  it("keeps a non-owner viewer read-only", async () => {
    vi.mocked(bridge.list_devices).mockResolvedValue({ currentDeviceId: "self", devices: [
      { id: "self", name: "This browser", kind: "browser", role: "device", revoked: false },
      { id: "owner", name: "Owner phone", kind: "android", role: "owner", revoked: false },
    ] });
    render(<DevicesSettings />);
    expect(await screen.findByText("Only an owner can remove another device.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Remove Owner phone" })).not.toBeInTheDocument();
  });

  it("maps a last-owner server rejection after reloading", async () => {
    vi.mocked(bridge.revoke_device).mockRejectedValue({ code: "last_active_owner", message: "ignored" });
    render(<DevicesSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove Pixel 8" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove device" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("At least one active owner must remain.");
  });

  it("moves an accepted removal into history and returns focus to the settings heading", async () => {
    vi.mocked(bridge.list_devices)
      .mockResolvedValueOnce({ currentDeviceId: "self", devices: [
        { id: "self", name: "This Mac", kind: "desktop", role: "owner", revoked: false },
        { id: "phone", name: "Pixel 8", kind: "android", role: "owner", revoked: false },
      ] })
      .mockResolvedValueOnce({ currentDeviceId: "self", devices: [
        { id: "self", name: "This Mac", kind: "desktop", role: "owner", revoked: false },
        { id: "phone", name: "Pixel 8", kind: "android", role: "owner", revoked: true },
      ] });
    render(<DevicesSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove Pixel 8" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove device" }));
    await waitFor(() => expect(screen.getByText("Previously removed")).toBeInTheDocument());
    await waitFor(() => expect(screen.getByRole("heading", { name: "Devices" })).toHaveFocus());
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("keeps the roster visible when accepted removal cannot reload", async () => {
    vi.mocked(bridge.revoke_device).mockResolvedValue(undefined);
    vi.mocked(bridge.list_devices)
      .mockResolvedValueOnce({ currentDeviceId: "self", devices: [
        { id: "self", name: "This Mac", kind: "desktop", role: "owner", revoked: false },
        { id: "phone", name: "Pixel 8", kind: "android", role: "owner", revoked: false },
      ] })
      .mockRejectedValueOnce(new Error("Offline"));
    render(<DevicesSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove Pixel 8" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove device" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("accepted, but the latest roster could not be loaded");
    expect(screen.getByRole("button", { name: "Remove Pixel 8" })).toBeEnabled();

    fireEvent.click(screen.getByRole("button", { name: "Retry loading devices" }));

    await waitFor(() => expect(bridge.list_devices).toHaveBeenCalledTimes(3));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    expect(screen.queryByRole("button", { name: "Retry loading devices" })).not.toBeInTheDocument();
  });

  it("keeps the dialog open and focused while a removal is pending", async () => {
    const showModal = vi.fn(function (this: HTMLDialogElement) { this.open = true; });
    const close = vi.fn(function (this: HTMLDialogElement) { this.open = false; });
    const originalShowModal = HTMLDialogElement.prototype.showModal;
    const originalClose = HTMLDialogElement.prototype.close;
    HTMLDialogElement.prototype.showModal = showModal;
    HTMLDialogElement.prototype.close = close;
    vi.mocked(bridge.revoke_device).mockReturnValue(new Promise<void>(() => undefined));
    const { unmount } = render(<DevicesSettings />);
    try {
      fireEvent.click(await screen.findByRole("button", { name: "Remove Pixel 8" }));
      const heading = screen.getByRole("heading", { name: "Remove Pixel 8?" });
      expect(heading).toHaveFocus();
      fireEvent.click(screen.getByRole("button", { name: "Remove device" }));
      await waitFor(() => expect(bridge.revoke_device).toHaveBeenCalledWith("phone"));
      fireEvent.keyDown(document, { key: "Escape" });
      expect(showModal).toHaveBeenCalledTimes(1);
      expect(close).not.toHaveBeenCalled();
      expect(screen.getByRole("dialog")).toBeInTheDocument();
      expect(heading).toHaveFocus();
      expect(screen.getByRole("button", { name: "Remove device" })).toBeDisabled();
      expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled();
    } finally {
      unmount();
      HTMLDialogElement.prototype.showModal = originalShowModal;
      HTMLDialogElement.prototype.close = originalClose;
    }
  });

  it("explains a roster that does not list this device without naming a role", async () => {
    vi.mocked(bridge.list_devices).mockResolvedValue({ currentDeviceId: "ghost", devices: [
      { id: "phone", name: "Pixel 8", kind: "android", role: "owner", revoked: false },
    ] });
    render(<DevicesSettings />);
    expect(await screen.findByText("This device is not listed in the roster.")).toBeInTheDocument();
    expect(screen.queryByText("Only an owner can remove another device.")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Remove Pixel 8" })).not.toBeInTheDocument();
  });

  it("binds delayed presence to its device metadata badge", async () => {
    vi.mocked(bridge.list_devices).mockResolvedValue({ currentDeviceId: "self", devices: [
      { id: "self", name: "This Mac", kind: "desktop", role: "owner", revoked: false },
      { id: "phone", name: "Pixel 8", kind: "android", role: "owner", revoked: false, presence: { state: "seen", last_seen_age_seconds: 300 }, wakeRoute: true },
    ] });
    render(<DevicesSettings />);
    await screen.findByText("Pixel 8");
    const phone = document.querySelector('[data-device-id="phone"]')!;
    expect(phone.querySelector('[data-device-badge="platform"]')).toHaveTextContent("Android");
    expect(phone.querySelector('[data-device-badge="role"]')).toHaveTextContent("Owner");
    expect(phone.querySelector('[data-device-badge="enrollment"]')).toHaveTextContent("Active");
    expect(phone.querySelector('[data-device-badge="current"]')).not.toBeInTheDocument();
    expect(phone.querySelector('[data-device-badge="presence"]')).toHaveAttribute("data-presence-state", "delayed");
    expect(phone.querySelector('[data-device-badge="presence"]')).toHaveTextContent("Last seen 5m ago");
  });

  it("shows current and unknown device details as separate badges", async () => {
    vi.mocked(bridge.list_devices).mockResolvedValue({ currentDeviceId: "self", devices: [
      { id: "self", name: "Unknown desktop", kind: "unrecognized", role: "unknown", revoked: false },
    ] });
    render(<DevicesSettings />);
    await screen.findByText("Unknown desktop");
    const currentDevice = document.querySelector('[data-device-id="self"]')!;
    expect(currentDevice.querySelector('[data-device-badge="platform"]')).toHaveTextContent("Unknown platform");
    expect(currentDevice.querySelector('[data-device-badge="role"]')).toHaveTextContent("Unknown role");
    expect(currentDevice.querySelector('[data-device-badge="enrollment"]')).toHaveTextContent("Active");
    expect(currentDevice.querySelector('[data-device-badge="current"]')).toHaveTextContent("This device");
    expect(currentDevice.querySelector('[data-device-badge="presence"]')).toHaveAttribute("data-presence-state", "unknown");
  });

  it("binds connected and never presence only to active device badges", async () => {
    vi.mocked(bridge.list_devices).mockResolvedValue({ currentDeviceId: "self", devices: [
      { id: "self", name: "This Mac", kind: "desktop", role: "owner", revoked: false, presence: { state: "connected", last_seen_age_seconds: 0 } },
      { id: "never", name: "Browser", kind: "browser", role: "device", revoked: false, presence: { state: "never", last_seen_age_seconds: null } },
    ] });
    render(<DevicesSettings />);
    await screen.findByText("This Mac");
    expect(document.querySelector('[data-device-id="self"] [data-device-badge="presence"]')).toHaveAttribute("data-presence-state", "connected");
    expect(document.querySelector('[data-device-id="self"] [data-device-badge="presence"]')).toHaveTextContent("Connected");
    expect(document.querySelector('[data-device-id="never"] [data-device-badge="presence"]')).toHaveAttribute("data-presence-state", "never");
    expect(document.querySelector('[data-device-id="never"] [data-device-badge="presence"]')).toHaveTextContent("Never connected");
  });

  it("omits live presence from removed device details", async () => {
    vi.mocked(bridge.list_devices).mockResolvedValue({ currentDeviceId: "self", devices: [
      { id: "self", name: "This Mac", kind: "desktop", role: "owner", revoked: false },
      { id: "old", name: "Old phone", kind: "ios", role: "owner", revoked: true, presence: { state: "connected", last_seen_age_seconds: 0 } },
    ] });
    render(<DevicesSettings />);
    await screen.findByText("This Mac");
    const removedDevice = document.querySelector('[data-device-id="old"]')!;
    expect(removedDevice.querySelector('[data-device-badge="enrollment"]')).toHaveTextContent("Removed");
    expect(removedDevice.querySelector('[data-device-badge="presence"]')).not.toBeInTheDocument();
  });

  it("keeps a loaded roster when a later load reports the server offline", async () => {
    vi.mocked(bridge.list_devices)
      .mockResolvedValueOnce({ currentDeviceId: "self", devices: [
        { id: "self", name: "This Mac", kind: "desktop", role: "owner", revoked: false },
        { id: "phone", name: "Pixel 8", kind: "android", role: "owner", revoked: false },
      ] })
      .mockRejectedValueOnce({ code: "offline", message: "Could not reach the configured server." });
    const { rerender } = render(<DevicesSettings serverOffline={false} />);
    expect(await screen.findByText("This Mac")).toBeInTheDocument();
    rerender(<DevicesSettings serverOffline />);
    rerender(<DevicesSettings serverOffline={false} />);
    await waitFor(() => expect(document.querySelector("[data-devices-offline-notice]")).toBeInTheDocument());
    expect(document.querySelector("[data-devices-offline-notice]")).toHaveTextContent("Server offline. Showing the last loaded list; removing devices is unavailable.");
    expect(screen.getByText("This Mac")).toBeInTheDocument();
    expect(screen.queryByText("The device roster could not be loaded.")).not.toBeInTheDocument();
  });

  it("shows an offline empty state after the first load fails", async () => {
    vi.mocked(bridge.list_devices).mockRejectedValue({ code: "offline", message: "Could not reach the configured server." });
    render(<DevicesSettings />);
    await waitFor(() => expect(document.querySelector("[data-devices-offline-notice]")).toHaveTextContent("Server offline. The device list will load when the server is back."));
    expect(screen.getByRole("button", { name: "Retry loading devices" })).toHaveAttribute("id", "devices-retry");
    expect(document.querySelector(".settings-error")).not.toBeInTheDocument();
  });

  it("replaces remove buttons while the server is offline", async () => {
    render(<DevicesSettings serverOffline />);
    expect(await screen.findByText("This Mac")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Remove Pixel 8" })).not.toBeInTheDocument();
    expect(document.querySelector("[data-remove-offline]")).toHaveTextContent("Unavailable while offline");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("retains the roster after an offline revoke without reloading", async () => {
    vi.mocked(bridge.revoke_device).mockRejectedValue({ code: "offline", message: "Could not reach the configured server." });
    render(<DevicesSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove Pixel 8" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove device" }));
    await waitFor(() => expect(document.querySelector("[data-devices-remove-error]")).toHaveTextContent("The device may not have been removed because the server is offline. The list refreshes when Peppy reconnects, or you can retry now."));
    expect(screen.getByText("This Mac")).toBeInTheDocument();
    expect(bridge.list_devices).toHaveBeenCalledTimes(1);
  });

  it("offers a manual retry when an outage is only seen by a failed revoke", async () => {
    vi.mocked(bridge.revoke_device).mockRejectedValue({ code: "offline", message: "Could not reach the configured server." });
    render(<DevicesSettings serverOffline={false} />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove Pixel 8" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove device" }));
    await waitFor(() => expect(document.querySelector("[data-remove-offline]")).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "Retry loading devices" }));

    expect(await screen.findByRole("button", { name: "Remove Pixel 8" })).toBeEnabled();
    expect(document.querySelector("[data-devices-offline-notice]")).not.toBeInTheDocument();
    expect(document.querySelector("[data-devices-remove-error]")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry loading devices" })).not.toBeInTheDocument();
  });

  it("reloads once when the server returns and restores device removal", async () => {
    const { rerender } = render(<DevicesSettings serverOffline={false} />);
    expect(await screen.findByText("This Mac")).toBeInTheDocument();
    expect(bridge.list_devices).toHaveBeenCalledTimes(1);
    rerender(<DevicesSettings serverOffline />);
    await waitFor(() => expect(document.querySelector("[data-devices-offline-notice]")).toBeInTheDocument());
    rerender(<DevicesSettings serverOffline={false} />);
    await waitFor(() => expect(bridge.list_devices).toHaveBeenCalledTimes(2));
    expect(document.querySelector("[data-devices-offline-notice]")).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove Pixel 8" })).toBeEnabled();
    await act(async () => {});
    expect(bridge.list_devices).toHaveBeenCalledTimes(2);
  });

  it("shows a non-offline load error message and preserves the roster", async () => {
    vi.mocked(bridge.list_devices)
      .mockResolvedValueOnce({ currentDeviceId: "self", devices: [
        { id: "self", name: "This Mac", kind: "desktop", role: "owner", revoked: false },
        { id: "phone", name: "Pixel 8", kind: "android", role: "owner", revoked: false },
      ] })
      .mockRejectedValueOnce({ code: "server-response", message: "Server returned an invalid roster." });
    const { rerender } = render(<DevicesSettings serverOffline={false} />);
    expect(await screen.findByText("This Mac")).toBeInTheDocument();
    rerender(<DevicesSettings serverOffline />);
    rerender(<DevicesSettings serverOffline={false} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Server returned an invalid roster.");
    expect(screen.getByText("This Mac")).toBeInTheDocument();
  });

  it("uses fallback copy for a non-offline load error without a message", async () => {
    vi.mocked(bridge.list_devices).mockRejectedValue({ code: "server-response" });
    render(<DevicesSettings />);
    expect(await screen.findByRole("alert")).toHaveTextContent("The device roster could not be loaded.");
  });

  it("closes the removal dialog when the server becomes offline", async () => {
    const { rerender } = render(<DevicesSettings serverOffline={false} />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove Pixel 8" }));
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    rerender(<DevicesSettings serverOffline />);
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });
});
