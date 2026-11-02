import { useEffect, useRef, useState, type ReactElement } from "react";
import { Info } from "lucide-react";
import type { AttachmentRetentionStatusResult } from "./bridge-contracts";
import { Modal } from "./Modal";

export type AttachmentStorageCopy = {
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

export function AttachmentStorageSettings({ result, loading, stale, copy, onRefresh, onManageStorage, manageStorageUrl, manageStorageError }: AttachmentStorageSettingsProps): ReactElement | null {
  const [infoOpen, setInfoOpen] = useState(false);
  const infoButton = useRef<HTMLButtonElement>(null);
  const infoOpener = useRef<HTMLElement>(null);
  const available = result?.kind === "available" ? result : null;
  useEffect(() => {
    setInfoOpen(false);
  }, [available?.scopeId]);
  const manageHref = safeHttpsUrl(manageStorageUrl);
  const manageAction = (onManageStorage || manageHref) && <div id="attachment-storage-actions">
    {manageHref ? <a id="attachment-storage-manage" className="secondary-button" href={manageHref} target="_blank" rel="noopener noreferrer">{copy.attachment_storage_manage_cta}<span className="visually-hidden"> (opens in a new tab)</span></a> : <button id="attachment-storage-manage" className="secondary-button" type="button" onClick={() => { onManageStorage?.(); }}>{copy.attachment_storage_manage_cta}</button>}
    <p>{copy.attachment_storage_billing_handoff}</p>
    {manageStorageError && <p id="attachment-storage-manage-error" className="settings-error" role="alert">{copy.attachment_storage_billing_open_failed}</p>}
  </div>;
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
    {manageAction}
    {infoDialog}
  </section>;
}

export function attachmentStorageRemovalText(localAvailable: boolean, copy: Pick<AttachmentStorageCopy, "attachment_storage_removed_title" | "attachment_storage_removed" | "attachment_storage_removed_local_copy">): { title: string; body: string } {
  return localAvailable ? { title: "", body: copy.attachment_storage_removed_local_copy } : { title: copy.attachment_storage_removed_title, body: copy.attachment_storage_removed };
}
