import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { openReleaseNotes, UpdatesSettings } from "./UpdatesSettings";
import { UPDATE_STATUS_SERDE_FIXTURE, type UpdateStatus } from "./update-contract";
import { updateApi, useUpdateStatus } from "./updates";
import { bridge } from "../bridge/bridge";

vi.mock("./updates", () => ({
  updateApi: { check: vi.fn(), defer: vi.fn(), restart: vi.fn(), setAutomatic: vi.fn() },
  useUpdateStatus: vi.fn(),
}));
vi.mock("../bridge/bridge", { spy: true });

const result = (status: UpdateStatus | null, error = "") => ({ status, error, busy: false, refresh: vi.fn(), run: vi.fn() });

describe("UpdatesSettings", () => {
  beforeEach(() => {
    vi.mocked(useUpdateStatus).mockReturnValue(result(UPDATE_STATUS_SERDE_FIXTURE));
  });
  afterEach(cleanup);

  it("presents a ready update with accessible progress and bounded actions", () => {
    render(<UpdatesSettings />);

    expect(screen.getByRole("heading", { name: "Updates" })).toBeInTheDocument();
    expect(screen.getByRole("progressbar", { name: "Update download progress" })).toHaveAttribute("value", "100");
    expect(screen.getByRole("button", { name: "Restart now" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Later" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "Check now" }));
    expect(vi.mocked(useUpdateStatus).mock.results[0]?.value.run).toHaveBeenCalledWith(updateApi.check);
  });

  it("preserves deferred readiness and exposes explicit OS authorization", () => {
    vi.mocked(useUpdateStatus).mockReturnValue(result({ ...UPDATE_STATUS_SERDE_FIXTURE, phase: "deferred", deferredUntil: "2026-10-16T13:00:00Z" }));
    const { rerender } = render(<UpdatesSettings />);
    expect(screen.getByText("Restart into version 1.2.4 is deferred until 2026-10-16T13:00:00Z.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Later" })).not.toBeInTheDocument();

    vi.mocked(useUpdateStatus).mockReturnValue(result({ ...UPDATE_STATUS_SERDE_FIXTURE, phase: "authorization_required", packageKind: "deb", unattended: false }));
    rerender(<UpdatesSettings />);
    expect(screen.getByRole("button", { name: "Authorize and restart" })).toBeInTheDocument();
    expect(screen.getByText(/Package updates require operating system authorization/)).toBeInTheDocument();
  });

  it("presents an installed update with restart controls", () => {
    const updateStatus = result({
      ...UPDATE_STATUS_SERDE_FIXTURE,
      phase: "installed",
      availableVersion: "1.2.4",
      message: "Version 1.2.4 is installed. Restart Peppy to use it.",
    });
    vi.mocked(useUpdateStatus).mockReturnValue(updateStatus);
    render(<UpdatesSettings />);

    expect(screen.getByRole("status")).toHaveTextContent("Version 1.2.4 is installed. Restart Peppy to use it.");
    expect(screen.getAllByText("Version 1.2.4 is installed. Restart Peppy to use it.")).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Restart now" }));
    expect(updateStatus.run).toHaveBeenCalledWith(updateApi.restart);
    expect(screen.getByRole("button", { name: "Later" })).toBeEnabled();
  });

  it("announces installed-update restart failures while keeping restart available", () => {
    vi.mocked(useUpdateStatus).mockReturnValue(result({
      ...UPDATE_STATUS_SERDE_FIXTURE,
      phase: "installed",
      availableVersion: "1.2.4",
      message: "Peppy could not restart into version 1.2.4.",
    }));
    render(<UpdatesSettings />);

    expect(screen.getByRole("alert")).toHaveTextContent("Peppy could not restart into version 1.2.4.");
    expect(screen.getByRole("button", { name: "Restart now" })).toBeEnabled();
  });

  it("does not present a network failure as up to date", () => {
    vi.mocked(useUpdateStatus).mockReturnValue(result({ ...UPDATE_STATUS_SERDE_FIXTURE, phase: "error", message: "Network unavailable" }));
    render(<UpdatesSettings />);
    expect(screen.getByRole("alert")).toHaveClass("settings-error");
    expect(screen.getByRole("alert")).toHaveTextContent("Network unavailable");
    expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
    expect(screen.queryByText("Your update check is complete.")).not.toBeInTheDocument();
  });

  it("shows only the version and App Store delivery copy for App Store installs", () => {
    vi.mocked(useUpdateStatus).mockReturnValue(result({
      ...UPDATE_STATUS_SERDE_FIXTURE,
      distribution: "app_store",
      phase: "disabled",
      channel: "disabled",
      automatic: false,
      lastCheckedAt: "2026-10-16T12:00:00Z",
      message: "Updates are delivered by the App Store.",
    }));
    const { container } = render(<UpdatesSettings />);

    expect(screen.getByRole("heading", { name: "Updates" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Updates are delivered by the App Store.");
    expect(screen.getByText(`Peppy ${UPDATE_STATUS_SERDE_FIXTURE.currentVersion}`)).toBeInTheDocument();
    expect(container.querySelector("#settings-updates")).toHaveAttribute("data-distribution", "app_store");
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
    expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
    expect(screen.queryByText(/Last checked:/)).not.toBeInTheDocument();
    expect(screen.queryByText(/channel|stable|disabled/i)).not.toBeInTheDocument();
  });

  it("keeps a direct install's update status and controls", () => {
    render(<UpdatesSettings />);

    expect(document.querySelector("#settings-updates")).not.toHaveAttribute("data-distribution");
    expect(screen.getByRole("button", { name: "Check now" })).toBeInTheDocument();
    expect(screen.getByLabelText("Automatically download updates")).toBeInTheDocument();
    expect(screen.getByText(/Last checked:/)).toBeInTheDocument();
  });

  it("hides update controls for disabled builds and keeps a persistent status region", () => {
    vi.mocked(useUpdateStatus).mockReturnValue(result({ ...UPDATE_STATUS_SERDE_FIXTURE, phase: "disabled", channel: "disabled" }));
    render(<UpdatesSettings />);
    expect(screen.getByRole("status")).toHaveTextContent("Updates are unavailable for this build.");
    expect(screen.queryByLabelText("Automatically download updates")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Check now" })).not.toBeInTheDocument();
    expect(screen.queryByText(/Last checked:/)).not.toBeInTheDocument();
  });

  it("keeps installed update restart controls visible on disabled channels", () => {
    vi.mocked(useUpdateStatus).mockReturnValue(result({
      ...UPDATE_STATUS_SERDE_FIXTURE,
      channel: "disabled",
      phase: "installed",
      availableVersion: "1.2.4",
    }));
    render(<UpdatesSettings />);

    expect(screen.getByRole("button", { name: "Restart now" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Later" })).toBeEnabled();
    expect(screen.getByRole("status")).toHaveTextContent("Version 1.2.4 is installed. Restart Peppy to use it.");
    expect(screen.queryByRole("button", { name: "Check now" })).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Automatically download updates")).not.toBeInTheDocument();
  });

  it("offers one inline retry when a loaded error status also has a hook error", () => {
    const updateStatus = result({ ...UPDATE_STATUS_SERDE_FIXTURE, phase: "error", message: "Network unavailable" }, "Update check failed.");
    vi.mocked(useUpdateStatus).mockReturnValue(updateStatus);
    render(<UpdatesSettings />);

    expect(screen.getByText("Update check failed.")).toBeInTheDocument();
    expect(screen.getByText("Network unavailable")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry loading updates" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(updateStatus.run).toHaveBeenCalledWith(updateApi.check);
    expect(updateStatus.refresh).not.toHaveBeenCalled();
  });

  it("reloads status when the hook fails before any status loads", () => {
    const updateStatus = result(null, "Update manifest could not be verified. (update-feed)");
    vi.mocked(useUpdateStatus).mockReturnValue(updateStatus);
    render(<UpdatesSettings />);

    expect(screen.getByRole("alert")).toHaveTextContent("Update manifest could not be verified. (update-feed)");
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Retry loading updates" }));
    expect(updateStatus.refresh).toHaveBeenCalledWith(true);
    expect(updateStatus.run).not.toHaveBeenCalled();
  });

  it("shows no retry button for a loaded non-error status with a hook error", () => {
    vi.mocked(useUpdateStatus).mockReturnValue(result({ ...UPDATE_STATUS_SERDE_FIXTURE, phase: "idle" }, "Network unavailable"));
    render(<UpdatesSettings />);

    expect(screen.getByRole("alert")).toHaveTextContent("Network unavailable");
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry loading updates" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Check now" })).toBeInTheDocument();
  });

  it("uses indeterminate progress while a download has no percentage", () => {
    vi.mocked(useUpdateStatus).mockReturnValue(result({ ...UPDATE_STATUS_SERDE_FIXTURE, phase: "downloading", progressPercent: null }));
    render(<UpdatesSettings />);
    expect(screen.getByRole("progressbar", { name: "Update download progress" })).not.toHaveAttribute("value");
  });

  it("opens only validated GitHub release notes through the native bridge", async () => {
    await expect(openReleaseNotes("https://user:secret@github.com/mattv8/peppy/releases/tag/v1.2.4")).rejects.toThrow("not recognized");
    await expect(openReleaseNotes("https://github.com:8443/mattv8/peppy/releases/tag/v1.2.4")).rejects.toThrow("not recognized");
    expect(bridge.open_external_url).not.toHaveBeenCalled();
    await openReleaseNotes(UPDATE_STATUS_SERDE_FIXTURE.releaseNotesUrl!);
    expect(bridge.open_external_url).toHaveBeenCalledWith(UPDATE_STATUS_SERDE_FIXTURE.releaseNotesUrl);
  });

  it.each([
    "http://github.com/mattv8/peppy/releases/tag/v1.2.4",
    "javascript:alert(1)",
    "https://github.com.evil.test/mattv8/peppy/releases/tag/v1.2.4",
    "https://evil.test/github.com/mattv8/peppy/releases/tag/v1.2.4",
    "https://github.com/mattv8/peppy/releases-lookalike/tag/v1.2.4",
    "https://github.com/mattv8/other/releases/tag/v1.2.4",
    "https://github.com/mattv8/peppy/releases/../issues/1",
  ])("refuses hostile release-note destination %s before invoking the bridge", async url => {
    vi.mocked(bridge.open_external_url!).mockClear();
    await expect(openReleaseNotes(url)).rejects.toThrow();
    expect(bridge.open_external_url).not.toHaveBeenCalled();
  });

  it("shows inline failure feedback when opening release notes is rejected", async () => {
    vi.mocked(bridge.open_external_url!).mockRejectedValueOnce(new Error("OS open rejected"));
    render(<UpdatesSettings />);
    const releaseNotes = screen.getByRole("button", { name: "Release notes" });

    fireEvent.click(releaseNotes);

    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(/release notes.*opened/i));
    expect(bridge.open_external_url).toHaveBeenLastCalledWith(UPDATE_STATUS_SERDE_FIXTURE.releaseNotesUrl);
    expect(releaseNotes).toBeEnabled();
    expect(screen.getByRole("heading", { name: "Updates" })).toBeVisible();
  });
});
