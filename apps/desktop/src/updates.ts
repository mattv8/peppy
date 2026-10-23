import { useCallback, useEffect, useRef, useState } from "react";
import type { UpdateStatus } from "./update-contract";
import { errorCode, errorText } from "./bridgeErrors";

const UPDATE_EVENT = "peppy://update-state";
const ACTIVITY_THROTTLE_MS = 1_000;
const HEARTBEAT_MS = 30_000;

const updateErrorText = (error: unknown) => {
  const code = errorCode(error);
  const message = errorText(error);
  return code && !message.includes(code) ? `${message} (${code})` : message;
};

export const isTauriUpdateHost = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const invokeUpdate = async <T>(command: string, args?: Record<string, unknown>) => {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(command, args);
};

export const updateApi = {
  status: () => invokeUpdate<UpdateStatus>("update_status"),
  check: () => invokeUpdate<UpdateStatus>("check_for_updates"),
  restart: () => invokeUpdate<UpdateStatus>("restart_for_update"),
  defer: () => invokeUpdate<UpdateStatus>("defer_update"),
  setAutomatic: (enabled: boolean) => invokeUpdate<UpdateStatus>("set_automatic_updates", { enabled }),
  reportActivity: (active: boolean) => invokeUpdate<void>("report_update_activity", { active }),
  subscribe(listener: () => void) {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void import("@tauri-apps/api/event")
      .then(({ listen }) => listen(UPDATE_EVENT, listener))
      .then((stop) => { unlisten = stop; if (disposed) stop(); })
      .catch(() => undefined);
    return () => { disposed = true; unlisten?.(); };
  },
};

export function useUpdateActivity(enabled: boolean): void {
  const lastActivityAt = useRef(0);

  useEffect(() => {
    if (!enabled || !isTauriUpdateHost()) return;
    const report = (active: boolean) => { void updateApi.reportActivity(active).catch(() => undefined); };
    const reportInteraction = () => {
      const now = Date.now();
      if (now - lastActivityAt.current < ACTIVITY_THROTTLE_MS) return;
      lastActivityAt.current = now;
      report(true);
    };
    report(true);
    const events: (keyof DocumentEventMap)[] = ["pointerdown", "keydown", "input", "focusin", "wheel"];
    events.forEach((event) => document.addEventListener(event, reportInteraction));
    const reportVisibility = () => report(document.visibilityState === "visible");
    window.addEventListener("focus", reportInteraction);
    document.addEventListener("visibilitychange", reportVisibility);
    const heartbeat = window.setInterval(() => report(false), HEARTBEAT_MS);
    return () => {
      events.forEach((event) => document.removeEventListener(event, reportInteraction));
      window.removeEventListener("focus", reportInteraction);
      document.removeEventListener("visibilitychange", reportVisibility);
      window.clearInterval(heartbeat);
    };
  }, [enabled]);
}

export function useUpdateStatus(enabled: boolean) {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const mounted = useRef(true);
  const busyRef = useRef(false);
  const refreshRequest = useRef(0);
  const actionGeneration = useRef(0);
  const settledActionGeneration = useRef(0);
  const actionError = useRef(false);

  const refresh = useCallback(async (explicit = false) => {
    if (!enabled) return;
    const request = ++refreshRequest.current;
    const actionAtStart = actionGeneration.current;
    const settledActionAtStart = settledActionGeneration.current;
    const canApply = () => mounted.current &&
      request === refreshRequest.current &&
      actionAtStart === actionGeneration.current &&
      settledActionAtStart === settledActionGeneration.current;
    try {
      const next = await updateApi.status();
      if (canApply()) {
        setStatus(next);
        if (explicit || !actionError.current) {
          actionError.current = false;
          setError("");
        }
      }
    } catch (error) {
      if (canApply() && (explicit || !actionError.current)) setError(updateErrorText(error));
    }
  }, [enabled]);

  useEffect(() => {
    mounted.current = true;
    void refresh();
    return () => { mounted.current = false; };
  }, [refresh]);
  useEffect(() => enabled ? updateApi.subscribe(() => void refresh()) : undefined, [enabled, refresh]);

  const run = useCallback(async (action: () => Promise<UpdateStatus>) => {
    if (busyRef.current) return;
    const generation = ++actionGeneration.current;
    busyRef.current = true;
    setBusy(true);
    setError("");
    actionError.current = false;
    try {
      const next = await action();
      if (mounted.current) { actionError.current = false; setStatus(next); setError(""); }
    } catch (error) {
      if (mounted.current) { actionError.current = true; setError(updateErrorText(error)); }
    } finally {
      settledActionGeneration.current = generation;
      busyRef.current = false;
      if (mounted.current) setBusy(false);
    }
  }, []);

  return { status, error, busy, refresh, run };
}
