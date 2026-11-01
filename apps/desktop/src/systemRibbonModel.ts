import type { AccountNoticeDto, AccountNoticeKindDto, AccountNoticeSeverityDto } from "@peppy/desktop-ui/bridge-contracts";
import { formatCopy, peppyCopy } from "./generated/peppyCopy";

export type SystemRibbonKind = "server_offline" | AccountNoticeKindDto;

/** Display model for the one global ribbon; copy is already resolved for the current locale and time. */
export type SystemRibbonModel = {
  kind: SystemRibbonKind;
  severity: AccountNoticeSeverityDto;
  title: string;
  body: string;
  actionLabel: string | null;
  actionUrl: string | null;
  dismissible: boolean;
};

const LOOPBACK_HOSTS = new Set(["localhost", "127.0.0.1", "[::1]"]);

const copyText = (key: string): string => (peppyCopy as Record<string, string>)[key] ?? "";

const serverOfflineRibbon: SystemRibbonModel = {
  kind: "server_offline",
  severity: "warning",
  title: copyText("server_offline_title"),
  body: copyText("server_offline_body"),
  actionLabel: null,
  actionUrl: null,
  dismissible: false,
};

/** Locale-aware medium date; the year is omitted when the deadline falls in the current year. */
export function formatAccountDeadline(deadlineUnixSeconds: number, now: number): string {
  const deadline = new Date(deadlineUnixSeconds * 1000);
  const sameYear = deadline.getFullYear() === new Date(now).getFullYear();
  const options: Intl.DateTimeFormatOptions = sameYear ? { month: "short", day: "numeric" } : { dateStyle: "medium" };
  return new Intl.DateTimeFormat(undefined, options).format(deadline);
}

export function accountRibbonModel(notice: AccountNoticeDto, now: number): SystemRibbonModel {
  const stem = `account_${notice.kind}`;
  const deadlineText = notice.deadline !== null && notice.deadline * 1000 > now
    ? formatAccountDeadline(notice.deadline, now)
    : null;
  const formatArguments = deadlineText === null ? [] : [deadlineText];
  const bodyKey = notice.kind === "payment_failed_grace" && deadlineText === null ? `${stem}_body_nodate` : `${stem}_body`;
  return {
    kind: notice.kind,
    severity: notice.severity,
    dismissible: notice.dismissible,
    title: formatCopy(copyText(`${stem}_title`), ...formatArguments),
    body: formatCopy(copyText(bodyKey), ...formatArguments),
    actionLabel: notice.manageUrl === null ? null : copyText(`${stem}_action`),
    actionUrl: notice.manageUrl,
  };
}

/** A cached ending notice whose deadline has passed is stale (matches iOS): the ribbon stays hidden. */
function isEndedNotice(notice: AccountNoticeDto, now: number): boolean {
  return notice.kind === "ending_scheduled" && notice.deadline !== null && notice.deadline * 1000 <= now;
}

/** Priority: server offline, then the account notice. Only one ribbon is ever shown. */
export function resolveSystemRibbon(serverOffline: boolean, notice: AccountNoticeDto | null, now: number): SystemRibbonModel | null {
  if (serverOffline) return serverOfflineRibbon;
  return notice && !isEndedNotice(notice, now) ? accountRibbonModel(notice, now) : null;
}

function isOpenableManageUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "https:" || (url.protocol === "http:" && LOOPBACK_HOSTS.has(url.hostname));
  } catch {
    return false;
  }
}

/**
 * Opens a manage URL in the system browser. The desktop goes through the Rust-validated
 * `open_external_url` bridge command, never the renderer opener. Returns false when nothing was opened.
 */
export async function openAccountManageUrl(
  url: string | null,
  browserHost: boolean,
  openExternalUrl: ((url: string) => Promise<void>) | undefined,
): Promise<boolean> {
  if (url === null || !isOpenableManageUrl(url)) return false;
  try {
    if (browserHost) {
      window.open(url, "_blank", "noopener,noreferrer");
      return true;
    }
    if (!openExternalUrl) return false;
    await openExternalUrl(url);
    return true;
  } catch {
    return false;
  }
}
