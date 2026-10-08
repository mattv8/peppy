/**
 * UI-only native boundary. These DTOs intentionally contain no credentials,
 * cryptographic material, device tokens, or raw filesystem paths.
 */
export type ConnectionState = "connected" | "offline" | "missing-native-host" | "error";
export type MessageStatus = "queued-local" | "server-accepted" | "gateway-persisted" | "preparing" | "submitted" | "sent" | "delivery-confirmed" | "failed-before-submit" | "failed-confirmed" | "unknown";
export type AttachmentState = "pending" | "uploading" | "failed" | "ready";
export type AttachmentView = { id: string; name: string; mediaType: string; byteSize: number; state: AttachmentState; error?: string; previewUrl?: string; transfer?: "upload" | "download"; retryable?: boolean };
export type MessageView = { id: string; revision: string; sender: "self" | "other"; body: string; timestamp: string; status?: MessageStatus; attachments: AttachmentView[]; transport?: "sms" | "mms"; subject?: string; participants?: string[] };
/** `participants` are native-provided reply addressees and exclude a confirmed self address. */
export type ConversationView = { id: string; name: string; preview: string; unread: number; messages: MessageView[]; participants?: string[]; replyBlockedReason?: string };
export type GatewayView = { id: string; name: string; simId: string; online: boolean; simulated: boolean; supportsSms: boolean; supportsMms: boolean; capabilityNote?: string; mmsContentVersion?: number; mmsMaxBytes?: number; mmsLimitSource?: "carrier" | "fallback"; mmsMaxRecipients?: number };
export type Draft = { id: string; conversationId: string; text: string; recipientIds: string[]; attachmentIds: string[]; gatewayId?: string; simId?: string; revision: string };
export type DesktopSnapshot = { version: "1"; mode: "fixture" | "native" | "browser"; connection: { state: ConnectionState; origin?: string; errorCode?: string }; encryption: { state: "locked" | "unlocked" | "preview" | "mismatch"; profileFingerprint?: string }; credentialExportAvailable?: boolean; gateways: GatewayView[]; conversations: ConversationView[]; activeConversationId?: string; draft?: Draft; desktop?: { trayAvailable: boolean; startAtLogin: boolean; startupSupported: boolean; background: boolean }; deviceRole?: string; head: { enabled: boolean; capability: "supported" | "unsupported" | "unconfirmed"; note?: string; panel?: boolean; pinnedConversationIds?: string[] }; pendingCount: number; quarantineCount: number; notifications: MirroredNotification[]; appFilters: AppFilter[]; notificationPreferences: NotificationPreferences; contactResolution?: ContactResolutionMap; contactBooks?: ContactBookView[]; contactsPendingCount?: number; contactSync?: ContactSyncStatus };
/** gatewayId/simId are optional on saves (omitted = keep the stored route) and required on sends. */
export type DraftInput = Pick<Draft, "id" | "conversationId" | "text" | "recipientIds" | "attachmentIds"> & { expectedRevision: string; gatewayId?: string; simId?: string };
export type SendDraftInput = DraftInput & { gatewayId: string; simId: string };
/** A separate, server-readable re-encoded copy created only after native confirmation. */
export type PublicCopy = { url: string; expiresInSeconds: number };
export type NotificationTarget = { sourceDeviceId: string; notificationKey: string; lifetime: string };
export type MirroredNotification = { target: NotificationTarget; packageName: string; appName: string; title: string; text: string; category?: string; postedAt: number; dismissible: boolean; seen: boolean; dismissalPending: boolean };
export type AppFilter = { sourceDeviceId: string; packageName: string; appName: string; muted: boolean };
export type NotificationPreferences = { messageBanners: boolean; mirroredBanners: boolean; preview: "full" | "hidden" };
export type ContactBookState = "active" | "limited" | "unavailable" | "retired";
/** Native values: requests go to the owner phone, so `canRestore` follows `canWrite`; `supportsNotes`/`supportsBirthday` follow the phone's platform (iOS has no notes). */
export type BookCapabilities = { canWrite: boolean; canDelete: boolean; supportsNotes: boolean; supportsPhoto: boolean; canRestore?: boolean; supportsBirthday?: boolean };
export type ContactBookView = { id: string; deviceName: string; state: ContactBookState; capabilities: BookCapabilities; contactCount: number; defaultAccountLabel?: string; lastSyncAt?: string; stalenessNote?: string; pendingEditCount: number };
/** `label` is the wire value (round-trips unchanged); `displayLabel` is for people (e.g. Apple raw labels). */
export type ContactPhone = { id?: string; label: string; displayLabel?: string; number: string; readOnly?: boolean };
export type ContactEmail = { id?: string; label: string; displayLabel?: string; address: string; readOnly?: boolean };
/** Unshown components (poBox, subLocality, isoCountryCode, …) must round-trip unchanged; `readOnly` items cannot be edited from this computer. */
export type ContactAddress = { id?: string; label: string; displayLabel?: string; formatted?: string; street?: string; poBox?: string; neighborhood?: string; subLocality?: string; city?: string; subAdministrativeArea?: string; state?: string; postalCode?: string; country?: string; isoCountryCode?: string; readOnly?: boolean };
export type ContactBirthday = { year?: number; month?: number; day?: number };
export type ContactView = { id: string; bookId: string; revision?: string; displayName: string; givenName?: string; familyName?: string; nickname?: string; phones: ContactPhone[]; emails: ContactEmail[]; addresses: ContactAddress[]; organization?: string; title?: string; birthday?: ContactBirthday; notes?: string; photoDataUrl?: string; photoPending?: boolean; pendingEditId?: string; pendingEditState?: "pending" | "awaiting-approval" | "outcome-unknown" | "conflict" | "rejected" | "failed" | "expired"; pendingEditSummary?: string; deletedAt?: string };
export type ContactEditRequestInput = { targetBookId: string; kind: "create" | "update" | "delete"; contactId?: string; baseRevision?: string; patches: unknown[]; photo: { kind: "keep" } | { kind: "remove" } | { kind: "set"; croppedDataUrl: string } };
export type ContactEditOutcome = { state: "pending"; requestId: string } | { state: "applied"; newRevision: string } | { state: "awaiting-approval" } | { state: "conflict"; conflictSummary: string } | { state: "rejected"; reason: string } | { state: "expired" };
/** Requester-ledger status of one edit request; only an owner result moves it past `pending`/`awaiting-approval`. */
export type ContactEditStatus = { requestId: string; bookId: string; state: "pending" | "awaiting-approval" | "outcome-unknown" | "applied" | "conflict" | "rejected" | "expired" | "failed"; reason?: string; kind?: "create" | "update" | "delete"; contactId?: string; displayName?: string; summary?: string; expiresAt?: number };
/** Contact projection health: a failed or pending rebuild means contacts may be stale. */
export type ContactSyncStatus = { repairRequired: boolean; projection?: { state: "current" | "rebuilding" | "failed"; reason?: string }; readiness?: { state: "server_unsupported" | "needs_unlock" | "backfill_pending" | "ready"; contacts_ready: boolean; server_active: boolean; backfill_unreadable: number } };
/** A phone number of a matching contact; `address` (never the contact ID) is the recipient. `normalized` is false for numbers kept as entered (e.g. short codes). */
export type RecipientSuggestion = { address: string; displayName: string; number: string; label?: string; normalized: boolean; contactId: string; phoneId: string; avatarUrl?: string };
export type RestorableContact = { id: string; bookId: string; displayName: string; deletedAt: string; photoDataUrl?: string };
export type ResolvedContact = { contactId: string; bookId: string; displayName: string; photoDataUrl?: string };
export type ContactResolutionMap = Record<string, ResolvedContact>;
/** `currentRevision` is set when the stored draft revision is known (stale saves/sends, refused sends). */
export type BridgeError = { code: string; message: string; currentRevision?: string };
/** Public pairing intent data only; credentials and challenge tokens stay native. */
export type PairingIntent = { httpsOrigin: string; intentToken: string; expiresInSeconds: number };
export type PairingStatus = { claimed: boolean; approved: boolean; deviceId?: string; keyDigest?: string; sas?: string; expiresInSeconds: number };
export type HostedAccountView = { available: boolean; signedIn: boolean; accountLabel: string | null; classification: "new" | "pending" | "incomplete" | "provisioning" | "existing" | "lapsed" | null; entitlement: string | null; access: "read_write" | "read_only" | null; hasVault: boolean; resumable: boolean };
export type JoinView = { state: "idle" | "waiting" | "claimed" | "confirm" | "approved" | "expired" | "denied" | "failed"; qrPayload?: string; expiresInSeconds?: number; sas?: string; origin?: string; errorCode?: string };
export type HostedPreviewView = {
  scenario: string; screen: string; accountState: string; entitlementState: string;
  approvalState: string; unlocked: boolean; rejected: boolean;
  statusKey: string | null; localError: string | null; operationId: string | null;
  fixture: { accountLabel: string | null; signInProvider: "apple" | "google" | null; subscription: { displayPrice: string; status: string } | null; approvalCode: string | null; hostedOrigin: string | null };
  scenarios: string[];
};

export interface DesktopBridge {
  load_state(conversationId?: string): Promise<DesktopSnapshot>;
  configure_server(origin: string): Promise<void>;
  import_credentials(): Promise<void>;
  export_credentials(): Promise<boolean>;
  unlock_sync(): Promise<void>;
  /** Browser-only capability for locking a shared encrypted session. */
  lock_sync?(): Promise<void>;
  create_pairing_intent(): Promise<PairingIntent>;
  pairing_intent_status(intentToken: string): Promise<PairingStatus>;
  approve_pairing_intent(intentToken: string, keyDigest: string): Promise<void>;
  save_draft(input: DraftInput): Promise<Draft>;
  /** `accepted` means durably queued in the local encrypted outbox; `revision` is the stored draft revision afterwards. */
  send_draft(input: SendDraftInput): Promise<{ accepted: boolean; status: MessageStatus; reason?: string; revision?: string }>;
  mark_seen(visibleMessageIds: string[]): Promise<void>;
  dismiss_notification(target: NotificationTarget): Promise<void>;
  dismiss_all_notifications(): Promise<void>;
  set_app_muted(sourceDeviceId: string, packageName: string, appName: string, muted: boolean): Promise<void>;
  mark_notifications_seen(targets: NotificationTarget[]): Promise<void>;
  set_notification_preferences(preferences: NotificationPreferences): Promise<void>;
  set_notification_context(view: "conversations" | "notifications" | "settings" | "contacts", conversationId?: string): Promise<void>;
  request_notification_permission(): Promise<"granted" | "denied" | "unknown">;
  pick_attachments(): Promise<AttachmentView[]>;
  retry_attachment(id: string): Promise<void>;
  save_attachment(id: string): Promise<boolean>;
  /** Resolves null when the person cancels the native confirmation. */
  publish_attachment(id: string): Promise<PublicCopy | null>;
  /** Explicit user action only; omitting the ID starts a new-conversation draft. */
  open_composer(conversationId?: string): Promise<void>;
  set_start_at_login(enabled: boolean): Promise<void>;
  popout_conversation(conversationId: string): Promise<{ headCreated: boolean; warning?: string }>;
  hide_head(conversationId: string): Promise<void>;
  close_composer(): Promise<void>;
  close_head_panel(): Promise<void>;
  subscribe(listener: () => void): () => void;
  subscribe_lifecycle(listener: (request: { id: string; action: "quit" | "close" | "collapse" }) => void): () => void;
  subscribe_lifecycle_finished(listener: (result: { id: string; ok: boolean }) => void): () => void;
  acknowledge_lifecycle(id: string, ok: boolean): Promise<void>;
  window(action: "minimize" | "maximize" | "close"): Promise<void>;
  list_contact_books(): Promise<ContactBookView[]>;
  forget_contact_book(bookId: string): Promise<void>;
  /** One page of at most 200 contacts; request the next page with `offset`. */
  list_contacts(bookId: string, query?: string, offset?: number): Promise<ContactView[]>;
  submit_contact_edit(input: ContactEditRequestInput): Promise<ContactEditOutcome>;
  list_contact_edits(bookId?: string): Promise<ContactEditStatus[]>;
  search_contact_recipients(query: string, sourceDeviceId?: string): Promise<RecipientSuggestion[]>;
  request_contact_repair(): Promise<ContactSyncStatus>;
  pick_contact_photo(): Promise<{ dataUrl: string; naturalWidth: number; naturalHeight: number } | null>;
  list_restorable_contacts(bookId: string): Promise<RestorableContact[]>;
  restore_contact(bookId: string, contactId: string): Promise<ContactEditOutcome>;
  hosted_account(): Promise<HostedAccountView>;
  hosted_sign_in(provider: "google"): Promise<HostedAccountView>;
  hosted_sign_out(): Promise<void>;
  hosted_open_billing(): Promise<void>;
  hosted_provision(): Promise<void>;
  join_start(origin: string | null): Promise<JoinView>;
  join_status(): Promise<JoinView>;
  join_cancel(): Promise<void>;
  join_confirm(): Promise<JoinView>;

}
