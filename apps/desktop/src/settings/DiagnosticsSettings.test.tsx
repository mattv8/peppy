import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DiagnosticsExportResult, DiagnosticsState } from "@peppy/desktop-ui/bridge-contracts";
import { peppyCopy } from "../generated/peppyCopy";
import { DiagnosticsSettings } from "./DiagnosticsSettings";
import type { DiagnosticsBridge } from "./useDiagnostics";

const MIB = 1024 * 1024;
const STORAGE_TEXT = "Diagnostics storage is unavailable. Try again later.";
const EXPORT_TEXT = "Could not export the diagnostic log. Try again.";
const CLEAR_TEXT = "Could not clear diagnostic logs. Try again.";
const base: DiagnosticsState = { enabled: false, retainedBytes: 0, limitBytes: 4 * MIB, droppedEvents: 0, storageError: false };

function bridgeWith(initial: Partial<DiagnosticsState> = {}) {
  let current: DiagnosticsState = { ...base, ...initial };
  return {
    get_diagnostics_state: vi.fn(async () => ({ ...current })),
    set_diagnostics_enabled: vi.fn(async ({ enabled }: { enabled: boolean }) => {
      current = { ...current, enabled };
      return { ...current };
    }),
    export_diagnostics_log: vi.fn(async (): Promise<DiagnosticsExportResult> => ({ outcome: "saved" })),
    clear_diagnostics_log: vi.fn(async () => {
      current = { ...current, retainedBytes: 0 };
      return { ...current };
    }),
  };
}

const toggle = () => screen.getByRole<HTMLInputElement>("switch", { name: "Verbose logging" });
const exportButton = () => screen.getByRole("button", { name: "Export log" });
const clearButton = () => screen.getByRole("button", { name: "Clear logs" });
const statusMessage = () => document.getElementById("diagnostics-status-message")!;
const rejectionError = (code: string) => ({ code, message: "native detail" });

async function renderLoaded(bridge: ReturnType<typeof bridgeWith>) {
  const view = render(<DiagnosticsSettings bridge={bridge as DiagnosticsBridge} />);
  await waitFor(() => expect(toggle()).toBeEnabled());
  return view;
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("DiagnosticsSettings", () => {
  it("renders the section, its heading, and the switch with its description", async () => {
    const { container } = await renderLoaded(bridgeWith());

    expect(container.querySelector("section#settings-diagnostics")).toHaveAttribute("data-settings-section", "diagnostics");
    expect(screen.getByRole("region", { name: "Diagnostics" })).toBe(container.querySelector("section#settings-diagnostics"));
    expect(toggle()).toHaveAccessibleDescription("Records typing and connection events on this computer for troubleshooting.");
    expect(toggle()).toHaveAttribute("aria-describedby", "diagnostics-enabled-hint");
    expect(document.getElementById("settings-diagnostics-title")).toHaveAttribute("aria-labelledby", "settings-diagnostics-name");
    expect(document.querySelector("section#settings-diagnostics")).not.toHaveAttribute("aria-busy");
    expect(document.querySelector("#diagnostics-actions #diagnostics-status-message")).toBeInTheDocument();
  });

  it("disables every control until the first state arrives", async () => {
    let resolveLoad!: (state: DiagnosticsState) => void;
    const bridge = bridgeWith();
    bridge.get_diagnostics_state.mockReturnValueOnce(new Promise<DiagnosticsState>(resolve => { resolveLoad = resolve; }));
    render(<DiagnosticsSettings bridge={bridge as DiagnosticsBridge} />);

    expect(screen.getByText("Loading diagnostics…")).toBeInTheDocument();
    expect(toggle()).toBeDisabled();
    expect(exportButton()).toBeDisabled();
    expect(clearButton()).toBeDisabled();

    await act(async () => resolveLoad({ ...base }));
    expect(screen.queryByText("Loading diagnostics…")).not.toBeInTheDocument();
    expect(toggle()).toBeEnabled();
  });

  it("shows the storage message when loading fails and reloads from Try again", async () => {
    const bridge = bridgeWith();
    bridge.get_diagnostics_state.mockRejectedValueOnce(rejectionError("other"));
    render(<DiagnosticsSettings bridge={bridge as DiagnosticsBridge} />);

    expect(await screen.findByRole("alert")).toHaveTextContent(STORAGE_TEXT);
    expect(toggle()).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));

    await waitFor(() => expect(toggle()).toBeEnabled());
    expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it.each([
    [{ enabled: true, retainedBytes: 0 }, "Active"],
    [{ enabled: false, retainedBytes: 2048 }, "Off. Retained logs are still available."],
    [{ enabled: false, retainedBytes: 0 }, "Off"],
  ])("describes recording for %o as %s", async (initial, text) => {
    await renderLoaded(bridgeWith(initial));

    expect(document.getElementById("diagnostics-recording-value")?.textContent).toBe(text);
  });

  it("shows the retained size against the limit with a decorative bar", async () => {
    await renderLoaded(bridgeWith({ retainedBytes: 1.5 * MIB }));

    expect(document.getElementById("diagnostics-size-value")?.textContent).toContain("1.5 MiB of 4 MiB used");
    const bar = document.getElementById("diagnostics-size-bar");
    expect(bar).toHaveAttribute("aria-hidden", "true");
    expect(bar?.querySelector("span")).toHaveStyle({ width: "37.5%" });
  });

  it("omits the bar when the limit is zero", async () => {
    await renderLoaded(bridgeWith({ limitBytes: 0 }));

    expect(document.getElementById("diagnostics-size-bar")).toBeNull();
  });

  it("shows the dropped event count, zero included", async () => {
    await renderLoaded(bridgeWith({ droppedEvents: 0 }));

    expect(document.getElementById("diagnostics-dropped-value")?.textContent).toBe("0");
  });

  it("saves the switch through the host and renders the returned state", async () => {
    const bridge = bridgeWith();
    await renderLoaded(bridge);

    fireEvent.click(toggle());

    expect(bridge.set_diagnostics_enabled).toHaveBeenCalledWith({ enabled: true });
    await waitFor(() => expect(toggle()).toBeChecked());
    expect(document.getElementById("diagnostics-recording-value")?.textContent).toBe("Active");
  });

  it("keeps the old state and reports the generic setting error when saving is rejected", async () => {
    const bridge = bridgeWith();
    bridge.set_diagnostics_enabled.mockRejectedValueOnce(rejectionError("other"));
    await renderLoaded(bridge);

    fireEvent.click(toggle());

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveAttribute("id", "diagnostics-error");
    expect(alert).toHaveAttribute("data-diagnostics-error", "setting");
    expect(alert).toHaveTextContent(peppyCopy.setting_update_error);
    expect(toggle()).not.toBeChecked();
    expect(toggle()).toHaveAttribute("aria-describedby", "diagnostics-enabled-hint diagnostics-error");
    expect(document.querySelector("#diagnostics-enabled-group #diagnostics-error")).toBe(alert);
    expect(document.querySelectorAll("#diagnostics-error")).toHaveLength(1);
  });

  it("disables Export and Clear when no logs are retained", async () => {
    await renderLoaded(bridgeWith({ retainedBytes: 0 }));

    expect(exportButton()).toBeDisabled();
    expect(clearButton()).toBeDisabled();
  });

  it("keeps Export and Clear available after recording stops while logs remain", async () => {
    const bridge = bridgeWith({ enabled: true, retainedBytes: 4096 });
    await renderLoaded(bridge);

    fireEvent.click(toggle());

    await waitFor(() => expect(toggle()).not.toBeChecked());
    expect(exportButton()).toBeEnabled();
    expect(clearButton()).toBeEnabled();
    expect(document.getElementById("diagnostics-recording-value")?.textContent).toBe("Off. Retained logs are still available.");
  });

  it("reports a saved export and refreshes the state", async () => {
    const bridge = bridgeWith({ retainedBytes: 4096 });
    await renderLoaded(bridge);

    fireEvent.click(exportButton());

    expect(await screen.findByText("Log exported.")).toHaveAttribute("id", "diagnostics-status-message");
    await waitFor(() => expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(2));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("shows nothing for a cancelled export and keeps focus on Export throughout", async () => {
    const bridge = bridgeWith({ retainedBytes: 4096 });
    let finishExport!: (result: DiagnosticsExportResult) => void;
    bridge.export_diagnostics_log.mockReturnValueOnce(new Promise<DiagnosticsExportResult>(resolve => { finishExport = resolve; }));
    await renderLoaded(bridge);
    exportButton().focus();
    const onBlur = vi.fn();
    exportButton().addEventListener("blur", onBlur);

    fireEvent.click(exportButton());
    expect(exportButton()).toBeEnabled();
    expect(exportButton()).toHaveFocus();

    await act(async () => finishExport({ outcome: "cancelled" }));
    await waitFor(() => expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(2));
    expect(onBlur).not.toHaveBeenCalled();
    expect(exportButton()).toHaveFocus();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(statusMessage().textContent).toBe("");
  });

  it("makes no further native call when an export settles after unmount", async () => {
    const bridge = bridgeWith({ retainedBytes: 4096 });
    let finishExport!: (result: DiagnosticsExportResult) => void;
    bridge.export_diagnostics_log.mockReturnValueOnce(new Promise<DiagnosticsExportResult>(resolve => { finishExport = resolve; }));
    const view = await renderLoaded(bridge);
    fireEvent.click(exportButton());

    view.unmount();
    await act(async () => finishExport({ outcome: "saved" }));

    expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(1);
  });

  it("makes no further native call when a clear fails after unmount", async () => {
    const bridge = bridgeWith({ retainedBytes: 4096 });
    bridge.clear_diagnostics_log.mockRejectedValueOnce(rejectionError("other"));
    const view = await renderLoaded(bridge);
    fireEvent.click(clearButton());
    fireEvent.click(document.getElementById("diagnostics-clear-confirm")!);

    view.unmount();
    await act(async () => undefined);

    expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(1);
  });

  it("ignores toggle changes while an export is running", async () => {
    const bridge = bridgeWith({ retainedBytes: 4096 });
    bridge.export_diagnostics_log.mockReturnValueOnce(new Promise<DiagnosticsExportResult>(() => undefined));
    await renderLoaded(bridge);
    fireEvent.click(exportButton());

    fireEvent.click(toggle());

    expect(bridge.set_diagnostics_enabled).not.toHaveBeenCalled();
    expect(toggle()).not.toBeChecked();
  });

  it("closes the clear dialog on Escape and returns focus to Clear logs", async () => {
    await renderLoaded(bridgeWith({ retainedBytes: 4096 }));
    fireEvent.click(clearButton());

    fireEvent.keyDown(document, { key: "Escape" });

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    await waitFor(() => expect(clearButton()).toHaveFocus());
  });

  it("refreshes the state when the window regains focus", async () => {
    const bridge = bridgeWith();
    await renderLoaded(bridge);
    bridge.get_diagnostics_state.mockResolvedValueOnce({ ...base, retainedBytes: 2048 });

    await act(async () => { window.dispatchEvent(new Event("focus")); });

    await waitFor(() => expect(document.getElementById("diagnostics-size-value")?.textContent).toContain("2 KiB of 4 MiB used"));
  });

  it("clears a load error once a later refresh succeeds", async () => {
    const bridge = bridgeWith();
    bridge.get_diagnostics_state.mockRejectedValueOnce(rejectionError("other"));
    render(<DiagnosticsSettings bridge={bridge as DiagnosticsBridge} />);
    expect(await screen.findByRole("alert")).toHaveTextContent(STORAGE_TEXT);

    await act(async () => { window.dispatchEvent(new Event("focus")); });

    await waitFor(() => expect(toggle()).toBeEnabled());
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Try again" })).not.toBeInTheDocument();
  });

  it("shows the storage message when loading is rejected with the storage code", async () => {
    const bridge = bridgeWith();
    bridge.get_diagnostics_state.mockRejectedValueOnce(rejectionError("diagnostics-storage"));
    render(<DiagnosticsSettings bridge={bridge as DiagnosticsBridge} />);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(STORAGE_TEXT);
    expect(alert).toHaveAttribute("data-diagnostics-error", "load");
  });

  it("re-reads the state after a failed clear", async () => {
    const bridge = bridgeWith({ retainedBytes: 4096 });
    bridge.clear_diagnostics_log.mockRejectedValueOnce(rejectionError("other"));
    await renderLoaded(bridge);
    fireEvent.click(clearButton());
    fireEvent.click(document.getElementById("diagnostics-clear-confirm")!);

    await waitFor(() => expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(2));
    expect(await screen.findByRole("alert")).toHaveTextContent(CLEAR_TEXT);
  });

  it("shows the export message when the export is rejected", async () => {
    const bridge = bridgeWith({ retainedBytes: 4096 });
    bridge.export_diagnostics_log.mockRejectedValueOnce(rejectionError("native-dialog"));
    await renderLoaded(bridge);

    fireEvent.click(exportButton());

    expect(await screen.findByRole("alert")).toHaveTextContent(EXPORT_TEXT);
    expect(screen.getByRole("alert")).toHaveAttribute("data-diagnostics-error", "export");
    expect(document.getElementById("diagnostics-enabled-group")?.querySelector("#diagnostics-error")).toBeNull();
    const errorParagraph = document.getElementById("diagnostics-error");
    expect(errorParagraph?.previousElementSibling?.id).toBe("diagnostics-privacy");
    expect(errorParagraph?.nextElementSibling?.id).toBe("diagnostics-actions");
    await waitFor(() => expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(2));
  });

  it("shows the storage message when an export fails with the storage code", async () => {
    const bridge = bridgeWith({ retainedBytes: 4096 });
    bridge.export_diagnostics_log.mockRejectedValueOnce(rejectionError("diagnostics-storage"));
    await renderLoaded(bridge);

    fireEvent.click(exportButton());

    expect(await screen.findByRole("alert")).toHaveTextContent(STORAGE_TEXT);
  });

  it("opens the clear dialog with the exact copy and cancels without clearing", async () => {
    const bridge = bridgeWith({ retainedBytes: 4096 });
    await renderLoaded(bridge);

    fireEvent.click(clearButton());

    const dialog = screen.getByRole("dialog", { name: "Clear diagnostic logs?" });
    expect(dialog).toHaveAttribute("id", "diagnostics-clear-dialog");
    expect(dialog).toHaveTextContent("This removes the logs kept on this computer. It cannot erase copies you already exported.");
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(bridge.clear_diagnostics_log).not.toHaveBeenCalled();
    await waitFor(() => expect(clearButton()).toHaveFocus());
  });

  it("clears once on confirm, announces success, and moves focus to the heading", async () => {
    const bridge = bridgeWith({ retainedBytes: 4096 });
    await renderLoaded(bridge);

    fireEvent.click(clearButton());
    fireEvent.click(document.getElementById("diagnostics-clear-confirm")!);

    expect(await screen.findByText("Logs cleared.")).toBeInTheDocument();
    expect(bridge.clear_diagnostics_log).toHaveBeenCalledTimes(1);
    expect(document.getElementById("settings-diagnostics-title")).toHaveFocus();
  });

  it("shows the clear message and refocuses Clear logs when clearing is rejected", async () => {
    const bridge = bridgeWith({ retainedBytes: 4096 });
    bridge.clear_diagnostics_log.mockRejectedValueOnce(rejectionError("other"));
    await renderLoaded(bridge);

    fireEvent.click(clearButton());
    fireEvent.click(document.getElementById("diagnostics-clear-confirm")!);

    expect(await screen.findByRole("alert")).toHaveTextContent(CLEAR_TEXT);
    expect(screen.getByRole("alert")).toHaveAttribute("data-diagnostics-error", "clear");
    await waitFor(() => expect(clearButton()).toHaveFocus());
  });

  it("ignores export and clear while an export is in flight and marks them busy", async () => {
    const bridge = bridgeWith({ retainedBytes: 4096 });
    let finishExport!: (result: { outcome: "saved" }) => void;
    bridge.export_diagnostics_log.mockReturnValueOnce(new Promise(resolve => { finishExport = resolve; }));
    await renderLoaded(bridge);

    fireEvent.click(exportButton());

    expect(statusMessage()).toHaveTextContent("Working…");
    expect(exportButton()).toHaveAttribute("aria-disabled", "true");
    expect(exportButton()).toHaveAttribute("aria-busy", "true");
    expect(clearButton()).toHaveAttribute("aria-disabled", "true");
    expect(document.getElementById("settings-diagnostics")).not.toHaveAttribute("aria-busy");
    fireEvent.click(exportButton());
    fireEvent.click(clearButton());
    expect(bridge.export_diagnostics_log).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    await act(async () => finishExport({ outcome: "saved" }));
    expect(statusMessage()).toHaveTextContent("Log exported.");
    expect(exportButton()).not.toHaveAttribute("aria-busy");
  });

  it("shows the storage message from storageError and lets a local action error take precedence", async () => {
    const bridge = bridgeWith({ storageError: true });
    bridge.set_diagnostics_enabled.mockRejectedValueOnce(rejectionError("other"));
    await renderLoaded(bridge);

    expect(screen.getByRole("alert")).toHaveTextContent(STORAGE_TEXT);

    fireEvent.click(toggle());

    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(peppyCopy.setting_update_error));
  });

  it("opens the help dialog with three paragraphs and returns focus on Close", async () => {
    await renderLoaded(bridgeWith());

    fireEvent.click(screen.getByRole("button", { name: "About diagnostics" }));

    const dialog = screen.getByRole("dialog", { name: "About diagnostics" });
    expect(dialog).toHaveTextContent("Verbose logging adds a little overhead. Turn it off after testing.");
    expect(dialog).toHaveTextContent("When the log is full, the oldest records are removed.");
    expect(dialog).toHaveTextContent("Dropped events counts records Peppy could not save because the log was busy or unavailable.");
    fireEvent.click(screen.getByRole("button", { name: "Close" }));

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "About diagnostics" })).toHaveFocus();
  });

  it("shows the load error and Try again when a refresh fails before anything has loaded", async () => {
    const bridge = bridgeWith();
    let resolveInitial!: (state: DiagnosticsState) => void;
    bridge.get_diagnostics_state.mockReturnValueOnce(new Promise<DiagnosticsState>(resolve => { resolveInitial = resolve; }));
    bridge.get_diagnostics_state.mockRejectedValueOnce(rejectionError("other"));
    render(<DiagnosticsSettings bridge={bridge as DiagnosticsBridge} />);
    expect(screen.getByText("Loading diagnostics…")).toBeInTheDocument();

    await act(async () => { window.dispatchEvent(new Event("focus")); });

    expect(await screen.findByRole("alert")).toHaveTextContent(STORAGE_TEXT);
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
    expect(screen.queryByText("Loading diagnostics…")).not.toBeInTheDocument();

    await act(async () => resolveInitial({ ...base }));

    await waitFor(() => expect(toggle()).toBeEnabled());
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  describe("polling", () => {
    async function flush() {
      await act(async () => {
        for (let turn = 0; turn < 5; turn += 1) await Promise.resolve();
      });
    }

    function setVisibility(value: DocumentVisibilityState) {
      Object.defineProperty(document, "visibilityState", { configurable: true, get: () => value });
    }

    it("refreshes the size every 2 s while recording and visible", async () => {
      vi.useFakeTimers();
      const bridge = bridgeWith({ enabled: true, retainedBytes: 0 });
      render(<DiagnosticsSettings bridge={bridge as DiagnosticsBridge} />);
      await flush();
      expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(1);

      bridge.get_diagnostics_state.mockResolvedValueOnce({ ...base, enabled: true, retainedBytes: 5 * 1024 });
      await act(async () => { vi.advanceTimersByTime(2_000); });
      await flush();

      expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(2);
      expect(document.getElementById("diagnostics-size-value")?.textContent).toContain("5 KiB of 4 MiB used");
    });

    it("does not poll while recording is off", async () => {
      vi.useFakeTimers();
      const bridge = bridgeWith({ enabled: false });
      render(<DiagnosticsSettings bridge={bridge as DiagnosticsBridge} />);
      await flush();

      await act(async () => { vi.advanceTimersByTime(6_000); });
      expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(1);
    });

    it("does not poll while the window is hidden and resumes when it is visible", async () => {
      vi.useFakeTimers();
      const bridge = bridgeWith({ enabled: true });
      setVisibility("hidden");
      try {
        render(<DiagnosticsSettings bridge={bridge as DiagnosticsBridge} />);
        await flush();
        await act(async () => { vi.advanceTimersByTime(6_000); });
        expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(1);

        setVisibility("visible");
        await act(async () => { document.dispatchEvent(new Event("visibilitychange")); });
        await flush();
        const callsAfterShow = bridge.get_diagnostics_state.mock.calls.length;
        await act(async () => { vi.advanceTimersByTime(2_000); });
        expect(bridge.get_diagnostics_state.mock.calls.length).toBeGreaterThan(callsAfterShow);
      } finally {
        setVisibility("visible");
      }
    });

    it("stops polling when the section unmounts", async () => {
      vi.useFakeTimers();
      const bridge = bridgeWith({ enabled: true });
      const view = render(<DiagnosticsSettings bridge={bridge as DiagnosticsBridge} />);
      await flush();
      view.unmount();

      await act(async () => { vi.advanceTimersByTime(6_000); });
      expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(1);
    });

    it("refreshes after an action even when an earlier poll never settles", async () => {
      vi.useFakeTimers();
      const bridge = bridgeWith({ enabled: true, retainedBytes: 4096 });
      render(<DiagnosticsSettings bridge={bridge as DiagnosticsBridge} />);
      await flush();
      bridge.get_diagnostics_state.mockReturnValueOnce(new Promise<DiagnosticsState>(() => undefined));
      await act(async () => { vi.advanceTimersByTime(2_000); });
      expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(2);

      fireEvent.click(document.getElementById("diagnostics-export")!);
      await flush();

      expect(bridge.export_diagnostics_log).toHaveBeenCalledTimes(1);
      expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(3);
    });

    it("starts no second silent poll while one is unresolved", async () => {
      vi.useFakeTimers();
      const bridge = bridgeWith({ enabled: true });
      render(<DiagnosticsSettings bridge={bridge as DiagnosticsBridge} />);
      await flush();
      bridge.get_diagnostics_state.mockReturnValueOnce(new Promise<DiagnosticsState>(() => undefined));

      for (let tick = 0; tick < 3; tick += 1) {
        await act(async () => { vi.advanceTimersByTime(2_000); });
      }

      expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(2);
    });

    it("keeps the last values and shows nothing when a silent poll fails", async () => {
      vi.useFakeTimers();
      const bridge = bridgeWith({ enabled: true, retainedBytes: 1024 });
      render(<DiagnosticsSettings bridge={bridge as DiagnosticsBridge} />);
      await flush();
      bridge.get_diagnostics_state.mockRejectedValueOnce(rejectionError("other"));

      await act(async () => { vi.advanceTimersByTime(2_000); });
      await flush();

      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
      expect(document.getElementById("diagnostics-size-value")?.textContent).toContain("1 KiB of 4 MiB used");
      expect(toggle()).toBeChecked();
    });

    it("does not poll while an export is running", async () => {
      vi.useFakeTimers();
      const bridge = bridgeWith({ enabled: true, retainedBytes: 4096 });
      bridge.export_diagnostics_log.mockReturnValueOnce(new Promise<DiagnosticsExportResult>(() => undefined));
      render(<DiagnosticsSettings bridge={bridge as DiagnosticsBridge} />);
      await flush();
      fireEvent.click(document.getElementById("diagnostics-export")!);

      await act(async () => { vi.advanceTimersByTime(6_000); });

      expect(bridge.get_diagnostics_state).toHaveBeenCalledTimes(1);
    });

    it("ignores a poll that started before a toggle when it resolves after the toggle", async () => {
      vi.useFakeTimers();
      const bridge = bridgeWith({ enabled: true, retainedBytes: 0 });
      render(<DiagnosticsSettings bridge={bridge as DiagnosticsBridge} />);
      await flush();

      let resolvePoll!: (state: DiagnosticsState) => void;
      bridge.get_diagnostics_state.mockReturnValueOnce(new Promise<DiagnosticsState>(resolve => { resolvePoll = resolve; }));
      await act(async () => { vi.advanceTimersByTime(2_000); });

      bridge.set_diagnostics_enabled.mockResolvedValueOnce({ ...base, enabled: false, retainedBytes: 777 });
      fireEvent.click(toggle());
      await flush();
      expect(document.getElementById("diagnostics-size-value")?.textContent).toContain("777 B of 4 MiB used");

      await act(async () => resolvePoll({ ...base, enabled: true, retainedBytes: 1 }));
      await flush();
      expect(document.getElementById("diagnostics-recording-value")?.textContent).toBe("Off. Retained logs are still available.");
      expect(document.getElementById("diagnostics-size-value")?.textContent).toContain("777 B of 4 MiB used");
    });
  });
});
