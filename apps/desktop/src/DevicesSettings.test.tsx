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
    vi.mocked(bridge.list_devices).mockResolvedValue({ currentDeviceId: "self", devices: [
      { id: "self", name: "This Mac", kind: "desktop", role: "owner", revoked: false },
      { id: "phone", name: "Pixel 8", kind: "android", role: "owner", revoked: false },
      { id: "old", name: "Old phone", kind: "ios", role: "owner", revoked: true },
    ] });
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
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const { unmount } = render(<DevicesSettings />);
    unmount();

    await act(async () => pendingLoad.resolve({ currentDeviceId: "self", devices: [] }));

    expect(consoleError).not.toHaveBeenCalled();
    consoleError.mockRestore();
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

  it("reconciles the roster after an uncertain revoke response", async () => {
    vi.mocked(bridge.revoke_device).mockRejectedValue({ code: "offline", message: "Offline" });
    render(<DevicesSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove Pixel 8" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove device" }));
    await waitFor(() => expect(bridge.list_devices).toHaveBeenCalledTimes(2));
    expect(screen.getByRole("alert")).toHaveTextContent("outcome is unknown");
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
    expect(screen.getByRole("heading", { name: "Devices" })).toHaveFocus();
  });

  it("keeps the roster non-actionable when accepted removal cannot reload", async () => {
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
    expect(screen.queryByRole("button", { name: "Remove Pixel 8" })).not.toBeInTheDocument();
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

  it("shows raw presence without treating a mobile owner as something other than a phone", async () => {
    vi.mocked(bridge.list_devices).mockResolvedValue({ currentDeviceId: "self", devices: [
      { id: "self", name: "This Mac", kind: "desktop", role: "owner", revoked: false },
      { id: "phone", name: "Pixel 8", kind: "android", role: "owner", revoked: false, presence: { state: "seen", last_seen_age_seconds: 300 }, wakeRoute: true },
    ] });
    render(<DevicesSettings />);
    expect(await screen.findByText(/Android · Owner · Active · Last seen 5m ago/)).toBeInTheDocument();
  });
});
