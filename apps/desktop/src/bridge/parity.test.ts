import { expect, it, vi } from "vitest";
import { BrowserBridge } from "../../../web/src/browser-bridge";
import { fixtureBridge, tauriBridge } from "./bridge";

// Native integrations not implemented by the fixture host.
const FIXTURE_MISSING_NATIVE = [
  "copy_attachment_image", "hydrate_media_preview", "launch_progress",
  "open_emoji_picker", "open_media_viewer", "subscribe_launch_progress",
];

// Native-only or currently unsupported browser integrations, including diagnostics and notifications.
const NATIVE_ONLY_INTEGRATIONS = [
  "ack_notification_reply_restore", "cancel_send", "claim_notification_reply_restore",
  "clear_diagnostics_log", "copy_attachment_image", "discard_notification_reply",
  "export_diagnostics_log", "get_diagnostics_state", "get_notification_quick_reply",
  "get_notification_sound", "hydrate_media_preview", "launch_progress",
  "notification_reply_recoveries", "open_emoji_picker", "open_external_url",
  "open_mcp_oauth_redirect", "open_media_viewer", "release_notification_reply_restore",
  "set_diagnostics_enabled", "set_notification_quick_reply", "set_notification_sound",
  "subscribe_drafts", "subscribe_launch_progress",
];

// Web enrollment and lifecycle APIs are outside the desktop bridge contract.
const WEB_ENROLLMENT_METHODS = [
  "dispose", "enrollment_approve_vault_request", "enrollment_create_invite",
  "enrollment_deny_vault_request", "enrollment_list_invites", "enrollment_list_vault_requests",
  "enrollment_list_vaults", "enrollment_management", "enrollment_pending_invite",
  "enrollment_revoke_device", "enrollment_revoke_invite", "enrollment_set_hosted_csrf",
  "lock_sync", "onReady", "onStopped", "setAccountUrl",
];

// BrowserBridge internals change during refactors; a failure here means update this list, not a parity break.
const BROWSER_RUNTIME_PRIVATE = [
  "call", "collectConfirmedAdminPassword", "createOwnerClaimDraft", "enrollmentSecretAction",
  "failPending", "finishOwnerClaim", "flushDraftSync", "flushDraftSyncWhenHidden",
  "isValidAdminPassword", "mediaOpen", "notificationContextArgs", "notificationPersistence",
  "notificationPersistenceSupported", "receive", "refreshNotificationContext",
  "refreshNotificationContextOnStorage", "releaseMediaUrl", "requiresNotificationInteraction",
  "revokeBlobUrls", "revokeMediaUrl", "showBanner", "showTyping", "stop", "workerError",
];

const diffKeys = (left: string[], right: string[]) => left.filter(key => !right.includes(key)).sort();

it("keeps the host bridge method sets in parity", () => {
  const port = { start: vi.fn(), close: vi.fn(), postMessage: vi.fn() } as unknown as MessagePort;
  const browser = new BrowserBridge(port);
  try {
    const browserMethods = [...new Set([...Object.getOwnPropertyNames(Object.getPrototypeOf(browser)), ...Object.keys(browser)])]
      .filter(name => name !== "constructor" && typeof (browser as unknown as Record<string, unknown>)[name] === "function");
    const native = Object.keys(tauriBridge).sort();
    const fixture = Object.keys(fixtureBridge).sort();
    expect({ nativeOnly: diffKeys(native, fixture), fixtureOnly: diffKeys(fixture, native) })
      .toEqual({ nativeOnly: FIXTURE_MISSING_NATIVE, fixtureOnly: [] });
    expect({ nativeOnly: diffKeys(native, browserMethods), browserOnly: diffKeys(browserMethods, native) }).toEqual({
      nativeOnly: NATIVE_ONLY_INTEGRATIONS,
      browserOnly: [...WEB_ENROLLMENT_METHODS, ...BROWSER_RUNTIME_PRIVATE].sort(),
    });
  } finally {
    browser.dispose();
  }
});
