import type {
  GroupView,
  MessageView,
  ConversationView,
  Draft,
  DesktopSnapshot,
  DraftInput,
  OneTimeCodeTarget,
  NotificationPersistence,
  ContactBookView,
  ContactSort,
  ContactView,
  ContactNameCandidate,
  BridgeError,
  HostedAccountView,
  HostedPreviewView,
  ManagedDeviceView,
  ContactScanStatus,
  LocalSearchHit,
  LocalSearchRequest,
  LocalSearchPage,
  SearchIndexProgress,
  HistorySearchSnapshot,
  MessageWindowRequest,
  MessageWindow,
  WebhookCreated,
  WebhookInput,
  WebhookList,
  WebhookTestResult,
  WebhookView,
  DiagnosticsExportResult,
  DiagnosticsState,
} from "@peppy/desktop-ui/bridge-contracts";
import type { DesktopBridge } from "./bridge";
import { normalizePhoneNumber } from "@peppy/desktop-ui";
import { fixtureRcsMediaMessages } from "./rcsMediaFixtures";

type FixtureConversationInput = Omit<ConversationView, "recipientlessDraft" | "draftOnly" | "pinned" | "folder" | "canUnblock"> & Partial<Pick<ConversationView, "recipientlessDraft" | "draftOnly" | "pinned" | "folder" | "canUnblock">>;

const fixtureConversation = (conversation: FixtureConversationInput): ConversationView => ({
  recipientlessDraft: false,
  draftOnly: false,
  pinned: false,
  folder: "inbox",
  canUnblock: false,
  ...conversation,
});

const fixtureMessages: MessageView[] = [
  { id: "message-aurora-1", revision: "1", sender: "other", body: "Can you send over the estimate?", timestamp: "1788434460000", attachments: [] },
  { id: "message-aurora-2", revision: "2", sender: "self", body: "I'll have it to you shortly.\nIt was received by the phone and can still be cancelled.", timestamp: "1788434580000", status: "gateway-persisted", statusAt: "1788434640000", subject: "Fixture subject", attachments: [{ id: "attachment-aurora-1", name: "fixture-photo.png", mediaType: "image/png", byteSize: 67, state: "ready", previewUrl: "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==" }], sendCancellation: { state: "none", canCancel: true, requiresGatewayUpdate: false } },
  { id: "message-aurora-3", revision: "1", sender: "other", body: "Your verification code is 482913.", oneTimeCode: "482913", timestamp: "1788434700000", attachments: [] },
];
const fixtureReactionMessages: MessageView[] = [
  { id: "message-sam-1", revision: "1", sender: "self", body: "Did they discuss the NICU at all in the group conversation that I had muted?? but yes, people suck. “Jerk chicken” are you trying to send a message? 😘", timestamp: "1788434700000", attachments: [] },
  { id: "message-sam-2", revision: "1", sender: "other", senderAddress: "+15555550142", body: "lol it autocorrected", timestamp: "1788434760000", attachments: [] },
  { id: "message-sam-3", revision: "1", sender: "other", senderAddress: "+15555550142", body: "Reacted 😆 to “Did they discuss the NICU at all in the group conversation that I had muted?? but yes, people suck. “Jerk chicken” are you trying to send a message? 😘”", timestamp: "1788434820000", attachments: [] },
  { id: "message-sam-4", revision: "1", sender: "self", body: "Loved “lol it autocorrected”", timestamp: "1788434880000", attachments: [] },
  { id: "message-sam-5", revision: "1", sender: "other", senderAddress: "+15555550142", body: "Reacted 👍 to “a message from last year that is not loaded…”", timestamp: "1788434940000", attachments: [] },
];
/** The text route body, as spec 4.3: NFC, whitespace collapsed, quote cut to 79 graphemes plus an ellipsis past 80. */
function composeFixtureReactionText(body: string, emoji: string): string {
  const graphemes = Array.from(body.normalize("NFC").trim().replace(/\s+/g, " "));
  const quote = graphemes.length > 80 ? `${graphemes.slice(0, 79).join("").trimEnd()}…` : graphemes.join("");
  return `Reacted ${emoji} to “${quote}”`;
}

/** Reaction states from spec 4.8 and section 9, one conversation per route situation. */
export const fixtureReactionConversations: ConversationView[] = ([
  { id: "conv-reaction-states", name: "Reaction states", preview: "Are we still on for 5 tonight?", unread: 0, reactionSupport: "native", reactionGatewayId: "gateway-pixel8", participants: ["Sam Rivera", "Alex Chen", "Jo Lee", "Maya Ortiz", "Priya Shah"], messages: [
    { id: "reaction-group-dinner", revision: "1", sender: "other", senderAddress: "+15555550142", body: "Dinner at 7 at the usual place?", timestamp: "1788434100000", transport: "rcs", attachments: [], reactable: "native", reactions: [
      { emoji: "👍", actors: [{ self: false, address: "+15555550101" }, { self: false, address: "+15555550102" }, { self: false, address: "+15555550103" }] },
      { emoji: "❤️", actors: [{ self: false, address: "+15555550104" }, { self: false, address: "+15555550105" }] },
      { emoji: "😮", actors: [{ self: false, address: "+15555550106" }] },
    ] },
    { id: "reaction-own-sent", revision: "1", sender: "self", body: "Yes, see you there!", timestamp: "1788434160000", transport: "rcs", status: "delivery-confirmed", attachments: [], reactable: "native", reactions: [{ emoji: "😂", actors: [{ self: true }] }] },
    { id: "reaction-pending", revision: "1", sender: "other", senderAddress: "+15555550142", body: "Are we still on for 5 tonight?", timestamp: "1788434220000", transport: "rcs", attachments: [], reactable: "native", ownReactionRequest: { requestId: "fixture-request-sending", emoji: "😮", state: "sending" } },
    { id: "reaction-removing", revision: "1", sender: "other", senderAddress: "+15555550142", body: "Running late, start without me", timestamp: "1788434280000", transport: "rcs", attachments: [], reactable: "native", reactions: [{ emoji: "🙏", actors: [{ self: true }] }], ownReactionRequest: { requestId: "fixture-request-removing", emoji: null, previousEmoji: "🙏", state: "sending" } },
    { id: "reaction-not-sent", revision: "1", sender: "other", senderAddress: "+15555550142", body: "Sounds good", timestamp: "1788434340000", transport: "rcs", attachments: [], reactable: "native", ownReactionRequest: { requestId: "fixture-request-not-sent", emoji: "🙏", state: "not_sent", refusal: "rejected" } },
    { id: "reaction-unknown", revision: "1", sender: "other", senderAddress: "+15555550142", body: "Did you get the parcel?", timestamp: "1788434400000", transport: "rcs", attachments: [], reactable: "native", ownReactionRequest: { requestId: "fixture-request-unknown", emoji: "❤️", previousEmoji: "👍", state: "unknown" } },
    // A photo-only RCS message with another person's reaction: its badge overlays the photo (media overlay placement).
    { id: "reaction-photo", revision: "1", sender: "other", senderAddress: "+15555550180", body: "", timestamp: "1788434900000", transport: "rcs", attachments: [{ id: "attachment-reaction-photo", name: "harbour.gif", mediaType: "image/gif", byteSize: 67, state: "ready", previewUrl: "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==" }], reactable: "native", reactions: [{ emoji: "😍", actors: [{ self: false, address: "+15555550181" }] }] },
    { id: "reaction-photo-note", revision: "1", sender: "self", body: "Nice shots", timestamp: "1788434950000", transport: "rcs", status: "delivery-confirmed", attachments: [] },
    // Two photos in one message, reacted: the badge must render once for the message, not once per photo.
    { id: "reaction-photo-pair", revision: "1", sender: "other", senderAddress: "+15555550180", body: "", timestamp: "1788434960000", transport: "rcs", attachments: [
      { id: "attachment-reaction-pair-1", name: "pier.gif", mediaType: "image/gif", byteSize: 67, state: "ready", previewUrl: "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==" },
      { id: "attachment-reaction-pair-2", name: "dune.gif", mediaType: "image/gif", byteSize: 67, state: "ready", previewUrl: "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==" },
    ], reactable: "native", reactions: [{ emoji: "🙌", actors: [{ self: false, address: "+15555550182" }] }] },
  ] },
  { id: "conv-reaction-text", name: "Dana Ortiz", preview: "Sent it", unread: 0, reactionSupport: "text", reactionGatewayId: "gateway-pixel8", messages: [
    { id: "reaction-text-incoming", revision: "1", sender: "other", senderAddress: "+15555550150", body: "Running late, start without me", timestamp: "1788434500000", transport: "sms", attachments: [], reactable: "text" },
    { id: "reaction-text-self", revision: "1", sender: "self", body: "Sent it", timestamp: "1788434560000", transport: "sms", status: "sent", attachments: [], reactable: "text" },
    { id: "reaction-text-failed", revision: "1", sender: "self", body: "Reacted 👍 to “Running late, start without me”", timestamp: "1788434620000", transport: "sms", status: "failed-before-submit", attachments: [] },
  ] },
  { id: "conv-needs-update", name: "Priya Shah", preview: "Can we move the review to Friday?", unread: 0, reactionSupport: "needs_phone_update", reactionGatewayId: "gateway-pixel8", messages: [
    { id: "reaction-needs-update", revision: "1", sender: "other", senderAddress: "+15555550160", body: "Can we move the review to Friday?", timestamp: "1788434700000", transport: "rcs", attachments: [] },
  ] },
  { id: "conv-phone-offline", name: "Lee Park", preview: "Are you coming tonight?", unread: 0, reactionSupport: "native", reactionGatewayId: "gateway-offline", messages: [
    { id: "reaction-offline", revision: "1", sender: "other", senderAddress: "+15555550170", body: "Are you coming tonight?", timestamp: "1788434800000", transport: "rcs", attachments: [], reactable: "native" },
  ] },
] satisfies FixtureConversationInput[]).map(fixtureConversation);
const fixtureChatNotificationIcon = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAaUlEQVR42mPQrPz/fyAxw6gDkDmXXiAwLg3UUoMzBOjtCAZaWkCMGgZaW0BIDd5ESA9HEMwFtHYEUdmQ2o7Amw3p4QiC5QC9yoDRgmi0IBotiEYLotGCaLQgGi2IRguiwVcQjfYNBwIDAAwPd9Z4B+yXAAAAAElFTkSuQmCC";
const fixtureMailNotificationIcon = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAaUlEQVR42mP4nx3+fyAxw6gDUAQe3kJgXJqopQZnCNDZEQw0tYAINQw09yUBNfgTIR0cQTgX0NgRxGVDajsCbzakhyMIlgN0KgNGC6LRgmi0IBotiEYLotGCaLQgGi2IBmFBNNo3HAAMAKbz3HgryQcuAAAAAElFTkSuQmCC";
/** Tests only: adds a message (for example one in a send-recovery state) to a fixture conversation. Not used by the app. */
export function addFixtureMessageForTests(conversationId: string, message: MessageView): void {
  fixtureSnapshot.conversations.find((conversation) => conversation.id === conversationId)?.messages.push(message);
}
/**
 * Tests and the browser fixture: merges `from` into `to` as the core does for a merged-away conversation.
 * The survivor gains the source's messages, `from` leaves the list, and later requests for `from` return
 * `requestedRedirect` with `to` as the active conversation. The returned function undoes the merge.
 */
export function mergeFixtureConversation(from: string, to: string): () => void {
  const conversations = fixtureSnapshot.conversations;
  const source = conversations.find((conversation) => conversation.id === from);
  const survivor = conversations.find((conversation) => conversation.id === to);
  if (!source || !survivor) throw new Error(`Fixture merge needs both conversations: ${from} -> ${to}`);
  const { messages: survivorMessages, unread: survivorUnread } = survivor;
  survivor.messages = [...source.messages, ...survivorMessages].sort((left, right) => Number(left.timestamp) - Number(right.timestamp));
  survivor.unread += source.unread;
  fixtureSnapshot.conversations = conversations.filter((conversation) => conversation.id !== from);
  fixtureRedirects.set(from, to);
  return () => {
    survivor.messages = survivorMessages;
    survivor.unread = survivorUnread;
    fixtureSnapshot.conversations = conversations;
    fixtureRedirects.delete(from);
  };
}

const fixtureSnapshot: DesktopSnapshot = { writesPaused: false, version: "1", mode: "fixture", connection: { state: "connected", origin: "https://push.example.com" }, encryption: { state: "unlocked" }, gateways: [{ id: "gateway-pixel8", name: "Pixel 8", simId: "sim-1", online: true, simulated: true, supportsSms: true, supportsMms: true, availability: "unknown", settingUp: false, wakeRoute: false, mmsContentVersion: 2, mmsMaxBytes: 300 * 1024, mmsLimitSource: "fallback", mmsMaxRecipients: 20 }], conversations: ([{ id: "conv-aurora", name: "Aurora Chen", preview: "Can you send over the estimate?", unread: 2, messages: fixtureMessages }, { id: "conv-river", name: "River Park", preview: "Attachment received", unread: 0, participants: ["River Park", "Mina Torres"], messages: [{ id: "message-river-1", revision: "1", sender: "other", body: "Attachment received", timestamp: "1788348060000", transport: "mms", subject: "Estimate", attachments: [{ id: "attachment-river-1", name: "estimate.pdf", mediaType: "application/pdf", byteSize: 182000, state: "ready", transfer: "download", retryable: true }] }] }, { id: "conv-short-code", name: "5556", preview: "Short code", unread: 0, messages: [] }, { id: "conv-unresolved-number", name: "+18015551234", preview: "Unresolved number", unread: 0, participants: ["+18015551234"], messages: [] }, { id: "conv-group-numbers", name: "+18015551234, +15555550199", preview: "Group conversation", unread: 0, participants: ["+18015551234", "+15555550199"], messages: [] }] satisfies FixtureConversationInput[]).map(fixtureConversation), activeConversationId: "conv-aurora", draft: undefined, head: { enabled: true, capability: "supported" }, deviceRole: "owner", pendingCount: 1, quarantineCount: 0, notifications: [{ target: { sourceDeviceId: "gateway-pixel8", notificationKey: "chat-42", lifetime: "1" }, packageName: "com.example.chat", appName: "Chat", appIconPngBase64: fixtureChatNotificationIcon, title: "Morgan", text: "Are we still meeting after lunch?", postedAt: Date.now() - 120000, dismissible: true, seen: false, dismissalPending: false }, { target: { sourceDeviceId: "gateway-pixel8", notificationKey: "chat-43", lifetime: "1" }, packageName: "com.example.chat", appName: "Chat", appIconPngBase64: fixtureChatNotificationIcon, title: "Verification", text: "Your verification code is 839201.", oneTimeCode: "839201", postedAt: Date.now() - 180000, dismissible: true, seen: false, dismissalPending: false }, { target: { sourceDeviceId: "gateway-pixel8", notificationKey: "mail-7", lifetime: "1" }, packageName: "com.example.mail", appName: "Mail", appIconPngBase64: fixtureMailNotificationIcon, title: "Project update", text: "A detailed update is ready for your review.", postedAt: Date.now() - 3600000, dismissible: true, seen: false, dismissalPending: false }], appFilters: [], notificationPreferences: { messageBanners: true, mirroredBanners: true, preview: "full", muteWithHeads: true }, notificationDismissalBlocks: [] };
fixtureSnapshot.notifications.push(
  { target: { sourceDeviceId: "gateway-pixel8", notificationKey: "slack-1", lifetime: "1" }, packageName: "com.slack", appName: "Slack", title: "Design", text: "Alex mentioned you in #product.", postedAt: Date.now() - 900000, dismissible: true, seen: false, dismissalPending: false },
  { target: { sourceDeviceId: "gateway-work", notificationKey: "calendar-1", lifetime: "1" }, packageName: "com.google.android.calendar", appName: "Calendar", title: "Planning session", text: "Starts tomorrow at 9:00 AM.", postedAt: Date.now() - 26 * 60 * 60 * 1000, dismissible: true, seen: true, dismissalPending: false },
  { target: { sourceDeviceId: "gateway-work", notificationKey: "bank-1", lifetime: "1" }, packageName: "com.chase.sig.android", appName: "Bank", title: "Sign-in code", text: "Your code is 473820.", oneTimeCode: "473820", postedAt: Date.now() - 2 * 24 * 60 * 60 * 1000, dismissible: true, seen: true, dismissalPending: false },
  { target: { sourceDeviceId: "gateway-pixel8", notificationKey: "food-1", lifetime: "1" }, packageName: "com.ubercab.eats", appName: "Food", title: "Your order is on the way", text: "Your driver is nearby.", postedAt: Date.now() - 4 * 60 * 60 * 1000, dismissible: true, seen: false, dismissalPending: true },
);
const fixtureSeenMessageIds = new Set<string>();
const fixtureDevices: ManagedDeviceView[] = [
  { id: "device-desktop", name: "This Mac", kind: "desktop", role: "owner", revoked: false },
  { id: "device-pixel8", name: "Pixel 8", kind: "android", role: "gateway", revoked: false },
  { id: "device-browser", name: "Work browser", kind: "browser", role: "device", revoked: false },
  { id: "device-old-iphone", name: "Old iPhone", kind: "ios", role: "owner", revoked: true },
];
const fixtureScanStatuses = new Map<string, ContactScanStatus>();
const fixtureLookupExcludedBookIds = new Set<string>();
/** Test-only gate for `delete_contact_book`: exercises the host's offline and sync-not-current refusals. */
let fixtureContactDeleteGate: "ready" | "offline" | "sync-not-current" = "ready";
export function setFixtureContactDeleteGateForTests(gate: "ready" | "offline" | "sync-not-current"): void {
  fixtureContactDeleteGate = gate;
}
const fixtureNameCandidates: Array<ContactNameCandidate & { address: string }> = [
  { address: "+12025550123", bookId: "book-pixel8", contactId: "contact-aurora", displayName: "Aurora Chen" },
  { address: "+12025550124", bookId: "book-pixel8", contactId: "contact-river", displayName: "River Park" },
];
const fixtureNamePins = new Map<string, { bookId: string; contactId: string }>();
const fixtureNameCandidatesFor = (address: string): ContactNameCandidate[] => fixtureNameCandidates
  .filter(candidate => candidate.address === address && !fixtureLookupExcludedBookIds.has(candidate.bookId))
  .map(({ bookId, contactId, displayName }) => ({ bookId, contactId, displayName }));
// Exercise the same display-only contact resolution path as the native snapshot.
fixtureSnapshot.conversations[0].name = "+12025550123";
fixtureSnapshot.conversations[0].participants = ["+12025550123"];
fixtureSnapshot.contactResolution = {
  "+12025550123": { contactId: "contact-aurora", bookId: "book-pixel8", displayName: "Aurora Chen" },
};
fixtureSnapshot.contactsPendingCount = 1;
fixtureSnapshot.conversations.push(...fixtureReactionConversations);
fixtureSnapshot.conversations.push(fixtureConversation({ id: "conv-sam", name: "Sam Rivera", preview: "Reacted 👍 to “a message from last year that is not loaded…”", unread: 0, participants: ["+15555550142"], messages: fixtureReactionMessages }));

// Group fixtures, one per UX state (ux-spec §2, §4, §7). Member addresses are fixture-only.
const fixtureGroupMembers = ["+15555550142", "+15555550199"];
const fixtureGroupNamed = (name: string, group: Omit<GroupView, "name" | "members" | "canCreate" | "createNeedsConfirmation" | "notices">): GroupView => ({ name, members: fixtureGroupMembers, canCreate: false, createNeedsConfirmation: false, notices: [], ...group });
fixtureSnapshot.conversations.push(...([
  { id: "conv-group-live", name: "Weekend crew", preview: "Saturday works", unread: 0, participants: fixtureGroupMembers, group: { ...fixtureGroupNamed("Weekend crew", { state: "live", memberRevision: "3" }), notices: [{ kind: "added", member: "+15555550199", atMs: "1788434100000" }, { kind: "renamed", name: "Weekend crew", atMs: "1788434150000" }] }, messages: [
    { id: "message-group-live-1", revision: "1", sender: "other", senderAddress: "+15555550142", body: "Plans for Saturday?", timestamp: "1788434200000", transport: "rcs", attachments: [] },
    { id: "message-group-live-2", revision: "1", sender: "other", body: "Sent from a number that is not in the roster", timestamp: "1788434300000", transport: "rcs", attachments: [] },
    { id: "message-group-live-3", revision: "1", sender: "self", body: "Saturday works", timestamp: "1788434400000", transport: "rcs", status: "delivery-confirmed", attachments: [] },
  ] },
  { id: "conv-group-unnamed", name: "+15555550142, +15555550199", preview: "Unnamed group", unread: 0, participants: fixtureGroupMembers, group: { members: fixtureGroupMembers, state: "live", memberRevision: "1", canCreate: false, createNeedsConfirmation: false, notices: [] }, messages: [] },
  { id: "conv-group-paused", name: "Project team", preview: "Members changed", unread: 0, participants: fixtureGroupMembers, group: { ...fixtureGroupNamed("Project team", { state: "paused", memberRevision: "5" }) }, messages: [
    { id: "message-group-paused-1", revision: "1", sender: "other", senderAddress: "+15555550199", body: "Moving the review to Monday", timestamp: "1788434500000", transport: "rcs", attachments: [] },
  ] },
  { id: "conv-group-withdrawn", name: "Old crew", preview: "You left this group", unread: 0, participants: fixtureGroupMembers, group: { ...fixtureGroupNamed("Old crew", { state: "withdrawn", memberRevision: "4" }) }, messages: [
    { id: "message-group-withdrawn-1", revision: "1", sender: "other", senderAddress: "+15555550142", body: "Thanks everyone", timestamp: "1788434600000", transport: "rcs", attachments: [] },
  ] },
  { id: "conv-group-long-name", name: "Long group name for the header and details dialog", preview: "Long name", unread: 0, participants: fixtureGroupMembers, group: { ...fixtureGroupNamed("Long group name for the header and details dialog that keeps going well past the header width and wraps inside the dialog", { state: "live", memberRevision: "2" }) }, messages: [] },
] satisfies FixtureConversationInput[]).map(fixtureConversation));
fixtureSnapshot.conversations.find(conversation => conversation.id === "conv-group-numbers")!.group = { members: ["+18015551234", "+15555550199"], state: "unlinked", memberRevision: "1", canCreate: true, createNeedsConfirmation: false, notices: [] };
// RCS media UX states (incoming placeholders and outgoing send statuses); see rcsMediaFixtures.ts.
fixtureSnapshot.conversations.push(fixtureConversation({ id: "conv-rcs-media", name: "Google Messages media", preview: "Ten files from the trip", unread: 0, participants: ["+15555550142"], messages: fixtureRcsMediaMessages }));
// Send-again UX states (resend-ux spec, Fixtures). Chains link only through sentAgain; ids are fixture-only.
const fixtureResendParticipants = ["+15555550188"];
const fixtureResendMessages: MessageView[] = [
  { id: "fixture-resend-not-sent", revision: "1", sender: "self", body: "Running 10 minutes late", timestamp: "1788436000000", transport: "sms", participants: fixtureResendParticipants, status: "failed-before-submit", statusAt: "1788436030000", attachments: [], sendRecovery: { kind: "not-sent", canSendAgain: true } },
  { id: "fixture-resend-unconfirmed", revision: "1", sender: "self", body: "Can you bring the charger?", timestamp: "1788436060000", transport: "sms", participants: fixtureResendParticipants, status: "gateway-persisted", statusAt: "1788436090000", attachments: [], sendRecovery: { kind: "unconfirmed-removed", canSendAgain: true } },
  { id: "fixture-resend-pair-a", revision: "1", sender: "self", body: "Did the parcel arrive?", timestamp: "1788436120000", transport: "sms", participants: fixtureResendParticipants, status: "gateway-persisted", statusAt: "1788436150000", attachments: [], sendRecovery: { kind: "unconfirmed-removed", canSendAgain: false, sentAgain: { messageId: "fixture-resend-pair-b" } }, sentAgain: { messageId: "fixture-resend-pair-b" } },
  { id: "fixture-resend-pair-b", revision: "1", sender: "self", body: "Did the parcel arrive?", timestamp: "1788436180000", transport: "sms", participants: fixtureResendParticipants, status: "server-accepted", statusAt: "1788436200000", attachments: [] },
  { id: "fixture-resend-delivered-a", revision: "1", sender: "self", body: "Table for four at 7?", timestamp: "1788436240000", transport: "sms", participants: fixtureResendParticipants, status: "gateway-persisted", statusAt: "1788436270000", attachments: [], sendRecovery: { kind: "unconfirmed-removed", canSendAgain: false, sentAgain: { messageId: "fixture-resend-delivered-b" } }, sentAgain: { messageId: "fixture-resend-delivered-b" } },
  { id: "fixture-resend-delivered-b", revision: "1", sender: "self", body: "Table for four at 7?", timestamp: "1788436300000", transport: "sms", participants: fixtureResendParticipants, status: "delivery-confirmed", statusAt: "1788436330000", attachments: [] },
  { id: "fixture-resend-triple-a", revision: "1", sender: "self", body: "Bring the blue folder.", timestamp: "1788436360000", transport: "sms", participants: fixtureResendParticipants, status: "gateway-persisted", statusAt: "1788436390000", attachments: [], sendRecovery: { kind: "unconfirmed-removed", canSendAgain: false, sentAgain: { messageId: "fixture-resend-triple-b" } }, sentAgain: { messageId: "fixture-resend-triple-b" } },
  { id: "fixture-resend-triple-b", revision: "1", sender: "self", body: "Bring the blue folder.", timestamp: "1788436420000", transport: "sms", participants: fixtureResendParticipants, status: "failed-before-submit", statusAt: "1788436450000", attachments: [], sendRecovery: { kind: "not-sent", canSendAgain: false, sentAgain: { messageId: "fixture-resend-triple-c" } }, sentAgain: { messageId: "fixture-resend-triple-c" } },
  { id: "fixture-resend-triple-c", revision: "1", sender: "self", body: "Bring the blue folder.", timestamp: "1788436480000", transport: "sms", participants: fixtureResendParticipants, status: "failed-before-submit", statusAt: "1788436510000", attachments: [], sendRecovery: { kind: "not-sent", canSendAgain: true } },
  { id: "fixture-resend-gap-a", revision: "1", sender: "self", body: "Is the 3pm still on?", timestamp: "1788436540000", transport: "sms", participants: fixtureResendParticipants, status: "gateway-persisted", statusAt: "1788436570000", attachments: [], sendRecovery: { kind: "unconfirmed-removed", canSendAgain: false, sentAgain: { messageId: "fixture-resend-gap-copy" } }, sentAgain: { messageId: "fixture-resend-gap-copy" } },
  { id: "fixture-resend-gap-reply", revision: "1", sender: "other", senderAddress: "+15555550188", body: "Yes, see you then.", timestamp: "1788436600000", transport: "sms", attachments: [] },
  { id: "fixture-resend-gap-copy", revision: "1", sender: "self", body: "Is the 3pm still on?", timestamp: "1788436660000", transport: "sms", participants: fixtureResendParticipants, status: "server-accepted", statusAt: "1788436690000", attachments: [] },
];
fixtureSnapshot.conversations.push(fixtureConversation({ id: "conv-resend", name: "Resend states", preview: "Is the 3pm still on?", unread: 0, participants: fixtureResendParticipants, messages: fixtureResendMessages }));

/** Fixture drafts keyed by conversation ID; mirrors the native compose-draft save contract (session.rs). */
const fixtureDrafts = new Map<string, Draft>();
const fixtureDraftRedirects = new Map<string, string>();
/** Merged-away conversation ID to its survivor; requests for the former resolve to the latter, as the core does. */
const fixtureRedirects = new Map<string, string>();
const fixtureToneOverrides = new Map<string, number>();
let fixtureCreated = 0;
let fixturePhoneApproved = false;
let fixtureMcpEnabled = false;
let fixtureMcpGeneration = 0;
const fixtureMcpGrants = [{ id: "11111111-1111-4111-8111-111111111111", clientName: "Fixture assistant", scopes: ["conversations:read" as const], createdAt: "Fixture", lastUsedAt: null }];
const fixtureWebhooks: WebhookView[] = [
  { id: "22222222-2222-4222-8222-222222222222", name: "Home Assistant", nameIsDerived: false, url: "https://home.example.com/peppy", events: ["message.received"], filterMode: "all", filter: [], paused: false, createdAt: "2026-11-01T12:00:00Z", lastDeliveryState: "delivered", lastDeliveryAt: "2026-11-07T12:00:00Z", lastStatusCode: 200, lastError: null, pendingDeliveries: 0 },
  { id: "33333333-3333-4333-8333-333333333333", name: "Zapier: Alice & Bob", nameIsDerived: false, url: "https://hooks.zapier.com/example", events: ["message.received"], filterMode: "only", filter: [{ address: "+12025550123", label: "Alice", contactId: null, bookId: null }, { address: "+12025550124", label: "Bob", contactId: null, bookId: null }], paused: false, createdAt: "2026-11-02T12:00:00Z", lastDeliveryState: "failed", lastDeliveryAt: "2026-11-07T12:00:00Z", lastStatusCode: 503, lastError: "http_503", pendingDeliveries: 1 },
  { id: "44444444-4444-4444-8444-444444444444", name: "n8n triage", nameIsDerived: false, url: "https://n8n.example.com/webhook/triage", events: ["message.received"], filterMode: "except", filter: [{ address: "+12025550125", label: "Mina", contactId: null, bookId: null }], paused: true, createdAt: "2026-11-03T12:00:00Z", lastDeliveryState: "never", lastDeliveryAt: null, lastStatusCode: null, lastError: null, pendingDeliveries: 0 },
];
let fixtureWebhookSequence = 4;
const fixtureWebhookSecrets = new Map(fixtureWebhooks.map((webhook) => [webhook.id, `whsec_fixture_${webhook.id.replaceAll("-", "")}`]));
const fixtureWebhook = (input: WebhookInput, id: string): WebhookView => ({ id, name: input.name?.trim() || new URL(input.url).hostname.replace(/^www\./, ""), nameIsDerived: !input.name?.trim(), url: input.url, events: [...input.events], filterMode: input.filterMode, filter: input.filter.map(entry => ({ ...entry })), paused: input.paused, createdAt: "2026-11-07T12:00:00Z", lastDeliveryState: "never", lastDeliveryAt: null, lastStatusCode: null, lastError: null, pendingDeliveries: 0 });
const fixtureError = (code: string, message: string): BridgeError => ({ code, message });
const FIXTURE_CONTACT_PAGE_SIZE = 200;
const FIXTURE_CONTACT_THROUGH_MAX = 2_000;
/** Browser-only pause so contact loading states show in screenshots; tests stay fast. */
const fixtureContactLatency = async () => {
  if (import.meta.env.MODE !== "test") await new Promise(resolve => setTimeout(resolve, 600));
};
const fixtureNamedContacts = (bookId: string): ContactView[] => [{ id: "contact-aurora", bookId, revision: "1", displayName: "Aurora Chen", givenName: "Aurora", familyName: "Chen", phones: [{ label: "mobile", number: "+12025550123" }], emails: [{ label: "work", address: "aurora@example.com" }], addresses: [], organization: "Northstar", title: "Director", notes: "Prefers SMS", pendingEditId: "edit-aurora", pendingEditState: "pending", pendingEditSummary: "Phone number update" }, { id: "contact-river", bookId, revision: "1", displayName: "River Park", givenName: "River", familyName: "Park", phones: [{ label: "mobile", number: "+12025550124" }], emails: [], addresses: [] }, { id: "contact-mina", bookId, revision: "1", displayName: "Mina Torres", givenName: "Mina", familyName: "Torres", phones: [{ label: "work", number: "+12025550125" }], emails: [], addresses: [] }];
const fixtureFirstNames = ["Ava", "Benjamin", "Chloe", "Diego", "Elena", "Farid", "Grace", "Hiroshi", "Isla", "Jamal", "Keiko", "Liam", "Maya", "Noah", "Olivia", "Priya", "Quentin", "Rosa", "Samir", "Tessa", "Uma", "Victor", "Wren", "Ximena", "Yusuf", "Zoe", "Émile", "Ingrid", "Mateo", "Ngozi"];
const fixtureLastNames = ["Abbott", "Bianchi", "Carver", "Delgado", "Eriksen", "Fontaine", "Gallagher", "Haddad", "Ishikawa", "Jensen", "Kowalski", "Lindqvist", "Moreau", "Nakamura", "Okafor", "Patel", "Quinn", "Rossi", "Schmidt", "Tanaka", "Underwood", "Vasquez", "Whitfield", "Yamamoto", "Zimmer", "Ångström", "Brandt", "Costa", "Dubois", "Fernandes"];
const fixtureNicknames = ["Coach", "Bean", "Sparky", "Mags", "Doc", "Juno"];
const fixtureOrganizations = ["Brightline Dental", "Northwind Plumbing", "Juniper Bakery", "Halcyon Gym", "Summit Auto Care", "Pinecrest Vet"];
const fixturePhotoColors = ["#4f7cff", "#e4572e", "#29a19c", "#f3a712", "#8e5ea2", "#17bebb"];
/** A tiny inline person silhouette on a coloured tile; `<img>` renders SVG data URLs. */
const fixturePhotoDataUrl = (index: number) => `data:image/svg+xml;utf8,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" fill="${fixturePhotoColors[index % fixturePhotoColors.length]}"/><circle cx="32" cy="25" r="11" fill="#fff" fill-opacity=".9"/><path d="M12 60c3-13 13-19 20-19s17 6 20 19z" fill="#fff" fill-opacity=".9"/></svg>`)}`;
const fixtureAsciiSlug = (value: string) => value.normalize("NFD").replace(/\p{Diacritic}/gu, "").toLowerCase();
/**
 * Deterministic generated contact `index` of a fixture book. Names are a pair walk over the two name arrays, so
 * every name pair is distinct; a few index classes become organization, nickname, team, or given-name-only contacts.
 */
const fixtureGeneratedContact = (bookId: string, index: number, phonePrefix: string): ContactView => {
  const id = `contact-${bookId.replace("book-", "")}-${String(index).padStart(3, "0")}`;
  const given = fixtureFirstNames[index % fixtureFirstNames.length];
  const family = fixtureLastNames[(index * 7 + Math.floor(index / fixtureFirstNames.length)) % fixtureLastNames.length];
  const phoneCount = [1, 1, 2, 1, 3, 1, 2, 1, 1, 4][index % 10];
  const phones = Array.from({ length: phoneCount }, (_, slot) => ({ label: ["mobile", "work", "home", "other"][slot], number: `${phonePrefix}${(slot + 1) * 1000 + index}` }));
  const shared: ContactView = { id, bookId, revision: "1", displayName: given, phones, emails: [], addresses: [], ...(index % 6 === 0 ? { photoDataUrl: fixturePhotoDataUrl(index) } : {}) };
  if (index % 17 === 4) {
    const organization = fixtureOrganizations[index % fixtureOrganizations.length];
    return { ...shared, displayName: organization, organization };
  }
  if (index % 19 === 6) {
    const nickname = fixtureNicknames[index % fixtureNicknames.length];
    return { ...shared, displayName: nickname, nickname };
  }
  if (index % 31 === 9) {
    const organization = fixtureOrganizations[(index + 1) % fixtureOrganizations.length];
    return { ...shared, displayName: `${organization} (Team)`, organization };
  }
  if (index % 11 === 3) return { ...shared, displayName: given, givenName: given };
  const organization = index % 6 === 1 ? fixtureOrganizations[index % fixtureOrganizations.length] : undefined;
  const emails = index % 5 === 0 ? [{ label: "work", address: `${fixtureAsciiSlug(given)}.${fixtureAsciiSlug(family)}@example.com` }] : [];
  return { ...shared, displayName: `${given} ${family}`, givenName: given, familyName: family, emails, ...(organization ? { organization } : {}) };
};
const fixtureContactRowsByBook = new Map<string, ContactView[]>([
  ["book-pixel8", [...fixtureNamedContacts("book-pixel8"), ...Array.from({ length: 427 }, (_, index) => fixtureGeneratedContact("book-pixel8", index, "+1801555"))]],
  ["book-phone-retired", Array.from({ length: 25 }, (_, index) => fixtureGeneratedContact("book-phone-retired", index, "+1206555"))],
]);
const fixtureContactRows = (bookId: string): ContactView[] => fixtureContactRowsByBook.get(bookId) ?? [];
/** Removes a fixture book with its rows and lookup state; returns the removed book. */
const dropFixtureContactBook = (index: number): ContactBookView => {
  const [book] = fixtureContactBooks.splice(index, 1);
  fixtureContactRowsByBook.delete(book.id);
  fixtureLookupExcludedBookIds.delete(book.id);
  return book;
};
const fixtureContactBooks: ContactBookView[] = [{ id: "book-pixel8", deviceName: "Android phone", state: "active", capabilities: { canWrite: true, canDelete: true, canRestore: true, supportsNotes: true, supportsPhoto: true, supportsBirthday: true, canSync: true }, ownerDeviceId: "device-pixel8", contactCount: fixtureContactRows("book-pixel8").length, defaultAccountLabel: "Google, aurora@example.com", lastSyncAt: "Just now", pendingEditCount: 1 }, { id: "book-phone-retired", deviceName: "Phone", state: "retired", capabilities: { canWrite: false, canDelete: false, supportsNotes: false, supportsPhoto: true, canSync: false }, sourceRevoked: true, ownerDeviceId: "device-old-iphone", contactCount: fixtureContactRows("book-phone-retired").length, lastSyncAt: "3 days ago", pendingEditCount: 0 }];
/** The fixture book's contacts in the requested order; `list_contacts` and `list_contacts_page` share it. */
const sortedFixtureContacts = (bookId: string, query: string, sort: ContactSort): ContactView[] => {
  const contacts = fixtureContactRows(bookId);
  const sortKey: "givenName" | "familyName" = sort.key === "given" ? "givenName" : "familyName";
  const otherSortKey: "givenName" | "familyName" = sort.key === "given" ? "familyName" : "givenName";
  const direction = sort.direction === "desc" ? -1 : 1;
  const compareText = (left: string, right: string) => left < right ? -1 : left > right ? 1 : 0;
  const sortParts = (contact: ContactView) => {
    const selected = (contact[sortKey] ?? "").trim().toLowerCase();
    const other = (contact[otherSortKey] ?? "").trim().toLowerCase();
    const display = contact.displayName.trim().toLowerCase();
    return { selected, other, display, primary: selected || other || display };
  };
  return contacts.filter(contact => !query || `${contact.displayName} ${contact.phones.map(phone => phone.number).join(" ")}`.toLowerCase().includes(query.toLowerCase())).sort((left, right) => {
    const leftParts = sortParts(left);
    const rightParts = sortParts(right);
    if (!leftParts.primary || !rightParts.primary) return !leftParts.primary && !rightParts.primary ? compareText(left.id, right.id) : !leftParts.primary ? 1 : -1;
    return direction * (compareText(leftParts.primary, rightParts.primary) || compareText(leftParts.other, rightParts.other) || compareText(leftParts.display, rightParts.display)) || compareText(left.id, right.id);
  });
};
/** Opaque to callers; the fixture encodes the row offset of the next page. */
const encodeFixtureContactCursor = (offset: number) => btoa(`fixture-offset:${offset}`);
const decodeFixtureContactCursor = (cursor: string | undefined): number => {
  if (cursor === undefined) return 0;
  const match = /^fixture-offset:(\d+)$/.exec(atob(cursor));
  if (!match) throw fixtureError("invalid-cursor", "The contact page cursor is invalid.");
  return Number(match[1]);
};
/** Every live fixture contact, for local contact search. */
const fixtureSearchContacts = () => [...fixtureContactRowsByBook].flatMap(([bookId, rows]) => rows.map(row => ({ id: row.id, bookId, title: row.displayName, phones: row.phones.map(phone => phone.number), organization: row.organization })));
const fixtureSearchContactHit = (contact: ReturnType<typeof fixtureSearchContacts>[number], query: string): LocalSearchHit & { kind: "contact" } => {
  const bookLabel = fixtureContactBooks.find(book => book.id === contact.bookId)?.deviceName;
  const matchSnippet = contact.phones.find(phone => phone.toLowerCase().includes(query));
  const phoneMatch = !contact.title.toLowerCase().includes(query) && matchSnippet !== undefined ? { matchField: "Phone", matchSnippet } : {};
  return {
    kind: "contact",
    bookId: contact.bookId,
    contactId: contact.id,
    title: contact.title,
    subtitle: contact.phones[0] ?? "",
    phones: contact.phones.slice(0, 3),
    phoneCount: contact.phones.length,
    ...(contact.organization ? { organization: contact.organization } : {}),
    ...(bookLabel ? { bookLabel } : {}),
    ...phoneMatch,
  };
};
const fixtureSearch = (input: LocalSearchRequest): LocalSearchPage => {
  const query = input.query.trim().toLocaleLowerCase();
  if (!query) return { query: input.query, items: [], indexState: "ready", hasMore: { conversation: false, message: false, contact: false } };
  const limit = Math.min(input.limit ?? 6, 50);
  const accepts = (kind: "conversation" | "message" | "contact") => !input.kind || input.kind === "all" || input.kind === kind;
  const conversations = fixtureSnapshot.conversations.filter(item => `${item.name} ${item.participants?.join(" ") ?? ""}`.toLocaleLowerCase().includes(query)).map(item => ({ kind: "conversation" as const, conversationId: item.id, title: item.name, subtitle: item.preview })).slice(0, limit);
  const messages = fixtureSnapshot.conversations.flatMap(conversation => conversation.messages.filter(message => `${message.body} ${message.subject ?? ""}`.toLocaleLowerCase().includes(query)).map(message => ({ kind: "message" as const, conversationId: conversation.id, messageId: message.id, title: conversation.name, snippet: message.body, timestamp: message.timestamp }))).slice(0, limit);
  const contacts = fixtureSearchContacts().filter(contact => `${contact.title} ${contact.phones.join(" ")}`.toLocaleLowerCase().includes(query)).map(contact => fixtureSearchContactHit(contact, query)).slice(0, limit);
  const items = [...(accepts("conversation") ? conversations : []), ...(accepts("message") ? messages : []), ...(accepts("contact") ? contacts : [])];
  return { query: input.query, items, indexState: "ready", hasMore: { conversation: false, message: false, contact: false } };
};
/** Listeners the app refreshes from. The fixture publishes after each reaction change, as the core does. */
const fixtureChangeListeners = new Set<() => void>();
const publishFixtureChange = () => { fixtureChangeListeners.forEach(listener => listener()); };

const fixtureMessageWindow = (input: MessageWindowRequest): MessageWindow => {
  const conversation = fixtureSnapshot.conversations.find(item => item.id === input.conversationId);
  if (!conversation) throw fixtureError("not-found", "The requested stored item was not found.");
  if (input.direction === "refresh") return { conversationId: conversation.id, messages: conversation.messages.filter(message => input.messageIds.includes(message.id)), hasOlder: false, hasNewer: false };
  const anchor = conversation.messages.findIndex(message => message.id === input.messageId);
  if (anchor < 0) throw fixtureError("not-found", "The requested stored item was not found.");
  const start = input.direction === "older" ? Math.max(0, anchor - 50) : input.direction === "newer" ? anchor + 1 : Math.max(0, anchor - 24);
  const end = input.direction === "older" ? anchor : Math.min(conversation.messages.length, start + 50);
  return { conversationId: conversation.id, messages: conversation.messages.slice(start, end), hasOlder: start > 0, hasNewer: end < conversation.messages.length };
};
const canonicalRecipient = (recipient: string) => normalizePhoneNumber(recipient) ?? recipient.trim();
const canonicalRecipients = (recipientIds: string[]) => [...new Set(recipientIds.map(canonicalRecipient).filter(Boolean))];
const recipientSetKey = (recipientIds: string[]) => canonicalRecipients(recipientIds).sort().join("\n");
const sameRecipientSet = (left: string[], right: string[]) => recipientSetKey(left) === recipientSetKey(right);
const isEstablishedFixtureConversation = (conversationId: string) => fixtureSnapshot.conversations.some((conversation) => conversation.id === conversationId);
const matchingFixtureConversationId = (recipientIds: string[]) => fixtureSnapshot.conversations.find((conversation) => sameRecipientSet(conversation.participants ?? [], recipientIds))?.id
  ?? [...fixtureDrafts.values()].find((draft) => !isEstablishedFixtureConversation(draft.conversationId) && sameRecipientSet(draft.recipientIds, recipientIds))?.conversationId;
const mergeDraftText = (target: string, incoming: string) => target && incoming && target !== incoming ? `${target}\n${incoming}` : target || incoming;
const mergeAttachmentIds = (target: string[], incoming: string[]) => [...new Set([...target, ...incoming])];
const newFixtureDraft = (conversationId: string): Draft => ({
  id: `draft-fixture-${conversationId}`,
  conversationId,
  text: "",
  recipientIds: [],
  attachmentIds: [],
  revision: "0",
});
const fixtureMovedError = (target: Draft) => ({
  code: "draft-moved",
  message: "Draft moved to an existing unsent draft; its content was preserved.",
  conversationId: target.conversationId,
  draftId: target.id,
  currentRevision: target.revision,
  details: { conversationId: target.conversationId, draftId: target.id, currentRevision: target.revision },
});
const fixtureMovedDraft = (id: string): Draft | undefined => {
  const targetId = fixtureDraftRedirects.get(id);
  return targetId ? [...fixtureDrafts.values()].find(draft => draft.id === targetId) : undefined;
};
const fixtureAccount = (): HostedAccountView => ({ available: true, signedIn: false, accountLabel: null, classification: null, entitlement: null, access: null, hasVault: false, resumable: false, providers: ["apple", "google"] });
/** Display-only fixture views deliberately have no reducer or event transition logic. */
const cannedHostedView = (screen = "welcome", scenario = "new"): HostedPreviewView => ({
  scenario, screen, accountState: screen === "welcome" ? "anonymous" : "signed_in", entitlementState: screen === "lapsed" ? "expired" : "active", approvalState: screen === "approval" ? "pending" : "none", unlocked: screen === "settings", rejected: false,
  statusKey: screen === "subscription_verifying" ? "hosted_purchase_verifying" : screen === "purchase_pending" ? "hosted_purchase_pending" : null, localError: null, operationId: screen === "provisioning" ? "preview-operation" : null,
  fixture: { accountLabel: screen === "welcome" ? null : "Preview account", signInProvider: null, subscription: screen === "signin" || screen === "welcome" ? null : { displayPrice: "$4.99/month · Preview price", status: "active" }, approvalCode: screen === "join" || screen === "approval" ? "418 207" : null, hostedOrigin: screen === "welcome" || screen === "signin" ? null : "preview.peppy.pro (preview)" },
  scenarios: ["new", "store_unavailable", "provision_retry", "join", "lapsed"],
});
const fixtureConversationIds = () => [...fixtureSnapshot.conversations.map(c => c.id), ...fixtureDrafts.keys()];
/** Draft-only conversations are listed like the host lists them. */
const fixtureDraftConversations = (): ConversationView[] => [...fixtureDrafts.values()]
  .filter(draft => !fixtureSnapshot.conversations.some(c => c.id === draft.conversationId))
  .map(draft => fixtureConversation({ id: draft.conversationId, name: draft.recipientIds.join(", ") || "New message", preview: draft.text ? `Draft: ${draft.text.slice(0, 80)}` : "Draft", unread: 0, messages: [], draftOnly: true, recipientlessDraft: draft.recipientIds.length === 0 }));
/**
 * Empty draft ID: create the draft (or attach to the conversation's existing one); an empty
 * conversation ID also creates a new conversation. Saves are CAS on a numeric revision.
 */
const fixtureSave = (input: DraftInput): Draft => {
  const expected = Number(input.expectedRevision);
  if (!/^\d+$/.test(input.expectedRevision)) throw fixtureError("invalid-draft", "The draft revision is invalid.");
  const recipientIds = canonicalRecipients(input.recipientIds);
  const source = input.id ? [...fixtureDrafts.values()].find((draft) => draft.id === input.id) : undefined;
  if (input.id && !source) {
    const moved = fixtureMovedDraft(input.id);
    if (moved) throw fixtureMovedError(moved);
    throw fixtureError("not-found", "The requested stored item was not found.");
  }
  if (source && input.conversationId && input.conversationId !== source.conversationId) throw fixtureError("invalid-draft", "The draft does not belong to this conversation.");
  if (source && Number(source.revision) !== expected) throw fixtureError("stale-draft", `The draft changed elsewhere (current revision ${source.revision}); both versions were kept.`);
  if (Boolean(input.gatewayId) !== Boolean(input.simId)) throw fixtureError("invalid-route", "Select a gateway and SIM together.");
  const emptyCreate = !source && !input.conversationId && !recipientIds.length;
  const recipientless = emptyCreate ? [...fixtureDrafts.values()].find((draft) => !isEstablishedFixtureConversation(draft.conversationId) && !draft.recipientIds.length) : undefined;
  if (recipientless) {
    if (!input.text && !input.attachmentIds.length) return recipientless;
    throw fixtureMovedError(recipientless);
  }

  const matchingConversationId = recipientIds.length ? matchingFixtureConversationId(recipientIds) : undefined;
  const sourceRecipients = source ? canonicalRecipients(source.recipientIds) : [];
  const recipientSetChanged = source && !sameRecipientSet(sourceRecipients, recipientIds);
  const adoptedHistory = Boolean(source && isEstablishedFixtureConversation(source.conversationId));
  const shouldFork = adoptedHistory && recipientSetChanged && !matchingConversationId;
  const targetConversationId = shouldFork
    ? `conv-fixture-new-${++fixtureCreated}`
    : ((matchingConversationId ?? source?.conversationId ?? input.conversationId) || `conv-fixture-new-${++fixtureCreated}`);
  const target = fixtureDrafts.get(targetConversationId) ?? newFixtureDraft(targetConversationId);
  if (!source && !input.conversationId && fixtureDrafts.has(targetConversationId)) {
    if (!input.text && !input.attachmentIds.length) return target;
    throw fixtureMovedError(target);
  }
  if (!source && Number(target.revision) !== expected) {
    throw fixtureError("stale-draft", `The draft changed elsewhere (current revision ${target.revision}); both versions were kept.`);
  }
  if (source && target !== source && Number(target.revision) > 0 && !matchingConversationId) {
    throw fixtureError("stale-draft", `The draft changed elsewhere (current revision ${target.revision}); both versions were kept.`);
  }
  const incomingText = input.text;
  const incomingAttachments = input.attachmentIds;
  const saved: Draft = {
    id: target.id,
    conversationId: targetConversationId,
    text: !source || target === source ? incomingText : mergeDraftText(target.text, incomingText),
    recipientIds,
    attachmentIds: !source || target === source ? incomingAttachments : mergeAttachmentIds(target.attachmentIds, incomingAttachments),
    gatewayId: input.gatewayId || target.gatewayId || source?.gatewayId,
    simId: input.simId || target.simId || source?.simId,
    revision: String(Number(target.revision) + 1),
  };
  if (source && source.conversationId !== targetConversationId) {
    fixtureDrafts.delete(source.conversationId);
    for (const [id, targetId] of fixtureDraftRedirects) {
      if (targetId === source.id) fixtureDraftRedirects.set(id, saved.id);
    }
    fixtureDraftRedirects.set(source.id, saved.id);
  }
  fixtureDrafts.set(saved.conversationId, saved);
  return saved;
};
let fixtureNotificationSound = false;
const FIXTURE_DIAGNOSTICS_LIMIT_BYTES = 4 * 1024 * 1024;
const FIXTURE_DIAGNOSTICS_GROWTH_BYTES = 64 * 1024;
const initialFixtureDiagnostics = (): DiagnosticsState => ({ enabled: false, retainedBytes: 1.5 * 1024 * 1024, limitBytes: FIXTURE_DIAGNOSTICS_LIMIT_BYTES, droppedEvents: 0, storageError: false });
let fixtureDiagnostics = initialFixtureDiagnostics();
/** Recording grows the retained log on each read, capped at the limit, as a real writer would. */
const readFixtureDiagnostics = (): DiagnosticsState => {
  if (fixtureDiagnostics.enabled) fixtureDiagnostics = { ...fixtureDiagnostics, retainedBytes: Math.min(fixtureDiagnostics.limitBytes, fixtureDiagnostics.retainedBytes + FIXTURE_DIAGNOSTICS_GROWTH_BYTES) };
  return { ...fixtureDiagnostics };
};
/** Restores the seeded fixture state; tests call it between cases. */
export function resetFixtureDiagnosticsForTests(): void {
  fixtureDiagnostics = initialFixtureDiagnostics();
}
let fixtureNotificationPersistence: NotificationPersistence = { seconds: 5 };
let fixtureNotificationQuickReply = false;
export const fixtureBridge: DesktopBridge = {
  load_state: async (conversationId, historyFromMessageId, options) => {
    const redirectedTo = conversationId ? fixtureRedirects.get(conversationId) : undefined;
    const active = redirectedTo ?? (conversationId && fixtureConversationIds().includes(conversationId) ? conversationId : fixtureSnapshot.activeConversationId!);
    const requestedRedirect = redirectedTo && conversationId ? { from: conversationId, to: redirectedTo } : undefined;
    const conversations = fixtureSnapshot.conversations.map(conversation => {
      if (conversation.id !== active || options?.metadataOnly) return { ...conversation, messages: [] };
      const anchorIndex = historyFromMessageId === undefined ? -1 : conversation.messages.findIndex(message => message.id === historyFromMessageId);
      if (historyFromMessageId !== undefined && anchorIndex < 0) throw fixtureError("not-found", "The requested stored item was not found.");
      const messages = historyFromMessageId === undefined ? conversation.messages.slice(-50) : conversation.messages.slice(anchorIndex);
      return { ...conversation, messages, hasOlderMessages: historyFromMessageId === undefined ? conversation.messages.length > messages.length : anchorIndex > 0 };
    });
    return { ...fixtureSnapshot, conversations: [...conversations, ...fixtureDraftConversations()], activeConversationId: active, draft: fixtureDrafts.get(active), ...(requestedRedirect ? { requestedRedirect } : {}) };
  },
  search_local: async input => fixtureSearch(input),
  advance_search_index: async (): Promise<SearchIndexProgress> => ({ indexState: "ready", processed: 0, remaining: 0 }),
  // No private history source exists in fixtures: report unsupported, never fake activity.
  get_history_search: async (): Promise<HistorySearchSnapshot> => ({ state: "idle", canSearchFurther: false, progressVersion: "0", indexVersion: "0" }),
  start_history_search: async (): Promise<HistorySearchSnapshot> => ({ state: "blocked", reason: "unsupported", canSearchFurther: false, progressVersion: "0", indexVersion: "0" }),
  cancel_history_search: async (): Promise<HistorySearchSnapshot> => ({ state: "cancelled", canSearchFurther: false, progressVersion: "0", indexVersion: "0" }),
  load_message_window: async input => fixtureMessageWindow(input),
  cancel_send: async ({ messageId }) => {
    for (const conversation of fixtureSnapshot.conversations) {
      const index = conversation.messages.findIndex(message => message.id === messageId);
      if (index < 0) continue;
      const message = conversation.messages[index];
      if (!message.sendCancellation?.canCancel) return message.sendCancellation ?? { state: "unknown", canCancel: false, requiresGatewayUpdate: false };
      const sendCancellation = { ...message.sendCancellation, state: "requested" as const, canCancel: false };
      conversation.messages[index] = { ...message, sendCancellation };
      return sendCancellation;
    }
    return { state: "unknown", canCancel: false, requiresGatewayUpdate: false };
  },
  send_again: async ({ messageId }) => {
    for (const conversation of fixtureSnapshot.conversations) {
      const index = conversation.messages.findIndex(message => message.id === messageId);
      if (index < 0) continue;
      const original = conversation.messages[index];
      if (!original.sendRecovery) throw fixtureError("invalid-request", "This message can't be sent again.");
      if (original.sendRecovery.sentAgain) throw fixtureError("already-sent-again", "This message was already sent again.");
      if (!original.sendRecovery.canSendAgain) throw fixtureError("invalid-request", "This message can't be sent again.");
      const newMessageId = `${messageId}-sent-again`;
      conversation.messages[index] = { ...original, sendRecovery: { ...original.sendRecovery, canSendAgain: false, sentAgain: { messageId: newMessageId } }, sentAgain: { messageId: newMessageId } };
      const timestamp = String(Date.now());
      conversation.messages.push({ id: newMessageId, revision: "1", sender: "self", body: original.body, timestamp, status: "queued-local", statusAt: timestamp, attachments: original.attachments, transport: original.transport, subject: original.subject, participants: original.participants });
      return { messageId: newMessageId };
    }
    throw fixtureError("not-found", "The requested stored item was not found.");
  },
  react_to_message: async ({ messageId, emoji }) => {
    for (const conversation of fixtureSnapshot.conversations) {
      const index = conversation.messages.findIndex(message => message.id === messageId);
      if (index < 0) continue;
      const message = conversation.messages[index];
      const gateway = fixtureSnapshot.gateways.find(candidate => candidate.id === conversation.reactionGatewayId);
      if (conversation.reactionGatewayId && gateway?.online !== true) return { accepted: false, reason: "offline" };
      if (conversation.reactionSupport === "needs_phone_update" && !message.reactable) return { accepted: false, reason: "needs_phone_update" };
      const route = message.reactable;
      if (!route) return { accepted: false, reason: "not_reactable" };
      if (route === "text") {
        if (!emoji) return { accepted: false, reason: "invalid_emoji" };
        conversation.messages.push({ id: `fixture-reaction-${Date.now()}`, revision: "1", sender: "self", body: composeFixtureReactionText(message.body, emoji), timestamp: String(Date.now()), attachments: [], transport: "sms", status: "sent" });
        publishFixtureChange();
        return { accepted: true, route };
      }
      if (message.ownReactionRequest?.state === "sending" || message.ownReactionRequest?.state === "unknown") return { accepted: false, reason: "busy" };
      const previousEmoji = message.reactions?.find(reaction => reaction.actors.some(actor => actor.self))?.emoji ?? undefined;
      const requestId = `fixture-request-${Date.now()}`;
      conversation.messages[index] = { ...message, ownReactionRequest: { requestId, emoji, previousEmoji, state: "sending" } };
      publishFixtureChange();
      setTimeout(() => {
        const current = conversation.messages[index];
        const others = (current.reactions ?? []).map(reaction => ({ emoji: reaction.emoji, actors: reaction.actors.filter(actor => !actor.self) })).filter(reaction => reaction.actors.length);
        conversation.messages[index] = { ...current, reactions: emoji ? [...others, { emoji, actors: [{ self: true }] }] : others, ownReactionRequest: undefined };
        publishFixtureChange();
      }, 1_000);
      return { accepted: true, requestId, route: "native" };
    }
    return { accepted: false, reason: "not_reactable" };
  },
  dismiss_reaction_request: async ({ requestId }) => {
    fixtureSnapshot.conversations.forEach(conversation => {
      conversation.messages = conversation.messages.map(message => message.ownReactionRequest?.requestId === requestId ? { ...message, ownReactionRequest: undefined } : message);
    });
    publishFixtureChange();
  },
  open_external_url: async url => { if (!/^(https?:\/\/|cursor:\/\/anysphere\.cursor-deeplink\/mcp\/install\?)/.test(url)) throw fixtureError("invalid-request", "The URL is invalid."); },
  open_mcp_oauth_redirect: async url => { if (!/^(?:https?:\/\/|(?:vscode|vscode-insiders|cursor|windsurf|jetbrains):\/\/[^/?#@]+(?:[/?]|$))/.test(url)) throw fixtureError("invalid-request", "The URL is invalid."); },
  load_history: async (conversationId, beforeMessageId) => {
    const conversation = fixtureSnapshot.conversations.find(item => item.id === conversationId);
    const anchorIndex = conversation?.messages.findIndex(message => message.id === beforeMessageId) ?? -1;
    if (!conversation || anchorIndex < 0) throw fixtureError("not-found", "The requested stored item was not found.");
    const start = Math.max(0, anchorIndex - 50);
    return { conversationId, messages: conversation.messages.slice(start, anchorIndex), hasMore: start > 0 };
  },
  configure_server: async () => undefined,
  unlock_sync: async () => undefined,
  get_app_lock_status: async () => ({ enabled: false, locked: false, storageAvailable: true, pending: false }),
  retry_app_lock_check: async () => ({ enabled: false, locked: false, storageAvailable: true, pending: false }),
  set_app_lock: async () => true,
  unlock_app: async () => true,
  save_draft: async input => fixtureSave(input),
  discard_draft: async input => {
    const moved = fixtureMovedDraft(input.id);
    if (moved) throw fixtureMovedError(moved);
    const draft = fixtureDrafts.get(input.conversationId);
    if (!draft) return;
    if (draft.id !== input.id || draft.revision !== input.expectedRevision) throw fixtureError("stale-draft", "The draft changed elsewhere; reload before deleting it.");
    if (draft.recipientIds.length || fixtureSnapshot.conversations.some(conversation => conversation.id === draft.conversationId && conversation.messages.length)) throw fixtureError("draft-not-discardable", "Only an empty recipientless draft can be deleted.");
    fixtureDrafts.delete(input.conversationId);
  },
  send_draft: async () => ({ accepted: false, status: "failed-before-submit", reason: "Fixture gateway is offline; the draft was preserved." }),
  mark_seen: async () => undefined,
  copy_one_time_code: async (target: OneTimeCodeTarget) => {
    if (target.kind === "message") {
      const conversation = fixtureSnapshot.conversations.find(candidate => candidate.messages.some(message => message.id === target.messageId));
      if (conversation && !fixtureSeenMessageIds.has(target.messageId)) {
        fixtureSeenMessageIds.add(target.messageId);
        conversation.unread = Math.max(0, conversation.unread - 1);
      }
      return;
    }
    fixtureSnapshot.notifications.forEach(notification => {
      if (notification.target.sourceDeviceId === target.target.sourceDeviceId && notification.target.notificationKey === target.target.notificationKey && notification.target.lifetime === target.target.lifetime) notification.seen = true;
    });
  },
  mark_all_read: async () => {
    const unread = fixtureSnapshot.conversations.reduce((total, conversation) => total + conversation.unread, 0);
    if (unread) fixtureSnapshot.conversations.forEach(conversation => { conversation.unread = 0; });
    return String(unread);
  },
  set_conversation_pinned: async (conversationId, pinned) => {
    const conversation = fixtureSnapshot.conversations.find(item => item.id === conversationId);
    if (!conversation) throw fixtureError("not-found", "The requested stored item was not found.");
    conversation.pinned = pinned;
  },
  set_conversation_folder: async (conversationId, folder) => {
    const conversation = fixtureSnapshot.conversations.find(item => item.id === conversationId);
    if (!conversation) throw fixtureError("not-found", "The requested stored item was not found.");
    conversation.folder = folder;
  },
  request_sender_unblock: async conversationId => {
    const conversation = fixtureSnapshot.conversations.find(item => item.id === conversationId);
    if (!conversation) throw fixtureError("not-found", "The requested stored item was not found.");
    conversation.folder = "inbox";
    conversation.unblock = "pending";
  },
  set_conversation_tone: async (seed, paletteIndex) => {
    if (!seed || (paletteIndex !== null && (!Number.isInteger(paletteIndex) || paletteIndex < 0 || paletteIndex > 119))) {
      throw fixtureError("invalid-request", "The request is invalid.");
    }
    if (paletteIndex === null) fixtureToneOverrides.delete(seed);
    else fixtureToneOverrides.set(seed, paletteIndex);
    fixtureSnapshot.toneOverrides = Object.fromEntries(fixtureToneOverrides);
  },
  dismiss_notifications: async targets => {
    if (targets.length > 100) throw { code: "invalid-request", message: "too many notification targets" };
    return targets.map(target => {
      const item = fixtureSnapshot.notifications.find(notification => notification.target.sourceDeviceId === target.sourceDeviceId && notification.target.notificationKey === target.notificationKey && notification.target.lifetime === target.lifetime);
      if (!item) return { target, outcome: "gone" as const };
      if (fixtureSnapshot.appFilters.some(filter => filter.sourceDeviceId === item.target.sourceDeviceId && filter.packageName === item.packageName && filter.muted)) return { target, outcome: "muted" as const };
      if (!item.dismissible) return { target, outcome: "notDismissible" as const };
      if (item.dismissalPending) return { target, outcome: "alreadyRecorded" as const };
      item.dismissalPending = true;
      return { target, outcome: "recorded" as const };
    });
  },
  set_app_muted: async (sourceDeviceId, packageName, appName, muted) => { const index = fixtureSnapshot.appFilters.findIndex(filter => filter.sourceDeviceId === sourceDeviceId && filter.packageName === packageName); const filter = { sourceDeviceId, packageName, appName, muted }; if (index < 0) fixtureSnapshot.appFilters.push(filter); else fixtureSnapshot.appFilters[index] = filter; },
  mark_notifications_seen: async targets => { fixtureSnapshot.notifications.forEach(notification => { if (targets.some(target => target.sourceDeviceId === notification.target.sourceDeviceId && target.notificationKey === notification.target.notificationKey && target.lifetime === notification.target.lifetime)) notification.seen = true; }); },
  set_notification_preferences: async preferences => { fixtureSnapshot.notificationPreferences = preferences; },
  set_notification_context: async () => undefined,
  request_notification_permission: async () => ({ status: "granted", testSent: true, canOpenSettings: false }),
  open_notification_settings: async () => undefined,
  pick_attachments: async () => [{ id: `attachment-fixture-${Date.now()}`, name: "fixture-image.png", mediaType: "image/png", byteSize: 2400, state: "ready", previewUrl: "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==" }],
  prepare_pasted_attachments: async files => ({ views: files.map((file, index) => ({ id: `attachment-fixture-pasted-${Date.now()}-${index}`, name: file.name, mediaType: file.mediaType, byteSize: file.bytes.byteLength, state: "ready" as const, draftState: "local" as const, previewUrl: "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==" })), failures: [] }),
  resolve_draft_conflict: async () => { throw fixtureError("not-available", "Draft conflict resolution is not available yet."); },
  acknowledge_draft_notice: async () => { throw fixtureError("not-available", "Draft notices are not available yet."); },
  retry_attachment: async () => undefined,
  retry_snapshot_import: async () => undefined,
  retryRcsMediaDownload: async () => undefined,
  save_attachment: async () => false,
  request_attachment_download: async () => undefined,
  media_sync_request: async attachmentIds => {
    if (attachmentIds.length < 1 || attachmentIds.length > 32) throw fixtureError("invalid-request", "Choose between one and 32 attachments to sync.");
    const attachments = fixtureSnapshot.conversations.flatMap(conversation => conversation.messages).flatMap(message => message.attachments);
    if (attachmentIds.some(id => !attachments.some(attachment => attachment.id === id))) throw fixtureError("not-found", "The requested stored item was not found.");
    attachments.forEach(attachment => { if (attachmentIds.includes(attachment.id) && attachment.mediaHold === "held") attachment.mediaHold = "requested"; });
  },
  media_page: async (conversationId, cursor) => {
    const conversation = fixtureSnapshot.conversations.find(item => item.id === conversationId);
    if (!conversation || cursor) return { items: [] };
    return { items: conversation.messages.flatMap((message) => message.attachments.map((attachment, attachmentPosition) => ({ messageId: message.id, senderAddress: message.senderAddress, timestamp: message.timestamp, attachment, cursor: { sortAtMs: message.timestamp, localOrder: message.revision, messageId: message.id, attachmentPosition } }))).reverse() };
  },
  media_page_around: async (conversationId, attachmentId, messageId) => {
    const conversation = fixtureSnapshot.conversations.find(item => item.id === conversationId);
    const target = conversation?.messages.flatMap(message => message.attachments.map((attachment, attachmentPosition) => ({ messageId: message.id, senderAddress: message.senderAddress, timestamp: message.timestamp, attachment, cursor: { sortAtMs: message.timestamp, localOrder: message.revision, messageId: message.id, attachmentPosition } }))).find(item => item.attachment.id === attachmentId && (!messageId || item.messageId === messageId));
    if (!target) throw fixtureError("not-found", "The media item was not found.");
    return { target, newer: [], older: [] };
  },
  media_page_newer: async () => ({ items: [] }),
  open_media: async id => {
    const attachment = fixtureSnapshot.conversations.flatMap(conversation => conversation.messages).flatMap(message => message.attachments).find(item => item.id === id);
    return attachment?.previewUrl ? { state: "ready" as const, url: attachment.previewUrl, mediaType: attachment.mediaType, byteSize: attachment.byteSize } : { state: "pending" as const };
  },
  close_media: async () => undefined,
  load_media: async id => fixtureBridge.open_media(id),
  publish_attachment: async () => null,
  open_composer: async () => undefined,
  set_start_at_login: async enabled => { fixtureSnapshot.desktop = { trayAvailable: true, startAtLogin: enabled, startupSupported: true, background: false }; },
  startup_status: async () => ({ state: fixtureSnapshot.desktop?.startAtLogin ? "on" : "off", background: fixtureSnapshot.desktop?.background ?? false }),
  set_open_in_background: async enabled => { fixtureSnapshot.desktop = { trayAvailable: true, startAtLogin: fixtureSnapshot.desktop?.startAtLogin ?? false, startupSupported: true, background: enabled }; },
  open_login_items: async () => undefined,
  get_notification_sound: async () => fixtureNotificationSound,
  set_notification_sound: async enabled => { fixtureNotificationSound = enabled; },
  get_notification_persistence: async () => ({ ...fixtureNotificationPersistence, supported: true, platform: "linux" }),
  set_notification_persistence: async value => { fixtureNotificationPersistence = value; },
  get_notification_quick_reply: async () => ({ enabled: fixtureNotificationQuickReply, supported: true }),
  set_notification_quick_reply: async enabled => { fixtureNotificationQuickReply = enabled; },
  notification_reply_recoveries: async () => ({ sessionGeneration: "fixture", items: [] }),
  claim_notification_reply_restore: async () => false,
  release_notification_reply_restore: async () => undefined,
  ack_notification_reply_restore: async () => undefined,
  discard_notification_reply: async () => undefined,
  popout_conversation: async conversationId => {
    if (!fixtureConversationIds().includes(conversationId)) throw fixtureError("not-found", "The requested conversation was not found.");
    fixtureSnapshot.head = { ...fixtureSnapshot.head, enabled: true, capability: "unconfirmed", pinnedConversationIds: [...new Set([...(fixtureSnapshot.head.pinnedConversationIds ?? []), conversationId])] };
    return { headCreated: true, warning: "Simulated fixture only; native floating input is not available." };
  },
  hide_head: async conversationId => { fixtureSnapshot.head = { ...fixtureSnapshot.head, pinnedConversationIds: fixtureSnapshot.head.pinnedConversationIds?.filter(id => id !== conversationId) }; }, close_composer: async () => undefined, close_head_panel: async () => undefined, set_maximize_button_region: async () => undefined,
  subscribe: listener => {
    fixtureChangeListeners.add(listener);
    return () => { fixtureChangeListeners.delete(listener); };
  },
  subscribe_maximize_button_hover: () => () => undefined,
  subscribe_drafts: () => () => undefined,
  send_typing: async () => undefined,
  set_typing_sharing: async () => undefined,
  get_diagnostics_state: async () => readFixtureDiagnostics(),
  set_diagnostics_enabled: async ({ enabled }) => {
    fixtureDiagnostics = { ...fixtureDiagnostics, enabled };
    return { ...fixtureDiagnostics };
  },
  export_diagnostics_log: async (): Promise<DiagnosticsExportResult> => ({ outcome: fixtureDiagnostics.retainedBytes > 0 ? "saved" : "cancelled" }),
  clear_diagnostics_log: async () => {
    fixtureDiagnostics = { ...fixtureDiagnostics, retainedBytes: 0, storageError: false };
    return { ...fixtureDiagnostics };
  },
  subscribe_typing: () => () => undefined,
  subscribe_lifecycle: () => () => undefined,
  subscribe_lifecycle_finished: () => () => undefined,
  acknowledge_lifecycle: async () => undefined,
  window: async () => undefined,
  list_devices: async () => ({ currentDeviceId: "device-desktop", devices: fixtureDevices.map(device => ({ ...device })) }),
  rename_device: async (deviceId, name) => {
    const device = fixtureDevices.find(item => item.id === deviceId);
    if (!device) throw fixtureError("device_not_found", "This device is no longer enrolled.");
    if (device.revoked) throw fixtureError("device_already_revoked", "This device was already removed.");
    device.name = name.trim();
  },
  revoke_device: async deviceId => {
    const device = fixtureDevices.find(item => item.id === deviceId);
    if (!device) throw fixtureError("device_not_found", "This device is no longer enrolled.");
    if (device.revoked) throw fixtureError("device_already_revoked", "This device was already removed.");
    if (device.id === "device-desktop") throw fixtureError("current_device", "This device cannot remove itself here.");
    device.revoked = true;
    if (device.id.startsWith("fixture-mcp-device-")) { fixtureMcpEnabled = false; fixtureMcpGrants.splice(0); }
  },
  mcp_status: async () => ({ vaultId: "11111111-1111-4111-8111-111111111111", available: true, serverOrigin: "https://push.example.com", hosted: true, device: fixtureMcpEnabled ? { deviceId: `fixture-mcp-device-${fixtureMcpGeneration}`, name: "Peppy MCP", state: "online" as const, lastSyncedAt: "Fixture", cleanupPending: false } : null }),
  mcp_enable: async () => { fixtureMcpEnabled = true; fixtureMcpGeneration += 1; fixtureDevices.push({ id: `fixture-mcp-device-${fixtureMcpGeneration}`, name: "Peppy MCP", kind: "desktop", role: "device", revoked: false }); return fixtureBridge.mcp_status(); },
  mcp_grants: async () => fixtureMcpGrants.map(grant => ({ ...grant })),
  mcp_revoke_grant: async id => { const index = fixtureMcpGrants.findIndex(grant => grant.id === id); if (index >= 0) fixtureMcpGrants.splice(index, 1); },
  webhooks_list: async (): Promise<WebhookList> => ({ available: true, webhooks: fixtureWebhooks.map(webhook => ({ ...webhook, filter: webhook.filter.map(entry => ({ ...entry })) })) }),
  webhook_create: async (input): Promise<WebhookCreated> => { const id = `00000000-0000-4000-8000-${String(++fixtureWebhookSequence).padStart(12, "0")}`; const webhook = fixtureWebhook(input, id); const secret = `whsec_fixture_${id.replaceAll("-", "")}`; fixtureWebhooks.push(webhook); fixtureWebhookSecrets.set(id, secret); return { webhook: { ...webhook }, secret }; },
  webhook_update: async (id, input) => { const index = fixtureWebhooks.findIndex(webhook => webhook.id === id); if (index < 0) throw fixtureError("webhook_not_found", "Webhook not found."); const current = fixtureWebhooks[index]; const webhook = { ...fixtureWebhook(input, id), createdAt: current.createdAt, lastDeliveryState: current.lastDeliveryState, lastDeliveryAt: current.lastDeliveryAt, lastStatusCode: current.lastStatusCode, lastError: current.lastError, pendingDeliveries: current.pendingDeliveries }; fixtureWebhooks[index] = webhook; return { ...webhook }; },
  webhook_delete: async id => { const index = fixtureWebhooks.findIndex(webhook => webhook.id === id); if (index < 0) throw fixtureError("webhook_not_found", "Webhook not found."); fixtureWebhooks.splice(index, 1); fixtureWebhookSecrets.delete(id); },
  webhook_reveal_secret: async id => { const secret = fixtureWebhookSecrets.get(id); if (!secret) throw fixtureError("webhook_not_found", "Webhook not found."); return { id, secret }; },
  webhook_rotate_secret: async id => { if (!fixtureWebhooks.some((webhook) => webhook.id === id)) throw fixtureError("webhook_not_found", "Webhook not found."); const secret = `whsec_fixture_${id.replaceAll("-", "")}_${++fixtureWebhookSequence}`; fixtureWebhookSecrets.set(id, secret); return { id, secret }; },
  webhook_test: async (): Promise<WebhookTestResult> => ({ ok: true, statusCode: 200, durationMs: 142, error: null }),
  mcp_authorization: async () => { throw fixtureError("unsupported", "MCP authorization is unavailable."); },
  mcp_approve_authorization: async () => { throw fixtureError("unsupported", "MCP authorization is unavailable."); },
  mcp_deny_authorization: async () => { throw fixtureError("unsupported", "MCP authorization is unavailable."); },
  list_contact_books: async () => fixtureContactBooks.map(book => ({ ...book, lookupExcluded: fixtureLookupExcludedBookIds.has(book.id) })),
  forget_contact_book: async bookId => {
    const index = fixtureContactBooks.findIndex(book => book.id === bookId);
    if (index < 0) throw fixtureError("not-found", "Contact book not found.");
    dropFixtureContactBook(index);
  },
  delete_contact_book: async bookId => {
    const index = fixtureContactBooks.findIndex(book => book.id === bookId);
    if (fixtureContactDeleteGate === "offline") throw fixtureError("offline", "Connect to the server to delete this book for all devices.");
    if (fixtureContactDeleteGate === "sync-not-current") throw fixtureError("sync-not-current", "Finish syncing to delete this book for all devices.");
    if (index < 0 || !fixtureContactBooks[index].sourceRevoked) throw fixtureError("contact-book-not-deletable", "This contact book can't be deleted for all devices.");
    const book = dropFixtureContactBook(index);
    return { bookId, contacts: book.contactCount };
  },
  set_contact_book_lookup: async (bookId, enabled) => {
    if (!fixtureContactBooks.some(book => book.id === bookId)) throw fixtureError("not-found", "Contact book not found.");
    if (enabled) fixtureLookupExcludedBookIds.delete(bookId);
    else fixtureLookupExcludedBookIds.add(bookId);
  },
  list_contact_name_candidates: async address => {
    const candidates = fixtureNameCandidatesFor(address);
    const pin = fixtureNamePins.get(address);
    const pinned = pin && candidates.some(candidate => candidate.bookId === pin.bookId && candidate.contactId === pin.contactId) ? pin : null;
    return { candidates, pinned, moreIndexing: false };
  },
  set_contact_name_pin: async (address, bookId, contactId) => {
    if (!fixtureNameCandidatesFor(address).some(candidate => candidate.bookId === bookId && candidate.contactId === contactId)) throw fixtureError("not-found", "Contact not found.");
    fixtureNamePins.set(address, { bookId, contactId });
  },
  clear_contact_name_pin: async address => { fixtureNamePins.delete(address); },
  list_contacts: async (bookId, query = "", offset = 0, sort = { key: "family" as const, direction: "asc" as const }) => {
    await fixtureContactLatency();
    return sortedFixtureContacts(bookId, query, sort).slice(offset, offset + FIXTURE_CONTACT_PAGE_SIZE);
  },
  list_contacts_page: async (bookId, request) => {
    await fixtureContactLatency();
    const sorted = sortedFixtureContacts(bookId, request.query?.trim() ?? "", request.sort ?? { key: "family", direction: "asc" });
    const start = decodeFixtureContactCursor(request.cursor);
    // Mirrors core: `through` extends the page to include the target, at most 2,000 rows from the start.
    const throughIndex = request.through === undefined ? -1 : sorted.findIndex(contact => contact.id === request.through);
    const found = throughIndex >= start && throughIndex - start < FIXTURE_CONTACT_THROUGH_MAX;
    const count = found ? Math.max(FIXTURE_CONTACT_PAGE_SIZE, throughIndex - start + 1) : FIXTURE_CONTACT_PAGE_SIZE;
    const pageEnd = Math.min(sorted.length, start + count);
    return {
      contacts: sorted.slice(start, pageEnd),
      total: sorted.length,
      ...(pageEnd < sorted.length ? { nextCursor: encodeFixtureContactCursor(pageEnd) } : {}),
      ...(request.through === undefined ? {} : { throughFound: found }),
    };
  },
  submit_contact_edit: async () => ({ state: "pending", requestId: `fixture-${Date.now()}` }),
  list_contact_edits: async bookId => [{ requestId: "edit-aurora", bookId: bookId ?? "book-pixel8", state: "pending" }],
  search_contact_recipients: async query => {
    const needle = query.trim().toLowerCase();
    if (!needle) return [];
    const people = [
      { contactId: "contact-aurora", name: "Aurora Chen", phones: [{ id: "p-aurora", label: "mobile", number: "+12025550123" }] },
      { contactId: "contact-sol", name: "Sol Rivera", phones: [{ id: "p-sol-1", label: "mobile", number: "+12025550160" }, { id: "p-sol-2", label: "work", number: "+12025550161" }] },
    ];
    const digits = needle.replace(/\D/g, "");
    return people.flatMap(person => person.phones
      .filter(phone => person.name.toLowerCase().includes(needle) || (digits.length >= 2 && phone.number.includes(digits)))
      .map(phone => ({ address: phone.number, bookId: "book-pixel8", displayName: person.name, number: phone.number, label: phone.label, normalized: true, contactId: person.contactId, phoneId: phone.id })));
  },
  request_contact_repair: async () => ({ repairRequired: true }),
  request_contact_sync: async bookId => {
    const status: ContactScanStatus = { requestId: `scan-${bookId}`, bookId, state: "waiting", expiresAt: Math.floor(Date.now() / 1000) + 604800 };
    fixtureScanStatuses.set(bookId, status);
    return status;
  },
  get_contact_sync_request: async bookId => fixtureScanStatuses.get(bookId) ?? null,
  pick_contact_photo: async () => ({ dataUrl: "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==", naturalWidth: 1, naturalHeight: 1 }),
  list_restorable_contacts: async bookId => [{ id: "contact-deleted", bookId, displayName: "Casey Rowan", deletedAt: "Yesterday" }],
  restore_contact: async () => ({ state: "pending", requestId: `fixture-restore-${Date.now()}` }),
  hosted_account: async () => fixtureAccount(),
  attachment_retention_status: async () => ({ kind: "unsupported" }),
  account_access: async () => ({ kind: "unsupported" }),
  attachment_retention_state: async () => ({ remote: "available", localAvailable: false }),
  hosted_origin_url: async () => "https://peppy.pro",
  hosted_sign_in: async () => ({ ...fixtureAccount(), signedIn: true, accountLabel: "Fixture account", classification: "new" }),
  hosted_sign_out: async () => undefined,
  hosted_delete_account: async () => "deleted",
  hosted_deletion_status: async () => ({ executeAfter: null, cancellable: false }),
  hosted_cancel_account_deletion: async () => undefined,
  hosted_open_billing: async () => undefined,
  hosted_provision: async () => undefined,
  billing_summary: async () => ({ provider: "none", manageableIn: "website", planActive: false, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false }),
  purchase_options: async () => ({ appStore: false, website: true }),
  store_products: async () => [
    { id: "org.peppy.desktop.plan.monthly", kind: "plan", displayName: "Peppy Hosted Monthly", displayPrice: "$4.99", period: "month" },
    { id: "org.peppy.desktop.storage.packs.1", kind: "storage", displayName: "Extra storage", displayPrice: "$0.99", period: "month", storageBytes: 5 * 1024 * 1024 * 1024 },
  ],
  store_purchase: async () => "purchased",
  store_restore: async () => undefined,
  open_store_subscriptions: async () => undefined,
  open_play_subscriptions: async () => undefined,
  enrollment_discover: async origin => ({ mode: "community", stage: "server", origin }),
  enrollment_start_over: async () => ({ mode: "community", stage: "setup" }),
  enrollment_create_owner: async () => ({ mode: "community", stage: "ready", owner: true }),
  enrollment_sign_in: async () => ({ mode: "community", stage: "locked" }),
  enrollment_connect_shared_secret: async () => ({ mode: "community", stage: "waiting_phone" }),
  enrollment_begin_vault_request: async input => ({ mode: "community", stage: "invite_waiting", displayName: input.displayName }),
  enrollment_vault_request_status: async () => ({ mode: "community", stage: "invite_waiting" }),
  enrollment_create_vault: async () => ({ mode: "community", stage: "ready", owner: true }),
  enrollment_add_phone: async () => ({ mode: "community", stage: "waiting_phone" }),
  enrollment_rotate_phone: async () => ({ mode: "community", stage: "waiting_phone" }),
  enrollment_cancel_phone: async () => ({ mode: "community", stage: "ready", owner: true }),
  reveal_vault_secret: async () => undefined,
  enrollment_status: async () => fixturePhoneApproved
    ? ({ mode: "community", stage: "phone_connected", owner: true })
    : ({ mode: "community", stage: "server" }),
  enrollment_approve: async () => {
    fixturePhoneApproved = true;
    return { mode: "community", stage: "phone_approved", owner: true };
  },
  enrollment_cancel: async () => undefined,
  enrollment_sign_out: async () => undefined,
  enrollment_connect_computer: async () => ({ stage: "computer_waiting", qrPayload: "fixture-computer-qr", awaitingOwner: true, computerClaimant: true }),
  enrollment_computer_status: async () => ({ stage: "computer_waiting", qrPayload: "fixture-computer-qr", awaitingOwner: true, computerClaimant: true }),
  enrollment_confirm_computer: async () => ({ stage: "computer_finishing", awaitingOwner: false, computerClaimant: true }),
  enrollment_cancel_computer: async () => undefined,


};
