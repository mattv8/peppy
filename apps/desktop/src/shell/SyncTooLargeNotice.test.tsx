import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SyncTooLargeNotice } from "./SyncTooLargeNotice";

const updates = vi.hoisted(() => ({
  distribution: undefined as string | undefined,
  phase: "idle" as string,
  updateHost: false,
  useUpdateStatus: vi.fn(),
}));

vi.mock("../updates/updates", () => ({
  useUpdateStatus: updates.useUpdateStatus,
  isTauriUpdateHost: () => updates.updateHost,
}));

beforeEach(() => {
  updates.distribution = undefined;
  updates.phase = "idle";
  updates.updateHost = false;
  updates.useUpdateStatus.mockImplementation(() => ({
    status: updates.distribution ? { distribution: updates.distribution, phase: updates.phase } : null,
  }));
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("SyncTooLargeNotice", () => {
  it("renders the direct native update action when the update host can show the Updates card", () => {
    updates.updateHost = true;
    updates.distribution = "direct";
    const open = vi.fn();
    render(<SyncTooLargeNotice browserHost={false} onCheckForUpdates={open} onReload={vi.fn()} />);
    expect(screen.getByRole("region", { name: "History too large for this version" })).toHaveAttribute("data-sync-issue", "history-too-large");
    fireEvent.click(screen.getByRole("button", { name: "Check for updates" }));
    expect(open).toHaveBeenCalledOnce();
    expect(document.querySelector("#sync-too-large-body")).toHaveTextContent("computer");
  });

  it("reads the update status only while mounted, and only on native hosts", () => {
    const { unmount } = render(<SyncTooLargeNotice browserHost={false} onCheckForUpdates={vi.fn()} onReload={vi.fn()} />);
    expect(updates.useUpdateStatus).toHaveBeenLastCalledWith(true);
    unmount();

    render(<SyncTooLargeNotice browserHost onCheckForUpdates={vi.fn()} onReload={vi.fn()} />);
    expect(updates.useUpdateStatus).toHaveBeenLastCalledWith(false);
  });

  it("shows the App Store delivery note without an action", () => {
    updates.distribution = "app_store";
    updates.updateHost = true;
    render(<SyncTooLargeNotice browserHost={false} onCheckForUpdates={vi.fn()} onReload={vi.fn()} />);
    expect(document.querySelector("#sync-too-large-app-store-note")).toHaveTextContent("App Store");
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("omits the update action when the native build cannot show the Updates card", () => {
    updates.updateHost = false;
    render(<SyncTooLargeNotice browserHost={false} onCheckForUpdates={vi.fn()} onReload={vi.fn()} />);
    expect(screen.queryByRole("button", { name: "Check for updates" })).toBeNull();
    expect(document.querySelector("#sync-too-large-body")).toHaveTextContent("computer");
  });

  it("omits the update action when updates are disabled for this build", () => {
    updates.updateHost = true;
    updates.distribution = "direct";
    updates.phase = "disabled";
    render(<SyncTooLargeNotice browserHost={false} onCheckForUpdates={vi.fn()} onReload={vi.fn()} />);
    expect(screen.queryByRole("button", { name: "Check for updates" })).toBeNull();
  });

  it("asks the worker for one fresh import attempt before reloading in the browser", () => {
    const onReload = vi.fn();
    render(<SyncTooLargeNotice browserHost onCheckForUpdates={vi.fn()} onReload={onReload} />);
    fireEvent.click(screen.getByRole("button", { name: "Reload" }));
    expect(onReload).toHaveBeenCalledOnce();
  });

  it("renders browser reload and server guidance", () => {
    render(<SyncTooLargeNotice browserHost onCheckForUpdates={vi.fn()} onReload={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Reload" })).toBeInTheDocument();
    expect(document.querySelector("#sync-too-large-server-note")).toHaveTextContent("whoever runs your server");
  });
});
