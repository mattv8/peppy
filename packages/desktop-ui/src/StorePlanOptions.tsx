import { useRef, useState, type ReactElement } from "react";
import { BusyIndicator } from "./BusyIndicator";
import { BillingElsewhereNotice, conflictVariant, type BillingElsewhereCopy } from "./BillingElsewhereNotice";
import type { BillingProvider, PurchaseOptions, StoreProduct } from "./bridge-contracts";

export type StorePlanOptionsCopy = BillingElsewhereCopy & {
  hosted_subscribe_cta: string;
  hosted_subscribe_body: string;
  hosted_purchase_pending: string;
  hosted_purchase_verifying: string;
  production_pairing_checking: string;
  attachment_storage_allowance: string;
  try_again: string;
  privacy_policy: string;
  store_period_month: string;
  store_period_year: string;
  store_renewal_notice: string;
  store_terms_cta: string;
  store_restore_cta: string;
  store_restore_working: string;
  store_restore_done: string;
  store_restore_failed: string;
  store_purchase_waiting: string;
  store_purchase_pending_body: string;
  store_purchase_failed: string;
  store_purchase_rejected: string;
  store_unavailable: string;
};

export type StorePlanState = "loading" | "ready" | "unavailable";
export type StorePlanOptionsProps = {
  purchaseOptions: PurchaseOptions;
  products: StoreProduct[];
  state: StorePlanState;
  copy: StorePlanOptionsCopy;
  subscribeLabel?: string;
  /** Account billing provider for the store-conflict notice; unknown omits the provider-specific variant. */
  billingProvider?: BillingProvider;
  onPurchase(productId: string): Promise<"purchased" | "pending" | "cancelled">;
  onRestore(): Promise<void>;
  onRetry?(): void;
  onOpenTerms?(): void;
  onOpenPrivacy?(): void;
  onPurchased?(): void;
  onManageWebsite?(): Promise<void> | void;
};

type PurchaseStatus = "idle" | "waiting" | "verifying" | "pending" | "error" | "rejected" | "unavailable";

function storeErrorCode(error: unknown): string | undefined {
  return typeof error === "object" && error !== null && "code" in error && typeof error.code === "string" ? error.code : undefined;
}

function periodCopy(period: StoreProduct["period"], copy: StorePlanOptionsCopy): string {
  return period === "year" ? copy.store_period_year : copy.store_period_month;
}

export function StorePlanOptions({ purchaseOptions, products, state, copy, subscribeLabel, billingProvider, onPurchase, onRestore, onRetry, onOpenTerms, onOpenPrivacy, onPurchased, onManageWebsite }: StorePlanOptionsProps): ReactElement | null {
  const [purchaseStatus, setPurchaseStatus] = useState<PurchaseStatus>("idle");
  const [restoreStatus, setRestoreStatus] = useState<"idle" | "working" | "done" | "error">("idle");
  const [conflict, setConflict] = useState(false);
  const clickedButton = useRef<HTMLButtonElement>(null);
  const plans = products.filter((product) => product.kind === "plan" && (product.period === "month" || product.period === "year")).sort((left, right) => left.period === "month" ? -1 : right.period === "month" ? 1 : 0);
  const busy = purchaseStatus === "waiting" || purchaseStatus === "verifying" || restoreStatus === "working";
  if (!purchaseOptions.appStore) return null;

  const returnFocus = () => requestAnimationFrame(() => clickedButton.current?.focus());
  const purchase = async (productId: string, button: HTMLButtonElement) => {
    clickedButton.current = button;
    try {
      setPurchaseStatus("waiting");
      const result = await onPurchase(productId);
      if (result === "purchased") {
        setPurchaseStatus("verifying");
        onPurchased?.();
      }
      else if (result === "pending") setPurchaseStatus("pending");
      else {
        setPurchaseStatus("idle");
        returnFocus();
      }
    } catch (error) {
      const code = storeErrorCode(error);
      if (code === "store-conflict") setConflict(true);
      else setPurchaseStatus(code === "store-rejected" ? "rejected" : code === "store-unavailable" ? "unavailable" : "error");
      returnFocus();
    }
  };
  const restore = async () => {
    try {
      setRestoreStatus("working");
      await onRestore();
      setRestoreStatus("done");
    } catch {
      setRestoreStatus("error");
    }
  };
  const purchaseMessage = purchaseStatus === "waiting" ? copy.store_purchase_waiting
    : purchaseStatus === "verifying" ? copy.hosted_purchase_verifying
      : purchaseStatus === "pending" ? `${copy.hosted_purchase_pending} ${copy.store_purchase_pending_body}` : "";
  const purchaseError = purchaseStatus === "rejected" ? copy.store_purchase_rejected : purchaseStatus === "unavailable" ? copy.store_unavailable : purchaseStatus === "error" ? copy.store_purchase_failed : "";
  const unavailable = state === "unavailable" || (state === "ready" && plans.length === 0);

  return <section id="hosted-store-plan-options" data-purchase-mode={purchaseOptions.website ? "both" : "iap"}>
    <p id="hosted-subscribe-body">{copy.hosted_subscribe_body}</p>
    {conflict ? <BillingElsewhereNotice variant={conflictVariant(billingProvider, purchaseOptions.website)} provider={billingProvider === "stripe" || billingProvider === "apple" ? billingProvider : undefined} copy={copy} onManageWebsite={onManageWebsite} /> : unavailable ? <div id="hosted-store-unavailable"><p role="alert">{copy.store_unavailable}</p>{onRetry && <button id="hosted-store-retry" className="secondary-button" type="button" onClick={onRetry}>{copy.try_again}</button>}</div> : <div id="hosted-store-plans" aria-busy={state === "loading" || undefined}>
      {state === "loading" ? <BusyIndicator id="hosted-store-loading" label={copy.production_pairing_checking} /> :
      <ul id="hosted-plan-options">
        {plans.map((product) => {
          const planId = `hosted-plan-${encodeURIComponent(product.id)}`;
          return <li key={product.id} id={planId} data-product-id={product.id} data-period={product.period}>
            <h3 id={`${planId}-name`}>{product.displayName}</h3>
            <p id={`${planId}-price`}><span>{product.displayPrice}</span> <span>{periodCopy(product.period, copy)}</span></p>
            <p>{copy.attachment_storage_allowance}</p>
            <button id={`${planId}-subscribe`} className="primary-button" type="button" disabled={busy || state !== "ready"} aria-busy={busy && clickedButton.current?.id === `${planId}-subscribe` || undefined} aria-describedby={`${planId}-name ${planId}-price`} onClick={(event) => { void purchase(product.id, event.currentTarget); }}>{subscribeLabel ?? copy.hosted_subscribe_cta}</button>
          </li>;
        })}
      </ul>}
    </div>}
    <p id="hosted-purchase-status" role="status">{purchaseMessage}</p>
    <p id="hosted-purchase-error" role="alert">{purchaseError}</p>
    <p id="hosted-paywall-renewal" className="settings-note">{copy.store_renewal_notice}</p>
    <ul id="hosted-paywall-links">
      <li><button id="hosted-paywall-terms-button" className="link-button" type="button" onClick={onOpenTerms}>{copy.store_terms_cta}<span className="visually-hidden"> (opens in your browser)</span></button></li>
      <li><button id="hosted-paywall-privacy-button" className="link-button" type="button" onClick={onOpenPrivacy}>{copy.privacy_policy}<span className="visually-hidden"> (opens in your browser)</span></button></li>
    </ul>
    <div id="hosted-store-actions" className="enrollment-actions">
      <button id="hosted-restore-button" className="secondary-button" type="button" disabled={busy} aria-busy={restoreStatus === "working" || undefined} onClick={() => { void restore(); }}>{copy.store_restore_cta}</button>
      <p id="hosted-restore-status" role="status">{restoreStatus === "working" ? copy.store_restore_working : restoreStatus === "done" ? copy.store_restore_done : ""}</p>
      <p id="hosted-restore-error" role="alert">{restoreStatus === "error" ? copy.store_restore_failed : ""}</p>
    </div>
  </section>;
}
