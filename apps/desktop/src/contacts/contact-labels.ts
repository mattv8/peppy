import type { ContactBookView, ContactScanStatus } from "../bridge/bridge";

const EDIT_STATE_LABELS: Record<string, string> = {
  pending: "Waiting for the phone",
  "awaiting-approval": "Waiting for approval on the phone",
  "outcome-unknown": "Outcome unknown — check the phone",
  conflict: "Conflict — changed on the phone",
  rejected: "Rejected by the phone",
  failed: "Failed on the phone",
  expired: "Expired before the phone applied it",
};

export const editStateLabel = (state: string) => EDIT_STATE_LABELS[state] ?? state;

/** Names a book by device; adds "ID xxxx" only when another book shares the device name. */
export function bookLabels(books: ContactBookView[]) {
  const counts = new Map<string, number>();
  for (const item of books) counts.set(item.deviceName, (counts.get(item.deviceName) ?? 0) + 1);
  const suffix = (item: ContactBookView) => (counts.get(item.deviceName) ?? 0) > 1 ? `ID ${item.id.slice(-4)}` : "";
  const label = (item: ContactBookView) => [item.deviceName, suffix(item)].filter(Boolean).join(", ");
  return { suffix, label };
}

export const bookStateText = (book: ContactBookView) => book.sourceRevoked
  ? "Phone removed"
  : book.state === "active"
    ? "Active"
    : book.state === "limited"
      ? "Limited access"
      : book.state === "retired"
        ? "Retired"
        : "Unavailable";

const contactCountFormat = new Intl.NumberFormat();

export const bookContactCountText = (count: number) => `${contactCountFormat.format(count)} ${count === 1 ? "contact" : "contacts"}`;

/** First line of a book row: state and contact count, e.g. "Active, 430 contacts". */
export const bookSummaryText = (book: ContactBookView) => `${bookStateText(book)}, ${bookContactCountText(book.contactCount)}`;

export const contactErrorText = (error: unknown): string =>
  error instanceof Error
    ? error.message
    : typeof error === "object" && error !== null && "message" in error && typeof error.message === "string"
      ? error.message
      : String(error);

export const scanReasonText = (reason?: string) => ({ permission_denied: "The phone has not allowed contact access.", limited_access: "The phone has limited contact access.", sync_disabled: "Contact sync is disabled on the phone.", source_revoked: "The source device was removed.", source_retired: "This book is retired.", needs_unlock: "Unlock sync before requesting a scan.", stale_epoch: "The phone's contact context changed before the scan. Use Sync now to request a fresh scan.", superseded_generation: "A newer contact book version replaced this scan. Use Sync now to request a fresh scan.", incomplete_scan: "The phone's scan did not finish. Use Sync now to try again.", cancelled: "The phone cancelled this scan. Use Sync now to try again." }[reason ?? ""] ?? "The phone could not complete this scan.");

export const scanStatusText = (status: ContactScanStatus) => ({
  queued: "Scan request queued locally.", waiting: "Waiting for the phone to scan contacts.", scanning: "The phone is scanning contacts.", completed: "Scan completed.", blocked: `Scan blocked. ${scanReasonText(status.reason)}`, failed: `Scan failed. ${scanReasonText(status.reason)}`, expired: "Scan request expired. Request another scan.",
}[status.state]);
