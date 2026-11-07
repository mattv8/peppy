import { useEffect, useRef, useState } from "react";
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
  const generation = useRef(0);

  useEffect(() => () => { generation.current += 1; }, []);

  const isCurrent = (requestGeneration: number) => generation.current === requestGeneration;

  const attempt = async (action: (requestGeneration: number) => Promise<void>) => {
    const requestGeneration = generation.current;
    setBusy(true);
    setError("");
    try {
      await action(requestGeneration);
    } catch (reason) {
      if (isCurrent(requestGeneration)) setError(webhookErrorMessage(errorCode(reason)));
    } finally {
      if (isCurrent(requestGeneration)) setBusy(false);
    }
  };

  const loadSecret = async (requestGeneration: number): Promise<string> => {
    if (value !== null) return value;
    if (webhookId === null) throw new Error("Save the webhook first.");
    const fetched = (await bridge.webhook_reveal_secret(webhookId)).secret;
    if (isCurrent(requestGeneration)) setValue(fetched);
    return fetched;
  };

  const toggleReveal = () => {
    if (revealed) {
      setRevealed(false);
      return;
    }
    void attempt(async (requestGeneration) => {
      await loadSecret(requestGeneration);
      if (!isCurrent(requestGeneration)) return;
      setRevealed(true);
      status.clear();
    });
  };

  const copy = () => {
    void attempt(async (requestGeneration) => {
      const current = await loadSecret(requestGeneration);
      if (!isCurrent(requestGeneration)) return;
      try {
        await navigator.clipboard.writeText(current);
        if (!isCurrent(requestGeneration)) return;
        status.show(COPIED_MESSAGE, COPIED_DURATION_MS);
      } catch {
        if (!isCurrent(requestGeneration)) return;
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
    void attempt(async (requestGeneration) => {
      if (webhookId === null) return;
      const next = await bridge.webhook_rotate_secret(webhookId);
      if (!isCurrent(requestGeneration)) return;
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
