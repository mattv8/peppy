import { useEffect, useRef, useState, type ReactElement } from "react";
import { Info, TriangleAlert } from "lucide-react";
import type { BillingSummaryView, PurchaseOptions, StoreProduct, AttachmentRetentionStatusResult } from "./bridge-contracts";
import { BillingElsewhereNotice, conflictVariant, type BillingElsewhereCopy } from "./BillingElsewhereNotice";
import { BusyIndicator } from "./BusyIndicator";
import { Modal } from "./Modal";

export type AttachmentStorageCopy = BillingElsewhereCopy & {
  attachment_storage_section_title: string;
  attachment_storage_allowance: string;
  attachment_storage_billing_handoff: string;
  attachment_storage_disabled: string;
  attachment_storage_read_only: string;
  attachment_storage_unavailable: string;
  attachment_storage_reload_cta: string;
  attachment_storage_manage_cta: string;
  attachment_storage_cleanup_pending: string;
  attachment_storage_removed_title: string;
  attachment_storage_removed: string;
  attachment_storage_removed_local_copy: string;
  attachment_storage_stale_usage: string;
  attachment_storage_usage: string;
  attachment_storage_limit: string;
  attachment_storage_base_included: string;
  attachment_storage_billing_pending: string;
  attachment_storage_billing_unavailable: string;
  attachment_storage_billing_open_failed: string;
  attachment_storage_device_update_required: string;
  attachment_storage_loading: string;
  attachment_storage_info_label: string;
  attachment_storage_info_title: string;
  attachment_storage_info_body_cleanup: string;
  attachment_storage_info_body_protected: string;
  attachment_storage_info_body_purchase: string;
  attachment_storage_info_dismiss: string;
  attachment_storage_near_full: string;
  attachment_storage_full_cleanup: string;
  attachment_storage_learn_more: string;
  attachment_storage_addon_title: string;
  attachment_storage_addon_body: string;
  attachment_storage_addon_none: string;
  attachment_storage_addon_current: string;
  attachment_storage_addon_choose: string;
  attachment_storage_addon_downgrade_note: string;
  attachment_storage_addon_remove_hint: string;
  attachment_storage_addon_working: string;
  attachment_storage_addon_success: string;
  attachment_storage_addon_unavailable: string;
  attachment_storage_addon_requires_plan: string;
  attachment_storage_addon_lapsed_warning: string;
  hosted_purchase_pending: string;
  store_purchase_pending_body: string;
  store_purchase_failed: string;
  store_purchase_rejected: string;
  store_period_month: string;
  store_period_year: string;
  store_manage_cta: string;
  store_manage_failed: string;
  store_unavailable: string;
  try_again: string;
};

export type StoragePurchaseState = "loading" | "ready" | "purchasing" | "pending" | "success" | "error" | "unavailable" | "requires-plan";
export type StoragePurchaseProps = {
  purchaseOptions: PurchaseOptions;
  billingSummary: BillingSummaryView;
  products: StoreProduct[];
  currentPacks: number;
  state: StoragePurchaseState;
  onPurchase(productId: string): Promise<"purchased" | "pending" | "cancelled">;
  onManageAppStore(): Promise<void> | void;
  onRetry?(): void;
  onPurchased?(): void;
  onManageWebsite?(): Promise<void> | void;
};

export type AttachmentStorageSettingsProps = {
  result: AttachmentRetentionStatusResult | null;
  loading?: boolean;
  stale?: boolean;
  copy: AttachmentStorageCopy;
  onRefresh?(): void;
  onManageStorage?(): void;
  manageStorageUrl?: string;
  manageStorageError?: boolean;
  purchase?: StoragePurchaseProps;
};

type AvailableStorage = Extract<AttachmentRetentionStatusResult, { kind: "available" }>;
type CleanupMode = "active" | "pending" | "disabled" | "read_only" | "device_not_ready" | "billing_pending" | "billing_unavailable" | "unknown";
type CapacityLevel = "normal" | "near-full" | "full" | "invalid";

function formatBytes(value: number): string {
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(value / 1_000_000_000) + " GB";
}

function safeHttpsUrl(value: string | undefined): string | null {
  if (!value) return null;
  try {
    const url = new URL(value);
    return url.protocol === "https:" ? url.toString() : null;
  } catch {
    return null;
  }
}

function capacityLevel(usedBytes: number, limitBytes: number): CapacityLevel {
  if (!Number.isSafeInteger(usedBytes) || !Number.isSafeInteger(limitBytes) || usedBytes < 0 || limitBytes <= 0) return "invalid";
  if (usedBytes >= limitBytes) return "full";
  return usedBytes * 100 >= limitBytes * 90 ? "near-full" : "normal";
}

function cleanupMode(status: AvailableStorage["status"]): CleanupMode {
  const reason = status.cleanupUnavailableReason;
  if (status.cleanupState === "available" && reason === null) return "active";
  if (status.cleanupState === "pending") return "pending";
  if (status.cleanupState === "disabled" || reason === "cleanup_disabled") return "disabled";
  if (reason === "read_only") return "read_only";
  if (reason === "device_not_ready") return "device_not_ready";
  if (reason === "billing_mutation_pending") return "billing_pending";
  if (reason === "entitlement_unavailable" || reason === "provider_reconciliation_pending") return "billing_unavailable";
  return "unknown";
}

function cleanupStateCopy(mode: CleanupMode, copy: AttachmentStorageCopy): string | null {
  if (mode === "pending") return copy.attachment_storage_cleanup_pending;
  if (mode === "disabled") return copy.attachment_storage_disabled;
  if (mode === "read_only") return copy.attachment_storage_read_only;
  if (mode === "device_not_ready") return copy.attachment_storage_device_update_required;
  if (mode === "billing_pending") return copy.attachment_storage_billing_pending;
  if (mode === "billing_unavailable") return copy.attachment_storage_billing_unavailable;
  return mode === "unknown" ? copy.attachment_storage_unavailable : null;
}

function storagePackCount(product: StoreProduct): number | null {
  const match = /\.([1-9]\d*)$/.exec(product.id);
  return match ? Number(match[1]) : null;
}

function storeErrorCode(error: unknown): string | undefined {
  return typeof error === "object" && error !== null && "code" in error && typeof error.code === "string" ? error.code : undefined;
}

function StorageAddOns({ purchase, copy, directManageAction }: { purchase: StoragePurchaseProps; copy: AttachmentStorageCopy; directManageAction: ReactElement | null | undefined }): ReactElement | null {
  const [status, setStatus] = useState<"idle" | "working" | "pending" | "success" | "error" | "rejected" | "unavailable">("idle");
  const [manageError, setManageError] = useState(false);
  const [conflict, setConflict] = useState(false);
  const clickedButton = useRef<HTMLButtonElement>(null);
  const { billingSummary, purchaseOptions } = purchase;
  const storageProducts = purchase.products.filter((product) => product.kind === "storage" && (product.period === "month" || product.period === "year")).map((product) => ({ product, packs: storagePackCount(product) })).filter((entry): entry is { product: StoreProduct; packs: number } => entry.packs !== null).sort((left, right) => left.packs - right.packs);
  const unavailable = purchase.state === "unavailable" || (purchase.state === "ready" && storageProducts.length === 0);
  const lapsedWarning = billingSummary.provider === "apple" && !billingSummary.planActive && billingSummary.storageRenewsWithoutPlan;
  const requiresPlan = purchase.state === "requires-plan" || (!billingSummary.planActive && !lapsedWarning);
  const returnFocus = () => requestAnimationFrame(() => clickedButton.current?.focus());
  const choose = async (productId: string, button: HTMLButtonElement) => {
    clickedButton.current = button;
    try {
      setStatus("working");
      const result = await purchase.onPurchase(productId);
      if (result === "purchased") {
        setStatus("success");
        purchase.onPurchased?.();
      } else if (result === "pending") setStatus("pending");
      else {
        setStatus("idle");
        returnFocus();
      }
    } catch (error) {
      const code = storeErrorCode(error);
      if (code === "store-conflict") setConflict(true);
      else setStatus(code === "store-rejected" ? "rejected" : code === "store-unavailable" ? "unavailable" : "error");
      returnFocus();
    }
  };
  const manage = async () => {
    try {
      setManageError(false);
      await purchase.onManageAppStore();
    } catch {
      setManageError(true);
    }
  };
  if (!purchaseOptions.appStore) return billingSummary.provider === "apple"
    ? <BillingElsewhereNotice variant="apple" provider="apple" copy={copy} onManageAppStore={purchase.onManageAppStore} headingLevel={4} />
    : directManageAction ?? null;
  if (billingSummary.provider === "stripe" || conflict) return <BillingElsewhereNotice variant={conflictVariant(billingSummary.provider, purchaseOptions.website)} provider={billingSummary.provider === "stripe" || billingSummary.provider === "apple" ? billingSummary.provider : undefined} copy={copy} onManageWebsite={purchase.onManageWebsite} headingLevel={4} />;
  if (lapsedWarning) return <div id="attachment-storage-addons" data-purchase-mode="iap" role="group" aria-labelledby="attachment-storage-addons-title"><h4 id="attachment-storage-addons-title">{copy.attachment_storage_addon_title}</h4><p id="attachment-storage-addon-lapsed-warning" data-tone="warning" role="status"><TriangleAlert size={16} aria-hidden />{copy.attachment_storage_addon_lapsed_warning}</p><button id="attachment-storage-addon-manage" className="secondary-button" type="button" onClick={() => { void manage(); }}>{copy.store_manage_cta}</button><p id="attachment-storage-addon-error" role="alert">{manageError ? copy.store_manage_failed : ""}</p></div>;
  if (requiresPlan) return <div id="attachment-storage-addons" data-purchase-mode="iap"><p id="attachment-storage-addon-requires-plan" role="status">{copy.attachment_storage_addon_requires_plan}</p></div>;
  return <div id="attachment-storage-addons" data-purchase-mode="iap" aria-labelledby="attachment-storage-addons-title">
    <h4 id="attachment-storage-addons-title">{copy.attachment_storage_addon_title}</h4>
    <p>{copy.attachment_storage_addon_body}</p>
    {purchase.state === "loading" ? <div id="attachment-storage-addon-skeleton" aria-busy="true"><BusyIndicator id="attachment-storage-addon-loading" label={copy.attachment_storage_loading} /></div> : unavailable ? <><p id="attachment-storage-addon-unavailable" role="alert">{copy.attachment_storage_addon_unavailable}</p>{purchase.onRetry && <button id="attachment-storage-addon-retry" className="secondary-button" type="button" onClick={purchase.onRetry}>{copy.try_again}</button>}</> : <ul id="attachment-storage-addon-options">
      <li id="attachment-storage-addon-0" data-packs="0"><span id="attachment-storage-addon-0-label">{copy.attachment_storage_addon_none}</span>{purchase.currentPacks === 0 && <span className="badge">{copy.attachment_storage_addon_current}</span>}</li>
      {storageProducts.map(({ product, packs }) => {
        const rowId = `attachment-storage-addon-${packs}`;
        const current = packs === purchase.currentPacks;
        const storageBytes = product.storageBytes;
        const hasStorageBytes = typeof storageBytes === "number" && storageBytes > 0;
        const description = hasStorageBytes ? `${rowId}-label ${rowId}-price` : `${rowId}-price`;
        return <li key={product.id} id={rowId} data-product-id={product.id} data-packs={packs}>{hasStorageBytes && <span id={`${rowId}-label`}>+{formatBytes(storageBytes!)}</span>}<span id={`${rowId}-price`}>{product.displayPrice} {product.period === "year" ? copy.store_period_year : copy.store_period_month}</span>{current ? <span className="badge">{copy.attachment_storage_addon_current}</span> : <button id={`${rowId}-choose`} className="secondary-button" type="button" disabled={status === "working"} aria-busy={status === "working" && clickedButton.current?.id === `${rowId}-choose` || undefined} aria-describedby={description} onClick={(event) => { void choose(product.id, event.currentTarget); }}>{copy.attachment_storage_addon_choose}</button>}</li>;
      })}
    </ul>}
    {purchase.currentPacks > 0 && <><p id="attachment-storage-addon-downgrade" className="settings-note">{copy.attachment_storage_addon_downgrade_note}</p><p id="attachment-storage-addon-remove" className="settings-note">{copy.attachment_storage_addon_remove_hint}</p></>}
    {(purchase.currentPacks > 0 || billingSummary.provider === "apple") && <button id="attachment-storage-addon-manage" className="secondary-button" type="button" onClick={() => { void manage(); }}>{copy.store_manage_cta}</button>}
    <p id="attachment-storage-addon-status" role="status">{status === "working" ? copy.attachment_storage_addon_working : status === "pending" ? `${copy.hosted_purchase_pending} ${copy.store_purchase_pending_body}` : status === "success" ? copy.attachment_storage_addon_success : ""}</p>
    <p id="attachment-storage-addon-error" role="alert">{status === "error" ? copy.store_purchase_failed : status === "rejected" ? copy.store_purchase_rejected : status === "unavailable" ? copy.store_unavailable : manageError ? copy.store_manage_failed : ""}</p>
  </div>;
}

export function AttachmentStorageSettings({ result, loading, stale, copy, onRefresh, onManageStorage, manageStorageUrl, manageStorageError, purchase }: AttachmentStorageSettingsProps): ReactElement | null {
  const [infoOpen, setInfoOpen] = useState(false);
  const infoButton = useRef<HTMLButtonElement>(null);
  const infoOpener = useRef<HTMLElement>(null);
  const available = result?.kind === "available" ? result : null;
  useEffect(() => {
    setInfoOpen(false);
  }, [available?.scopeId]);
  const manageHref = safeHttpsUrl(manageStorageUrl);
  const manageAction: ReactElement | null = onManageStorage || manageHref ? <div id="attachment-storage-actions">
    {manageHref ? <a id="attachment-storage-manage" className="secondary-button" href={manageHref} target="_blank" rel="noopener noreferrer">{copy.attachment_storage_manage_cta}<span className="visually-hidden"> (opens in a new tab)</span></a> : <button id="attachment-storage-manage" className="secondary-button" type="button" onClick={() => { onManageStorage?.(); }}>{copy.attachment_storage_manage_cta}</button>}
    <p>{copy.attachment_storage_billing_handoff}</p>
    {manageStorageError && <p id="attachment-storage-manage-error" className="settings-error" role="alert">{copy.attachment_storage_billing_open_failed}</p>}
  </div> : null;
  const openInfo = (opener: HTMLElement) => {
    infoOpener.current = opener;
    setInfoOpen(true);
  };
  const infoAction = available && <button id="attachment-storage-info-button" ref={infoButton} className="attachment-storage-info-button" type="button" aria-label={copy.attachment_storage_info_label} aria-haspopup="dialog" title={copy.attachment_storage_info_label} onClick={(event) => openInfo(event.currentTarget)}><Info size={16} aria-hidden="true" /></button>;
  const infoDialog = infoOpen && <Modal id="attachment-storage-info-dialog" title={copy.attachment_storage_info_title} opener={infoOpener} focusFallback={infoButton} onDismiss={() => setInfoOpen(false)} actions={<button id="attachment-storage-info-dismiss" className="secondary-button" type="button" onClick={() => setInfoOpen(false)}>{copy.attachment_storage_info_dismiss}</button>}>
    <div id="attachment-storage-info-body" data-attachment-storage-info>
      <p id="attachment-storage-info-body-cleanup">{copy.attachment_storage_info_body_cleanup}</p>
      <p id="attachment-storage-info-body-protected">{copy.attachment_storage_info_body_protected}</p>
      <p id="attachment-storage-info-body-purchase">{copy.attachment_storage_info_body_purchase}</p>
    </div>
  </Modal>;

  if (result?.kind === "unsupported") return <section id="attachment-storage" data-settings-section="attachment-storage" className="settings-card"><h3>{copy.attachment_storage_section_title}</h3>{manageAction}</section>;
  const refresh = () => { if (!loading) onRefresh?.(); };
  if (loading && !available) return <section id="attachment-storage" data-settings-section="attachment-storage" className="settings-card" aria-busy="true"><div id="attachment-storage-heading-row"><h3>{copy.attachment_storage_section_title}</h3></div><p id="attachment-storage-loading" role="status" className="settings-note">{copy.attachment_storage_loading}</p>{onRefresh && <button id="attachment-storage-refresh" className="secondary-button" type="button" onClick={refresh} aria-disabled="true">{copy.attachment_storage_reload_cta}</button>}{manageAction}</section>;
  if (!available) return <section id="attachment-storage" data-settings-section="attachment-storage" className="settings-card"><div id="attachment-storage-heading-row"><h3>{copy.attachment_storage_section_title}</h3></div>{loading && <p id="attachment-storage-loading" role="status" className="settings-note">{copy.attachment_storage_loading}</p>}<p role="status">{copy.attachment_storage_unavailable}</p>{onRefresh && <button id="attachment-storage-refresh" className="secondary-button" type="button" onClick={refresh} aria-disabled={loading || undefined}>{copy.attachment_storage_reload_cta}</button>}{manageAction}</section>;

  const { status } = available;
  const level = stale ? "invalid" : capacityLevel(status.usedBytes, status.limitBytes);
  const mode = cleanupMode(status);
  const cause = cleanupStateCopy(mode, copy);
  const showLead = (mode === "active" && (level === "near-full" || level === "full")) || (mode !== "active" && mode !== "pending" && level === "near-full");
  const showCause = mode !== "active";
  const leadCopy = level === "full" ? copy.attachment_storage_full_cleanup : copy.attachment_storage_near_full;
  const canLearnMore = showLead && (mode === "active" || level === "near-full");
  return <section id="attachment-storage" data-settings-section="attachment-storage" className="settings-card" aria-busy={loading || undefined}>
    <div id="attachment-storage-heading-row"><h3>{copy.attachment_storage_section_title}</h3>{infoAction}</div>
    {stale && <p id="attachment-storage-stale-usage" className="settings-note">{copy.attachment_storage_stale_usage}</p>}
    <dl id="attachment-storage-usage"><div><dt>{copy.attachment_storage_usage}</dt><dd>{formatBytes(status.usedBytes)}</dd></div><div><dt>{copy.attachment_storage_limit}</dt><dd>{formatBytes(status.limitBytes)}</dd></div><div><dt>{copy.attachment_storage_base_included}</dt><dd>{formatBytes(status.baseBytes)}</dd></div></dl>
    <p>{copy.attachment_storage_allowance}</p>
    <div id="attachment-storage-policy-state">
      {showLead && <div data-attachment-storage-capacity-row><p id="attachment-storage-capacity-notice" data-capacity={level} role="status">{leadCopy}</p>{canLearnMore && <button id="attachment-storage-learn-more" className="attachment-storage-learn-more" type="button" aria-describedby="attachment-storage-capacity-notice" onClick={(event) => openInfo(event.currentTarget)}>{copy.attachment_storage_learn_more}</button>}</div>}
      {showCause && cause && <p id="attachment-storage-cause" role="status">{cause}</p>}
    </div>
    {onRefresh && <button id="attachment-storage-refresh" className="secondary-button" type="button" onClick={refresh} aria-disabled={loading || undefined}>{copy.attachment_storage_reload_cta}</button>}
    {purchase ? <StorageAddOns purchase={purchase} copy={copy} directManageAction={manageAction} /> : manageAction}
    {infoDialog}
  </section>;
}

export function attachmentStorageRemovalText(localAvailable: boolean, copy: Pick<AttachmentStorageCopy, "attachment_storage_removed_title" | "attachment_storage_removed" | "attachment_storage_removed_local_copy">): { title: string; body: string } {
  return localAvailable ? { title: "", body: copy.attachment_storage_removed_local_copy } : { title: copy.attachment_storage_removed_title, body: copy.attachment_storage_removed };
}
