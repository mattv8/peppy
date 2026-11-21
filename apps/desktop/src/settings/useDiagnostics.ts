import { useEffect, useRef, useState } from "react";
import type { DiagnosticsState } from "@peppy/desktop-ui/bridge-contracts";
import type { DesktopBridge } from "../bridge/bridge";
import { errorCode } from "../bridge/bridgeErrors";
import { recordingText } from "./diagnosticsDisplay";
import { useTransientMessage } from "./useTransientMessage";

export type DiagnosticsBridge = Required<Pick<DesktopBridge, "get_diagnostics_state" | "set_diagnostics_enabled" | "export_diagnostics_log" | "clear_diagnostics_log">>;

/** Narrows the host to the diagnostics bridge only when every method exists. */
export function hasDiagnosticsBridge<T extends DesktopBridge>(source: T): source is T & DiagnosticsBridge {
  return Boolean(source.get_diagnostics_state && source.set_diagnostics_enabled && source.export_diagnostics_log && source.clear_diagnostics_log);
}

export type DiagnosticsErrorKind = "load" | "setting" | "storage" | "export" | "clear";
export type DiagnosticsBusy = "toggle" | "export" | "clear";
/** `skipped` means the action was refused (another action is running) or the section has unmounted. */
export type DiagnosticsActionResult = "done" | "failed" | "skipped";

const DIAGNOSTICS_STORAGE_CODE = "diagnostics-storage";
const POLL_INTERVAL_MS = 2_000;
const WORKING_TEXT = "Working…";
const EXPORTED_TEXT = "Log exported.";
const CLEARED_TEXT = "Logs cleared.";

const isDocumentVisible = () => document.visibilityState !== "hidden";

/** A rejection with the storage code names the cause for any action; otherwise the failing action decides. */
const failureKind = (error: unknown, fallback: DiagnosticsErrorKind): DiagnosticsErrorKind => errorCode(error) === DIAGNOSTICS_STORAGE_CODE ? "storage" : fallback;

/**
 * Lifecycle of the diagnostics section: loading, polling while recording, focus and visibility refresh,
 * and the actions. Only `runAction` checks for a running action, so there is one busy guard.
 */
export function useDiagnostics(bridge: DiagnosticsBridge) {
  const [state, setState] = useState<DiagnosticsState | null>(null);
  const [busy, setBusy] = useState<DiagnosticsBusy | null>(null);
  const [error, setError] = useState<DiagnosticsErrorKind | null>(null);
  const [visible, setVisible] = useState(isDocumentVisible);
  const status = useTransientMessage();
  // Only an action invalidates loads sent before it. Among loads, a response applies only if it is newer than the last one applied.
  const actionEpoch = useRef(0);
  const loadSeq = useRef(0);
  const appliedLoad = useRef(0);
  const busyRef = useRef<DiagnosticsBusy | null>(null);
  const mountedRef = useRef(false);
  // The request number of the silent load in flight, if any. An action clears it, so a poll that never settles cannot block later refreshes.
  const silentRequest = useRef<number | null>(null);
  // Mirrors `state` for callbacks that must know whether anything has loaded yet.
  const stateRef = useRef<DiagnosticsState | null>(null);

  const applyState = (next: DiagnosticsState) => {
    stateRef.current = next;
    setState(next);
  };

  const loadState = async ({ silent }: { silent: boolean }) => {
    if (!mountedRef.current || (silent && (busyRef.current || silentRequest.current !== null))) return;
    const request = ++loadSeq.current;
    const epoch = actionEpoch.current;
    const isLive = () => mountedRef.current && epoch === actionEpoch.current;
    if (silent) silentRequest.current = request;
    try {
      const next = await bridge.get_diagnostics_state();
      if (isLive() && request > appliedLoad.current) {
        appliedLoad.current = request;
        applyState(next);
        // A later successful read proves storage answers again, so a stale load failure goes.
        setError(previous => previous === "load" ? null : previous);
      }
    } catch {
      // A failed refresh only matters when nothing has loaded yet; otherwise keep the last values silently.
      if (isLive() && request > appliedLoad.current && (!silent || stateRef.current === null)) setError("load");
    } finally {
      if (silentRequest.current === request) silentRequest.current = null;
    }
  };

  const runAction = async <T,>(kind: DiagnosticsBusy, failure: DiagnosticsErrorKind, action: () => Promise<T>, apply: (value: T) => void): Promise<DiagnosticsActionResult> => {
    if (busyRef.current || !mountedRef.current) return "skipped";
    busyRef.current = kind;
    actionEpoch.current += 1;
    silentRequest.current = null;
    setBusy(kind);
    setError(null);
    status.clear();
    try {
      const value = await action();
      if (!mountedRef.current) return "skipped";
      apply(value);
      return "done";
    } catch (caught) {
      if (mountedRef.current) setError(failureKind(caught, failure));
      return "failed";
    } finally {
      busyRef.current = null;
      if (mountedRef.current) setBusy(null);
    }
  };

  const setRecording = (enabled: boolean) => runAction("toggle", "setting", () => bridge.set_diagnostics_enabled({ enabled }), applyState);

  const exportLog = async () => {
    const result = await runAction("export", "export", () => bridge.export_diagnostics_log(), outcome => {
      // A cancelled save panel is not an error and announces nothing.
      if (outcome.outcome === "saved") status.show(EXPORTED_TEXT);
    });
    // Size may have grown; re-read whatever the outcome.
    void loadState({ silent: true });
    return result;
  };

  const clearLogs = async () => {
    const result = await runAction("clear", "clear", () => bridge.clear_diagnostics_log(), next => {
      applyState(next);
      status.show(CLEARED_TEXT);
    });
    // A partial delete may have changed the size.
    if (result === "failed") void loadState({ silent: true });
    return result;
  };

  const retryLoad = () => {
    setError(null);
    void loadState({ silent: false });
  };

  useEffect(() => {
    mountedRef.current = true;
    void loadState({ silent: false });
    const onFocus = () => { if (isDocumentVisible()) void loadState({ silent: true }); };
    const onVisibilityChange = () => {
      const nowVisible = isDocumentVisible();
      setVisible(nowVisible);
      if (nowVisible) void loadState({ silent: true });
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      mountedRef.current = false;
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [bridge]);

  const recording = state?.enabled === true;
  useEffect(() => {
    if (!recording || !visible || busy !== null) return;
    const timer = window.setInterval(() => void loadState({ silent: true }), POLL_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [recording, visible, busy, bridge]);

  return {
    state,
    busy,
    errorKind: error ?? (state?.storageError ? "storage" : null),
    statusText: busy === "export" || busy === "clear" ? WORKING_TEXT : status.text,
    recordingLabel: state ? recordingText(state) : "",
    setRecording,
    exportLog,
    clearLogs,
    retryLoad,
  };
}
