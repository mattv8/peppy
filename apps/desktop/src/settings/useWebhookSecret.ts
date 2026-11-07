import { useState } from "react";
import { bridge } from "../bridge/bridge";
import { errorCode } from "../bridge/bridgeErrors";
import { webhookErrorMessage } from "./webhookDisplay";
import { useTransientMessage } from "./useTransientMessage";

const COPY_FAILED_MESSAGE = "Couldn't copy. Reveal the secret and copy it manually.";
const ROTATED_MESSAGE = "New secret created. Update receivers before they verify signatures.";
const COPIED_MESSAGE = "Copied.";
const COPIED_DURATION_MS = 2_000;

/** Signing-secret state for one webhook. The secret is fetched on demand and held only in this editor. */
export function useWebhookSecret(webhookId: string | null) {
  const [value, setValue] = useState<string | null>(null);
  const [revealed, setRevealed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [rotateOpen, setRotateOpen] = useState(false);
  /** Increments when a freshly created secret arrives, so the field can take focus once. */
  const [adoptedRevision, setAdoptedRevision] = useState(0);
  const status = useTransientMessage();

  const attempt = async (action: () => Promise<void>) => {
    setBusy(true);
    setError("");
    try {
      await action();
    } catch (reason) {
      setError(webhookErrorMessage(errorCode(reason)));
    } finally {
      setBusy(false);
    }
  };

  const loadSecret = async (): Promise<string> => {
    if (value !== null) return value;
    if (webhookId === null) throw new Error("Save the webhook first.");
    const fetched = (await bridge.webhook_reveal_secret(webhookId)).secret;
    setValue(fetched);
    return fetched;
  };

  const toggleReveal = () => {
    if (revealed) {
      setRevealed(false);
      return;
    }
    void attempt(async () => {
      await loadSecret();
      setRevealed(true);
      status.clear();
    });
  };

  const copy = () => {
    void attempt(async () => {
      const current = await loadSecret();
      try {
        await navigator.clipboard.writeText(current);
        status.show(COPIED_MESSAGE, COPIED_DURATION_MS);
      } catch {
        status.show(COPY_FAILED_MESSAGE);
      }
    });
  };

  const startRotate = () => {
    setError("");
    status.clear();
    setRotateOpen(true);
  };

  const cancelRotate = () => setRotateOpen(false);

  const confirmRotate = () => {
    void attempt(async () => {
      if (webhookId === null) return;
      const next = await bridge.webhook_rotate_secret(webhookId);
      setValue(next.secret);
      setRevealed(true);
      setRotateOpen(false);
      status.show(ROTATED_MESSAGE);
    });
  };

  /** Shows a secret the server just returned on create, revealed, without a second fetch. */
  const adopt = (created: string, message: string) => {
    setValue(created);
    setRevealed(true);
    setRotateOpen(false);
    setError("");
    setAdoptedRevision((revision) => revision + 1);
    status.show(message);
  };

  return {
    value,
    revealed,
    busy,
    status: status.text,
    error,
    rotateOpen,
    adoptedRevision,
    toggleReveal,
    copy,
    startRotate,
    cancelRotate,
    confirmRotate,
    adopt,
  };
}
