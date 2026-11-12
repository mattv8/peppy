import { useEffect, useRef, useState } from "react";
import type { AppLockStatus } from "../bridge/bridge";

/**
 * Native-only app lock UI. Passwords are entered in native prompts; nothing here collects,
 * stores, or renders a password.
 */

const FAILED_CHANGE = "Peppy could not change the password requirement.";

const messageOf = (error: unknown, fallback = FAILED_CHANGE): string =>
  typeof error === "object" && error !== null && "message" in error && typeof error.message === "string" && error.message
    ? error.message
    : fallback;

export function AppLockSettings({
  status,
  onChange,
}: {
  status: AppLockStatus | null;
  onChange(enabled: boolean): Promise<boolean>;
}) {
  const [pending, setPending] = useState(false);
  const [message, setMessage] = useState("");
  const storageAvailable = status?.storageAvailable ?? true;

  const change = async (enabled: boolean) => {
    setPending(true);
    setMessage("");
    try {
      const changed = await onChange(enabled);
      if (!changed) setMessage("The password requirement was not changed.");
    } catch (error) {
      setMessage(messageOf(error));
    } finally {
      setPending(false);
    }
  };

  return (
    <section id="settings-app-lock" className="settings-card" data-settings-section="app-lock">
      <h2>App lock</h2>
      <div id="app-lock-enabled-row" className="settings-control-row">
        <label htmlFor="app-lock-enabled">Require a password when Peppy starts</label>
        <span className="settings-switch">
          <input
            id="app-lock-enabled"
            type="checkbox"
            role="switch"
            checked={status?.enabled ?? false}
            aria-checked={status?.enabled ?? false}
            disabled={pending || !status || !storageAvailable || status.pending}
            aria-busy={pending || undefined}
            onChange={(event) => void change(event.target.checked)}
          />
          <span aria-hidden />
        </span>
      </div>
      <p className="settings-field-hint">
        Changes apply the next time Peppy starts. Peppy does not lock while it is already open.
      </p>
      {!storageAvailable && (
        <p className="settings-error" role="alert">
          Secure storage is unavailable, so this setting cannot be changed.
        </p>
      )}
      {message && <p className="settings-error" role="alert">{message}</p>}
    </section>
  );
}

export function AppLockScreen({
  state,
  onUnlock,
  onRetry,
}: {
  state: "checking" | "storage-unavailable" | "locked";
  onUnlock(): Promise<boolean>;
  onRetry(): Promise<void>;
}) {
  const [pending, setPending] = useState(false);
  const [message, setMessage] = useState("");
  const previousState = useRef(state);
  const retryButton = useRef<HTMLButtonElement>(null);
  const restoreRetryFocusRef = useRef(false);

  useEffect(() => {
    const wasChecking = previousState.current === "checking";
    previousState.current = state;
    if (!wasChecking || state === "checking" || !document.hasFocus()) return;
    const activeElement = document.activeElement;
    if (activeElement !== document.body && !activeElement?.closest("#app-lock-card")) return;
    document.getElementById(state === "locked" ? "app-lock-unlock" : "app-lock-retry")?.focus();
  }, [state]);

  useEffect(() => {
    if (!restoreRetryFocusRef.current || pending || state !== "storage-unavailable" || !document.hasFocus()) return;
    restoreRetryFocusRef.current = false;
    retryButton.current?.focus();
  }, [pending, state]);

  const run = async (action: () => Promise<void>, fallback: string, restoreRetryFocus = false) => {
    setPending(true);
    setMessage("");
    try {
      await action();
    } catch (error) {
      setMessage(messageOf(error, fallback));
      restoreRetryFocusRef.current = restoreRetryFocus;
    } finally {
      setPending(false);
    }
  };

  if (state === "checking") {
    return (
      <main id="app-lock-screen" className="app-lock-screen" data-app-lock-state="checking" aria-busy="true">
        <section id="app-lock-card" className="app-lock-card" aria-labelledby="app-lock-heading" aria-describedby="app-lock-status">
          <h1 id="app-lock-heading">Checking Peppy&apos;s lock</h1>
          <p id="app-lock-status" role="status">
            Peppy is checking its password setting in secure storage. Your conversations stay hidden until this finishes. If your system asks for keychain access, allow it to continue.
          </p>
          <button id="app-lock-unlock" className="primary-button" disabled aria-describedby="app-lock-status">Unlock</button>
        </section>
      </main>
    );
  }

  if (state === "storage-unavailable") {
    return (
      <main id="app-lock-screen" className="app-lock-screen" data-app-lock-state="storage-unavailable" aria-busy={pending || undefined}>
        <section id="app-lock-card" className="app-lock-card" aria-labelledby="app-lock-heading">
          <h1 id="app-lock-heading">Peppy cannot check its lock</h1>
          <p>
            Secure storage on this computer is unavailable, so Peppy cannot open until it can read its password setting.
            Make sure your system keychain is available, then try again. If you declined keychain access, choose Try again and allow it.
          </p>
          <button ref={retryButton} id="app-lock-retry" className="secondary-button" disabled={pending} aria-busy={pending || undefined} onClick={() => void run(onRetry, "Peppy still cannot reach secure storage.", true)}>
            Try again
          </button>
          {message && <p role="alert" className="settings-error">{message}</p>}
        </section>
      </main>
    );
  }

  return (
    <main id="app-lock-screen" className="app-lock-screen" data-app-lock-state="locked" aria-busy={pending || undefined}>
      <section id="app-lock-card" className="app-lock-card" aria-labelledby="app-lock-heading">
        <h1 id="app-lock-heading">Peppy is locked</h1>
        <p>Unlock Peppy with your password to see your conversations.</p>
        <button
          id="app-lock-unlock"
          className="primary-button"
          disabled={pending}
          aria-busy={pending || undefined}
          onClick={() => void run(async () => {
            if (!(await onUnlock())) setMessage("Peppy could not unlock.");
          }, FAILED_CHANGE)}
        >
          Unlock
        </button>
        {message && <p role="alert" className="settings-error">{message}</p>}
      </section>
    </main>
  );
}
