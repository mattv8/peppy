import type { AccountNoticeDto, AccountNoticeKindDto, AccountNoticeSeverityDto, BillingProvider } from "@peppy/desktop-ui/bridge-contracts";
import { formatCopy, peppyCopy } from "../generated/peppyCopy";

export type SystemRibbonKind = "server_offline" | AccountNoticeKindDto;

/** Where the ribbon action leads: the website manage URL, or Apple's subscription management. */
export type SystemRibbonActionTarget = "web" | "appStore";

/** Display model for the one global ribbon; copy is already resolved for the current locale and time. */
export type SystemRibbonModel = {
  kind: SystemRibbonKind;
  severity: AccountNoticeSeverityDto;
  title: string;
  body: string;
  actionLabel: string | null;
  actionUrl: string | null;
  actionTarget: SystemRibbonActionTarget | null;
  dismissible: boolean;
};

/**
 * Billing facts for the current account, each resolved independently. A `null` field is unresolved
 * and turns off only the target that needs it.
 */
export type RibbonBillingFacts = {
  provider: BillingProvider | null;
  websitePurchase: boolean | null;
};

/** Everything that decides the ribbon action, resolved in one place so it cannot flicker. */
export type RibbonActionContext = {
  browserHost: boolean;
  billing: RibbonBillingFacts | null;
};

const LOOPBACK_HOSTS = new Set(["localhost", "127.0.0.1", "[::1]"]);

/** Billing-recovery kinds that Apple-billed users must manage in Apple's subscription settings. */
const APPLE_RECOVERY_KINDS: ReadonlySet<AccountNoticeKindDto> = new Set<AccountNoticeKindDto>([
  "payment_failed_grace",
  "ending_scheduled",
  "read_only_past_due",
  "read_only_canceled",
  "read_only_incomplete",
]);

const copyText = (key: string): string => (peppyCopy as Record<string, string>)[key] ?? "";

const serverOfflineRibbon: SystemRibbonModel = {
  kind: "server_offline",
  severity: "warning",
  title: copyText("server_offline_title"),
  body: copyText("server_offline_body"),
  actionLabel: null,
  actionUrl: null,
  actionTarget: null,
  dismissible: false,
};

/** Locale-aware medium date; the year is omitted when the deadline falls in the current year. */
export function formatAccountDeadline(deadlineUnixSeconds: number, now: number): string {
  const deadline = new Date(deadlineUnixSeconds * 1000);
  const sameYear = deadline.getFullYear() === new Date(now).getFullYear();
  const options: Intl.DateTimeFormatOptions = sameYear ? { month: "short", day: "numeric" } : { dateStyle: "medium" };
  return new Intl.DateTimeFormat(undefined, options).format(deadline);
}

export function formatDeletionDeadline(deadlineUnixSeconds: number): string {
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(new Date(deadlineUnixSeconds * 1000));
}

/**
 * The one action rule. Apple-billed recovery notices go to Apple. Otherwise the website manage URL is used
 * only when website purchasing is resolved as allowed; recovery notices also need a resolved provider, so
 * an unknown provider never sends a billing-recovery user to the web.
 */
export function resolveRibbonActionTarget(
  kind: AccountNoticeKindDto,
  hasManageUrl: boolean,
  context: RibbonActionContext,
): SystemRibbonActionTarget | null {
  if (!hasManageUrl) return null;
  if (context.browserHost) return "web";
  const billing = context.billing;
  if (billing?.provider === "apple" && APPLE_RECOVERY_KINDS.has(kind)) return "appStore";
  const webAllowed = billing?.websitePurchase === true && (billing.provider !== null || !APPLE_RECOVERY_KINDS.has(kind));
  return webAllowed ? "web" : null;
}

export function accountRibbonModel(notice: AccountNoticeDto, now: number, context: RibbonActionContext): SystemRibbonModel {
  const stem = `account_${notice.kind}`;
  const deadlineText = notice.deadline !== null && notice.deadline * 1000 > now
    ? formatAccountDeadline(notice.deadline, now)
    : null;
  const formatArguments = deadlineText === null ? [] : [deadlineText];
  const bodyKey = notice.kind === "read_only_account_deleting" && !context.browserHost
    ? "account_read_only_account_deleting_desktop_body"
    : notice.kind === "payment_failed_grace" && deadlineText === null ? `${stem}_body_nodate` : `${stem}_body`;
  const actionTarget = resolveRibbonActionTarget(notice.kind, notice.manageUrl !== null, context);
  return {
    kind: notice.kind,
    severity: notice.severity,
    dismissible: notice.dismissible,
    title: formatCopy(copyText(`${stem}_title`), ...formatArguments),
    body: formatCopy(copyText(bodyKey), ...formatArguments),
    actionLabel: actionTarget === null ? null : copyText(`${stem}_action`),
    actionUrl: actionTarget === "web" ? notice.manageUrl : null,
    actionTarget,
  };
}

/** A cached ending notice whose deadline has passed is stale (matches iOS): the ribbon stays hidden. */
function isEndedNotice(notice: AccountNoticeDto, now: number): boolean {
  return notice.kind === "ending_scheduled" && notice.deadline !== null && notice.deadline * 1000 <= now;
}

/** Priority: server offline, then the account notice. Only one ribbon is ever shown. */
export function resolveSystemRibbon(
  serverOffline: boolean,
  notice: AccountNoticeDto | null,
  now: number,
  context: RibbonActionContext,
): SystemRibbonModel | null {
  if (serverOffline) return serverOfflineRibbon;
  return notice && !isEndedNotice(notice, now) ? accountRibbonModel(notice, now, context) : null;
}

export function isOpenableManageUrl(value: string): boolean {
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
