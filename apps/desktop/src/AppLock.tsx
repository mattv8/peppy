import { useState } from "react";
import type { AppLockStatus } from "./bridge";

/**
 * Native-only app lock UI. Passwords are entered in native prompts; nothing here collects,
 * stores, or renders a password.
 */

const FAILED_CHANGE = "Peppy could not change the password requirement.";

const messageOf = (error: unknown): string =>
  typeof error === "object" && error !== null && "message" in error && typeof error.message === "string" && error.message
    ? error.message
    : FAILED_CHANGE;

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
      <label className="settings-check">
        <input
          id="app-lock-enabled"
          type="checkbox"
          checked={status?.enabled ?? false}
          disabled={pending || !status || !storageAvailable}
          aria-busy={pending || undefined}
          onChange={(event) => void change(event.target.checked)}
        />
        Require a password when Peppy starts
      </label>
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
  storageUnavailable,
  onUnlock,
  onRetry,
}: {
  storageUnavailable: boolean;
  onUnlock(): Promise<boolean>;
  onRetry(): Promise<void>;
}) {
  const [pending, setPending] = useState(false);
  const [message, setMessage] = useState("");

  const run = async (action: () => Promise<void>) => {
    setPending(true);
    setMessage("");
    try {
      await action();
    } catch (error) {
      setMessage(messageOf(error));
    } finally {
      setPending(false);
    }
  };

  if (storageUnavailable) {
    return (
      <main id="app-lock-screen" className="app-lock-screen" data-app-lock-state="storage-unavailable" aria-busy={pending || undefined}>
        <section className="app-lock-card" aria-labelledby="app-lock-heading">
          <h1 id="app-lock-heading">Peppy cannot check its lock</h1>
          <p>
            Secure storage on this computer is unavailable, so Peppy cannot open until it can read its password setting.
            Make sure your system keychain is available, then try again.
          </p>
          <button className="secondary-button" disabled={pending} aria-busy={pending || undefined} onClick={() => void run(onRetry)}>
            Try again
          </button>
          {message && <p role="alert" className="settings-error">{message}</p>}
        </section>
      </main>
    );
  }

  return (
    <main id="app-lock-screen" className="app-lock-screen" data-app-lock-state="locked" aria-busy={pending || undefined}>
      <section className="app-lock-card" aria-labelledby="app-lock-heading">
        <h1 id="app-lock-heading">Peppy is locked</h1>
        <p>Unlock Peppy with your password to see your conversations.</p>
        <button
          className="primary-button"
          disabled={pending}
          aria-busy={pending || undefined}
          onClick={() => void run(async () => {
            if (!(await onUnlock())) setMessage("Peppy is still locked.");
          })}
        >
          Unlock
        </button>
        {message && <p role="alert" className="settings-error">{message}</p>}
      </section>
    </main>
  );
}
