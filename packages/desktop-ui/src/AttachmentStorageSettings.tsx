import { useEffect, useRef, type ReactElement } from "react";
import { SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION, type AttachmentRetentionStatusResult } from "./bridge-contracts";

export type AttachmentStorageCopy = {
  attachment_storage_section_title: string;
  attachment_storage_allowance: string;
  attachment_storage_notice: string;
  attachment_storage_protected: string;
  attachment_storage_device_copies: string;
  attachment_storage_grace: string;
  attachment_storage_grace_paused: string;
  attachment_storage_grace_full: string;
  attachment_storage_readiness: string;
  attachment_storage_purchase: string;
  attachment_storage_activation_label: string;
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
  attachment_storage_notice_save_failed: string;
  attachment_storage_stale_usage: string;
  attachment_storage_usage: string;
  attachment_storage_limit: string;
  attachment_storage_base_included: string;
  attachment_storage_billing_pending: string;
  attachment_storage_billing_unavailable: string;
  attachment_storage_billing_open_failed: string;
  attachment_storage_privacy_open_failed: string;
  attachment_storage_device_update_required: string;
  attachment_storage_loading: string;
  privacy_policy: string;
};

export type AttachmentStorageSettingsProps = {
  result: AttachmentRetentionStatusResult | null;
  loading?: boolean;
  stale?: boolean;
  acknowledgementError?: boolean;
  acknowledgementRetryKey?: number;
  copy: AttachmentStorageCopy;
  onRefresh?(): void;
  onAcknowledge?(scopeId: string, policyVersion: string): Promise<boolean> | boolean | void;
  onManageStorage?(): void;
  manageStorageUrl?: string;
  onOpenPrivacy?(): Promise<void> | void;
  manageStorageError?: boolean;
  privacyError?: boolean;
  privacyUrl?: string;
};

function formatBytes(value: number): string {
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(value / 1_000_000_000) + " GB";
}

function formatDate(seconds: number): string {
  return new Intl.DateTimeFormat(undefined, { dateStyle: "long" }).format(new Date(seconds * 1_000));
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

function cleanupStateCopy(
  result: Extract<AttachmentRetentionStatusResult, { kind: "available" }>,
  copy: AttachmentStorageCopy,
): string | null {
  const { status } = result;
  if (status.notice.policyVersion !== SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION) return null;
  if (status.cleanupState === "disabled" || status.cleanupUnavailableReason === "cleanup_disabled") {
    return copy.attachment_storage_disabled;
  }
  if (status.cleanupUnavailableReason === "read_only") {
    return copy.attachment_storage_read_only;
  }
  if (
    status.usedBytes >= status.limitBytes
    && (status.notice.state === "grace_period" || status.cleanupUnavailableReason === "notice_not_eligible")
  ) {
    return copy.attachment_storage_grace_full;
  }
  if (status.cleanupState === "pending") return copy.attachment_storage_cleanup_pending;
  if (status.cleanupUnavailableReason === "device_not_ready") {
    return copy.attachment_storage_device_update_required;
  }
  if (status.cleanupUnavailableReason === "billing_mutation_pending") {
    return copy.attachment_storage_billing_pending;
  }
  if (status.cleanupUnavailableReason === "entitlement_unavailable" || status.cleanupUnavailableReason === "provider_reconciliation_pending") {
    return copy.attachment_storage_billing_unavailable;
  }
  if (status.notice.state === "grace_period" || status.cleanupUnavailableReason === "notice_not_eligible") {
    return copy.attachment_storage_grace_paused;
  }
  return null;
}

export function AttachmentStorageSettings({ result, loading, stale, acknowledgementError, acknowledgementRetryKey = 0, copy, onRefresh, onAcknowledge, onManageStorage, manageStorageUrl, onOpenPrivacy, manageStorageError, privacyError, privacyUrl = "https://peppy.pro/privacy" }: AttachmentStorageSettingsProps): ReactElement | null {
  const acknowledgedScopes = useRef(new Map<string, number>());
  const available = result?.kind === "available" ? result : null;
  const noticeSupported = available?.status.notice.policyVersion === SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION;
  const shouldRecordNotice = Boolean(available && noticeSupported && available.status.notice.state === "not_presented");
  const manageHref = safeHttpsUrl(manageStorageUrl);
  const manageAction = (onManageStorage || manageHref) && <div id="attachment-storage-actions">
    {manageHref ? <a id="attachment-storage-manage" className="secondary-button" href={manageHref} target="_blank" rel="noopener noreferrer">{copy.attachment_storage_manage_cta}<span className="visually-hidden"> (opens in a new tab)</span></a> : <button id="attachment-storage-manage" className="secondary-button" type="button" onClick={() => { void onManageStorage?.(); }}>{copy.attachment_storage_manage_cta}</button>}
    <p>{copy.attachment_storage_billing_handoff}</p>
    {manageStorageError && <p id="attachment-storage-manage-error" className="settings-error" role="alert">{copy.attachment_storage_billing_open_failed}</p>}
  </div>;

  useEffect(() => {
    if (!shouldRecordNotice || !available || !onAcknowledge || acknowledgedScopes.current.get(available.scopeId) === acknowledgementRetryKey) return;
    acknowledgedScopes.current.set(available.scopeId, acknowledgementRetryKey);
    void Promise.resolve(onAcknowledge(available.scopeId, SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION)).catch(() => undefined);
  }, [acknowledgementRetryKey, available, onAcknowledge, shouldRecordNotice]);

  if (result?.kind === "unsupported") return <section id="attachment-storage" data-settings-section="attachment-storage" className="settings-card">
    <h3>{copy.attachment_storage_section_title}</h3>
    {manageAction}
  </section>;
  const refresh = () => { if (!loading) onRefresh?.(); };
  if (loading && !available) return <section id="attachment-storage" data-settings-section="attachment-storage" className="settings-card" aria-busy="true">
    <h3>{copy.attachment_storage_section_title}</h3>
    <p id="attachment-storage-loading" role="status" className="settings-note">{copy.attachment_storage_loading}</p>
    {onRefresh && <button id="attachment-storage-refresh" className="secondary-button" type="button" onClick={refresh} aria-disabled="true">{copy.attachment_storage_reload_cta}</button>}
    {manageAction}
  </section>;
  if (!available) return <section id="attachment-storage" data-settings-section="attachment-storage" className="settings-card">
    <h3>{copy.attachment_storage_section_title}</h3>
    {loading && <p id="attachment-storage-loading" role="status" className="settings-note">{copy.attachment_storage_loading}</p>}
    <p role="status">{copy.attachment_storage_unavailable}</p>
    {onRefresh && <button id="attachment-storage-refresh" className="secondary-button" type="button" onClick={refresh} aria-disabled={loading || undefined}>{copy.attachment_storage_reload_cta}</button>}
    {manageAction}
  </section>;

  const { status } = available;
  const showGrace = noticeSupported && (status.notice.state === "not_presented" || status.notice.state === "grace_period");
  const privacyHref = onOpenPrivacy ? null : safeHttpsUrl(privacyUrl);
  const stateCopy = cleanupStateCopy(available, copy);
  const showStale = stale;
  return <section id="attachment-storage" data-settings-section="attachment-storage" className="settings-card" aria-busy={loading || undefined}>
    <h3>{copy.attachment_storage_section_title}</h3>
    {showStale && <p id="attachment-storage-stale-usage" className="settings-note">{copy.attachment_storage_stale_usage}</p>}
    <dl id="attachment-storage-usage">
      <div><dt>{copy.attachment_storage_usage}</dt><dd>{formatBytes(status.usedBytes)}</dd></div>
      <div><dt>{copy.attachment_storage_limit}</dt><dd>{formatBytes(status.limitBytes)}</dd></div>
      <div><dt>{copy.attachment_storage_base_included}</dt><dd>{formatBytes(status.baseBytes)}</dd></div>
    </dl>
    <p>{copy.attachment_storage_allowance}</p>
    {noticeSupported ? <div id="attachment-storage-notice" data-attachment-storage-notice>
      <p>{copy.attachment_storage_notice}</p>
      <p>{copy.attachment_storage_protected}</p>
      <p>{copy.attachment_storage_device_copies}</p>
      <p>{copy.attachment_storage_purchase}</p>
       {privacyHref && <a href={privacyHref} target="_blank" rel="noopener noreferrer">{copy.privacy_policy}</a>}
       {onOpenPrivacy && <button id="attachment-storage-privacy" className="secondary-button" type="button" onClick={() => void onOpenPrivacy()}>{copy.privacy_policy}</button>}
        {privacyError && <p id="attachment-storage-privacy-error" className="settings-error" role="alert">{copy.attachment_storage_privacy_open_failed}</p>}
    </div> : <p id="attachment-storage-notice-unavailable" role="status">{copy.attachment_storage_unavailable}</p>}
    {showGrace && <>
      <p>{copy.attachment_storage_grace}</p>
      <p>{copy.attachment_storage_readiness}</p>
      {status.notice.activationAtSeconds !== null && <p id="attachment-storage-activation" role="status" aria-label={`${copy.attachment_storage_activation_label} ${formatDate(status.notice.activationAtSeconds)}`}>{copy.attachment_storage_activation_label} {formatDate(status.notice.activationAtSeconds)}.</p>}
    </>}
    {stateCopy && <p id="attachment-storage-policy-state" role="status">{stateCopy}</p>}
    {acknowledgementError && <p id="attachment-storage-notice-error" role="alert" className="settings-error">{copy.attachment_storage_notice_save_failed}</p>}
    {onRefresh && <button id="attachment-storage-refresh" className="secondary-button" type="button" onClick={refresh} aria-disabled={loading || undefined}>{copy.attachment_storage_reload_cta}</button>}
    {manageAction}
  </section>;
}

export function attachmentStorageRemovalText(localAvailable: boolean, copy: Pick<AttachmentStorageCopy, "attachment_storage_removed_title" | "attachment_storage_removed" | "attachment_storage_removed_local_copy">): { title: string; body: string } {
  return localAvailable
    ? { title: "", body: copy.attachment_storage_removed_local_copy }
    : { title: copy.attachment_storage_removed_title, body: copy.attachment_storage_removed };
}
