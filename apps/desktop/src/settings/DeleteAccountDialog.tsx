import { Modal } from "@peppy/desktop-ui";
import type { BillingProvider } from "@peppy/desktop-ui/bridge-contracts";
import { type RefObject, useEffect, useRef, useState } from "react";
import { bridge, type BillingSummaryView } from "../bridge/bridge";
import { peppyCopy } from "../generated/peppyCopy";

export type AccountDeletionResult = { outcome: "deleted" | "pending"; provider: BillingProvider; executeAfter?: string | null; cancellable?: boolean };

const DELETION_STATUS_TIMEOUT_MS = 5000;

export function DeleteAccountDialog({ billing, opener, onDismiss, onDeleted }: { billing: BillingSummaryView; opener: RefObject<HTMLButtonElement | null>; onDismiss(): void; onDeleted(result: AccountDeletionResult): void }) {
  const [currentBilling, setCurrentBilling] = useState(billing);
  const [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const [failureMessage, setFailureMessage] = useState("");
  const [managementFailed, setManagementFailed] = useState(false);
  const submit = useRef<HTMLButtonElement>(null);
  useEffect(() => { submit.current?.focus(); }, [failed]);
  useEffect(() => { void bridge.billing_summary().then(setCurrentBilling).catch(() => undefined); }, []);
  const removeAccount = async () => {
    setBusy(true);
    setFailed(false);
    try {
      const outcome = await bridge.hosted_delete_account();
      if (outcome === "pending") {
        let timer: ReturnType<typeof setTimeout> | undefined;
        const status = await Promise.race([
          bridge.hosted_deletion_status().catch(() => null),
          new Promise<null>(resolve => { timer = setTimeout(() => resolve(null), DELETION_STATUS_TIMEOUT_MS); }),
        ]).finally(() => clearTimeout(timer));
        onDeleted({ outcome, provider: currentBilling.provider, executeAfter: status?.executeAfter ?? null, ...(status ? { cancellable: status.cancellable } : {}) });
      } else {
        onDeleted({ outcome, provider: currentBilling.provider });
      }
    } catch (error) {
      setFailureMessage(typeof error === "object" && error !== null && "code" in error && error.code === "hosted-session-required" && "message" in error && typeof error.message === "string" ? error.message : peppyCopy.account_delete_error);
      setFailed(true);
    } finally {
      setBusy(false);
    }
  };
  const openSubscriptionManagement = async () => {
    setManagementFailed(false);
    try {
      await bridge.open_store_subscriptions();
    } catch {
      setManagementFailed(true);
    }
  };
  const openGooglePlayManagement = async () => {
    setManagementFailed(false);
    try {
      await bridge.open_play_subscriptions();
    } catch {
      setManagementFailed(true);
    }
  };
  return <Modal id="account-delete-dialog" title={peppyCopy.settings_server_delete_title} opener={opener} busy={busy} onDismiss={onDismiss} actions={<><button id="account-delete-cancel" className="secondary-button" type="button" disabled={busy} onClick={onDismiss}>{peppyCopy.cancel}</button><button ref={submit} id="account-delete-submit" className="destructive-button" type="button" disabled={!confirmed || busy} aria-describedby="account-delete-warning" onClick={() => void removeAccount()}>{peppyCopy.settings_server_delete}</button></>}><div id="account-delete-body"><p id="account-delete-warning">{peppyCopy.account_delete_dialog_body}</p>{currentBilling.provider === "stripe" && <p id="account-delete-stripe-billing">{peppyCopy.account_delete_stripe_billing}</p>}{currentBilling.provider === "apple" && <div id="account-delete-apple-notice" data-provider="apple"><p>{peppyCopy.account_delete_apple_billing}</p><button id="account-delete-apple-manage" className="secondary-button" type="button" onClick={() => void openSubscriptionManagement()}>{peppyCopy.store_manage_cta}</button>{managementFailed && <p id="account-delete-management-error" role="alert">{peppyCopy.store_manage_failed}</p>}</div>}{currentBilling.provider === "google_play" && <div id="account-delete-google-play-notice" data-provider="google_play"><p>{peppyCopy.account_delete_google_play_billing}</p><button id="account-delete-google-play-manage" className="secondary-button" type="button" onClick={() => void openGooglePlayManagement()}>{peppyCopy.google_play_manage_cta}</button>{managementFailed && <p id="account-delete-management-error" role="alert">{peppyCopy.google_play_manage_failed}</p>}</div>}<label className="settings-check"><input id="account-delete-confirm" type="checkbox" checked={confirmed} disabled={busy} onChange={event => setConfirmed(event.target.checked)} /><span>{peppyCopy.account_delete_confirm_label}</span></label><p id="account-delete-working" role="status">{busy ? peppyCopy.account_delete_working : ""}</p><p id="account-delete-error" role="alert">{failed ? failureMessage : ""}</p></div></Modal>;
}
