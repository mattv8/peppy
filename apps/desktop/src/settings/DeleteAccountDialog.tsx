import { Modal } from "@peppy/desktop-ui";
import { type RefObject, useEffect, useRef, useState } from "react";
import { bridge, type BillingSummaryView } from "../bridge/bridge";
import { peppyCopy } from "../generated/peppyCopy";

export type AccountDeletionResult = { outcome: "deleted" | "pending"; provider: "none" | "stripe" | "apple" };

export function DeleteAccountDialog({ billing, opener, onDismiss, onDeleted }: { billing: BillingSummaryView; opener: RefObject<HTMLButtonElement | null>; onDismiss(): void; onDeleted(result: AccountDeletionResult): void }) {
  const [currentBilling, setCurrentBilling] = useState(billing);
  const [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const submit = useRef<HTMLButtonElement>(null);
  useEffect(() => { submit.current?.focus(); }, [failed]);
  useEffect(() => { void bridge.billing_summary().then(setCurrentBilling).catch(() => undefined); }, []);
  const removeAccount = async () => {
    setBusy(true);
    setFailed(false);
    try {
      const outcome = await bridge.hosted_delete_account();
      onDeleted({ outcome, provider: currentBilling.provider });
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  };
  return <Modal id="account-delete-dialog" title={peppyCopy.settings_server_delete_title} opener={opener} busy={busy} onDismiss={onDismiss} actions={<><button id="account-delete-cancel" className="secondary-button" type="button" disabled={busy} onClick={onDismiss}>{peppyCopy.cancel}</button><button ref={submit} id="account-delete-submit" className="destructive-button" type="button" disabled={!confirmed || busy} aria-describedby="account-delete-warning" onClick={() => void removeAccount()}>{peppyCopy.settings_server_delete}</button></>}><div id="account-delete-body"><p id="account-delete-warning">{peppyCopy.account_delete_dialog_body}</p>{currentBilling.provider === "stripe" && <p id="account-delete-stripe-billing">{peppyCopy.account_delete_stripe_billing}</p>}{currentBilling.provider === "apple" && <div id="account-delete-apple-notice" data-provider="apple"><p>{peppyCopy.account_delete_apple_billing}</p><button id="account-delete-apple-manage" className="secondary-button" type="button" onClick={() => void bridge.open_store_subscriptions()}>{peppyCopy.store_manage_cta}</button></div>}<label className="settings-check"><input id="account-delete-confirm" type="checkbox" checked={confirmed} disabled={busy} onChange={event => setConfirmed(event.target.checked)} /><span>{peppyCopy.account_delete_confirm_label}</span></label><p id="account-delete-working" role="status">{busy ? peppyCopy.account_delete_working : ""}</p><p id="account-delete-error" role="alert">{failed ? peppyCopy.account_delete_error : ""}</p></div></Modal>;
}
