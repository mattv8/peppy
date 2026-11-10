import { useEffect, useRef, useState } from "react";
import { Check, Copy, TriangleAlert } from "@peppy/desktop-ui";
import type { OneTimeCodeTarget } from "@peppy/desktop-ui/bridge-contracts";
import { errorCode } from "../bridge/bridgeErrors";
import { useTransientMessage } from "../settings/useTransientMessage";
import "./one-time-code.css";

const COPIED_LABEL = "Copied";
const COPIED_DURATION_MS = 2_000;
const ERROR_DURATION_MS = 6_000;
const NO_LONGER_AVAILABLE_TEXT = "This code is no longer available.";
const COPY_FAILED_TEXT = "Couldn't copy the code. Try again.";

type Props = {
  variant: "message" | "notification";
  hookId: string;
  target: OneTimeCodeTarget;
  code: string;
  /** Notification only: the app name, used in the accessible name. */
  contextName?: string;
  onCopy(target: OneTimeCodeTarget, code: string): Promise<void>;
};

const copyErrorText = (error: unknown) => errorCode(error) === "no-code" ? NO_LONGER_AVAILABLE_TEXT : COPY_FAILED_TEXT;

export function OneTimeCodeButton({ variant, hookId, target, code, contextName, onCopy }: Props) {
  const feedback = useTransientMessage();
  const [isPending, setIsPending] = useState(false);
  const pending = useRef(false);
  const mounted = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  const copied = feedback.text === COPIED_LABEL;
  const failed = Boolean(feedback.text) && !copied;
  const state = isPending ? "pending" : copied ? "copied" : failed ? "failed" : "idle";
  const appName = contextName?.trim();
  const context = variant === "notification" && appName ? ` from ${appName}` : "";
  const accessibleName = `${copied ? "Copied" : "Copy"} code ${code}${context}`;

  const copy = async () => {
    if (pending.current) return;
    pending.current = true;
    setIsPending(true);
    // A re-copy while "Copied" keeps the label; show() restarts the timer on success.
    if (failed) feedback.clear();
    try {
      await onCopy(target, code);
      if (mounted.current) feedback.show(COPIED_LABEL, COPIED_DURATION_MS);
    } catch (error) {
      if (mounted.current) feedback.show(copyErrorText(error), ERROR_DURATION_MS);
    } finally {
      pending.current = false;
      if (mounted.current) setIsPending(false);
    }
  };

  return <div className="one-time-code-action" data-one-time-code-action={hookId} data-variant={variant} data-state={state}>
    <button
      type="button"
      className="secondary-button one-time-code-button"
      data-one-time-code-button={hookId}
      aria-label={accessibleName}
      aria-busy={isPending || undefined}
      aria-disabled={isPending || undefined}
      onClick={() => void copy()}
    >
      {copied ? <Check size={12} aria-hidden /> : <Copy size={12} aria-hidden />}
      <span aria-hidden="true">{copied ? COPIED_LABEL : "Copy code"}</span>
    </button>
    <span className="sr-only" role="status" data-one-time-code-status>{copied ? "Code copied" : ""}</span>
    {failed && <span className="one-time-code-error" role="alert" data-one-time-code-error><TriangleAlert size={12} aria-hidden />{feedback.text}</span>}
  </div>;
}
