import { describe, expect, it } from "vitest";
import type { McpDeviceView, WebhookTestResult } from "@peppy/desktop-ui/bridge-contracts";
import {
  deliveryAnnouncement,
  derivedWebhookName,
  serverClientStatus,
  shortAge,
  webhookDeliveryView,
  webhookErrorMessage,
  webhookFieldForCode,
  webhookFilterSummary,
  webhookNamePlaceholder,
  webhookTestResultView,
} from "./webhookDisplay";

const NOW = Date.parse("2026-11-07T12:00:00Z");
const ago = (milliseconds: number) => new Date(NOW - milliseconds).toISOString();
const SECOND = 1_000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

const entry = (address: string, label: string | null) => ({ address, label, contactId: null, bookId: null });

describe("derivedWebhookName", () => {
  it("uses the host without a www prefix", () => {
    expect(derivedWebhookName("https://www.hooks.example.com/path")).toBe("hooks.example.com");
  });

  it("returns an empty name for text that is not a URL", () => {
    expect(derivedWebhookName("not a url")).toBe("");
    expect(webhookNamePlaceholder("not a url")).toBe("Derived from the URL");
  });
});

describe("webhookFilterSummary", () => {
  it("describes an all-messages webhook without names", () => {
    expect(webhookFilterSummary({ filterMode: "all", filter: [] })).toEqual({ lead: "All messages", names: "" });
  });

  it("lists up to two labels for an only filter", () => {
    const summary = webhookFilterSummary({
      filterMode: "only",
      filter: [entry("+12025550101", "Alice Chen"), entry("+12025550102", "Bob Ruiz")],
    });
    expect(summary).toEqual({ lead: "Only", names: "Alice Chen, Bob Ruiz" });
  });

  it("counts entries beyond the first two", () => {
    const summary = webhookFilterSummary({
      filterMode: "except",
      filter: [entry("+12025550125", "Mom"), entry("+12025550126", "Dad"), entry("+12025550127", null)],
    });
    expect(summary).toEqual({ lead: "Everyone except", names: "Mom, Dad, +1" });
  });

  it("formats an unlabeled number for display", () => {
    expect(webhookFilterSummary({ filterMode: "only", filter: [entry("+12025550127", null)] }))
      .toEqual({ lead: "Only", names: "(202) 555-0127" });
  });
});

describe("shortAge", () => {
  it("returns null for missing or unreadable timestamps", () => {
    expect(shortAge(null, NOW)).toBeNull();
    expect(shortAge("yesterday-ish", NOW)).toBeNull();
  });

  it("uses seconds, minutes, hours and days", () => {
    expect(shortAge(ago(12 * SECOND), NOW)).toBe("12 s");
    expect(shortAge(ago(3 * MINUTE), NOW)).toBe("3m");
    expect(shortAge(ago(2 * HOUR), NOW)).toBe("2h");
    expect(shortAge(ago(4 * DAY), NOW)).toBe("4d");
  });

  it("never reports a negative age for clock skew", () => {
    expect(shortAge(new Date(NOW + 5 * SECOND).toISOString(), NOW)).toBe("0 s");
  });
});

describe("webhookDeliveryView", () => {
  const base = { lastDeliveryAt: null, lastStatusCode: null, lastError: null };

  it("shows the status code and age after a delivery", () => {
    expect(webhookDeliveryView({ ...base, lastDeliveryState: "delivered", lastStatusCode: 200, lastDeliveryAt: ago(3 * MINUTE) }, NOW))
      .toEqual({ tone: "success", text: "200 · 3m" });
  });

  it("shows only the status code when the time is unknown", () => {
    expect(webhookDeliveryView({ ...base, lastDeliveryState: "delivered", lastStatusCode: 204 }, NOW))
      .toEqual({ tone: "success", text: "204" });
  });

  it("names the retry cause with the status code when there is one", () => {
    expect(webhookDeliveryView({ ...base, lastDeliveryState: "retrying", lastStatusCode: 502 }, NOW))
      .toEqual({ tone: "warning", text: "Retrying · 502" });
  });

  it.each([
    ["timeout", "timed out"],
    ["redirect_refused", "redirect refused"],
    ["webhook_url_blocked", "blocked"],
    ["blocked_address", "blocked"],
    ["queue_overflow", "queue full"],
    ["webhook_url_invalid", "unreachable"],
    ["something_new", "error"],
  ])("labels the %s error as %s, for failing and retrying alike", (lastError, label) => {
    expect(webhookDeliveryView({ ...base, lastDeliveryState: "failed", lastError }, NOW))
      .toEqual({ tone: "error", text: `Failing · ${label}` });
    expect(webhookDeliveryView({ ...base, lastDeliveryState: "retrying", lastError }, NOW))
      .toEqual({ tone: "warning", text: `Retrying · ${label}` });
  });

  it("prefers the status code over a mapped error label", () => {
    expect(webhookDeliveryView({ ...base, lastDeliveryState: "failed", lastStatusCode: 503, lastError: "timeout" }, NOW).text)
      .toBe("Failing · 503");
  });

  it("falls back to a generic failure label when no cause is recorded", () => {
    expect(webhookDeliveryView({ ...base, lastDeliveryState: "failed" }, NOW))
      .toEqual({ tone: "error", text: "Failing · error" });
  });

  it("describes the last delivery for assistive tech", () => {
    expect(deliveryAnnouncement({ ...base, lastDeliveryState: "failed", lastStatusCode: 503 })).toBe("Last delivery: failing, 503");
    expect(deliveryAnnouncement({ ...base, lastDeliveryState: "delivered", lastStatusCode: 200 })).toBe("Last delivery: delivered, 200");
    expect(deliveryAnnouncement({ ...base, lastDeliveryState: "retrying", lastError: "timeout" })).toBe("Last delivery: retrying, timed out");
    expect(deliveryAnnouncement({ ...base, lastDeliveryState: "never" })).toBe("Last delivery: none yet");
  });

  it.each([
    ["webhook_url_insecure", "url"],
    ["webhook_url_blocked", "url"],
    ["webhook_url_invalid", "url"],
    ["webhook_name_invalid", "name"],
    ["webhook_filter_invalid", "filter"],
    ["webhook_limit", null],
    [undefined, null],
  ])("maps server code %s to field %s", (code, field) => {
    expect(webhookFieldForCode(code)).toBe(field);
  });

  it("shows a dash when nothing has been delivered", () => {
    expect(webhookDeliveryView({ ...base, lastDeliveryState: "never" }, NOW)).toEqual({ tone: "idle", text: "—" });
  });
});

describe("serverClientStatus", () => {
  const device = (state: McpDeviceView["state"], lastSyncedAt: string | null): McpDeviceView => ({
    deviceId: "mcp-device",
    name: "Peppy MCP",
    state,
    lastSyncedAt,
    cleanupPending: false,
  });

  it("reports an online client with its last sync age", () => {
    expect(serverClientStatus(device("online", ago(12 * SECOND)), NOW))
      .toEqual({ tone: "success", text: "Server client online · synced 12 s ago" });
  });

  it("omits the sync age when it is unknown", () => {
    expect(serverClientStatus(device("online", null), NOW)).toEqual({ tone: "success", text: "Server client online" });
  });

  it("reports an offline client with its last sync", () => {
    expect(serverClientStatus(device("offline", ago(5 * MINUTE)), NOW))
      .toEqual({ tone: "warning", text: "Server client offline · last synced 5m ago" });
  });

  it("flags a client that needs approval", () => {
    expect(serverClientStatus(device("needs_approval", null), NOW))
      .toEqual({ tone: "error", text: "Server client needs approval" });
  });
});

describe("webhookErrorMessage", () => {
  it.each([
    ["owner_required", "Only vault owners can manage webhooks."],
    ["webhook_url_insecure", "Use an https:// URL."],
    ["webhook_url_blocked", "This address isn't allowed (private or local network)."],
    ["webhook_limit", "You can have up to 10 webhooks."],
    ["webhooks_unavailable", "Webhooks are turned off on this server."],
  ])("maps %s to friendly copy", (code, message) => {
    expect(webhookErrorMessage(code)).toBe(message);
  });

  it("falls back to generic copy for unknown or missing codes", () => {
    expect(webhookErrorMessage(undefined)).toBe("Something went wrong. Try again.");
    expect(webhookErrorMessage("upstream_exploded")).toBe("Something went wrong. Try again.");
    expect(webhookErrorMessage("toString")).toBe("Something went wrong. Try again.");
  });
});

describe("webhookTestResultView", () => {
  const result = (overrides: Partial<WebhookTestResult>): WebhookTestResult => ({
    ok: true,
    statusCode: 200,
    durationMs: 142,
    error: null,
    ...overrides,
  });

  it("reports a successful test with status and duration", () => {
    expect(webhookTestResultView(result({}))).toEqual({ tone: "success", text: "200 OK · 142 ms" });
  });

  it("omits the status code when the server did not return one", () => {
    expect(webhookTestResultView(result({ statusCode: null, durationMs: 9 }))).toEqual({ tone: "success", text: "OK · 9 ms" });
  });

  it("explains a timeout", () => {
    expect(webhookTestResultView(result({ ok: false, statusCode: null, durationMs: 10_000, error: "timeout" })))
      .toEqual({ tone: "error", text: "The endpoint didn't answer in time." });
  });

  it("reports the endpoint's status when it answered with an error", () => {
    expect(webhookTestResultView(result({ ok: false, statusCode: 503, error: "http_503" })))
      .toEqual({ tone: "error", text: "The endpoint answered 503." });
  });

  it("refuses to follow redirects with a clear message", () => {
    expect(webhookTestResultView(result({ ok: false, statusCode: null, error: "redirect_refused" })).text)
      .toBe("Redirects aren't followed. Use the final endpoint URL.");
  });

  it("uses generic copy for an unknown failure", () => {
    expect(webhookTestResultView(result({ ok: false, statusCode: null, error: "weird" })).text)
      .toBe("Test failed. Check the endpoint and try again.");
  });
});
