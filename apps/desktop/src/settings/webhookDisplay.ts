import { formatPhoneNumber } from "@peppy/desktop-ui";
import type { McpDeviceView, WebhookTestResult, WebhookView } from "@peppy/desktop-ui/bridge-contracts";

export const MAX_WEBHOOKS = 10;
export const MAX_FILTER_ENTRIES = 50;
export const MAX_NAME_LENGTH = 80;
export const OWNER_REQUIRED_CODE = "owner_required";
export const INVALID_URL_MESSAGE = "Enter a valid URL.";
export const NAME_TOO_LONG_MESSAGE = `Use ${MAX_NAME_LENGTH} characters or fewer.`;
export const FILTER_EMPTY_MESSAGE = "Add at least one contact or number.";
export const FILTER_TOO_LARGE_MESSAGE = `Use ${MAX_FILTER_ENTRIES} contacts or numbers or fewer.`;
export const WEBHOOK_LIMIT_MESSAGE = `You can have up to ${MAX_WEBHOOKS} webhooks.`;

const GENERIC_ERROR_MESSAGE = "Something went wrong. Try again.";
const FILTER_VISIBLE_NAMES = 2;
const MS_PER_SECOND = 1_000;
const SECONDS_PER_MINUTE = 60;
const MINUTES_PER_HOUR = 60;
const HOURS_PER_DAY = 24;

export type StatusTone = "success" | "warning" | "error" | "idle";
export type StatusView = { tone: StatusTone; text: string };
export type FilterSummary = { lead: string; names: string };
export type DeliveryFields = Pick<WebhookView, "lastDeliveryState" | "lastDeliveryAt" | "lastStatusCode" | "lastError">;

const SERVER_ERROR_MESSAGES = new Map<string, string>([
  [OWNER_REQUIRED_CODE, "Only vault owners can manage webhooks."],
  ["webhook_url_invalid", INVALID_URL_MESSAGE],
  ["webhook_url_insecure", "Use an https:// URL."],
  ["webhook_url_blocked", "This address isn't allowed (private or local network)."],
  ["webhook_name_invalid", NAME_TOO_LONG_MESSAGE],
  ["webhook_filter_invalid", "Check the contacts and numbers in this filter."],
  ["webhook_limit", WEBHOOK_LIMIT_MESSAGE],
  ["webhook_not_found", "This webhook no longer exists."],
  ["mcp_device_required", "Set up the Peppy server client in Devices first."],
]);

const TEST_FAILURE_MESSAGES = new Map<string, string>([
  ["timeout", "The endpoint didn't answer in time."],
  ["redirect_refused", "Redirects aren't followed. Use the final endpoint URL."],
]);

export function webhookErrorMessage(code?: string): string {
  return (code && SERVER_ERROR_MESSAGES.get(code)) || GENERIC_ERROR_MESSAGE;
}

export function derivedWebhookName(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return "";
  }
}

export function webhookNamePlaceholder(url: string): string {
  return derivedWebhookName(url) || "Derived from the URL";
}

export function webhookFilterSummary(webhook: Pick<WebhookView, "filterMode" | "filter">): FilterSummary {
  if (webhook.filterMode === "all") return { lead: "All messages", names: "" };
  const labels = webhook.filter.map((entry) => entry.label ?? formatPhoneNumber(entry.address));
  const visible = labels.slice(0, FILTER_VISIBLE_NAMES).join(", ");
  const hidden = labels.length - FILTER_VISIBLE_NAMES;
  return {
    lead: webhook.filterMode === "only" ? "Only" : "Everyone except",
    names: hidden > 0 ? `${visible}, +${hidden}` : visible,
  };
}

/** Compact age such as "12 s", "3m", "2h" or "4d"; null when the timestamp cannot be read. */
export function shortAge(iso: string | null, now: number): string | null {
  if (!iso) return null;
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return null;
  const seconds = Math.max(0, Math.floor((now - then) / MS_PER_SECOND));
  if (seconds < SECONDS_PER_MINUTE) return `${seconds} s`;
  const minutes = Math.floor(seconds / SECONDS_PER_MINUTE);
  if (minutes < MINUTES_PER_HOUR) return `${minutes}m`;
  const hours = Math.floor(minutes / MINUTES_PER_HOUR);
  if (hours < HOURS_PER_DAY) return `${hours}h`;
  return `${Math.floor(hours / HOURS_PER_DAY)}d`;
}

export function serverClientStatus(device: McpDeviceView, now: number): StatusView {
  const age = shortAge(device.lastSyncedAt, now);
  if (device.state === "online") {
    return { tone: "success", text: age ? `Server client online · synced ${age} ago` : "Server client online" };
  }
  if (device.state === "offline") {
    return { tone: "warning", text: age ? `Server client offline · last synced ${age} ago` : "Server client offline" };
  }
  return { tone: "error", text: "Server client needs approval" };
}

const FAILURE_CAUSE_LABELS = new Map<string, string>([
  ["timeout", "timed out"],
  ["redirect_refused", "redirect refused"],
  ["webhook_url_blocked", "blocked"],
  ["blocked_address", "blocked"],
  ["queue_overflow", "queue full"],
  ["webhook_url_invalid", "unreachable"],
]);

/** Short human cause for a failing delivery: the HTTP status when there is one, else a plain label. */
export function deliveryCause(webhook: Pick<WebhookView, "lastStatusCode" | "lastError">): string {
  if (webhook.lastStatusCode) return String(webhook.lastStatusCode);
  return FAILURE_CAUSE_LABELS.get(webhook.lastError ?? "") ?? "error";
}

export function webhookDeliveryView(webhook: DeliveryFields, now: number): StatusView {
  switch (webhook.lastDeliveryState) {
    case "delivered": {
      const age = shortAge(webhook.lastDeliveryAt, now);
      const parts = [webhook.lastStatusCode ? String(webhook.lastStatusCode) : undefined, age].filter(Boolean);
      return { tone: "success", text: parts.join(" · ") || "Delivered" };
    }
    case "retrying":
      return { tone: "warning", text: `Retrying · ${deliveryCause(webhook)}` };
    case "failed":
      return { tone: "error", text: `Failing · ${deliveryCause(webhook)}` };
    default:
      return { tone: "idle", text: "—" };
  }
}

/** Screen-reader summary of the last delivery, used where the visible column is hidden. */
export function deliveryAnnouncement(webhook: DeliveryFields): string {
  switch (webhook.lastDeliveryState) {
    case "delivered":
      return `Last delivery: delivered${webhook.lastStatusCode ? `, ${webhook.lastStatusCode}` : ""}`;
    case "retrying":
      return `Last delivery: retrying, ${deliveryCause(webhook)}`;
    case "failed":
      return `Last delivery: failing, ${deliveryCause(webhook)}`;
    default:
      return "Last delivery: none yet";
  }
}

export type WebhookField = "name" | "url" | "filter";

/** Maps a server validation code to the editor field it describes; other codes stay at form level. */
export function webhookFieldForCode(code?: string): WebhookField | null {
  if (!code) return null;
  if (code.startsWith("webhook_url_")) return "url";
  if (code === "webhook_name_invalid") return "name";
  if (code === "webhook_filter_invalid") return "filter";
  return null;
}

function webhookTestFailureMessage(result: WebhookTestResult): string {
  if (result.statusCode !== null) return `The endpoint answered ${result.statusCode}.`;
  return TEST_FAILURE_MESSAGES.get(result.error ?? "") ?? "Test failed. Check the endpoint and try again.";
}

export function webhookTestResultView(result: WebhookTestResult): StatusView {
  if (!result.ok) return { tone: "error", text: webhookTestFailureMessage(result) };
  const status = result.statusCode === null ? "OK" : `${result.statusCode} OK`;
  return { tone: "success", text: `${status} · ${result.durationMs} ms` };
}
