import { useEffect, useRef } from "react";
import { Copy, Eye, EyeOff, RefreshCw } from "@peppy/desktop-ui";

const MASKED_SECRET = `whsec_${"•".repeat(24)}`;
const NOT_SAVED_NOTE = "Created after you save.";
const VERIFY_HINT = "Verify with any Standard Webhooks library (Svix-compatible).";

type WebhookSecretFieldProps = {
  hasWebhook: boolean;
  secret: string | null;
  revealed: boolean;
  busy: boolean;
  status: string;
  error: string;
  rotateOpen: boolean;
  /** Changes when a new secret arrives after create; the field then moves focus to Copy. */
  focusKey: number;
  onToggleReveal(): void;
  onCopy(): void;
  onStartRotate(): void;
  onCancelRotate(): void;
  onConfirmRotate(): void;
};

export function WebhookSecretField({
  hasWebhook,
  secret,
  revealed,
  busy,
  status,
  error,
  rotateOpen,
  focusKey,
  onToggleReveal,
  onCopy,
  onStartRotate,
  onCancelRotate,
  onConfirmRotate,
}: WebhookSecretFieldProps) {
  const fieldRef = useRef<HTMLDivElement>(null);
  const copyButton = useRef<HTMLButtonElement>(null);
  const rotateButton = useRef<HTMLButtonElement>(null);
  const secretInput = useRef<HTMLInputElement>(null);
  const cancelButton = useRef<HTMLButtonElement>(null);
  const wasRotateOpen = useRef(false);
  /** Where focus goes when the rotate confirmation closes: back to Rotate on cancel, else the secret. */
  const rotateReturnTarget = useRef<"rotate" | "secret">("secret");

  useEffect(() => {
    if (focusKey === 0) return;
    fieldRef.current?.scrollIntoView?.({ block: "nearest" });
    copyButton.current?.focus({ preventScroll: true });
  }, [focusKey]);

  useEffect(() => {
    if (rotateOpen) {
      wasRotateOpen.current = true;
      cancelButton.current?.focus();
      return;
    }
    if (!wasRotateOpen.current) return;
    wasRotateOpen.current = false;
    const target = rotateReturnTarget.current === "rotate" ? rotateButton.current : secretInput.current;
    target?.focus();
    rotateReturnTarget.current = "secret";
  }, [rotateOpen]);

  const cancelRotate = () => {
    rotateReturnTarget.current = "rotate";
    onCancelRotate();
  };

  const shownValue = revealed && secret !== null ? secret : MASKED_SECRET;

  return (
    <div id="webhook-secret" ref={fieldRef} className="webhook-field" data-webhook-secret>
      <span id="webhook-secret-label" className="webhook-field-label">Signing secret</span>
      {hasWebhook ? (
        <>
          <div className="webhook-secret-row">
            <input
              ref={secretInput}
              id="webhook-secret-value"
              className="webhook-input webhook-input-mono"
              readOnly
              aria-labelledby="webhook-secret-label"
              value={shownValue}
            />
            <button
              id="webhook-secret-reveal"
              type="button"
              className="webhook-icon-button"
              aria-label={revealed ? "Hide secret" : "Reveal secret"}
              disabled={busy}
              onClick={onToggleReveal}
            >
              {revealed ? <EyeOff size={14} aria-hidden="true" /> : <Eye size={14} aria-hidden="true" />}
            </button>
            <button
              ref={copyButton}
              id="webhook-secret-copy"
              type="button"
              className="webhook-icon-button"
              aria-label="Copy secret"
              disabled={busy}
              onClick={onCopy}
            >
              <Copy size={14} aria-hidden="true" />
            </button>
            <button
              ref={rotateButton}
              id="webhook-secret-rotate"
              type="button"
              className="secondary-button webhook-compact-button"
              disabled={busy || rotateOpen}
              onClick={onStartRotate}
            >
              <RefreshCw size={13} aria-hidden="true" />
              Rotate
            </button>
          </div>
          {rotateOpen && (
            <div id="webhook-rotate-confirm" className="webhook-rotate-confirm">
              <p id="webhook-rotate-text">Rotate secret? Receivers must be updated.</p>
              <button
                ref={cancelButton}
                id="webhook-rotate-cancel"
                type="button"
                className="secondary-button webhook-compact-button"
                disabled={busy}
                onClick={cancelRotate}
              >
                Cancel
              </button>
              <button
                id="webhook-rotate-apply"
                type="button"
                className="destructive-button webhook-compact-button"
                disabled={busy}
                onClick={onConfirmRotate}
              >
                Rotate secret
              </button>
            </div>
          )}
        </>
      ) : (
        <p id="webhook-secret-note" className="webhook-field-note">{NOT_SAVED_NOTE}</p>
      )}
      {status && <p id="webhook-secret-status" className="webhook-field-status" role="status">{status}</p>}
      {error && <p id="webhook-secret-error" className="settings-error" role="alert">{error}</p>}
      <p className="webhook-field-hint">{VERIFY_HINT}</p>
    </div>
  );
}
