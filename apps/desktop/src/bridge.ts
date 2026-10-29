/// <reference types="vite/client" />

/**
 * UI-only native boundary. These DTOs intentionally contain no credentials,
 * cryptographic material, device tokens, or raw filesystem paths.
 */
import type {
  ConnectionState,
  MessageStatus,
  AttachmentState,
  AttachmentView,
  MessageView,
  ConversationView,
  HistoryPage,
  GatewayView,
  Draft,
  DesktopSnapshot,
  DraftInput,
  DiscardDraftInput,
  SendDraftInput,
  PublicCopy,
  NotificationTarget,
  MirroredNotification,
  AppFilter,
  NotificationPreferences,
  NotificationCheck,
  NotificationPermissionStatus,
  ContactBookState,
  BookCapabilities,
  ContactBookView,
  ContactPhone,
  ContactEmail,
  ContactAddress,
  ContactBirthday,
  ContactSort,
  ContactView,
  ContactEditRequestInput,
  ContactEditOutcome,
  ContactEditStatus,
  ContactSyncStatus,
  RecipientSuggestion,
  RestorableContact,
  ResolvedContact,
  ContactResolutionMap,
  BridgeError,
  HostedAccountView,
  EnrollmentView as SharedEnrollmentView,
  ComputerClaimantView,
  HostedPreviewView,
  AppLockStatus,
  MaximizeButtonRegion,
  ManagedDeviceView,
  DeviceRosterView,
  ContactScanStatus,
  MediaCursor,
  MediaMetadataView,
  MediaPageItem,
  MediaPage,
  MediaOpen,
  LocalSearchRequest,
  LocalSearchPage,
  SearchIndexProgress,
  HistorySearchAttemptInput,
  HistorySearchSnapshot,
  StartHistorySearchInput,
  MessageWindowRequest,
  MessageWindow,
  DesktopBridge as SharedDesktopBridge,
} from "@peppy/desktop-ui/bridge-contracts";
import { normalizePhoneNumber } from "@peppy/desktop-ui";

export {
  type ConnectionState,
  type MessageStatus,
  type AttachmentState,
  type AttachmentView,
  type MessageView,
  type ConversationView,
  type HistoryPage,
  type GatewayView,
  type Draft,
  type DesktopSnapshot,
  type DraftInput,
  type DiscardDraftInput,
  type SendDraftInput,
  type PublicCopy,
  type NotificationTarget,
  type MirroredNotification,
  type AppFilter,
  type NotificationPreferences,
  type NotificationCheck,
  type NotificationPermissionStatus,
  type ContactBookState,
  type BookCapabilities,
  type ContactBookView,
  type ContactPhone,
  type ContactEmail,
  type ContactAddress,
  type ContactBirthday,
  type ContactSort,
  type ContactView,
  type ContactEditRequestInput,
  type ContactEditOutcome,
  type ContactEditStatus,
  type ContactSyncStatus,
  type RecipientSuggestion,
  type RestorableContact,
  type ResolvedContact,
  type ContactResolutionMap,
  type BridgeError,
  type HostedAccountView,
  type ComputerClaimantView,
  type HostedPreviewView,
  type AppLockStatus,
  type MaximizeButtonRegion,
  type ManagedDeviceView,
  type DeviceRosterView,
  type ContactScanStatus,
  type MediaCursor,
  type MediaMetadataView,
  type MediaPageItem,
  type MediaPage,
  type MediaOpen,
  type LocalSearchRequest,
  type LocalSearchPage,
  type SearchIndexProgress,
  type HistorySearchAttemptInput,
  type HistorySearchSnapshot,
  type StartHistorySearchInput,
  type MessageWindowRequest,
  type MessageWindow,
} from "@peppy/desktop-ui/bridge-contracts";

export type EnrollmentView = SharedEnrollmentView & { computerClaimant?: ComputerClaimantView };
export type DesktopBridge = Omit<SharedDesktopBridge, "enrollment_discover"> & { enrollment_discover(origin?: string): Promise<EnrollmentView> };

const fixtureMessages: MessageView[] = [
  { id: "message-aurora-1", revision: "1", sender: "other", body: "Can you send over the estimate?", timestamp: "1788434460000", attachments: [] },
  { id: "message-aurora-2", revision: "2", sender: "self", body: "I'll have it to you shortly.", timestamp: "1788434580000", status: "sent", statusAt: "1788434640000", attachments: [{ id: "attachment-aurora-1", name: "fixture-photo.png", mediaType: "image/png", byteSize: 67, state: "ready", previewUrl: "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==" }] },
];
const fixtureReactionMessages: MessageView[] = [
  { id: "message-sam-1", revision: "1", sender: "self", body: "Did they discuss the NICU at all in the group conversation that I had muted?? but yes, people suck. “Jerk chicken” are you trying to send a message? 😘", timestamp: "1788434700000", attachments: [] },
  { id: "message-sam-2", revision: "1", sender: "other", senderAddress: "+15555550142", body: "lol it autocorrected", timestamp: "1788434760000", attachments: [] },
  { id: "message-sam-3", revision: "1", sender: "other", senderAddress: "+15555550142", body: "Reacted 😆 to “Did they discuss the NICU at all in the group conversation that I had muted?? but yes, people suck. “Jerk chicken” are you trying to send a message? 😘”", timestamp: "1788434820000", attachments: [] },
  { id: "message-sam-4", revision: "1", sender: "self", body: "Loved “lol it autocorrected”", timestamp: "1788434880000", attachments: [] },
  { id: "message-sam-5", revision: "1", sender: "other", senderAddress: "+15555550142", body: "Reacted 👍 to “a message from last year that is not loaded…”", timestamp: "1788434940000", attachments: [] },
];
const fixtureChatNotificationIcon = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAaUlEQVR42mPQrPz/fyAxw6gDkDmXXiAwLg3UUoMzBOjtCAZaWkCMGgZaW0BIDd5ESA9HEMwFtHYEUdmQ2o7Amw3p4QiC5QC9yoDRgmi0IBotiEYLotGCaLQgGi2IRguiwVcQjfYNBwIDAAwPd9Z4B+yXAAAAAElFTkSuQmCC";
const fixtureMailNotificationIcon = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAaUlEQVR42mP4nx3+fyAxw6gDUAQe3kJgXJqopQZnCNDZEQw0tYAINQw09yUBNfgTIR0cQTgX0NgRxGVDajsCbzakhyMIlgN0KgNGC6LRgmi0IBotiEYLotGCaLQgGi2IBmFBNNo3HAAMAKbz3HgryQcuAAAAAElFTkSuQmCC";
const fixtureSnapshot: DesktopSnapshot = { version: "1", mode: "fixture", connection: { state: "connected", origin: "https://push.example.com" }, encryption: { state: "unlocked" }, gateways: [{ id: "gateway-pixel8", name: "Pixel 8", simId: "sim-1", online: true, simulated: true, supportsSms: true, supportsMms: true, mmsContentVersion: 2, mmsMaxBytes: 300 * 1024, mmsLimitSource: "fallback", mmsMaxRecipients: 20 }], conversations: [{ id: "conv-aurora", name: "Aurora Chen", preview: "Can you send over the estimate?", unread: 2, messages: fixtureMessages }, { id: "conv-river", name: "River Park", preview: "Attachment received", unread: 0, participants: ["River Park", "Mina Torres"], messages: [{ id: "message-river-1", revision: "1", sender: "other", body: "Attachment received", timestamp: "1788348060000", transport: "mms", subject: "Estimate", attachments: [{ id: "attachment-river-1", name: "estimate.pdf", mediaType: "application/pdf", byteSize: 182000, state: "ready", transfer: "download", retryable: true }] }] }, { id: "conv-short-code", name: "5556", preview: "Short code", unread: 0, messages: [] }, { id: "conv-unresolved-number", name: "+18015551234", preview: "Unresolved number", unread: 0, participants: ["+18015551234"], messages: [] }, { id: "conv-group-numbers", name: "+18015551234, +15555550199", preview: "Group conversation", unread: 0, participants: ["+18015551234", "+15555550199"], messages: [] }], activeConversationId: "conv-aurora", draft: undefined, head: { enabled: true, capability: "supported" }, deviceRole: "owner", pendingCount: 1, quarantineCount: 0, notifications: [{ target: { sourceDeviceId: "gateway-pixel8", notificationKey: "chat-42", lifetime: "1" }, packageName: "com.example.chat", appName: "Chat", appIconPngBase64: fixtureChatNotificationIcon, title: "Morgan", text: "Are we still meeting after lunch?", postedAt: Date.now() - 120000, dismissible: true, seen: false, dismissalPending: false }, { target: { sourceDeviceId: "gateway-pixel8", notificationKey: "mail-7", lifetime: "1" }, packageName: "com.example.mail", appName: "Mail", appIconPngBase64: fixtureMailNotificationIcon, title: "Project update", text: "A detailed update is ready for your review.", postedAt: Date.now() - 3600000, dismissible: true, seen: false, dismissalPending: false }], appFilters: [], notificationPreferences: { messageBanners: true, mirroredBanners: true, preview: "full", muteWithHeads: true } };
const fixtureDismissibleNotifications = () => fixtureSnapshot.notifications
  .filter(notification => notification.dismissible && !notification.dismissalPending)
  .filter(notification => !fixtureSnapshot.appFilters.some(filter => filter.muted && filter.sourceDeviceId === notification.target.sourceDeviceId && filter.packageName === notification.packageName));
const fixtureDevices: ManagedDeviceView[] = [
  { id: "device-desktop", name: "This Mac", kind: "desktop", role: "owner", revoked: false },
  { id: "device-pixel8", name: "Pixel 8", kind: "android", role: "gateway", revoked: false },
  { id: "device-browser", name: "Work browser", kind: "browser", role: "device", revoked: false },
  { id: "device-old-iphone", name: "Old iPhone", kind: "ios", role: "owner", revoked: true },
];
const fixtureScanStatuses = new Map<string, ContactScanStatus>();
// Exercise the same display-only contact resolution path as the native snapshot.
fixtureSnapshot.conversations[0].name = "+12025550123";
fixtureSnapshot.conversations[0].participants = ["+12025550123"];
fixtureSnapshot.contactResolution = {
  "+12025550123": { contactId: "contact-aurora", bookId: "book-pixel8", displayName: "Aurora Chen" },
};
fixtureSnapshot.contactsPendingCount = 1;
fixtureSnapshot.conversations.push({ id: "conv-sam", name: "Sam Rivera", preview: "Reacted 👍 to “a message from last year that is not loaded…”", unread: 0, participants: ["+15555550142"], messages: fixtureReactionMessages });

/** Fixture drafts keyed by conversation ID; mirrors the native compose-draft save contract (session.rs). */
const fixtureDrafts = new Map<string, Draft>();
const fixtureToneOverrides = new Map<string, number>();
let fixtureCreated = 0;
let fixturePhoneApproved = false;
const fixtureError = (code: string, message: string): BridgeError => ({ code, message });
const fixtureSearchContacts = [
  { id: "contact-aurora", bookId: "book-pixel8", title: "Aurora Chen", subtitle: "+12025550123" },
  { id: "contact-river", bookId: "book-pixel8", title: "River Park", subtitle: "+12025550124" },
  { id: "contact-mina", bookId: "book-pixel8", title: "Mina Torres", subtitle: "+12025550125" },
];
const fixtureSearch = (input: LocalSearchRequest): LocalSearchPage => {
  const query = input.query.trim().toLocaleLowerCase();
  if (!query) return { query: input.query, items: [], indexState: "ready", hasMore: { conversation: false, message: false, contact: false } };
  const limit = Math.min(input.limit ?? 6, 50);
  const accepts = (kind: "conversation" | "message" | "contact") => !input.kind || input.kind === "all" || input.kind === kind;
  const conversations = fixtureSnapshot.conversations.filter(item => `${item.name} ${item.participants?.join(" ") ?? ""}`.toLocaleLowerCase().includes(query)).map(item => ({ kind: "conversation" as const, conversationId: item.id, title: item.name, subtitle: item.preview })).slice(0, limit);
  const messages = fixtureSnapshot.conversations.flatMap(conversation => conversation.messages.filter(message => `${message.body} ${message.subject ?? ""}`.toLocaleLowerCase().includes(query)).map(message => ({ kind: "message" as const, conversationId: conversation.id, messageId: message.id, title: conversation.name, snippet: message.body, timestamp: message.timestamp }))).slice(0, limit);
  const contacts = fixtureSearchContacts.filter(contact => `${contact.title} ${contact.subtitle}`.toLocaleLowerCase().includes(query)).map(contact => ({ kind: "contact" as const, bookId: contact.bookId, contactId: contact.id, title: contact.title, subtitle: contact.subtitle })).slice(0, limit);
  const items = [...(accepts("conversation") ? conversations : []), ...(accepts("message") ? messages : []), ...(accepts("contact") ? contacts : [])];
  return { query: input.query, items, indexState: "ready", hasMore: { conversation: false, message: false, contact: false } };
};
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
const fixtureAccount = (): HostedAccountView => ({ available: true, signedIn: false, accountLabel: null, classification: null, entitlement: null, access: null, hasVault: false, resumable: false });
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
  .map(draft => ({ id: draft.conversationId, name: draft.recipientIds.join(", ") || "New message", preview: draft.text ? `Draft: ${draft.text.slice(0, 80)}` : "Draft", unread: 0, messages: [], draftOnly: true, recipientlessDraft: draft.recipientIds.length === 0 }));
/**
 * Empty draft ID: create the draft (or attach to the conversation's existing one); an empty
 * conversation ID also creates a new conversation. Saves are CAS on a numeric revision.
 */
const fixtureSave = (input: DraftInput): Draft => {
  const expected = Number(input.expectedRevision);
  if (!/^\d+$/.test(input.expectedRevision)) throw fixtureError("invalid-draft", "The draft revision is invalid.");
  const recipientIds = canonicalRecipients(input.recipientIds);
  const source = input.id ? [...fixtureDrafts.values()].find((draft) => draft.id === input.id) : undefined;
  if (input.id && !source) throw fixtureError("not-found", "The requested stored item was not found.");
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
  if (source && source.conversationId !== targetConversationId) fixtureDrafts.delete(source.conversationId);
  fixtureDrafts.set(saved.conversationId, saved);
  return saved;
};
let fixtureNotificationSound = false;
export const fixtureBridge: DesktopBridge = {
  load_state: async (conversationId, historyFromMessageId, options) => {
    const active = conversationId && fixtureConversationIds().includes(conversationId) ? conversationId : fixtureSnapshot.activeConversationId!;
    const conversations = fixtureSnapshot.conversations.map(conversation => {
      if (conversation.id !== active || options?.metadataOnly) return { ...conversation, messages: [] };
      const anchorIndex = historyFromMessageId === undefined ? -1 : conversation.messages.findIndex(message => message.id === historyFromMessageId);
      if (historyFromMessageId !== undefined && anchorIndex < 0) throw fixtureError("not-found", "The requested stored item was not found.");
      const messages = historyFromMessageId === undefined ? conversation.messages.slice(-50) : conversation.messages.slice(anchorIndex);
      return { ...conversation, messages, hasOlderMessages: historyFromMessageId === undefined ? conversation.messages.length > messages.length : anchorIndex > 0 };
    });
    return { ...fixtureSnapshot, conversations: [...conversations, ...fixtureDraftConversations()], activeConversationId: active, draft: fixtureDrafts.get(active) };
  },
  search_local: async input => fixtureSearch(input),
  advance_search_index: async (): Promise<SearchIndexProgress> => ({ indexState: "ready", processed: 0, remaining: 0 }),
  // No private history source exists in fixtures: report unsupported, never fake activity.
  get_history_search: async (): Promise<HistorySearchSnapshot> => ({ state: "idle", canSearchFurther: false, progressVersion: "0", indexVersion: "0" }),
  start_history_search: async (): Promise<HistorySearchSnapshot> => ({ state: "blocked", reason: "unsupported", canSearchFurther: false, progressVersion: "0", indexVersion: "0" }),
  cancel_history_search: async (): Promise<HistorySearchSnapshot> => ({ state: "cancelled", canSearchFurther: false, progressVersion: "0", indexVersion: "0" }),
  load_message_window: async input => fixtureMessageWindow(input),
  open_external_url: async url => { if (!/^https?:\/\//.test(url)) throw fixtureError("invalid-request", "The URL is invalid."); },
  load_history: async (conversationId, beforeMessageId) => {
    const conversation = fixtureSnapshot.conversations.find(item => item.id === conversationId);
    const anchorIndex = conversation?.messages.findIndex(message => message.id === beforeMessageId) ?? -1;
    if (!conversation || anchorIndex < 0) throw fixtureError("not-found", "The requested stored item was not found.");
    const start = Math.max(0, anchorIndex - 50);
    return { conversationId, messages: conversation.messages.slice(start, anchorIndex), hasMore: start > 0 };
  },
  configure_server: async () => undefined,
  unlock_sync: async () => undefined,
  get_app_lock_status: async () => ({ enabled: false, locked: false, storageAvailable: true }),
  set_app_lock: async () => true,
  unlock_app: async () => true,
  save_draft: async input => fixtureSave(input),
  discard_draft: async input => {
    const draft = fixtureDrafts.get(input.conversationId);
    if (!draft) return;
    if (draft.id !== input.id || draft.revision !== input.expectedRevision) throw fixtureError("stale-draft", "The draft changed elsewhere; reload before deleting it.");
    if (draft.recipientIds.length || fixtureSnapshot.conversations.some(conversation => conversation.id === draft.conversationId && conversation.messages.length)) throw fixtureError("draft-not-discardable", "Only an empty recipientless draft can be deleted.");
    fixtureDrafts.delete(input.conversationId);
  },
  send_draft: async () => ({ accepted: false, status: "failed-before-submit", reason: "Fixture gateway is offline; the draft was preserved." }),
  mark_seen: async () => undefined,
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
  set_conversation_tone: async (seed, paletteIndex) => {
    if (!seed || (paletteIndex !== null && (!Number.isInteger(paletteIndex) || paletteIndex < 0 || paletteIndex > 119))) {
      throw fixtureError("invalid-request", "The request is invalid.");
    }
    if (paletteIndex === null) fixtureToneOverrides.delete(seed);
    else fixtureToneOverrides.set(seed, paletteIndex);
    fixtureSnapshot.toneOverrides = Object.fromEntries(fixtureToneOverrides);
  },
  dismiss_notification: async target => { const item = fixtureSnapshot.notifications.find(notification => notification.target.sourceDeviceId === target.sourceDeviceId && notification.target.notificationKey === target.notificationKey && notification.target.lifetime === target.lifetime); if (item) item.dismissalPending = true; },
  dismiss_all_notifications: async () => { fixtureDismissibleNotifications().slice(0, 100).forEach(notification => { notification.dismissalPending = true; }); },
  set_app_muted: async (sourceDeviceId, packageName, appName, muted) => { const index = fixtureSnapshot.appFilters.findIndex(filter => filter.sourceDeviceId === sourceDeviceId && filter.packageName === packageName); const filter = { sourceDeviceId, packageName, appName, muted }; if (index < 0) fixtureSnapshot.appFilters.push(filter); else fixtureSnapshot.appFilters[index] = filter; },
  mark_notifications_seen: async targets => { fixtureSnapshot.notifications.forEach(notification => { if (targets.some(target => target.sourceDeviceId === notification.target.sourceDeviceId && target.notificationKey === notification.target.notificationKey && target.lifetime === notification.target.lifetime)) notification.seen = true; }); },
  set_notification_preferences: async preferences => { fixtureSnapshot.notificationPreferences = preferences; },
  set_notification_context: async () => undefined,
  request_notification_permission: async () => ({ status: "granted", testSent: true, canOpenSettings: false }),
  open_notification_settings: async () => undefined,
  pick_attachments: async () => [{ id: `attachment-fixture-${Date.now()}`, name: "fixture-image.png", mediaType: "image/png", byteSize: 2400, state: "ready", previewUrl: "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==" }],
  retry_attachment: async () => undefined,
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
  get_notification_sound: async () => fixtureNotificationSound,
  set_notification_sound: async enabled => { fixtureNotificationSound = enabled; },
  popout_conversation: async conversationId => {
    if (!fixtureConversationIds().includes(conversationId)) throw fixtureError("not-found", "The requested conversation was not found.");
    fixtureSnapshot.head = { ...fixtureSnapshot.head, enabled: true, capability: "unconfirmed", pinnedConversationIds: [...new Set([...(fixtureSnapshot.head.pinnedConversationIds ?? []), conversationId])] };
    return { headCreated: true, warning: "Simulated fixture only; native floating input is not available." };
  },
  hide_head: async conversationId => { fixtureSnapshot.head = { ...fixtureSnapshot.head, pinnedConversationIds: fixtureSnapshot.head.pinnedConversationIds?.filter(id => id !== conversationId) }; }, close_composer: async () => undefined, close_head_panel: async () => undefined, set_maximize_button_region: async () => undefined,
  subscribe: () => () => undefined,
  subscribe_maximize_button_hover: () => () => undefined,
  subscribe_drafts: () => () => undefined,
  subscribe_lifecycle: () => () => undefined,
  subscribe_lifecycle_finished: () => () => undefined,
  acknowledge_lifecycle: async () => undefined,
  window: async () => undefined,
  list_devices: async () => ({ currentDeviceId: "device-desktop", devices: fixtureDevices.map(device => ({ ...device })) }),
  revoke_device: async deviceId => {
    const device = fixtureDevices.find(item => item.id === deviceId);
    if (!device) throw fixtureError("device_not_found", "This device is no longer enrolled.");
    if (device.revoked) throw fixtureError("device_already_revoked", "This device was already removed.");
    if (device.id === "device-desktop") throw fixtureError("current_device", "This device cannot remove itself here.");
    device.revoked = true;
  },
  list_contact_books: async () => [{ id: "book-pixel8", deviceName: "Pixel 8", state: "active", capabilities: { canWrite: true, canDelete: true, canRestore: true, supportsNotes: true, supportsPhoto: true, supportsBirthday: true, canSync: true }, ownerDeviceId: "device-pixel8", contactCount: 3, defaultAccountLabel: "Google · aurora@example.com", lastSyncAt: "Just now", pendingEditCount: 1 }, { id: "book-iphone-retired", deviceName: "iPhone", state: "retired", capabilities: { canWrite: false, canDelete: false, supportsNotes: false, supportsPhoto: true, canSync: false }, sourceRevoked: true, ownerDeviceId: "device-old-iphone", contactCount: 0, lastSyncAt: "3 days ago", pendingEditCount: 0 }],
  forget_contact_book: async () => undefined,
  list_contacts: async (bookId, query = "", offset = 0, sort = { key: "family" as const, direction: "asc" as const }) => {
    const contacts = [{ id: "contact-aurora", bookId, revision: "1", displayName: "Aurora Chen", givenName: "Aurora", familyName: "Chen", phones: [{ label: "mobile", number: "+12025550123" }], emails: [{ label: "work", address: "aurora@example.com" }], addresses: [], organization: "Northstar", title: "Director", notes: "Prefers SMS", pendingEditId: "edit-aurora", pendingEditState: "pending" as const, pendingEditSummary: "Phone number update" }, { id: "contact-river", bookId, revision: "1", displayName: "River Park", givenName: "River", familyName: "Park", phones: [{ label: "mobile", number: "+12025550124" }], emails: [], addresses: [] }, { id: "contact-mina", bookId, revision: "1", displayName: "Mina Torres", givenName: "Mina", familyName: "Torres", phones: [{ label: "work", number: "+12025550125" }], emails: [], addresses: [] }];
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
    }).slice(offset, offset + 200);
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
  hosted_origin_url: async () => "https://peppy.pro",
  hosted_sign_in: async () => ({ ...fixtureAccount(), signedIn: true, accountLabel: "Fixture account", classification: "new" }),
  hosted_sign_out: async () => undefined,
  hosted_open_billing: async () => undefined,
  hosted_provision: async () => undefined,
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

const invoke = async <T>(command: string, args?: Record<string, unknown>) => (await import("@tauri-apps/api/core")).invoke<T>(command, args);
/** Native claimant response, normalized before it reaches shared UI. */
type NativeComputerClaimantView = {
  stage: "waiting_owner" | "compare_codes" | "finishing" | "connected" | "denied" | "expired" | "error" | string;
  invitationId: string;
  qrPayload: string | null;
  awaitingOwner: boolean;
  state: "waiting" | "claimed" | "approved" | "confirmed" | "consumed" | "denied" | "expired";
  expiresInSeconds: number;
  verificationCode: string | null;
  errorCode?: ComputerClaimantView["errorCode"];
};
export const normalizeComputerClaimant = (view: NativeComputerClaimantView): ComputerClaimantView => ({
  stage: view.stage === "waiting_owner" ? "computer_waiting"
    : view.stage === "compare_codes" ? "computer_approved"
      : view.stage === "finishing" ? "computer_finishing"
        : view.stage === "connected" ? "computer_connected"
          : view.stage === "denied" ? "computer_denied"
            : view.stage === "expired" ? "computer_expired" : "computer_error",
  qrPayload: view.qrPayload ?? undefined,
  verificationCode: view.verificationCode ?? undefined,
  expiresInSeconds: view.expiresInSeconds,
  awaitingOwner: view.awaitingOwner,
  computerClaimant: true,
  errorCode: view.stage === "error" || !["waiting_owner", "compare_codes", "finishing", "connected", "denied", "expired"].includes(view.stage) ? view.errorCode ?? "computer_verification_failed" : view.errorCode,
});
const discoverEnrollment = async (origin?: string): Promise<EnrollmentView> => {
  const { computerClaimant, ...view } = await invoke<SharedEnrollmentView & { computerClaimant?: NativeComputerClaimantView | null }>("enrollment_discover", { origin });
  return { ...view, ...(computerClaimant ? { computerClaimant: normalizeComputerClaimant(computerClaimant) } : {}) };
};
const beginComputerClaimant = () => invoke<NativeComputerClaimantView>("enrollment_begin_computer_claimant").then(normalizeComputerClaimant);
const computerClaimantStatus = () => invoke<NativeComputerClaimantView>("enrollment_computer_claimant_status").then(normalizeComputerClaimant);
const confirmComputerClaimant = () => invoke<NativeComputerClaimantView>("enrollment_confirm_computer_claimant").then(normalizeComputerClaimant);
const cancelComputerClaimant = () => invoke<void>("enrollment_cancel_computer_claimant");
export const discardComputerClaimant = () => "__TAURI_INTERNALS__" in window
  ? invoke<void>("enrollment_discard_computer_claimant")
  : bridge.enrollment_cancel_computer();
export const renewHostedSession = () => "__TAURI_INTERNALS__" in window
  ? invoke<void>("hosted_session_renew")
  : Promise.resolve();
export const tauriBridge: DesktopBridge = {
  get_notification_sound: () => invoke("get_notification_sound"),
  set_notification_sound: enabled => invoke("set_notification_sound", { enabled }),
  media_sync_request: attachmentIds => invoke("media_sync_request", { attachmentIds }),
  media_page_around: (conversationId, attachmentId, messageId, limit) => invoke("media_page_around", { conversationId, attachmentId, messageId, limit }),
  media_page_newer: (conversationId, cursor, limit) => invoke("media_page_newer", { conversationId, cursor, limit }),
  hydrate_media_preview: (messageId, id) => invoke("hydrate_media_preview", { messageId, id }),
  copy_attachment_image: id => invoke("copy_attachment_image", { id }),
  load_state: (conversationId, historyFromMessageId, options) => invoke("load_state", { conversationId, historyFromMessageId, metadataOnly: options?.metadataOnly }), search_local: input => invoke("search_local", { input }), advance_search_index: input => invoke("advance_search_index", { input }), get_history_search: () => invoke("get_history_search"), start_history_search: ({ operationId }) => invoke("start_history_search", { input: { operationId } }), cancel_history_search: ({ attemptId }) => invoke("cancel_history_search", { input: { attemptId } }), load_message_window: input => invoke("load_message_window", { input }), open_external_url: url => invoke("open_external_url", { url }), load_history: (conversationId, beforeMessageId) => invoke("load_history", { conversationId, beforeMessageId }), configure_server: origin => invoke("configure_server", { origin }), unlock_sync: () => invoke("unlock_sync"), get_app_lock_status: () => invoke("get_app_lock_status"), set_app_lock: enabled => invoke("set_app_lock", { enabled }), unlock_app: () => invoke("unlock_app"), save_draft: input => invoke("save_draft", { input }), discard_draft: input => invoke("discard_draft", { input }), send_draft: input => invoke("send_draft", { input }), mark_seen: visibleMessageIds => invoke("mark_seen", { visibleMessageIds }), mark_all_read: () => invoke("mark_all_read"), dismiss_notification: target => invoke("dismiss_notification", { target }), dismiss_all_notifications: () => invoke("dismiss_all_notifications"), set_app_muted: (sourceDeviceId, packageName, appName, muted) => invoke("set_app_muted", { sourceDeviceId, packageName, appName, muted }), mark_notifications_seen: targets => invoke("mark_notifications_seen", { targets }), set_notification_preferences: preferences => invoke("set_notification_preferences", { preferences }), set_notification_context: (view, conversationId) => invoke("set_notification_context", { view, conversationId }), request_notification_permission: () => invoke("request_notification_permission"), open_notification_settings: () => invoke("open_notification_settings"), pick_attachments: () => invoke("pick_attachments"), retry_attachment: id => invoke("retry_attachment", { id }), save_attachment: id => invoke("save_attachment", { id }), request_attachment_download: id => invoke("request_attachment_download", { id }), media_page: (conversationId, cursor, limit) => invoke("media_page", { conversationId, cursor, limit }), open_media: id => invoke("open_media", { id }), close_media: (id, url) => invoke("close_media", { id, url }), load_media: id => invoke("load_media", { id }), publish_attachment: id => invoke("publish_attachment", { id }), open_composer: conversationId => invoke("open_composer", { conversationId }), set_start_at_login: enabled => invoke("set_start_at_login", { enabled }), popout_conversation: conversationId => invoke("popout_conversation", { conversationId }), hide_head: conversationId => invoke("hide_head", { conversationId }), close_composer: () => invoke("close_composer"), close_head_panel: () => invoke("close_head_panel"), set_maximize_button_region: region => invoke("set_maximize_button_region", { region }), subscribe: listener => subscribeEvent("peppy://state", () => listener()), subscribe_maximize_button_hover: listener => subscribeWindowEvent("peppy://maximize-button-hover", payload => { if (typeof payload === "boolean") listener(payload); }), subscribe_drafts: listener => subscribeWindowEvent("peppy://draft", payload => { if (isDraft(payload)) listener(payload); }), subscribe_lifecycle: listener => subscribeWindowEvent("peppy://lifecycle-request", (payload: unknown) => { if (isLifecycleRequest(payload)) listener(payload); }), subscribe_lifecycle_finished: listener => subscribeWindowEvent("peppy://lifecycle-finished", (payload: unknown) => { if (isLifecycleFinished(payload)) listener(payload); }), acknowledge_lifecycle: (id, ok) => invoke("acknowledge_lifecycle", { id, ok }),
   list_devices: () => invoke("list_devices"), revoke_device: deviceId => invoke("revoke_device", { deviceId }), list_contact_books: () => invoke("list_contact_books"), forget_contact_book: bookId => invoke("forget_contact_book", { bookId }), list_contacts: (bookId, query, offset, sort) => invoke("list_contacts", { bookId, query, offset, sort }), submit_contact_edit: input => invoke("submit_contact_edit", { input }), list_contact_edits: bookId => invoke("list_contact_edits", { bookId }), search_contact_recipients: (query, sourceDeviceId) => invoke("search_contact_recipients", { query, sourceDeviceId }), request_contact_repair: () => invoke("request_contact_repair"), request_contact_sync: bookId => invoke("request_contact_sync", { bookId }), get_contact_sync_request: bookId => invoke("get_contact_sync_request", { bookId }), pick_contact_photo: () => invoke("pick_contact_photo"), list_restorable_contacts: bookId => invoke("list_restorable_contacts", { bookId }), restore_contact: (bookId, contactId) => invoke("restore_contact", { bookId, contactId }), hosted_account: () => invoke("hosted_account"), hosted_origin_url: () => invoke("hosted_origin_url"), hosted_sign_in: provider => invoke("hosted_sign_in", { provider }), hosted_sign_out: () => invoke("hosted_sign_out"), hosted_open_billing: () => invoke("hosted_open_billing"), hosted_provision: () => invoke("hosted_provision"), enrollment_discover: discoverEnrollment, enrollment_start_over: () => invoke("enrollment_start_over"), enrollment_create_owner: input => invoke("enrollment_create_owner", { input }), enrollment_sign_in: input => invoke("enrollment_sign_in", { input }), enrollment_connect_shared_secret: input => invoke("enrollment_connect_shared_secret", { input }), enrollment_begin_vault_request: input => invoke("enrollment_begin_vault_request", { input }), enrollment_vault_request_status: () => invoke("enrollment_vault_request_status"), enrollment_create_vault: input => invoke("enrollment_create_vault", { input }), enrollment_add_phone: method => invoke("enrollment_add_phone", { method }), enrollment_rotate_phone: method => invoke("enrollment_rotate_phone", { method }), enrollment_cancel_phone: () => invoke("enrollment_cancel_phone"), reveal_vault_secret: () => invoke("reveal_vault_secret"), enrollment_status: () => invoke("enrollment_status"), enrollment_approve: () => invoke("enrollment_approve"), enrollment_cancel: () => invoke("enrollment_cancel"), enrollment_sign_out: () => invoke("enrollment_sign_out"),
  enrollment_connect_computer: beginComputerClaimant,
  enrollment_computer_status: computerClaimantStatus,
  enrollment_confirm_computer: confirmComputerClaimant,
  enrollment_cancel_computer: cancelComputerClaimant,
  set_conversation_pinned: (conversationId, pinned) => invoke("set_conversation_pinned", { conversationId, pinned }),
  set_conversation_tone: (seed, paletteIndex) => invoke("set_conversation_tone", { seed, paletteIndex }),
  async window(action) { const w = (await import("@tauri-apps/api/window")).getCurrentWindow(); if (action === "minimize") await w.minimize(); else if (action === "maximize") await w.toggleMaximize(); else await w.close(); },
};

const isLifecycleRequest = (value: unknown): value is { id: string; action: "quit" | "close" | "collapse" } => typeof value === "object" && value !== null && typeof (value as { id?: unknown }).id === "string" && ["quit", "close", "collapse"].includes((value as { action?: unknown }).action as string);
const isLifecycleFinished = (value: unknown): value is { id: string; ok: boolean } => typeof value === "object" && value !== null && typeof (value as { id?: unknown }).id === "string" && typeof (value as { ok?: unknown }).ok === "boolean";
const MAX_U64 = 18446744073709551615n;
const isCanonicalU64 = (value: unknown): value is string => typeof value === "string" && /^(0|[1-9]\d*)$/.test(value) && BigInt(value) <= MAX_U64;
const isStringArray = (value: unknown): value is string[] => Array.isArray(value) && value.every(item => typeof item === "string");
const isDraft = (value: unknown): value is Draft => {
  if (typeof value !== "object" || value === null) return false;
  const draft = value as Partial<Draft>;
  return typeof draft.id === "string" && typeof draft.conversationId === "string" &&
    typeof draft.text === "string" && isStringArray(draft.recipientIds) &&
    isStringArray(draft.attachmentIds) && isCanonicalU64(draft.revision) &&
    (draft.gatewayId === undefined || typeof draft.gatewayId === "string") &&
    (draft.simId === undefined || typeof draft.simId === "string");
};
const subscribeEvent = (event: string, listener: (payload: unknown) => void) => {
  let disposed = false;
  let unlisten: (() => void) | undefined;
  void import("@tauri-apps/api/event").then(({ listen }) => listen(event, payload => listener(payload.payload))).then(stop => { unlisten = stop; if (disposed) stop(); }).catch(() => {});
  return () => { disposed = true; unlisten?.(); };
};
const subscribeWindowEvent = (event: string, listener: (payload: unknown) => void) => {
  let disposed = false;
  let unlisten: (() => void) | undefined;
  void import("@tauri-apps/api/webviewWindow").then(({ getCurrentWebviewWindow }) => getCurrentWebviewWindow().listen(event, payload => listener(payload.payload))).then(stop => { unlisten = stop; if (disposed) stop(); }).catch(() => {});
  return () => { disposed = true; unlisten?.(); };
};
const missingHost = (method: keyof DesktopBridge) => async () => { throw { code: "missing-native-host", message: `Native host is required for ${String(method)}.` } satisfies BridgeError; };
export const missingHostBridge: DesktopBridge = Object.fromEntries((Object.keys(fixtureBridge) as (keyof DesktopBridge)[]).map(key => [key, key === "subscribe" || key === "subscribe_maximize_button_hover" || key === "subscribe_drafts" || key === "subscribe_lifecycle" || key === "subscribe_lifecycle_finished" ? (() => () => undefined) : missingHost(key)])) as unknown as DesktopBridge;
/** Fixtures are development/test-only and are never chosen after a native error. */
export let bridge: DesktopBridge = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window ? tauriBridge : (import.meta.env.DEV || import.meta.env.MODE === "test" ? fixtureBridge : missingHostBridge);

/** Installs a non-native host before the shared UI is rendered. */
export function installBrowserBridge(browserBridge: DesktopBridge): void {
  bridge = browserBridge;
}
