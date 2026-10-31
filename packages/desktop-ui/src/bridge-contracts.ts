/**
 * UI-only native boundary. These DTOs intentionally contain no credentials,
 * cryptographic material, device tokens, or raw filesystem paths.
 */
import type {
  HistorySearchAttemptInput,
  HistorySearchSnapshot,
  LocalSearchHit,
  LocalSearchPage,
  LocalSearchRequest,
  MessageWindow as GeneratedMessageWindow,
  MessageWindowRequest,
  SearchIndexProgress,
  SearchKind,
  StartHistorySearchInput,
} from "@peppy/generated/local-search";
import type {
  AttachmentRetentionStateDto,
  AttachmentRetentionNoticeAcknowledgementInput,
  AttachmentRetentionNoticeAcknowledgementResult,
  AttachmentStorageStatusDto,
  AttachmentRetentionStatusResult,
} from "@peppy/generated/attachment-retention";
import { SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION } from "@peppy/generated/attachment-retention";
export type {
  HistorySearchAttemptInput,
  HistorySearchReason,
  HistorySearchSnapshot,
  HistorySearchState,
  LocalSearchHit,
  LocalSearchPage,
  LocalSearchRequest,
  MessageWindowRequest,
  SearchHasMore,
  SearchIndexProgress,
  SearchIndexState,
  SearchKind,
  StartHistorySearchInput,
} from "@peppy/generated/local-search";
export type {
  AttachmentRetentionStateDto,
  AttachmentRetentionNoticeAcknowledgementInput,
  AttachmentRetentionNoticeAcknowledgementResult,
  AttachmentStorageStatusDto,
  AttachmentRetentionStatusResult,
} from "@peppy/generated/attachment-retention";
export { SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION } from "@peppy/generated/attachment-retention";

export type ConnectionState = "connected" | "offline" | "missing-native-host" | "error";
export type MessageStatus = "queued-local" | "server-accepted" | "gateway-persisted" | "preparing" | "submitted" | "sent" | "delivery-confirmed" | "failed-before-submit" | "failed-confirmed" | "unknown";
export type AttachmentState = "pending" | "uploading" | "failed" | "ready";
export type AttachmentView = { id: string; name: string; mediaType: string; byteSize: number; state: AttachmentState; error?: string; previewUrl?: string; transfer?: "upload" | "download"; retryable?: boolean; mediaHold?: "held" | "requested"; mediaMeta?: MediaMetadataView; remoteState?: "available" | "removed_by_storage_policy"; localAvailable?: boolean };
export type MessageView = { id: string; revision: string; sender: "self" | "other"; senderAddress?: string; body: string; timestamp: string; status?: MessageStatus; statusAt?: string; attachments: AttachmentView[]; transport?: "sms" | "mms"; subject?: string; participants?: string[] };
export type MessageWindow = GeneratedMessageWindow<MessageView>;
export type MediaCursor = { sortAtMs: string; localOrder: string; messageId: string; attachmentPosition: number };
export type MediaMetadataView = { width?: number; height?: number; durationMs?: string; thumbnail?: AttachmentView; previewUnavailable?: "thumbnail_unavailable" };
export type MediaPageItem = { messageId: string; senderAddress?: string; timestamp: string; attachment: AttachmentView; mediaMeta?: MediaMetadataView; cursor: MediaCursor };
export type MediaPage = { items: MediaPageItem[]; nextCursor?: MediaCursor };
export type MediaPageAround = { target: MediaPageItem; newer: MediaPageItem[]; older: MediaPageItem[]; newerCursor?: MediaCursor; olderCursor?: MediaCursor };
export type MediaOpen = { state: "ready"; url: string; mediaType: string; byteSize: number } | { state: "pending" };
/** `participants` are native-provided reply addressees and exclude a confirmed self address. */
/** Draft flags are authoritative host metadata; do not infer them from an unloaded message list. */
export type ConversationView = { id: string; name: string; preview: string; unread: number; messages: MessageView[]; hasOlderMessages?: boolean; participants?: string[]; recipientlessDraft?: boolean; draftOnly?: boolean; pinned?: boolean };
export type HistoryPage = { conversationId: string; messages: MessageView[]; hasMore: boolean };
export type GatewayAvailability = "unknown" | "connected" | "delayed" | "offline" | "never_connected" | "no_sms_sim" | "sms_permission_needed" | "sms_unavailable" | "capabilities_unknown" | "unsupported_platform";
export type GatewayPresence = { state: "connected" | "seen" | "never"; last_seen_age_seconds: number | null };
export type GatewayView = { id: string; name: string; simId: string; online: boolean; simulated: boolean; supportsSms: boolean; supportsMms: boolean; availability?: GatewayAvailability; lastSeenAgeSeconds?: number; presence?: GatewayPresence; wakeRoute?: boolean; capabilityNote?: string; mmsContentVersion?: number; mmsMaxBytes?: number; mmsLimitSource?: "carrier" | "fallback"; mmsMaxRecipients?: number };
export type Draft = { id: string; conversationId: string; text: string; recipientIds: string[]; attachmentIds: string[]; gatewayId?: string; simId?: string; revision: string };
/** Exact durable cursors only; `targetHighWater` is a transient authenticated replay cut, not a percentage denominator. */
export type SyncProgress = { phase: "receiving" | "applying" | "waitingForKeys" | "current"; receivedThrough: string; processedThrough: string; targetHighWater?: string; snapshotReceived?: string; snapshotExpected?: string; snapshotRemaining?: string };
export type DesktopSnapshot = { version: "1"; mode: "fixture" | "native" | "browser"; connection: { state: ConnectionState; origin?: string; errorCode?: string }; encryption: { state: "locked" | "unlocked" | "preview" | "mismatch"; profileFingerprint?: string }; gateways: GatewayView[]; conversations: ConversationView[]; activeConversationId?: string; draft?: Draft; desktop?: { trayAvailable: boolean; startAtLogin: boolean; startupSupported: boolean; background: boolean }; deviceRole?: string; sessionScopeId?: string; head: { enabled: boolean; capability: "available" | "supported" | "unsupported" | "unconfirmed"; note?: string; panel?: boolean; pinnedConversationIds?: string[] }; pendingCount: number; quarantineCount: number; notifications: MirroredNotification[]; appFilters: AppFilter[]; notificationPreferences: NotificationPreferences; contactResolution?: ContactResolutionMap; contactBooks?: ContactBookView[]; contactsPendingCount?: number; contactSync?: ContactSyncStatus; toneOverrides?: Record<string, number>; syncProgress?: SyncProgress };
/** gatewayId/simId are optional on saves (omitted = keep the stored route) and required on sends. */
export type DraftInput = Pick<Draft, "id" | "conversationId" | "text" | "recipientIds" | "attachmentIds"> & { expectedRevision: string; gatewayId?: string; simId?: string };
export type DiscardDraftInput = Pick<Draft, "id" | "conversationId"> & { expectedRevision: string };
export type SendDraftInput = DraftInput & { gatewayId: string; simId: string };
/** A separate, server-readable re-encoded copy created only after native confirmation. */
export type PublicCopy = { url: string; expiresInSeconds: number };
export type NotificationTarget = { sourceDeviceId: string; notificationKey: string; lifetime: string };
export type MirroredNotification = { target: NotificationTarget; packageName: string; appName: string; appIconPngBase64?: string; title: string; text: string; category?: string; postedAt: number; dismissible: boolean; seen: boolean; dismissalPending: boolean };
export type AppFilter = { sourceDeviceId: string; packageName: string; appName: string; muted: boolean };
export type NotificationPreferences = { messageBanners: boolean; mirroredBanners: boolean; preview: "full" | "hidden"; muteWithHeads?: boolean };
export type NotificationPermissionStatus = "granted" | "quiet" | "denied" | "unknown";
export type NotificationCheck = { status: NotificationPermissionStatus; testSent: boolean; canOpenSettings: boolean };
export type ContactBookState = "active" | "limited" | "unavailable" | "retired";
/** Native values: requests go to the owner phone, so `canRestore` follows `canWrite`; `supportsNotes`/`supportsBirthday` follow the phone's platform (iOS has no notes). */
export type BookCapabilities = { canWrite: boolean; canDelete: boolean; supportsNotes: boolean; supportsPhoto: boolean; canRestore?: boolean; supportsBirthday?: boolean; canSync?: boolean };
export type ContactBookView = { id: string; deviceName: string; state: ContactBookState; capabilities: BookCapabilities; contactCount: number; defaultAccountLabel?: string; lastSyncAt?: string; stalenessNote?: string; pendingEditCount: number; ownerDeviceId?: string; sourceRevoked?: boolean };
export type ManagedDeviceView = { id: string; name: string; kind: string | null; role: string; revoked: boolean; presence?: GatewayPresence; wakeRoute?: boolean };
export type DeviceRosterView = { currentDeviceId: string; devices: ManagedDeviceView[] };
export type ContactScanStatus = { requestId: string; bookId: string; state: "queued" | "waiting" | "scanning" | "completed" | "blocked" | "failed" | "expired"; reason?: string; expiresAt?: number };
/** `label` is the wire value (round-trips unchanged); `displayLabel` is for people (e.g. Apple raw labels). */
export type ContactPhone = { id?: string; label: string; displayLabel?: string; number: string; readOnly?: boolean };
export type ContactEmail = { id?: string; label: string; displayLabel?: string; address: string; readOnly?: boolean };
/** Unshown components (poBox, subLocality, isoCountryCode, …) must round-trip unchanged; `readOnly` items cannot be edited from this computer. */
export type ContactAddress = { id?: string; label: string; displayLabel?: string; formatted?: string; street?: string; poBox?: string; neighborhood?: string; subLocality?: string; city?: string; subAdministrativeArea?: string; state?: string; postalCode?: string; country?: string; isoCountryCode?: string; readOnly?: boolean };
export type ContactBirthday = { year?: number; month?: number; day?: number };
export type ContactSort = { key: "given" | "family"; direction: "asc" | "desc" };
export type ContactView = { id: string; bookId: string; revision?: string; displayName: string; givenName?: string; familyName?: string; nickname?: string; phones: ContactPhone[]; emails: ContactEmail[]; addresses: ContactAddress[]; organization?: string; title?: string; birthday?: ContactBirthday; notes?: string; photoDataUrl?: string; photoPending?: boolean; pendingEditId?: string; pendingEditState?: "pending" | "awaiting-approval" | "outcome-unknown" | "conflict" | "rejected" | "failed" | "expired"; pendingEditSummary?: string; deletedAt?: string };
export type ContactEditRequestInput = { targetBookId: string; kind: "create" | "update" | "delete"; contactId?: string; baseRevision?: string; patches: unknown[]; photo: { kind: "keep" } | { kind: "remove" } | { kind: "set"; croppedDataUrl: string } };
export type ContactEditOutcome = { state: "pending"; requestId: string } | { state: "applied"; newRevision: string } | { state: "awaiting-approval" } | { state: "conflict"; conflictSummary: string } | { state: "rejected"; reason: string } | { state: "expired" };
/** Requester-ledger status of one edit request; only an owner result moves it past `pending`/`awaiting-approval`. */
export type ContactEditStatus = { requestId: string; bookId: string; state: "pending" | "awaiting-approval" | "outcome-unknown" | "applied" | "conflict" | "rejected" | "expired" | "failed"; reason?: string; kind?: "create" | "update" | "delete"; contactId?: string; displayName?: string; summary?: string; expiresAt?: number };
/** Contact projection health: a failed or pending rebuild means contacts may be stale. */
export type ContactSyncStatus = { repairRequired: boolean; projection?: { state: "current" | "rebuilding" | "failed"; reason?: string }; readiness?: { state: "server_unsupported" | "needs_unlock" | "backfill_pending" | "ready"; contacts_ready: boolean; server_active: boolean; backfill_unreadable: number } };
/** A phone number of a matching contact; `address` (never the contact ID) is the recipient. `normalized` is false for numbers kept as entered (e.g. short codes). */
export type RecipientSuggestion = { address: string; bookId: string; displayName: string; number: string; label?: string; normalized: boolean; contactId: string; phoneId: string; avatarUrl?: string };
export type RestorableContact = { id: string; bookId: string; displayName: string; deletedAt: string; photoDataUrl?: string };
export type ResolvedContact = { contactId: string; bookId: string; displayName: string; photoDataUrl?: string };
export type ContactResolutionMap = Record<string, ResolvedContact>;
/** `currentRevision` is set when the stored draft revision is known (stale saves/sends, refused sends). */
export type BridgeError = { code: string; message: string; conversationId?: string; draftId?: string; currentRevision?: string };
export type HostedAccountView = { available: boolean; signedIn: boolean; accountLabel: string | null; classification: "new" | "pending" | "incomplete" | "provisioning" | "existing" | "lapsed" | null; entitlement: string | null; access: "read_write" | "read_only" | null; hasVault: boolean; resumable: boolean };
/** Sanitized enrollment state. Secrets and device credentials remain in the platform host. */
export type EnrollmentView = {
  mode: "community" | "hosted";
  stage: "server" | "setup" | "signin" | "locked" | "ready" | "waiting_phone" | "verify_phone" | "phone_approved" | "phone_connected" | "expired" | "error" | "invite_request" | "invite_waiting" | "invite_approved" | "invite_denied" | "invite_expired";
  origin?: string;
  owner?: boolean;
  hasGateway?: boolean;
  qrPayload?: string;
  verificationCode?: string;
  expiresInSeconds?: number;
  errorCode?: string;
  setupCodeRequired?: boolean;
  ownerResumePending?: boolean;
  vaultOwnerRequired?: boolean;
  /** Host-authoritative: this local identity has no saved binding and may request owner approval. */
  canConnectComputer?: boolean;
  displayName?: string;
  username?: string;
};
/** Sanitized computer-claimant state. The QR and comparison code are not credentials. */
export type ComputerClaimantView = {
  stage: "computer_waiting" | "computer_approved" | "computer_finishing" | "computer_connected" | "computer_expired" | "computer_denied" | "computer_error";
  qrPayload?: string;
  verificationCode?: string;
  expiresInSeconds?: number;
  awaitingOwner: boolean;
  computerClaimant: true;
  errorCode?: string;
};
export type HostedPreviewView = {
  scenario: string; screen: string; accountState: string; entitlementState: string;
  approvalState: string; unlocked: boolean; rejected: boolean;
  statusKey: string | null; localError: string | null; operationId: string | null;
  fixture: { accountLabel: string | null; signInProvider: "apple" | "google" | null; subscription: { displayPrice: string; status: string } | null; approvalCode: string | null; hostedOrigin: string | null };
  scenarios: string[];
};
/** App-open password state; no password or verifier crosses this boundary. */
export type AppLockStatus = { enabled: boolean; locked: boolean; storageAvailable: boolean };
export type MaximizeButtonRegion = { x: number; y: number; width: number; height: number };
export const attachmentRetentionNoticeCopySupported = (policyVersion: string | null | undefined): boolean =>
  policyVersion === SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION;

export interface DesktopBridge {
  load_state(conversationId?: string, historyFromMessageId?: string, options?: { metadataOnly?: boolean }): Promise<DesktopSnapshot>;
  search_local(input: LocalSearchRequest): Promise<LocalSearchPage>;
  advance_search_index(input?: { budgetRows?: number }): Promise<SearchIndexProgress>;
  get_history_search(): Promise<HistorySearchSnapshot>;
  start_history_search(input: StartHistorySearchInput): Promise<HistorySearchSnapshot>;
  cancel_history_search(input: HistorySearchAttemptInput): Promise<HistorySearchSnapshot>;
  load_message_window(input: MessageWindowRequest): Promise<MessageWindow>;
  open_external_url?(url: string): Promise<void>;
  load_history(conversationId: string, beforeMessageId: string): Promise<HistoryPage>;
  configure_server(origin: string): Promise<void>;
  unlock_sync(): Promise<void>;
  get_app_lock_status(): Promise<AppLockStatus>;
  set_app_lock(enabled: boolean): Promise<boolean>;
  unlock_app(): Promise<boolean>;
  /** Browser-only capability for locking a shared encrypted session. */
  lock_sync?(): Promise<void>;
  save_draft(input: DraftInput): Promise<Draft>;
  discard_draft(input: DiscardDraftInput): Promise<void>;
  /** `accepted` means durably queued in the local encrypted outbox; `revision` is the stored draft revision afterwards. */
  send_draft(input: SendDraftInput): Promise<{ accepted: boolean; status: MessageStatus; reason?: string; revision?: string }>;
  mark_seen(visibleMessageIds: string[]): Promise<void>;
  /** Actual number changed, encoded as a canonical unsigned decimal string. */
  mark_all_read(): Promise<string>;
  set_conversation_pinned(conversationId: string, pinned: boolean): Promise<void>;
  set_conversation_tone(seed: string, paletteIndex: number | null): Promise<void>;
  dismiss_notification(target: NotificationTarget): Promise<void>;
  dismiss_all_notifications(): Promise<void>;
  set_app_muted(sourceDeviceId: string, packageName: string, appName: string, muted: boolean): Promise<void>;
  mark_notifications_seen(targets: NotificationTarget[]): Promise<void>;
  set_notification_preferences(preferences: NotificationPreferences): Promise<void>;
  set_notification_context(view: "conversations" | "notifications" | "settings" | "contacts", conversationId?: string): Promise<void>;
  request_notification_permission(): Promise<NotificationCheck>;
  open_notification_settings(): Promise<void>;
  pick_attachments(): Promise<AttachmentView[]>;
  retry_attachment(id: string): Promise<void>;
  save_attachment(id: string): Promise<boolean>;
  request_attachment_download(id: string): Promise<void>;
  /** Explicit user request to release named held originals using mobile data. */
  media_sync_request?(attachmentIds: string[]): Promise<void>;
  /** Native composer-only capability; unsupported hosts use the in-window media viewer. */
  open_media_viewer?(conversationId: string, attachmentId: string, messageId: string): Promise<void>;
  media_page(conversationId: string, cursor?: MediaCursor, limit?: number): Promise<MediaPage>;
  media_page_around(conversationId: string, attachmentId: string, messageId?: string, limit?: number): Promise<MediaPageAround>;
  media_page_newer(conversationId: string, cursor: MediaCursor, limit?: number): Promise<MediaPage>;
  open_media(id: string): Promise<MediaOpen>;
  close_media(id: string, url?: string): Promise<void>;
  load_media(id: string): Promise<MediaOpen>;
  /** Native-only: `id` is the original attachment of `messageId`; the host resolves that exact message's metadata and returns its best local preview, attributed by the returned `id`. Never scans the vault or downloads originals. */
  hydrate_media_preview?(messageId: string, id: string): Promise<AttachmentView>;
  /** Native-only: copies one original image to the OS clipboard; rejects with `media-pending` while the original is still downloading. */
  copy_attachment_image?(id: string): Promise<void>;
  /** Resolves null when the person cancels the native confirmation. */
  publish_attachment(id: string): Promise<PublicCopy | null>;
  /** Explicit user action only; omitting the ID starts a new-conversation draft. */
  open_composer(conversationId?: string): Promise<void>;
  set_start_at_login(enabled: boolean): Promise<void>;
  get_notification_sound?(): Promise<boolean>;
  set_notification_sound?(enabled: boolean): Promise<void>;
  popout_conversation(conversationId: string): Promise<{ headCreated: boolean; warning?: string }>;
  hide_head(conversationId: string): Promise<void>;
  close_composer(): Promise<void>;
  close_head_panel(): Promise<void>;
  set_maximize_button_region(region: MaximizeButtonRegion | null): Promise<void>;
  subscribe(listener: () => void): () => void;
  subscribe_maximize_button_hover(listener: (hovered: boolean) => void): () => void;
  /** `ready` resolves once the native listener is installed and rejects if registration fails; legacy callers may ignore it. */
  subscribe_drafts?(listener: (draft: Draft) => void): (() => void) & { ready?: Promise<void> };
  subscribe_lifecycle(listener: (request: { id: string; action: "quit" | "close" | "collapse" }) => void): () => void;
  subscribe_lifecycle_finished(listener: (result: { id: string; ok: boolean }) => void): () => void;
  acknowledge_lifecycle(id: string, ok: boolean): Promise<void>;
  window(action: "minimize" | "maximize" | "close"): Promise<void>;
  list_contact_books(): Promise<ContactBookView[]>;
  list_devices(): Promise<DeviceRosterView>;
  revoke_device(deviceId: string): Promise<void>;
  forget_contact_book(bookId: string): Promise<void>;
  /** One page of at most 200 contacts; request the next page with `offset`. */
  list_contacts(bookId: string, query?: string, offset?: number, sort?: ContactSort): Promise<ContactView[]>;
  submit_contact_edit(input: ContactEditRequestInput): Promise<ContactEditOutcome>;
  list_contact_edits(bookId?: string): Promise<ContactEditStatus[]>;
  search_contact_recipients(query: string, sourceDeviceId?: string): Promise<RecipientSuggestion[]>;
  request_contact_repair(): Promise<ContactSyncStatus>;
  request_contact_sync(bookId: string): Promise<ContactScanStatus>;
  get_contact_sync_request(bookId: string): Promise<ContactScanStatus | null>;
  pick_contact_photo(): Promise<{ dataUrl: string; naturalWidth: number; naturalHeight: number } | null>;
  list_restorable_contacts(bookId: string): Promise<RestorableContact[]>;
  restore_contact(bookId: string, contactId: string): Promise<ContactEditOutcome>;
  hosted_account(): Promise<HostedAccountView>;
  /** Returns the host-validated public Peppy Hosted origin. */
  hosted_origin_url(): Promise<string>;
  hosted_sign_in(provider: "google"): Promise<HostedAccountView>;
  hosted_sign_out(): Promise<void>;
  hosted_open_billing(): Promise<void>;
  hosted_provision(): Promise<void>;
  /** Optional while older fixture hosts do not implement device retention status. Fetch only on settings open, explicit refresh, or one notice presentation. */
  attachment_retention_status?(): Promise<AttachmentRetentionStatusResult>;
  /** Optional device-authenticated acknowledgement. The host rejects a stale scope before credential selection or HTTP. */
  acknowledge_attachment_retention_notice?(input: AttachmentRetentionNoticeAcknowledgementInput): Promise<AttachmentRetentionNoticeAcknowledgementResult>;
  /** Optional host read of one attachment's core-owned remote/local availability. */
  attachment_retention_state?(attachmentId: string): Promise<AttachmentRetentionStateDto>;
  enrollment_discover(origin?: string): Promise<EnrollmentView>;
  enrollment_start_over(): Promise<EnrollmentView>;
  enrollment_create_owner(input?: { username?: string }): Promise<EnrollmentView>;
  enrollment_sign_in(input?: { username?: string }): Promise<EnrollmentView>;
  enrollment_connect_shared_secret(input?: { vaultOwner?: string }): Promise<EnrollmentView>;
  enrollment_begin_vault_request(input: { inviteLink: string; displayName: string }): Promise<EnrollmentView>;
  enrollment_vault_request_status(): Promise<EnrollmentView>;
  enrollment_create_vault(input: { username: string }): Promise<EnrollmentView>;
  enrollment_pending_invite?(): Promise<string | null>;
  enrollment_add_phone(method: "transfer" | "shared_secret"): Promise<EnrollmentView>;
  enrollment_rotate_phone(method: "transfer" | "shared_secret"): Promise<EnrollmentView>;
  enrollment_cancel_phone(): Promise<EnrollmentView>;
  reveal_vault_secret(): Promise<void>;
  enrollment_status(): Promise<EnrollmentView>;
  enrollment_approve(): Promise<EnrollmentView>;
  enrollment_cancel(): Promise<void>;
  enrollment_sign_out(): Promise<void>;
  enrollment_connect_computer(): Promise<ComputerClaimantView>;
  enrollment_computer_status(): Promise<ComputerClaimantView>;
  enrollment_confirm_computer(): Promise<ComputerClaimantView>;
  enrollment_cancel_computer(): Promise<ComputerClaimantView | void>;

}
