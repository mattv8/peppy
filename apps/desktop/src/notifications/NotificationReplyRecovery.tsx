import { useEffect, useRef, useState } from "react";
import { Modal } from "@peppy/desktop-ui";
import type { NotificationReplyRecoveries } from "@peppy/desktop-ui/bridge-contracts";
import { peppyCopy } from "../generated/peppyCopy";
import "./notificationReplyRecovery.css";

const MAX_DRAFT_BYTES = 64 * 1024;
const utf8Bytes = (value: string) => new TextEncoder().encode(value).byteLength;
const combinedText = (draft: string, reply: string) => draft && reply ? `${draft}\n${reply}` : `${draft}${reply}`;
const reasonText = (reason: string) => reason === "needs-route-choice"
  ? "Choose a phone or SIM in your draft before sending."
  : reason === "members-changed" ? "The group changed. Review the conversation before sending."
    : "This reply could not be queued. Review it before adding it to your draft.";

type RecoveryBridge = {
  load(conversationId: string): Promise<NotificationReplyRecoveries>;
  claim(input: { id: string; restoreId: string; sessionGeneration: string }): Promise<boolean>;
  release(input: { id: string; restoreId: string; sessionGeneration: string }): Promise<void>;
  acknowledge(input: { id: string; restoreId: string; sessionGeneration: string }): Promise<void>;
  discard(input: { id: string; sessionGeneration: string }): Promise<void>;
};
export class DefinitivePreEditRefusal extends Error {
  constructor() {
    super("Your draft and this reply are too long to combine. Shorten your draft, then try again.");
  }
}
type Context = { key: string; conversationId?: string; unlocked: boolean; generation: number };

export function NotificationReplyRecovery({ bridge, conversationId, sessionKey, refreshSignal, unlocked, draftText, onAppend }: {
  bridge?: RecoveryBridge;
  conversationId?: string;
  sessionKey: string;
  refreshSignal?: unknown;
  unlocked: boolean;
  draftText: string;
  onAppend(reply: string): Promise<void>;
}) {
  const [recoveries, setRecoveries] = useState<NotificationReplyRecoveries>();
  const [open, setOpen] = useState(false);
  const [selectedId, setSelectedId] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [confirmDiscard, setConfirmDiscard] = useState(false);
  const [ackPending, setAckPending] = useState<{ id: string; restoreId: string; generation: string }>();
  const [error, setError] = useState("");
  const opener = useRef<HTMLButtonElement>(null);
  const inFlight = useRef(false);
  const contextRef = useRef<Context>({ key: sessionKey, conversationId, unlocked, generation: 0 });
  const context = contextRef.current;
  if (context.key !== sessionKey || context.conversationId !== conversationId || context.unlocked !== unlocked)
    contextRef.current = { key: sessionKey, conversationId, unlocked, generation: context.generation + 1 };
  const active = recoveries?.items.find(item => item.id === selectedId);
  const restorableItems = recoveries?.items.filter(item => item.state !== "uncertain") ?? [];
  const stateNotice = active?.state === "claimed" ? "Check your draft before discarding this reply" : active?.state === "uncertain" ? "Checking reply status. Do not resend yet." : "";
  const current = (request: Context) => contextRef.current.generation === request.generation && contextRef.current.unlocked && contextRef.current.conversationId === request.conversationId;
  const applyRecoveries = (value: NotificationReplyRecoveries, request: Context) => {
    if (!current(request)) return;
    setRecoveries(value);
  };
  const reload = () => {
    const request = contextRef.current;
    if (!bridge || !request.conversationId || !request.unlocked) return;
    void bridge.load(request.conversationId).then(value => applyRecoveries(value, request)).catch(() => {
      if (current(request)) setError(peppyCopy.composer_status_unsent_replies_error);
    });
  };
  useEffect(() => {
    inFlight.current = false;
    setBusy(false); setOpen(false); setSelectedId(undefined); setAckPending(undefined); setConfirmDiscard(false); setError("");
    setRecoveries(undefined); reload();
  // Context values are read from the render-updated ref by reload.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [bridge, conversationId, sessionKey, unlocked]);
  useEffect(() => { reload(); /* subscription-driven snapshot refresh */ // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshSignal]);
  useEffect(() => {
    if (open && selectedId) document.getElementById("notification-reply-recovery-heading")?.focus();
  }, [open, selectedId]);

  const close = () => { setOpen(false); setSelectedId(undefined); setConfirmDiscard(false); if (!ackPending) setError(""); };
  const remove = (id: string) => setRecoveries(value => {
    if (!value) return value;
    const next = { ...value, items: value.items.filter(item => item.id !== id) };
    return next;
  });
  const add = async () => {
    const value = recoveries;
    if (!bridge || !active || !value || active.state !== "ready" || inFlight.current) return;
    if (utf8Bytes(combinedText(draftText, active.body)) > MAX_DRAFT_BYTES) {
      setError("Your draft and this reply are too long to combine. Shorten your draft, then try again.");
      return;
    }
    const request = contextRef.current;
    const restoreId = crypto.randomUUID();
    inFlight.current = true; setBusy(true); setError("");
    try {
      const claimed = await bridge.claim({ id: active.id, restoreId, sessionGeneration: value.sessionGeneration });
      if (!current(request)) return;
      if (!claimed) { setRecoveries(currentValue => currentValue && { ...currentValue, items: currentValue.items.map(item => item.id === active.id ? { ...item, state: "claimed" } : item) }); setError("Check your draft before discarding this reply"); reload(); return; }
      setRecoveries(currentValue => currentValue && { ...currentValue, items: currentValue.items.map(item => item.id === active.id ? { ...item, state: "claimed" } : item) });
      await onAppend(active.body);
      if (!current(request)) return;
      const pending = { id: active.id, restoreId, generation: value.sessionGeneration };
      setAckPending(pending);
      try {
        await bridge.acknowledge({ id: pending.id, restoreId: pending.restoreId, sessionGeneration: pending.generation });
        if (!current(request)) return;
        remove(active.id); setAckPending(undefined); close();
      } catch {
        if (current(request)) setError("Reply added to draft. Could not finish removing it from this list.");
      }
    } catch (failure) {
      if (current(request)) {
        if (failure instanceof DefinitivePreEditRefusal) {
          try {
            await bridge.release({ id: active.id, restoreId, sessionGeneration: value.sessionGeneration });
            if (!current(request)) return;
            setRecoveries(currentValue => currentValue && { ...currentValue, items: currentValue.items.map(item => item.id === active.id ? { ...item, state: "ready" } : item) });
            setError(failure.message);
          } catch {
            setError("Check your draft before discarding this reply");
          }
        } else { setRecoveries(currentValue => currentValue && { ...currentValue, items: currentValue.items.map(item => item.id === active.id ? { ...item, state: "claimed" } : item) }); setError("Check your draft before discarding this reply"); reload(); }
      }
    } finally {
      if (current(request)) setBusy(false);
      inFlight.current = false;
    }
  };
  const retryAcknowledgement = async () => {
    const pending = ackPending;
    const request = contextRef.current;
    if (!bridge || !pending || pending.id !== active?.id || inFlight.current) return;
    inFlight.current = true; setBusy(true);
    try { await bridge.acknowledge({ id: pending.id, restoreId: pending.restoreId, sessionGeneration: pending.generation }); if (current(request)) { remove(pending.id); setAckPending(undefined); close(); } }
    catch { if (current(request)) setError("Reply added to draft. Could not finish removing it from this list."); }
    finally { if (current(request)) setBusy(false); inFlight.current = false; }
  };
  const discard = async () => {
    const value = recoveries; const request = contextRef.current;
    if (!bridge || !active || !value || inFlight.current) return;
    inFlight.current = true; setBusy(true);
    try { await bridge.discard({ id: active.id, sessionGeneration: value.sessionGeneration }); if (current(request)) { remove(active.id); close(); } }
    catch { if (current(request)) setError("Could not load this reply. Try again."); }
    finally { if (current(request)) setBusy(false); inFlight.current = false; }
  };
  if (!bridge || !conversationId || !unlocked || (!recoveries?.items.length && !error)) return null;
  const checking = restorableItems.length === 0;
  return <div id="notification-reply-recovery-status">
    {error && !recoveries && <><span id="notification-reply-recovery-error" className="notification-reply-recovery-error" role="alert">{error}</span><button className="notification-reply-recovery-action" type="button" onClick={reload}>{peppyCopy.composer_status_unsent_replies_retry}</button></>}
    {recoveries && <button id="notification-reply-recovery-open" ref={opener} className="notification-reply-recovery-action" data-checking={checking || undefined} type="button" aria-haspopup="dialog" title={peppyCopy.composer_status_unsent_replies_title} onClick={() => { setOpen(true); setSelectedId(ackPending?.id ?? (restorableItems.length === 1 ? restorableItems[0]!.id : undefined)); }}>{checking ? peppyCopy.composer_status_unsent_replies_checking : peppyCopy.composer_status_unsent_replies.replace("{count}", String(restorableItems.length))}</button>}
    {open && recoveries && <Modal id="notification-reply-recovery-dialog" className="notification-reply-recovery-modal" title="Unsent notification replies" onDismiss={() => { if (!busy) close(); }} opener={opener} focusFallback={{ current: document.getElementById("composer-textarea") }} busy={busy} actions={<button className="secondary-button" type="button" disabled={busy} onClick={close}>Close</button>}>
      {active ? <section id="notification-reply-recovery-panel" data-notification-recovery-id={active.id} aria-busy={busy} aria-labelledby="notification-reply-recovery-heading"><h3 id="notification-reply-recovery-heading" tabIndex={-1}>Review reply</h3><p>{reasonText(active.reason)}</p><p id="notification-reply-recovery-warning">Kept only while Peppy stays unlocked and running.</p><div id="notification-reply-recovery-text" role="region" aria-label="Reply text" tabIndex={0}>{active.body}</div><p id="notification-reply-recovery-consequence">Adds this reply to the end of your current draft. Nothing is sent.</p>{busy && <p role="status">{ackPending ? "Reply added to draft. Finishing…" : "Adding to draft…"}</p>}{(error || stateNotice) && <p role={error ? "alert" : "status"}>{error || stateNotice}</p>}<div id="notification-reply-recovery-actions">{recoveries.items.length > 1 && <button className="secondary-button" type="button" disabled={busy || ackPending?.id === active.id} onClick={() => { setSelectedId(undefined); window.setTimeout(() => document.getElementById(`notification-reply-recovery-review-${active.id}`)?.focus()); }}>Back to replies</button>}{active.state !== "uncertain" && (confirmDiscard ? <div id="notification-reply-recovery-discard-confirmation" role="region" aria-label="Discard reply confirmation"><span>Discard this reply? This cannot be undone.</span><button className="secondary-button" type="button" disabled={busy} onClick={() => setConfirmDiscard(false)}>Cancel</button><button className="secondary-button" type="button" disabled={busy} onClick={() => void discard()}>Discard reply</button></div> : <button className="secondary-button" type="button" disabled={busy || ackPending?.id === active.id} onClick={() => setConfirmDiscard(true)}>Discard</button>)}{!confirmDiscard && <button className="primary-button" type="button" aria-describedby="notification-reply-recovery-warning notification-reply-recovery-consequence" disabled={busy || (!ackPending && active.state !== "ready")} onClick={() => void (ackPending?.id === active.id ? retryAcknowledgement() : add())}>{ackPending?.id === active.id ? "Finish removing reply" : "Add to draft"}</button>}</div></section> : <><p id="notification-reply-recovery-warning">Kept only while Peppy stays unlocked and running.</p><ul id="notification-reply-recovery-list" aria-label="Unsent notification replies">{recoveries.items.map(item => <li key={item.id} data-notification-recovery-id={item.id}><span>{item.state === "uncertain" ? "Checking reply status. Do not resend yet." : reasonText(item.reason)}</span><button id={`notification-reply-recovery-review-${item.id}`} className="secondary-button" type="button" disabled={busy || (ackPending ? ackPending.id !== item.id : false)} onClick={() => { setSelectedId(item.id); window.setTimeout(() => document.getElementById("notification-reply-recovery-heading")?.focus()); }}>Review reply</button></li>)}</ul></>}
    </Modal>}
  </div>;
}
