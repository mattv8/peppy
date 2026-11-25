import { UPDATE_SEND_FAILURE, UPDATE_SEND_REASON, UPDATE_STATUS_TERM } from "@peppy/desktop-ui";
import { UPDATE_PAUSED_DESCRIPTION } from "@peppy/desktop-ui";
import { isWritesPaused, useDelayedWritesPaused } from "./shell/updatePaused";
import {
  Component,
  useContext,
  useEffect,
  useLayoutEffect,
  useReducer,
  useRef,
  useState,
  useCallback,
  useMemo,
  type CSSProperties,
  type ReactNode,
  type RefCallback,
  type WheelEvent,
} from "react";
import { flushSync } from "react-dom";
import { StartupScreen } from "./shell/StartupScreen";
import { useLaunchProgress } from "./shell/useLaunchProgress";
import { MAX_DRAFT_ATTACHMENTS, type BillingManageableIn, type BillingProvider, type ConversationFolder, type LocalSearchHit, type LocalSearchRequest, type NotificationQuickReplySettings, type OneTimeCodeTarget } from "@peppy/desktop-ui/bridge-contracts";
import {
  AppTitlebar,
  attachmentStorageRemovalText,
  BusyIndicator,
  OmniSearch,
  Check,
  CheckCheck,
  Clock,
  Composer,
  ConnectionDot,
  ContactAvatar,
  ConversationList,
  ExternalLink,
  FileText,
  History,
  Image,
  Lock,
  LockKeyhole,
  LockOpen,
  Loader2,
  Modal,
  Info,
  MessageSquare,
  MessageSquarePlus,
  Minus,
  NavButtons,
  type NavigationPosition,
  type PopoutMode,
  Panel,
  RecipientPanel,
  ResizeHandle,
  ShieldAlert,
  ShieldCheck,
  Smartphone,
  StatusPopover,
  SyncProgress,
  TriangleAlert,
  Trash2,
  UserPlus,
  X,
  type Attachment,
  type AttachmentLayout,
  type AvatarIdentity,
  type Conversation,
  conversationAvatar,
  detectPlatform,
  installOverlayScrollbars,
  isRecipientPosition,
  isNavigationPosition,
  isPopoutMode,
  formatPhoneNumber,
  normalizePhoneNumber,
  formatDateTime,
  isoDateTime,
  seedKey,
  type RecipientPosition,
  type RecipientSuggestion,
  ToneOverridesProvider,
  TonePicker,
  incomingToneSeed,
  toneStyle,
  paletteIndex,
  type InlineStatusItem,
} from "@peppy/desktop-ui";
import "./settings/settings.css";
import {
  bridge,
  renewHostedSession,
  type AttachmentView,
  type ConversationView,
  type DesktopSnapshot,
  type GatewayView,
  type MessageStatus,
  type MessageView,
  type HistoryPage,
  type AppFilter,
  type MirroredNotification,
  type NotificationCheck,
  type NotificationPreferences,
  type NotificationPersistence,
  type NotificationPersistenceSettings,
  type DesktopBridge,
  type PublicCopy,
  type ContactResolutionMap,
  type AppLockStatus,
  type Draft,
} from "./bridge/bridge";
import { useMaximizeButtonRegion } from "./shell/useMaximizeButtonRegion";
import { dismissibleNotifications, notificationAppName, notificationMuteKey, notificationSourceName, NotificationsView, notificationTuple, phoneNameOf, shownDismissalBlocks, visibleNotifications, type NotificationAnnouncement } from "./notifications/Notifications";
import { useNotificationDismissals } from "./notifications/useNotificationDismissals";
import { newestDismissTargets } from "./notifications/dismissalOverlay";
import { blockedDismissAnnouncement } from "./notifications/notificationDismissCopy";
import { notificationKey } from "./notifications/notificationSections";
import type { NotificationActionNotice as NotificationActionNoticeState } from "./notifications/NotificationActionNotice";
import { useNotificationSearch } from "./notifications/useNotificationSearch";
import { useScopedSearch } from "./search/useScopedSearch";
import { NotificationMuteNotice, type NotificationMuteNotice as NotificationMuteNoticeState } from "./notifications/NotificationMuteNotice";
import { createNotificationActivationDrain, nativeNotificationActivation, type ActivationContext } from "./notifications/notificationActivation";
import type { NotificationActivation, NotificationActivationTarget } from "@peppy/generated/notification-activation";
import { NotificationSettings } from "./notifications/NotificationSettings";
import { DefinitivePreEditRefusal, NotificationReplyRecovery } from "./notifications/NotificationReplyRecovery";
import { ContactsView, type ContactNavigationGuard, type ContactTarget } from "./contacts/Contacts";
import { COMPACT_BOOK_LIST_QUERY, useMediaMatch } from "./contacts/useMediaMatch";
import { findContactTarget } from "./contacts/contact-target";
import { useHistorySearch } from "./search/useHistorySearch";
import { DevicesSettings } from "./settings/DevicesSettings";
import { McpConnectHelp } from "./settings/McpConnectHelp";
import { McpAuthorizationConsent } from "./settings/McpAuthorizationConsent";
import { AttachmentStorageSettingsHost, HostedAccountSettings } from "./settings/HostedAccountSettings";
import { FeedbackSettings } from "./settings/FeedbackSettings";
import { PrivacySettings } from "./settings/PrivacySettings";
import { RcsFeaturesSettings } from "./settings/RcsFeaturesSettings";
import { AdvancedSettings } from "./settings/AdvancedSettings";
import { DiagnosticsSettings, hasDiagnosticsBridge, type DiagnosticsBridge } from "./settings/DiagnosticsSettings";
import { searchRecipientSuggestions } from "./contacts/recipientSearch";
export { searchRecipientSuggestions } from "./contacts/recipientSearch";
import { SetupLanding, savedSetupMode } from "./setup/SetupLanding";
import { AppLockScreen, AppLockSettings } from "./shell/AppLock";
import { UpdatesSettings } from "./updates/UpdatesSettings";
import { isTauriUpdateHost, useUpdateActivity, useUpdateDistribution } from "./updates/updates";
import { connectionText, historyStatus, isServerUnreachable, statusSummary, statusTone, syncStatusText } from "./shell/status";
import { SyncTooLargeNotice } from "./shell/SyncTooLargeNotice";
import { isSettingUp, liveSettingUpGatewayFor, resolveStoredRoute, settingUpGatewayFor } from "./setup/settingUpRoute";
import { DEFAULT_MMS_LIMIT_BYTES, availabilityProblem, gatewayAvailability, importingRefusal, routeProblem, type RouteChoice } from "./setup/routeProblem";
import { fileSize } from "./fileSize";
import { formatCopy } from "./generated/peppyCopy";
import { SystemRibbon, SystemRibbonAnnouncement, SystemRibbonStrip } from "./notices/SystemRibbon";
import { formatDeletionDeadline, openAccountManageUrl, resolveSystemRibbon, type RibbonActionContext, type RibbonBillingFacts } from "./notices/systemRibbonModel";
import { useAccountNotice } from "./notices/useAccountNotice";
import { AppNoticeBanner, type AppNotice } from "./notices/AppNoticeBanner";
import { MarkAllReadAction } from "./conversation/MarkAllReadAction";
import { ConversationFolderConfirmation } from "./conversation/ConversationFolderConfirmation";
import { ConversationFolderNotice, type FolderNotice } from "./conversation/ConversationFolderNotice";
import { ConversationFolderSwitcher } from "./conversation/ConversationFolderSwitcher";
import { SenderUnblockConfirmation } from "./conversation/SenderUnblockConfirmation";
import { ConversationSpamNotice } from "./conversation/ConversationSpamNotice";
import { SenderUnblockNotice, type SenderUnblockNoticeState } from "./conversation/SenderUnblockNotice";
import { MediaLightbox } from "./media/MediaLightbox";
import { MediaPanel, type MediaPageItem as GalleryMediaPageItem } from "./media/MediaPanel";
import { MediaGrid } from "./media/MediaGrid";
import { MediaTile } from "./media/MediaTile";
import { expiryText } from "./media/publicCopyExpiry";
import { originalPending } from "./media/mediaState";
import { ConversationViewTabs, type ConversationView as ConversationTab } from "./ConversationViewTabs";
import { MediaActionsProvider, type SaveOutcome as GallerySaveOutcome } from "./media/MediaActionsContext";
import { clipboardImageSupported } from "./media/clipboard";
import { InlineMedia } from "./media/InlineMedia";
import { MessageText } from "./message-links";
import { groupMessages } from "./media/grouping";
import { DETACHED_HISTORY_LIMIT, boundedAroundMessages, mergeDetachedMessages, searchSessionKey as createSearchSessionKey, type DetachedHistory } from "./search/detachedHistory";

import { GatewayPicker } from "./setup/GatewayPicker";
import { installHostedSessionRenewal } from "./bridge/hostedSessionRenewal";
import { persistLayoutPatch, persistRecipientPosition, readCompactTimestamps, readComposerAttachmentLayout, readContactsBookCollapsed, readGroupSenderAvatars, readShareTyping } from "./shell/layout-persistence";
import { MmsGroupBadge, RcsBadge } from "./rcs/RcsBadge";
import { ConversationRcsNotice } from "./rcs/ConversationRcsNotice";
import { RcsMessageMeta } from "./rcs/RcsMessageMeta";
import { SEND_RECOVERY_REASON, SEND_RECOVERY_STATUS, SendAgainStoreContext, SendRecoveryNotice } from "./messages/SendAgain";
import { SendAgainHost } from "./messages/SendAgainHost";
import { registerResendRows, resendChains, type ResendChain } from "./messages/resendChains";
import type { SendAgainRouteLabeler } from "./messages/SendAgainRouteList";
import { currentRcsRoute, rcsDownloadRetryBlock, rcsSendBlock } from "./rcs/rcsRoute";
import { canRetryRcsDownload, rcsAttachmentIndex, rcsDirection, rcsOutcomeStatus, rcsPlaceholderState } from "./rcs/rcsMediaState";
import { RcsMediaActionsProvider } from "./rcs/RcsMediaActions";
import { RcsAttachmentSlot } from "./rcs/RcsAttachmentSlot";
import { RcsMediaFileRow } from "./rcs/RcsMediaPlaceholder";
import { useRcsRoute } from "./rcs/useRcsRoute";
import { TypingAnnouncer, TypingRow, type TypingRowData } from "./typing/TypingIndicator";
import { useTypingDisplay } from "./typing/useTypingDisplay";
import { useTypingSender } from "./typing/useTypingSender";
import { peppyCopy } from "./generated/peppyCopy";
import { GroupDetailsDialog } from "./group/GroupDetailsDialog";
import { GroupNameField } from "./group/GroupNameField";
import { groupComposerStatusItems } from "./group/groupComposerStatus";
import { composerStatusItems } from "./composer/composerStatusModel";
import { GroupNotice } from "./group/GroupNotice";
import { groupNoticeItems } from "./group/groupNotices";
import { interleaveGroupNotices } from "./group/groupTimeline";
import { normalizeGroupName } from "./group/groupName";
import { groupTypingLabel } from "./group/groupCopy";
import { groupSubtitle, groupTitle } from "./group/groupTitle";
import { type GroupComposerNotice, groupAllowsTypingSend, groupComposerNotice, groupIsReadOnly, groupMembers, groupNoticeState } from "./group/groupState";
import { CORAL_ACCENT_INDEX, accentAttribute, accentSwatchColor } from "./shell/accent-color";
import { useAccentColor } from "./shell/useAccentColor";
import { senderLabel, senderRunEdges } from "./sender/attribution";
import { conversationContactAddress } from "./contacts/conversation-contact";
import { ContactNameChooser } from "./contacts/ContactNameChooser";
import { hiddenTimeIds } from "./conversation/timestamp-bursts";
import { createSeenReactionLedger, linkReactions, locateReactionTarget, mergeReactions, NO_REACTIONS, ReactionAnnouncer, ReactionBadges, ReactionNotice, ReactionSettings, useSeenBadges, useShowReactions, type ActorName, type MergedReactionBadge, type OwnChipState, type ReactionNoticeInfo, type ReactionTargetResult, type SeenReactionLedger } from "./reactions";
import { type Announcement, isReactionEligible, MessageReactionLayer, NO_ANNOUNCEMENT, nativeSelection } from "./reactions/ReactionLayer";
import { pickAnnouncement, REFUSAL_COPY, requestAnnouncement, type RequestSnapshot, UNREACHABLE_COPY } from "./reactions/announcements";
import type { OwnReactionRequestView, ReactionSubmitView } from "@peppy/desktop-ui/bridge-contracts";
import { useReactionTarget } from "./reactions/useReactionTarget";
import { useRovingMessages } from "./reactions/useRovingMessages";
import { normalizeReactionText, quoteMatches } from "./reactions/parse";
import { FALLBACK_ERROR, errorCode, errorText } from "./bridge/bridgeErrors";
import { OneTimeCodeButton } from "./one-time-code/OneTimeCodeButton";
import { CancelSendControl, SendCancellationPending } from "./messages/CancelSendControl";
import {
  DraftStore,
  blankContent,
  isDirty,
  isLocalDraftKey,
  shouldConfirmPostFlushDelete,
  type DraftContent,
  type DraftSlot,
  type SaveOutcome,
  type DraftNoticeEvent,
} from "./drafts/DraftStore";
import {
  CANCELLED_ATTACHMENTS_SKIPPED_NOTICE,
  restorableContent,
  settleCancelledSendRestore,
  type CancelledSendRestore,
} from "./drafts/cancelledSendRestore";
import { DraftNotice } from "./drafts/DraftNotice";
import { CLOSE_SEND_WAIT_MS, waitForSend, type SendSettlement } from "./drafts/sendSettlement";
import { DraftSyncStatus } from "./drafts/DraftSyncStatus";
import type { DraftResolutionResult } from "./drafts/DraftConflictReviewDialog";
import { selectDraftNotice, type DraftNoticeCandidate } from "./drafts/draftNoticeModel";
import { MmsSizeHelp } from "./drafts/MmsSizeHelp";
import { planMms } from "./drafts/mmsPlan";
import { measureComposerChrome } from "./conversation/composerChrome";
import { IsolatedText } from "./notices/IsolatedText";

type RefreshRequest = {
  conversationId?: string;
  explicit: boolean;
  waiters: Array<{ resolve: () => void; reject: (error: unknown) => void }>;
};

type ActionNotice = Omit<AppNotice, "source">;
type CloseOutcome = SaveOutcome | { ok: false; reason: "send-unsent" | "still-sending" };
const CLOSE_SEND_NOTICES = { "still-sending": "Your message is still sending. You can close this window again once it finishes." };
type ComposerFeedback = { key: string; message: string; severity: AppNotice["severity"]; kind?: "attachment-feedback" };
type LocalCancellationState = "requested" | "too-late" | "unknown" | "failed";
type CancellationState = NonNullable<MessageView["sendCancellation"]>["state"];
export type PendingSend = {
  id: string;
  conversationId: string;
  content: DraftContent;
  createdAt: number;
  baselineSelfIds: ReadonlySet<string>;
};

/** A stable empty list, so effects keyed on the snapshot's notifications do not re-run while no snapshot is loaded. */
const EMPTY_NOTIFICATIONS: MirroredNotification[] = [];
const PENDING_SEND_ECHO_WINDOW_MS = 5 * 60_000;
/** Recent wheel input permits temporary reaction-hint suppression while unfocused. */
const SCROLL_ATTENTION_MS = 1_000;
const EMPTY_REACTION_IDS: ReadonlySet<string> = new Set();

/**
 * Pending bubbles to show for the active conversation. Nothing is shown while a mid-history window is open.
 * A bubble hides once a self message outside its baseline is timestamped at or after the send (minus a
 * clock-skew window); an unparseable timestamp counts as a match.
 */
export function visiblePendingSends(pendingSends: PendingSend[], conversation: ConversationView | undefined, detached: boolean): PendingSend[] {
  if (detached || !conversation) return [];
  return pendingSends.filter((pending) => pending.conversationId === conversation.id
    && !conversation.messages.some((message) => {
      if (message.sender !== "self" || pending.baselineSelfIds.has(message.id)) return false;
      const sentAt = Date.parse(message.timestamp);
      return Number.isNaN(sentAt) || sentAt >= pending.createdAt - PENDING_SEND_ECHO_WINDOW_MS;
    }));
}

export function cancellationDisplayState(authoritative: CancellationState | undefined, local: LocalCancellationState | undefined): CancellationState | LocalCancellationState | undefined {
  if (local === "too-late" || local === "unknown" || (local === "failed" && authoritative !== "requested")) return local;
  return authoritative && authoritative !== "none" ? authoritative : local;
}
type DraftNoticeState = { key: string; event: DraftNoticeEvent | "stale-send" | "send-failed" | "loading"; sequence: number; detail?: string; persistent?: boolean };
type FocusIntent = {
  origin: HTMLElement;
  userMovedFocus: boolean;
  dispose(): void;
};
type PinFocusRestoreIntent = FocusIntent & { conversationId: string };

function watchFocusIntent(origin: HTMLElement | null): FocusIntent | undefined {
  if (!origin) return;
  const intent: FocusIntent = {
    origin,
    userMovedFocus: false,
    dispose: () => {
      document.removeEventListener("focusin", watchFocus);
      document.removeEventListener("pointerdown", watchPointer);
    },
  };
  function watchFocus(event: FocusEvent) {
    if (event.target instanceof HTMLElement && event.target !== document.body && event.target !== intent.origin)
      intent.userMovedFocus = true;
  }
  function watchPointer(event: PointerEvent) {
    if (!(event.target instanceof Node) || !intent.origin.contains(event.target)) intent.userMovedFocus = true;
  }
  document.addEventListener("focusin", watchFocus);
  document.addEventListener("pointerdown", watchPointer);
  return intent;
}

const MMS_WARNING_THRESHOLD = 0.8;

const isOlderDraftView = (current: Draft | undefined, incoming: Draft) => {
  if (!current || current.id !== incoming.id) return false;
  try {
    return BigInt(incoming.revision) < BigInt(current.revision);
  } catch {
    return false;
  }
};

export const nextComposerDraftIdentity = (
  previousSlotKey: string,
  previousIdentity: string,
  slotKey: string,
  resolve: (key: string) => string,
) => previousSlotKey && resolve(previousSlotKey) === slotKey
  ? previousIdentity
  : slotKey;

function mmsHeaderSize(estimatedBytes: number, limitBytes: number): { text: string; tone: "default" | "warning" | "error" } {
  if (estimatedBytes > limitBytes) return { text: `${fileSize(estimatedBytes)} of ${fileSize(limitBytes)}`, tone: "error" };
  if (estimatedBytes >= limitBytes * MMS_WARNING_THRESHOLD) return { text: `${fileSize(estimatedBytes)} of ${fileSize(limitBytes)}`, tone: "warning" };
  return { text: fileSize(estimatedBytes), tone: "default" };
}

export { errorText } from "./bridge/bridgeErrors";
export {
  DraftStore,
  shouldConfirmPostFlushDelete,
  type DraftSlot,
  type SaveOutcome,
} from "./drafts/DraftStore";

export const REACTION_HIGHLIGHT_MS = 1600;
const REACTION_NOT_FOUND = "The original message couldn't be found.";
const REACTION_LIMITED_SEARCH = "The original message wasn't found in the searched results. Load more history and try again.";
const REACTION_INDEXING = "Search is still indexing. Try again shortly.";

function reactionUnavailableCopy(result: ReactionTargetResult): string {
  if (result.state === "indexing") return REACTION_INDEXING;
  return result.state === "not-found" && result.exhausted === false ? REACTION_LIMITED_SEARCH : REACTION_NOT_FOUND;
}

/**
 * Display-only names for phone addresses. Stored conversation names, addresses and draft
 * recipient IDs are never changed; unresolved or ambiguous numbers show as formatted numbers.
 */
export function displayAddressName(address: string, resolution: ContactResolutionMap): string {
  return resolution[address]?.displayName ?? formatPhoneNumber(address);
}

export function displayConversation(
  name: string,
  participants: string[] | undefined,
  resolution: ContactResolutionMap,
  id: string,
  recipientless = false,
): { name: string; avatar: AvatarIdentity } {
  const parts = participants?.length ? participants : [name];
  const avatar = avatarFor(conversationAvatar({ id, name, participants, recipientless }), parts, resolution);
  if (!parts.some((part) => resolution[part])) return { name: formatPhoneNumber(name), avatar };
  return {
    name: parts.map((part) => displayAddressName(part, resolution)).join(", "),
    avatar,
  };
}

function avatarFor(identity: AvatarIdentity, addresses: string[], resolution: ContactResolutionMap): AvatarIdentity {
  const resolved = identity.kind === "person" && addresses.length === 1 ? resolution[addresses[0]] : undefined;
  return resolved ? { ...identity, photoDataUrl: resolved.photoDataUrl, name: resolved.displayName } : identity;
}

/* ------------------------------------------------------------------ labels and errors */

const STATUS_LABEL: Record<MessageStatus, string> = {
  "queued-local": "Queued locally",
  "server-accepted": "Server accepted",
  "gateway-persisted": "Received by phone",
  preparing: "Preparing",
  submitted: "Submitted",
  sent: "Sent",
  "delivery-confirmed": "Delivery confirmed",
  "failed-before-submit": "Failed before submit",
  "failed-confirmed": "Failed",
  unknown: "Unknown delivery state — not retried",
  read: "Read",
};

const TIMELESS_OUTGOING_STATUSES = new Set<MessageStatus>(["queued-local", "server-accepted"]);

function rcsComposerReason(block: ReturnType<typeof rcsSendBlock>): string | undefined {
  if (block.kind === "unproven") return peppyCopy.rcs_composer_reason_unproven;
  return block.kind === "link" ? peppyCopy.rcs_composer_reason_link : undefined;
}

type MessageTime = {
  displayedTime?: string;
  deliveryTimeFallback?: boolean;
  deliveryTimeUnavailable?: boolean;
};

function messageTime(message: MessageView): MessageTime {
  if (message.status === "delivery-confirmed") {
    if (isoDateTime(message.statusAt)) return { displayedTime: message.statusAt };
    if (isoDateTime(message.timestamp)) return { displayedTime: message.timestamp, deliveryTimeFallback: true };
    return { deliveryTimeUnavailable: true };
  }

  if (message.sender === "other" || !message.status) return { displayedTime: message.timestamp };
  return { displayedTime: TIMELESS_OUTGOING_STATUSES.has(message.status) ? undefined : message.statusAt };
}

const COMPOSER_MIN_HEIGHT = 96;
const COMPOSER_ATTACHMENT_MIN_HEIGHT = 132;
const RAIL_WIDTH = 48;
const LIST_MIN_WIDTH = 200;
const LIST_MAX_WIDTH = 480;
const PANE_MIN_WIDTH = 360;
const LIST_COLLAPSE_THRESHOLD = 120;
const TEXTAREA_LINE_HEIGHT = 22;
const TEXTAREA_MAX_HEIGHT = 176;
const FOLDER_SETTLE_MS = 300;
const NARROW_BROWSER_QUERY = "(max-width: 640px)";

function useNarrowBrowserViewport(browserHost: boolean): boolean {
  const [narrow, setNarrow] = useState(false);

  useEffect(() => {
    if (!browserHost || typeof window === "undefined" || typeof window.matchMedia !== "function") {
      setNarrow(false);
      return;
    }
    const media = window.matchMedia(NARROW_BROWSER_QUERY);
    const update = () => setNarrow(media.matches);
    update();
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, [browserHost]);

  return narrow;
}

function mergeMessages(existing: MessageView[], refreshed: MessageView[]): MessageView[] {
  const refreshedById = new Map(refreshed.map((message) => [message.id, message]));
  const merged = existing.map((message) => {
    const replacement = refreshedById.get(message.id);
    return replacement ? retainAttachmentPreviews(message, replacement) : message;
  });
  const existingIds = new Set(existing.map((message) => message.id));
  refreshed.forEach((message) => {
    if (!existingIds.has(message.id)) merged.push(message);
  });
  return merged;
}

function retainAttachmentPreviews(previous: MessageView, refreshed: MessageView): MessageView {
  const previousById = new Map(previous.attachments.map((attachment) => [attachment.id, attachment]));
  return {
    ...refreshed,
    attachments: refreshed.attachments.map((attachment) => {
      const prior = previousById.get(attachment.id);
      return prior ? retainThumbnailPreview(prior, retainOriginalPreview(prior, attachment)) : attachment;
    }),
  };
}

function isRetainableImagePreview(refreshed: AttachmentView): boolean {
  return refreshed.state === "ready" && refreshed.mediaType.startsWith("image/");
}

function retainOriginalPreview(previous: AttachmentView, refreshed: AttachmentView): AttachmentView {
  if (refreshed.previewUrl || !previous.previewUrl || !isRetainableImagePreview(refreshed)) return refreshed;
  return { ...refreshed, previewUrl: previous.previewUrl };
}

function retainThumbnailPreview(previous: AttachmentView, refreshed: AttachmentView): AttachmentView {
  const priorThumbnail = previous.mediaMeta?.thumbnail;
  const thumbnail = refreshed.mediaMeta?.thumbnail;
  if (!priorThumbnail?.previewUrl || !thumbnail || thumbnail.previewUrl) return refreshed;
  if (priorThumbnail.id !== thumbnail.id || !isRetainableImagePreview(thumbnail)) return refreshed;
  return { ...refreshed, mediaMeta: { ...refreshed.mediaMeta, thumbnail: { ...thumbnail, previewUrl: priorThumbnail.previewUrl } } };
}

function mergeAnchoredMessages(existing: MessageView[], refreshed: MessageView[]): MessageView[] {
  const existingById = new Map(existing.map((message) => [message.id, message]));
  const anchor = refreshed[0]?.id;
  const prefixEnd = anchor ? existing.findIndex((message) => message.id === anchor) : -1;
  const prefix = prefixEnd > 0 ? existing.slice(0, prefixEnd) : [];
  const authoritativeRows = refreshed.map((message) => {
    const previous = existingById.get(message.id);
    return previous ? retainAttachmentPreviews(previous, message) : message;
  });
  return mergeMessages(prefix, authoritativeRows);
}

function attachmentState(file: AttachmentView): string {
  if (file.state === "failed")
    return `Failed${file.error ? `: ${file.error}` : ""}`;
  return file.state === "uploading"
    ? "Uploading…"
    : file.state.charAt(0).toUpperCase() + file.state.slice(1);
}

function videoAspectRatio(mediaMeta: AttachmentView["mediaMeta"]): string | undefined {
  const width = mediaMeta?.width;
  const height = mediaMeta?.height;
  return typeof width === "number" && Number.isFinite(width) && width > 0
    && typeof height === "number" && Number.isFinite(height) && height > 0
    ? `${width} / ${height}`
    : undefined;
}

/* ------------------------------------------------------------------ window context */

/** Composer windows are opened natively as `index.html?window=composer&conversationId=<uuid>`. */
export function readComposerConversation(search: string): string | null {
  const params = new URLSearchParams(search);
  const id = params.get("conversationId");
  return params.get("window") === "composer" &&
    id &&
    /^[A-Za-z0-9_-]{1,128}$/.test(id)
    ? id
    : null;
}

/* ------------------------------------------------------------------ gateway routes */

/** Route badge text for the Send again popover: the composer's SIM and availability labels, paused wording included. */
export function sendAgainRouteLabel(gateways: GatewayView[], accountReadOnly = false): SendAgainRouteLabeler {
  const labels = simLabels(gateways);
  return (gateway) => ({
    simLabel: gateway.simId ? (labels.get(routeKey(gateway.id, gateway.simId)) ?? gateway.simId) : undefined,
    statusLabel: availabilityLabel(gateway, accountReadOnly),
  });
}

/** Gateway and SIM together identify a route; one device can expose several SIMs. */
const routeKey = (gatewayId: string, simId: string) =>
  `${encodeURIComponent(gatewayId)} ${encodeURIComponent(simId)}`;

function formatAge(seconds: number): string { return seconds < 60 ? "under a minute" : seconds < 3_600 ? `${Math.floor(seconds / 60)}m` : `${Math.floor(seconds / 3_600)}h`; }
function simLabels(gateways: GatewayView[]): Map<string, string> {
  const labels = new Map<string, string>();
  const simCounts = new Map<string, number>();
  gateways.filter((gateway) => gateway.simId).forEach((gateway) => {
    const key = routeKey(gateway.id, gateway.simId);
    if (labels.has(key)) return;
    const simCount = (simCounts.get(gateway.id) ?? 0) + 1;
    simCounts.set(gateway.id, simCount);
    labels.set(key, `SIM ${simCount}`);
  });
  return labels;
}
/** While the account is read-only, a stale SIM route reads as paused, with the age kept as secondary text. */
function availabilityLabel(gateway: GatewayView, accountReadOnly = false): string {
  switch (gatewayAvailability(gateway)) {
    case "delayed": {
      const seen = gateway.lastSeenAgeSeconds !== undefined ? `Last seen ${formatAge(gateway.lastSeenAgeSeconds)} ago` : "Last seen";
      return accountReadOnly ? `${peppyCopy.account_send_paused_status}, ${seen}` : seen;
    }
    case "setting_up": return peppyCopy.gateway_status_setting_up;
    case "offline": return "Not responding";
    case "never_connected": return "Never connected";
    case "no_sms_sim": return "No SMS SIM";
    case "sms_permission_needed": return "Permission needed";
    case "sms_unavailable": return "SMS unavailable";
    case "unsupported_platform": return "Can't send SMS";
    case "capabilities_unknown": case "unknown": return "Status unknown";
    case "connected": return "Connected";
    default: return "Status unknown";
  }
}

/**
 * The stored route must match a reported gateway+SIM exactly; a vanished route is reported as
 * stale and never replaced. Only a draft without any stored route on a single-route setup uses
 * that one route implicitly.
 */
function resolveRoute(
  content: DraftContent,
  gateways: GatewayView[],
): RouteChoice {
  if (content.gatewayId && content.simId) return resolveStoredRoute({ gatewayId: content.gatewayId, simId: content.simId }, gateways);
  return gateways.length === 1
    ? { kind: "route", gateway: gateways[0], implicit: true }
    : { kind: "none" };
}

const requiresMms = (content: DraftContent, conversation?: ConversationView) =>
  content.recipientIds.length > 1 ||
  content.attachmentIds.length > 0 ||
  conversation?.messages.at(-1)?.transport === "mms";
/** Native participants are reply addressees and exclude the confirmed self address. */
const recipientCount = (content: DraftContent, conversation?: ConversationView) =>
  content.recipientIds.length || conversation?.participants?.length || 1;
const draftEstimateBytes = (content: DraftContent, attachments: Record<string, AttachmentView>) =>
  new TextEncoder().encode(content.text).length + content.attachmentIds.reduce((total, id) => total + (attachments[id]?.byteSize ?? 0), 0);
const planDraftMms = (content: DraftContent, route: RouteChoice, attachments: Record<string, AttachmentView>) =>
  planMms({
    limitBytes: route.kind === "route" ? route.gateway.mmsMaxBytes ?? DEFAULT_MMS_LIMIT_BYTES : DEFAULT_MMS_LIMIT_BYTES,
    gatewayVersion: route.kind === "route" ? route.gateway.mmsContentVersion : undefined,
    text: content.text,
    images: content.attachmentIds.map((id) => ({
      id,
      bytes: attachments[id]?.byteSize ?? 0,
      mediaType: attachments[id]?.mediaType ?? "",
    })),
  });

/* ------------------------------------------------------------------ presentational pieces */

type Theme = "light" | "dark" | "system";

function rcsStatusPresentation(block: ReturnType<typeof rcsSendBlock>) {
  if (block.kind === "unproven") return { label: peppyCopy.rcs_composer_badge_unproven, message: peppyCopy.rcs_composer_status_unproven };
  if (block.kind === "link") return block.help === "relink"
    ? { label: peppyCopy.rcs_composer_badge_relink, message: peppyCopy.rcs_composer_status_relink }
    : { label: peppyCopy.rcs_composer_badge_phone, message: peppyCopy.rcs_composer_status_phone };
  return undefined;
}

function GatewaySelector({
  gateways,
  content,
  problem,
  onSelect,
  accountReadOnly = false,
  rcsBlock = { kind: "none" },
}: {
  gateways: GatewayView[];
  content: DraftContent;
  problem?: string;
  onSelect(gateway: GatewayView): void;
  accountReadOnly?: boolean;
  rcsBlock?: ReturnType<typeof rcsSendBlock>;
}) {
  const route = resolveRoute(content, gateways);
  const labels = simLabels(gateways);
  const value =
    route.kind === "route"
      ? routeKey(route.gateway.id, route.gateway.simId)
      : route.kind === "stale"
        ? routeKey(route.gatewayId, route.simId)
        : "";
  const choose = (key: string) => {
    const gateway = gateways.find(
      (item) => routeKey(item.id, item.simId) === key,
    );
    if (gateway) onSelect(gateway);
  };
  const simLabel = (gateway: GatewayView) => gateway.simId ? (labels.get(routeKey(gateway.id, gateway.simId)) ?? gateway.simId) : "No SMS SIM";
  const selectedGateway = route.kind === "route" ? route.gateway : undefined;
  const selectedAvailability = selectedGateway && gatewayAvailability(selectedGateway);
  // The finished-setup hint says "You can send now", so it is raised only for a phone that is actually
  // sendable (the composer's own rule), and cleared as soon as that stops being true.
  const selectedSettingUp = Boolean(settingUpGatewayFor(route, gateways));
  const phoneId = settingUpGatewayFor(route, gateways)?.id ?? selectedGateway?.id;
  const sendableNow = Boolean(selectedGateway) && routeProblem(route, gateways, false, 1, { kind: "none" } as never) === undefined;
  const [finishedSetup, setFinishedSetup] = useState<{ id: string; name: string }>();
  const setupTracker = useRef<{ id?: string; settingUp: boolean }>({ settingUp: false });
  // The phone counts as "was setting up" until it is seen sendable; the flip to not-setting-up alone is not enough.
  const settingUpNow = Boolean(settingUpGatewayFor(route, gateways));
  useEffect(() => {
    const previous = setupTracker.current;
    if (previous.id !== phoneId) {
      setupTracker.current = { id: phoneId, settingUp: settingUpNow };
      setFinishedSetup(undefined);
      return;
    }
    if (settingUpNow) {
      setupTracker.current = { id: phoneId, settingUp: true };
      setFinishedSetup(undefined);
      return;
    }
    if (!sendableNow) {
      setFinishedSetup(undefined);
      return;
    }
    if (previous.settingUp && selectedGateway) {
      setupTracker.current = { id: phoneId, settingUp: false };
      setFinishedSetup({ id: selectedGateway.id, name: selectedGateway.name });
    }
  }, [phoneId, settingUpNow, sendableNow, selectedGateway]);
  const finishedSetupName = finishedSetup && finishedSetup.id === phoneId ? finishedSetup.name : undefined;
  const finishedSetupHint = finishedSetupName ? formatCopy(peppyCopy.gateway_presence_finished_setup, finishedSetupName) : undefined;
  const presenceHint = finishedSetupHint ?? (selectedGateway && (availabilityProblem(selectedGateway) ?? (selectedAvailability === "unknown" ? `${selectedGateway.name}'s status is unknown.` : undefined)));
  const rcsStatus = rcsStatusPresentation(rcsBlock);
  return (
    <section id="gateway-selector" aria-label="Phone and SIM" aria-describedby={presenceHint ? "gateway-presence-hint" : undefined} data-route-state={problem ? "blocked" : selectedAvailability === "delayed" ? "delayed" : "ready"} data-route-reason={selectedSettingUp ? "setting-up" : undefined}>
      <GatewayPicker
        options={[
          ...(route.kind === "stale" && !settingUpGatewayFor(route, gateways)
            ? [{
                value,
                name: "Unavailable route",
                simLabel: `SIM ${route.simId}`,
                disabled: true,
                gatewayId: route.gatewayId,
                simId: route.simId,
              }]
            : []),
          ...gateways.map((item) => ({
            value: routeKey(item.id, item.simId),
            name: item.name,
            simLabel: isSettingUp(item) ? undefined : simLabel(item),
            statusLabel: availabilityLabel(item, accountReadOnly),
            simulated: item.simulated,
            availability: gatewayAvailability(item),
            // A setting-up phone is listed and explained, but cannot be chosen (ux-design 6.1, ruling R13).
            disabled: isSettingUp(item) || gatewayAvailability(item) === "unsupported_platform",
            disabledReason: isSettingUp(item) ? importingRefusal(item, gateways) : undefined,
            gatewayId: item.id,
            simId: item.simId,
          })),
        ]}
        value={value}
        placeholder={gateways.length ? "Choose phone and SIM" : "No phone"}
        disabled={!gateways.length}
        onChange={choose}
        describedBy={presenceHint ? "gateway-presence-hint" : undefined}
        valueStatusOverride={rcsStatus ? { label: rcsStatus.label, availability: "unsupported_platform", title: rcsStatus.message } : undefined}
      />
      <span id="gateway-presence-hint" className="visually-hidden" role="status" aria-live="polite">{presenceHint}</span>
    </section>
  );
}

const seenText = (gateway: GatewayView) => gateway.lastSeenAgeSeconds === undefined ? "Last seen" : `Seen ${formatAge(gateway.lastSeenAgeSeconds)} ago`;

export function headRoutePresentation(route: RouteChoice, gateways: GatewayView[], accountReadOnly = false): { sim?: string; state: string; label: string } {
  if (route.kind === "none") {
    const state = gateways.length ? "No SIM chosen" : "No phone";
    return { state, label: state };
  }
  if (liveSettingUpGatewayFor(route, gateways)) return { state: peppyCopy.gateway_status_setting_up, label: peppyCopy.gateway_status_setting_up };
  if (route.kind === "stale") return { state: "SIM unavailable", label: "SIM unavailable" };
  const simLabel = simLabels(gateways).get(routeKey(route.gateway.id, route.gateway.simId)) ?? route.gateway.simId;
  if (gatewayAvailability(route.gateway) === "no_sms_sim") return { state: "No SMS SIM", label: "No SMS SIM" };
  let state: string;
  switch (gatewayAvailability(route.gateway)) {
    case "connected": state = "Online"; break;
    case "delayed": state = accountReadOnly ? `${peppyCopy.account_send_paused_status}, ${seenText(route.gateway)}` : seenText(route.gateway); break;
    case "offline": state = "Not responding"; break;
    default: state = availabilityLabel(route.gateway, accountReadOnly);
  }
  return { sim: simLabel, state, label: `${simLabel}, ${state}` };
}

function HeadRoutePill({ content, gateways, accountReadOnly = false, rcsBlock = { kind: "none" } }: { content: DraftContent; gateways: GatewayView[]; accountReadOnly?: boolean; rcsBlock?: ReturnType<typeof rcsSendBlock> }) {
  const route = resolveRoute(content, gateways);
  const { sim, state, label } = headRoutePresentation(route, gateways, accountReadOnly);
  const rcsStatus = rcsStatusPresentation(rcsBlock);
  const shownState = rcsStatus?.label ?? state;
  // Live wording comes from availability: an importing phone whose presence is offline or delayed is
  // presented by that presence, while its send refusal still holds (settingUpRoute.ts).
  const liveSetup = liveSettingUpGatewayFor(route, gateways);
  // A setting-up phone is not a route choice: its tooltip and description state the setup, not "choose".
  const settingUpMessage = liveSetup ? formatCopy(peppyCopy.gateway_problem_setting_up, liveSetup.name) : undefined;
  const tooltip = settingUpMessage ?? rcsStatus?.message ?? (route.kind === "route"
    ? `${route.gateway.name}: choose this phone and SIM in the main window.`
    : "Choose a phone and SIM in the main window.");
  const description = settingUpMessage ?? (route.kind === "route"
    ? `${route.gateway.name}, ${sim ?? shownState}. ${rcsStatus?.message ?? "Phone and SIM are selected in the main window."}`
    : "Choose a phone and SIM in the main window.");
  const routeState = liveSetup ? "setting_up" : route.kind === "route" ? gatewayAvailability(route.gateway) : route.kind;
  return <section id="head-panel-route-section" aria-label="Phone and SIM" aria-describedby="head-panel-route-description" data-route-state={routeState}>
    <span id="head-panel-route" className="head-panel-route" title={tooltip} data-gateway-availability={route.kind === "route" ? gatewayAvailability(route.gateway) : undefined}>{sim && <span data-head-route-sim data-route-badge="sim">{sim}</span>}<span data-head-route-state data-route-badge="status" data-route-send-state={rcsStatus ? "blocked" : undefined} data-gateway-availability={route.kind === "route" ? gatewayAvailability(route.gateway) : undefined}>{shownState}</span></span>
    <span id="head-panel-route-description" className="visually-hidden">{description}</span>
    <span className="visually-hidden" role="status" aria-live="polite">{rcsStatus ? `${sim ?? "Phone and SIM"}, ${shownState}. ${rcsStatus.message}` : label}</span>
  </section>;
}

type RowRegistry = (id: string, row: HTMLElement | null) => void;

/** Per-send options the group refusal actions use. Omitted = the latest group revision, no duplicate confirmation. */
type GroupSendOptions = { expectedMemberRevision?: string; groupCreateConfirmDuplicate?: boolean };

export function MessageMeta({ message: source, hideTime = false, onEditResend, onCreateAgain, resend }: { message: MessageView; hideTime?: boolean; onEditResend?(): void; onCreateAgain?(): void; resend?: ResendChain }) {
  const [dialog, setDialog] = useState<"details" | "history">();
  const detailsOpener = useRef<HTMLButtonElement>(null);
  const historyOpener = useRef<HTMLButtonElement>(null);
  const store = useContext(SendAgainStoreContext);
  useLayoutEffect(() => {
    if (resend?.attempts.slice(0, -1).some((attempt) => store?.consumeDoneFocus(attempt.id))) historyOpener.current?.focus();
  }, [resend, store]);
  const message = rcsOutcomeStatus(source);
  const { displayedTime, deliveryTimeFallback, deliveryTimeUnavailable } = messageTime(message);
  const formattedTime = formatDateTime(displayedTime);
  const isoTime = isoDateTime(displayedTime);
  const deliveryConfirmed = message.status === "delivery-confirmed";
  const rcsMessage = message.transport === "rcs";
  const read = rcsMessage && message.status === "read";
  const phoneHistory = !rcsMessage && message.sender === "self" && !message.status && Boolean(formattedTime);
  const compactStatus = message.status && ["gateway-persisted", "preparing", "submitted", "sent"].includes(message.status);
  const recovery = message.sendRecovery;
  const sentAgain = message.sentAgain?.messageId ?? recovery?.sentAgain?.messageId ?? store?.sentAgainFor(message.id);
  const statusLabel = recovery ? SEND_RECOVERY_STATUS[recovery.kind].label : message.status ? STATUS_LABEL[message.status] : undefined;
  const validTime = Boolean(formattedTime && isoTime);
  const showTime = validTime && !hideTime;
  const deliveryLabel = deliveryTimeFallback
    ? hideTime
      ? "Delivery confirmed; delivery time unavailable"
      : "Delivery confirmed; sent time shown because delivery time is unavailable"
    : deliveryTimeUnavailable
      ? "Delivery confirmed; time unavailable"
      : "Delivery confirmed";
  const statusIcon = message.status === "sent" ? <Check size={10} aria-hidden /> : message.status === "read" ? <CheckCheck size={10} aria-hidden /> : <Clock size={10} aria-hidden />;
  const showTextStatus = !read && !deliveryConfirmed && Boolean(statusLabel) && (Boolean(recovery) || !compactStatus || !validTime);
  const showCompactStatus = !deliveryConfirmed && !recovery && Boolean(compactStatus && validTime);
  const hasRcsMediaIssues = rcsMessage && (message.attachments.some((attachment) => ["not_sent", "send_failed", "outcome_unknown", "members_changed"].includes(attachment.rcsSendStatus ?? "")) || message.attachments.filter((attachment) => canRetryRcsDownload(attachment.rcsState)).length >= 2);
  if (!showTextStatus && !showTime && !showCompactStatus && !deliveryConfirmed && !phoneHistory && !read && !message.groupHint && !hasRcsMediaIssues && !recovery && !sentAgain && !resend) return null;
  const historyButton = resend && <button ref={historyOpener} type="button" className={`message-meta-action ${deliveryConfirmed ? "message-meta-delivered-action" : "message-meta-text-action"}`} id={`message-send-history-${message.id}`} aria-haspopup="dialog" aria-label={formatCopy(deliveryConfirmed ? peppyCopy.message_send_history_delivered_label : peppyCopy.message_send_history_label, String(resend.attempts.length))} title={deliveryConfirmed ? "Delivery confirmed" : undefined} onClick={() => setDialog("history")}>{deliveryConfirmed ? <CheckCheck size={10} aria-hidden /> : <><History size={12} aria-hidden />{peppyCopy.message_send_history_action}</>}</button>;
  const closeDialog = () => setDialog(undefined);
  return <div className="message-meta" data-message-meta={message.id} data-status={message.status} aria-live={rcsMessage ? "off" : undefined}>
    {resend && deliveryConfirmed ? historyButton : <>
    {showTextStatus && <span className="message-meta-status" data-recovery-tone={recovery ? SEND_RECOVERY_STATUS[recovery.kind].tone : undefined}>{recovery ? (recovery.kind === "not-sent" ? <X size={10} aria-hidden /> : <TriangleAlert size={10} aria-hidden />) : ["failed-before-submit", "failed-confirmed"].includes(message.status!) ? <X size={10} aria-hidden /> : message.status === "unknown" ? <TriangleAlert size={10} aria-hidden /> : statusIcon}{statusLabel}</span>}
    {recovery && <button ref={detailsOpener} type="button" className="message-meta-action" id={`message-send-details-${message.id}`} aria-haspopup="dialog" aria-label={formatCopy(peppyCopy.message_send_details_label, statusLabel ?? "")} onClick={() => setDialog("details")}><Info size={12} aria-hidden /></button>}
    {read && <RcsMessageMeta messageId={message.id} status={message.status} statusLabel={STATUS_LABEL[message.status!]} groupHint={message.groupHint} attachments={message.attachments} />}
    {!recovery && !resend && (showTextStatus || read) && showTime && <span className="message-meta-separator" aria-hidden>·</span>}
    {showTime && <time dateTime={isoTime}>{formattedTime}</time>}
    {showCompactStatus && <span className="message-meta-status" role="img" aria-label={statusLabel} title={statusLabel}>{statusIcon}</span>}
    {deliveryConfirmed && <span className="message-meta-status" role="img" aria-label={deliveryLabel} title={deliveryLabel}><CheckCheck size={10} aria-hidden /></span>}
    {phoneHistory && <span className="message-meta-status" role="img" aria-label="From phone history" title="From phone history" data-message-phone-icon={message.id}><Smartphone size={10} aria-hidden /></span>}
    {(rcsMessage || message.groupHint) && !read && <RcsMessageMeta messageId={message.id} status={message.status} rcsReported={message.rcsReported} statusLabel={message.status ? STATUS_LABEL[message.status] : ""} groupHint={message.groupHint} onEditResend={onEditResend} onCreateAgain={onCreateAgain} attachments={message.attachments} />}
    {resend && <><span className="message-sent-again-fact">{peppyCopy.message_send_again_sent}</span>{historyButton}</>}
    {(recovery || sentAgain) && <SendRecoveryNotice message={message} />}
    {resend && resend.earlierRisk !== "none" && <span className="message-resend-warning"><TriangleAlert size={12} aria-hidden />{resend.earlierRisk === "sent" ? peppyCopy.message_send_earlier_was_sent : peppyCopy.message_send_earlier_may_have_sent}</span>}
    </>}
    {dialog === "details" && recovery && <Modal id={`send-details-${message.id}`} title={statusLabel} opener={detailsOpener} onDismiss={closeDialog} actions={<button type="button" className="secondary-button" onClick={closeDialog}>{peppyCopy.message_send_history_close}</button>}><p>{SEND_RECOVERY_REASON[recovery.kind]}</p></Modal>}
    {dialog === "history" && resend && <Modal id={`send-history-${message.id}`} title={peppyCopy.message_send_history_title} opener={historyOpener} onDismiss={closeDialog} actions={<button type="button" className="secondary-button" onClick={closeDialog}>{peppyCopy.message_send_history_close}</button>}><ul className="message-send-history" data-send-history-list={message.id}>{[...resend.attempts].reverse().map((attempt, index) => {
      const time = messageTime(attempt).displayedTime;
      const label = attempt.sendRecovery ? SEND_RECOVERY_STATUS[attempt.sendRecovery.kind].label : attempt.status ? STATUS_LABEL[attempt.status] : undefined;
      const tone = attempt.sendRecovery ? SEND_RECOVERY_STATUS[attempt.sendRecovery.kind].tone : attempt.status === "failed-before-submit" || attempt.status === "failed-confirmed" ? "error" : attempt.status === "unknown" ? "warning" : undefined;
      const icon = tone === "error" ? <X size={10} aria-hidden /> : tone === "warning" ? <TriangleAlert size={10} aria-hidden /> : attempt.status === "delivery-confirmed" || attempt.status === "read" ? <CheckCheck size={10} aria-hidden /> : attempt.status === "sent" ? <Check size={10} aria-hidden /> : <Clock size={10} aria-hidden />;
      return <li key={attempt.id} data-send-attempt={attempt.id}><div className="message-history-heading"><strong>{index === 0 ? peppyCopy.message_send_history_latest : formatCopy(peppyCopy.message_send_history_attempt, String(resend.attempts.length - index))}</strong>{formatDateTime(time) && <time dateTime={isoDateTime(time)}>{formatDateTime(time)}</time>}</div>{label && <span className="message-meta-status" data-recovery-tone={tone}>{icon}{label}</span>}{attempt.sendRecovery && <p>{SEND_RECOVERY_REASON[attempt.sendRecovery.kind]}</p>}</li>;
    })}</ul></Modal>}
  </div>;
}

function RcsAttachmentStatus({ attachment, visible }: { attachment: AttachmentView; visible: boolean }) {
  if (!visible || !attachment.rcsSendStatus) return null;
  const text = attachment.rcsSendStatus === "sending" ? peppyCopy.rcs_media_state_sending
    : attachment.rcsSendStatus === "sent" ? peppyCopy.rcs_media_state_sent
      : attachment.rcsSendStatus === "outcome_unknown" ? peppyCopy.rcs_media_state_unknown
        : attachment.rcsSendStatus === "send_failed" ? peppyCopy.rcs_media_state_send_failed
        : peppyCopy.rcs_media_state_not_sent;
  const icon = attachment.rcsSendStatus === "sending" ? <Loader2 className="tile-action-spinner" size={12} aria-hidden />
    : attachment.rcsSendStatus === "sent" ? <Check size={12} aria-hidden />
      : attachment.rcsSendStatus === "outcome_unknown" || attachment.rcsSendStatus === "send_failed" ? <TriangleAlert size={12} aria-hidden />
        : <X size={12} aria-hidden />;
  return <small data-rcs-media-status data-rcs-media-state={attachment.rcsSendStatus} data-rcs-media-terminal-failure={attachment.rcsSendStatus === "send_failed" || undefined}>{icon}{text}</small>;
}

/** Image-only MMS and RCS messages render as media tiles, not as articles, so they are not keyboard roving stops. */
function isImageRun(message: MessageView): boolean {
  return (message.transport === "mms" || message.transport === "rcs") && message.attachments.length > 0 && message.attachments.every((attachment) => attachment.mediaType.startsWith("image/"));
}

/** Status of an outgoing reaction text, mirrored read-only on its text-sourced own chip. */
function textChipState(status: MessageStatus | undefined): OwnChipState {
  if (status === "queued-local" || status === "server-accepted" || status === "gateway-persisted" || status === "preparing" || status === "submitted") return "sending";
  if (status === "failed-before-submit" || status === "failed-confirmed") return "not_sent";
  if (status === "unknown") return "unknown";
  return "sent";
}

function withTextStatus(badges: readonly MergedReactionBadge[], state: OwnChipState | undefined): MergedReactionBadge[] {
  if (!state || state === "sent") return [...badges];
  return badges.map((badge) => badge.own?.source === "text" ? { ...badge, own: { ...badge.own, state } } : badge);
}

type PendingRequest = { request: OwnReactionRequestView; /** The snapshot sequence when the overlay was installed. */ installedAt: number };

/** A copy of the map without one key; the same map when the key is absent, so React skips the update. */
function withoutEntry<T>(map: ReadonlyMap<string, T>, key: string): ReadonlyMap<string, T> {
  if (!map.has(key)) return map;
  const next = new Map(map);
  next.delete(key);
  return next;
}

function showsReactionNoticeMeta(message: MessageView): boolean {
  return message.sender === "self" && Boolean(message.status && ["failed-before-submit", "failed-confirmed", "unknown"].includes(message.status));
}

type ReactionLayoutSnapshot = { bottom: boolean; anchor: { row: HTMLElement; top: number } | null };
type ReactionHintLayoutProps = {
  suppressed: ReadonlySet<string>;
  capture(): ReactionLayoutSnapshot | null;
  restore(snapshot: ReactionLayoutSnapshot): void;
  children: ReactNode;
};

class ReactionHintLayout extends Component<ReactionHintLayoutProps, Record<string, never>, ReactionLayoutSnapshot | null> {
  getSnapshotBeforeUpdate(previous: ReactionHintLayoutProps): ReactionLayoutSnapshot | null {
    const changed = previous.suppressed.size !== this.props.suppressed.size
      || [...this.props.suppressed].some(id => !previous.suppressed.has(id));
    return changed ? this.props.capture() : null;
  }

  componentDidUpdate(_previous: ReactionHintLayoutProps, _state: Record<string, never>, snapshot: ReactionLayoutSnapshot | null) {
    if (snapshot) this.props.restore(snapshot);
  }

  render() {
    return this.props.children;
  }
}

function PendingSendRow({ pending, message, attachmentViews, onOpenExternal }: {
  pending: PendingSend;
  message: MessageView;
  attachmentViews: Record<string, AttachmentView>;
  onOpenExternal?: (url: string) => Promise<void>;
}) {
  return (
    <div
      className="message-row self"
      data-pending-send-id={pending.id}
      data-sender="self"
      aria-hidden="true"
    >
      <article className="message-bubble self" data-pending-send-id={pending.id}>
        {message.body && <p><MessageText text={message.body} onOpenExternal={onOpenExternal} /></p>}
        {pending.content.attachmentIds.map((id) => {
          const attachment = attachmentViews[id];
          return (
            <div key={id} className="file-card" data-pending-attachment-id={id}>
              {attachment?.mediaType.startsWith("image/") ? <Image size={16} aria-hidden /> : <FileText size={16} aria-hidden />}
              <span><bdi>{attachment?.name ?? "Attachment"}</bdi></span>
              {attachment && <small>{fileSize(attachment.byteSize)}</small>}
            </div>
          );
        })}
      </article>
      <MessageMeta message={message} />
    </div>
  );
}

function MessageList({
  conversation,
  loading,
  registerRow,
  publicCopies,
  onPublish,
  onRetryAttachment,
  onSaveAttachment,
  listRef,
  onScroll,
  onWheel,
  hasOlderMessages,
  historyLoading,
  historyError,
  historyErrorDirection,
  onLoadOlder,
  suppressHistoryAnnouncements,
  historyExhausted,
  onStartConversation,
  onOpenMedia,
  requestOriginal,
  checkOriginal,
  releaseMedia,
  mediaRetryKey,
  onOpenExternal,
  onCopyOneTimeCode,
  searchTargetId,
  detachedHistory,
  onLoadNewer,
  onGoToLatest,
  compactTimestamps,
  showSenderAvatars,
  onOpenSender,
  senderLocked,
  resolution,
  showReactions,
  writesPaused,
  actorName,
  onOpenReactionTarget,
  pendingReactionId,
  reactionFeedback,
  seenReactions,
  pendingSeenReactionIds,
  badgeObservation,
  captureReactionLayout,
  restoreReactionLayout,
  toneOverrides = {},
  onEditGroupResend,
  onEditRcsUnsent,
  onCreateGroupAgain,
  takeSendCancellationSequence,
  onSendCancellationRequested,
  pendingSends,
  attachmentViews,
  typingRow,
  reactionRouteOffline = false,
}: {
  conversation?: ConversationView;
  /** The gateway that carries this conversation's reactions is not online. */
  reactionRouteOffline?: boolean;
  /** Copies a refused group message's body back into the composer. Nothing is sent. */
  onEditGroupResend?(message: MessageView): void;
  /** Copies an RCS message's unsent attachments (and its text, when the text carrier was unsent) back into the composer. Nothing is sent. */
  onEditRcsUnsent?(message: MessageView): void;
  /** Copies an unconfirmed group create's body back into the composer behind its confirmation. Nothing is sent. */
  onCreateGroupAgain?(message: MessageView): void;
  takeSendCancellationSequence?(): number;
  onSendCancellationRequested?(message: MessageView, sequence: number): void;
  loading: boolean;
  registerRow: RowRegistry;
  publicCopies: Record<string, PublicCopy>;
  onPublish(attachment: AttachmentView): void;
  onRetryAttachment(attachment: AttachmentView): void;
  onSaveAttachment(attachment: AttachmentView): void;
  listRef: RefCallback<HTMLDivElement>;
  onScroll(): void;
  onWheel(event: WheelEvent<HTMLDivElement>): void;
  hasOlderMessages: boolean;
  historyLoading: boolean;
  historyError: string | null;
  onLoadOlder(control: HTMLElement): void;
  historyErrorDirection: "older" | "newer";
  suppressHistoryAnnouncements: boolean;
  historyExhausted: boolean;
  onStartConversation?: () => void;
  onOpenMedia(item: { attachment: AttachmentView; message: MessageView }): void;
  requestOriginal(id: string): Promise<{ state: "ready"; url: string } | { state: "pending" }>;
  checkOriginal(id: string): Promise<{ state: "ready"; url: string } | { state: "pending" }>;
  releaseMedia(id: string, url: string): void;
  mediaRetryKey: unknown;
  onOpenExternal?: (url: string) => Promise<void>;
  onCopyOneTimeCode(target: OneTimeCodeTarget, code: string): Promise<void>;
  searchTargetId?: string;
  detachedHistory?: DetachedHistory;
  onLoadNewer(control: HTMLElement): void;
  onGoToLatest(): void;
  compactTimestamps: boolean;
  showSenderAvatars: boolean;
  onOpenSender?: (address: string) => void;
  senderLocked: boolean;
  resolution: ContactResolutionMap;
  showReactions: boolean;
  writesPaused: boolean;
  actorName: ActorName;
  onOpenReactionTarget(reaction: MessageView, notice: ReactionNoticeInfo): void;
  pendingReactionId?: string;
  reactionFeedback?: { reactionId: string; message: string };
  seenReactions: SeenReactionLedger;
  pendingSeenReactionIds: ReadonlySet<string>;
  badgeObservation: { root: HTMLElement | null; obscuredBottom: number | null; enabled: boolean; canRecord(): boolean; onSeen(ids: readonly string[]): void };
  captureReactionLayout(): ReactionLayoutSnapshot | null;
  restoreReactionLayout(snapshot: ReactionLayoutSnapshot): void;
  toneOverrides?: Record<string, number>;
  pendingSends: PendingSend[];
  attachmentViews: Record<string, AttachmentView>;
  typingRow?: TypingRowData;
}) {
  const sendAgainStore = useContext(SendAgainStoreContext);
  const reactions = useMemo(() => showReactions && conversation ? linkReactions(conversation.messages) : NO_REACTIONS, [showReactions, conversation?.messages]);
  const [focusedNoticeId, setFocusedNoticeId] = useState<string>();
  const reactionsEnabled = Boolean(bridge.react_to_message) && Boolean(conversation);
  // A callback ref in state, so the listeners re-attach whenever the content node is replaced.
  const [reactionContent, setReactionContent] = useState<HTMLDivElement | null>(null);
  const reactionBarRef = useRef<HTMLDivElement>(null);
  const reaction = useReactionTarget(reactionContent, reactionBarRef, reactionsEnabled);
  const reactionEligible = (message: MessageView) => reactionsEnabled && isReactionEligible(message, conversation?.reactionSupport);
  // Optimistic own requests (ux-design D). The overlay is only a bridge over the gap between an accepted call and the
  // next snapshot. Any snapshot that arrives after the overlay was installed is authoritative for its message, whether
  // or not it carries the request and whether or not the reactions changed (review round 2, R2-C).
  const [pendingRequests, setPendingRequests] = useState<ReadonlyMap<string, PendingRequest>>(new Map());
  const pendingRef = useRef(pendingRequests);
  pendingRef.current = pendingRequests;
  const snapshotSeq = useRef(0);
  const effectiveMessages = useMemo(() => (conversation?.messages ?? []).map((message) => {
    const pending = pendingRequests.get(message.id);
    return pending && !message.ownReactionRequest ? { ...message, ownReactionRequest: pending.request } : message;
  }), [conversation?.messages, pendingRequests]);
  const messagesById = useMemo(() => new Map(effectiveMessages.map((message) => [message.id, message])), [effectiveMessages]);
  // Each delivered snapshot advances the sequence. An overlay installed at sequence N is cleared by the first snapshot
  // after N. A cleared overlay whose snapshot no longer carries a request settles the outcome, announced as usual.
  useEffect(() => {
    snapshotSeq.current += 1;
    const seq = snapshotSeq.current;
    const current = pendingRef.current;
    if (!current.size) return;
    const messages = conversation?.messages ?? [];
    const cleared: string[] = [];
    for (const [messageId, pending] of current) {
      if (seq <= pending.installedAt) continue;
      cleared.push(messageId);
      const message = messages.find((candidate) => candidate.id === messageId);
      if (!message || message.ownReactionRequest) continue;
      const copy = requestAnnouncement(pending.request, undefined, nativeSelection(message));
      if (copy.alert) announceReaction(copy.alert, true);
      else if (copy.polite) announceReaction(copy.polite);
    }
    if (!cleared.length) return;
    const next = new Map(current);
    for (const messageId of cleared) next.delete(messageId);
    pendingRef.current = next;
    setPendingRequests(next);
  }, [conversation?.messages]);
  // Keyboard stops are articles only: media runs have their own tile focus, so they are not roving stops.
  const roving = useRovingMessages(useMemo(() => (conversation?.messages ?? [])
    .filter((message) => reactionEligible(message) && !isImageRun(message))
    .map((message) => message.id), [conversation?.messages, reactionsEnabled, conversation?.reactionSupport]), reaction.reveal);
  const [announcement, setAnnouncement] = useState<Announcement>(NO_ANNOUNCEMENT);
  const announceReaction = useCallback((text: string, alert = false) => {
    setAnnouncement((current) => ({ seq: current.seq + 1, polite: alert ? "" : text, alert: alert ? text : "" }));
  }, []);
  const textOwnStatus = useMemo(() => {
    const states = new Map<string, OwnChipState>();
    for (const message of conversation?.messages ?? []) {
      const notice = reactions.notices.get(message.id);
      if (!notice?.targetId || message.sender !== "self" || notice.action !== "add") continue;
      states.set(notice.targetId, textChipState(message.status));
    }
    return states;
  }, [conversation?.messages, reactions]);
  const badgesFor = (message: MessageView): MergedReactionBadge[] | undefined => {
    const current = messagesById.get(message.id) ?? message;
    const merged = mergeReactions({ parsed: reactions.badges.get(message.id), native: current.reactions, ownRequest: current.ownReactionRequest });
    return merged.length ? withTextStatus(merged, textOwnStatus.get(message.id)) : undefined;
  };
  /** Chips for a message, in its row or overlaid on a photo of a media run. */
  const badgeElement = (message: MessageView, placement: "row" | "overlay-bottom-start") => {
    const badges = badgesFor(message);
    return badges && <ReactionBadges
      messageId={message.id}
      badges={badges}
      actorName={actorName}
      chipRef={chipRef}
      placement={placement}
      onOwnChip={(badge) => { if (badge.own?.source === "native") pickFor(message.id, null); }}
      writesPaused={writesPaused}
      onRetry={(_requestId, emoji) => { if (!writesPaused) pickFor(message.id, emoji); }}
      onDismiss={(requestId) => { void dismissRequest(requestId); }}
    />;
  };
  /** Sends a pick from a chip (retry, or removing an own chip), with its polite line. */
  const pickFor = (messageId: string, emoji: string | null) => {
    if (writesPaused) return;
    const message = messagesById.get(messageId);
    const selected = message ? nativeSelection(message) : undefined;
    announceReaction(pickAnnouncement({ route: "native", emoji: emoji ?? selected ?? "", selected, removing: emoji === null }));
    void submitReaction(messageId, emoji);
  };
  /**
   * One submit path for the bar and the chips. A rejected call is reported once and never retried. The optimistic
   * request is held only from an accepted call, and only until the snapshot catches up.
   */
  const submitReaction = async (messageId: string, emoji: string | null): Promise<ReactionSubmitView | undefined> => {
    if (writesPaused) return undefined;
    const message = messagesById.get(messageId);
    const previousEmoji = message ? nativeSelection(message) ?? undefined : undefined;
    const startedAt = snapshotSeq.current;
    let result: ReactionSubmitView | undefined;
    try {
      result = await bridge.react_to_message?.({ messageId, emoji });
    } catch {
      setPendingRequests((current) => withoutEntry(current, messageId));
      announceReaction(UNREACHABLE_COPY, true);
      return undefined;
    }
    if (!result) return undefined;
    if (result.accepted && result.requestId) {
      // A snapshot that arrived during the call is already authoritative for this message: no overlay is needed.
      if (snapshotSeq.current === startedAt) {
        const request: OwnReactionRequestView = { requestId: result.requestId, emoji, previousEmoji, state: "sending" };
        setPendingRequests((current) => new Map(current).set(messageId, { request, installedAt: snapshotSeq.current }));
      }
    } else if (!result.accepted) {
      setPendingRequests((current) => withoutEntry(current, messageId));
      if (result.reason) announceReaction(REFUSAL_COPY[result.reason], true);
    }
    return result;
  };
  const dismissRequest = async (requestId: string) => {
    try {
      await bridge.dismiss_reaction_request?.({ requestId });
    } catch {
      announceReaction(UNREACHABLE_COPY, true);
    }
  };
  // Announces request transitions (failed, unknown, and resolved to sent) as they happen, not on first render.
  const requestStates = useRef(new Map<string, RequestSnapshot>());
  useEffect(() => {
    const seen = requestStates.current;
    for (const message of conversation?.messages ?? []) {
      const request = message.ownReactionRequest;
      const next: RequestSnapshot | undefined = request && { requestId: request.requestId, state: request.state, emoji: request.emoji, previousEmoji: request.previousEmoji };
      const previous = seen.get(message.id);
      // Only a request seen before can change; a request first seen on load is not news.
      if (previous) {
        const copy = requestAnnouncement(previous, next, nativeSelection(message));
        if (copy.alert) announceReaction(copy.alert, true);
        else if (copy.polite) announceReaction(copy.polite);
      }
      if (next) seen.set(message.id, next);
      else seen.delete(message.id);
    }
  }, [announceReaction, conversation?.messages]);
  const [cancellingMessageIds, setCancellingMessageIds] = useState<ReadonlySet<string>>(new Set());
  const [localCancellationStates, setLocalCancellationStates] = useState<ReadonlyMap<string, LocalCancellationState>>(new Map());
  const cancellingMessageIdsRef = useRef(new Set<string>());
  const cancellationRequestIds = useRef(new Map<string, number>());
  const cancellationFocusIntents = useRef(new Map<string, FocusIntent>());
  const cancellationViewGeneration = useRef(0);
  const cancellationStates = useRef(new Map<string, CancellationState | undefined>());
  useEffect(() => {
    cancellationViewGeneration.current += 1;
    return () => {
      cancellationViewGeneration.current += 1;
      cancellationRequestIds.current.clear();
      cancellingMessageIdsRef.current.clear();
      cancellationFocusIntents.current.forEach((intent) => intent.dispose());
      cancellationFocusIntents.current.clear();
    };
  }, [conversation?.id]);
  cancellationStates.current = new Map(conversation?.messages.map((message) => [message.id, message.sendCancellation?.state]) ?? []);
  // A message in a send-recovery state is terminal: cancel cannot reach the removed phone, so no cancel control or notice.
  const cancellationState = (message: MessageView) => {
    if (message.sendRecovery) return undefined;
    return cancellationDisplayState(message.sendCancellation?.state, localCancellationStates.get(message.id));
  };
  const hasCancellationAction = (message: MessageView) => {
    const state = cancellationState(message);
    return state === "requested" || state === "too-late" || state === "unknown" || state === "failed" || Boolean(message.sender === "self" && message.sendCancellation?.canCancel && !message.sendRecovery && bridge.cancel_send);
  };
  const requestSendCancellation = async (message: MessageView) => {
    if (!bridge.cancel_send || cancellingMessageIdsRef.current.has(message.id)) return;
    const sequence = takeSendCancellationSequence?.() ?? 0;
    const focusIntent = watchFocusIntent(document.activeElement instanceof HTMLElement ? document.activeElement : null);
    const requestId = (cancellationRequestIds.current.get(message.id) ?? 0) + 1;
    const viewGeneration = cancellationViewGeneration.current;
    cancellationRequestIds.current.set(message.id, requestId);
    if (focusIntent) cancellationFocusIntents.current.set(message.id, focusIntent);
    cancellingMessageIdsRef.current.add(message.id);
    setCancellingMessageIds(new Set(cancellingMessageIdsRef.current));
    let replacementFocusPending = false;
    try {
      const cancellation = await bridge.cancel_send({ messageId: message.id });
      const currentState = cancellationStates.current.get(message.id);
      if (cancellationViewGeneration.current !== viewGeneration || cancellationRequestIds.current.get(message.id) !== requestId || currentState === "confirmed" || currentState === "too-late" || currentState === "unknown") return;
      if (cancellation.state === "requested") {
        setLocalCancellationStates(states => new Map(states).set(message.id, "requested"));
        onSendCancellationRequested?.(message, sequence);
        replacementFocusPending = true;
        requestAnimationFrame(() => {
          const ownsIntent = cancellationFocusIntents.current.get(message.id) === focusIntent;
          const activeElement = document.activeElement;
          const canRestoreFocus = focusIntent && !focusIntent.userMovedFocus && (activeElement === focusIntent.origin || activeElement === document.body);
          if (ownsIntent) cancellationFocusIntents.current.delete(message.id);
          focusIntent?.dispose();
          if (cancellationViewGeneration.current === viewGeneration && cancellationRequestIds.current.get(message.id) === requestId && canRestoreFocus)
            document.querySelector<HTMLElement>(`[data-message-cancellation="${message.id}"]`)?.focus();
        });
      } else if (cancellation.state === "too-late" || cancellation.state === "unknown") {
        const terminalState = cancellation.state;
        setLocalCancellationStates(states => new Map(states).set(message.id, terminalState));
      }
    } catch {
      if (cancellationViewGeneration.current === viewGeneration && cancellationRequestIds.current.get(message.id) === requestId)
        setLocalCancellationStates(states => new Map(states).set(message.id, "failed"));
    } finally {
      if (cancellationViewGeneration.current !== viewGeneration || cancellationRequestIds.current.get(message.id) !== requestId) return;
      if (!replacementFocusPending) {
        if (cancellationFocusIntents.current.get(message.id) === focusIntent) cancellationFocusIntents.current.delete(message.id);
        focusIntent?.dispose();
      }
      cancellingMessageIdsRef.current.delete(message.id);
      setCancellingMessageIds(new Set(cancellingMessageIdsRef.current));
    }
  };
  useEffect(() => {
    setLocalCancellationStates((states) => {
      const next = new Map(states);
      conversation?.messages.forEach((message) => {
        const authoritative = message.sendCancellation?.state;
        if (authoritative === "confirmed" || authoritative === "too-late" || authoritative === "unknown" || (authoritative === "requested" && next.get(message.id) === "requested")) next.delete(message.id);
      });
      return next;
    });
  }, [conversation?.messages]);
  const chipRef = useSeenBadges({
    ...badgeObservation,
    badges: [...reactions.badges].flatMap(([id, badges]) => badges.map(badge => ({ key: JSON.stringify([id, badge.emoji]), reactionIds: badge.reactionIds }))),
  });
  const suppressed = new Set(conversation?.messages.filter(message => reactions.notices.has(message.id)
    && (seenReactions.has(conversation.id, message.id) || pendingSeenReactionIds.has(message.id)) && !showsReactionNoticeMeta(message)
    && pendingReactionId !== message.id && reactionFeedback?.reactionId !== message.id
    && searchTargetId !== message.id && focusedNoticeId !== message.id).map(message => message.id));
  if (loading)
    return (
      <div
        ref={listRef}
        id="message-list"
        role="log"
        aria-label="Messages"
        aria-busy="true"
        tabIndex={-1}
        onScroll={onScroll}
        onWheel={onWheel}
      >
        <p>Loading conversation…</p>
      </div>
    );
  if (!conversation)
    return (
      <div
        ref={listRef}
        id="message-list"
        role="log"
        aria-label="Messages"
        tabIndex={-1}
        onScroll={onScroll}
        onWheel={onWheel}
      >
        <div id="no-selection-state">
          <div id="no-selection-primary">
            <MessageSquare size={32} aria-hidden />
            {onStartConversation ? (
              <>
              <p>No conversation selected</p>
              <button
                id="new-conversation-cta"
                className="primary-button"
                type="button"
                data-action="new-conversation"
                onClick={onStartConversation}
              >
                <MessageSquarePlus size={16} aria-hidden />
                New conversation
              </button>
              </>
            ) : (
              <p>Select a conversation or start a new message</p>
            )}
          </div>
        </div>
      </div>
    );
  const conversationToneSeed = conversationAvatar({ id: conversation.id, name: conversation.name, participants: conversation.participants, recipientless: conversation.recipientlessDraft }).seed;
  const pendingById = new Map(pendingSends.map((pending) => [pending.id, pending]));
  const visualMessages = [
    ...conversation.messages.filter(message => message.sendCancellation?.state !== "confirmed" && !suppressed.has(message.id)),
    ...pendingSends.map((pending): MessageView => ({
      id: pending.id,
      revision: "pending",
      sender: "self",
      body: pending.content.text,
      timestamp: new Date(pending.createdAt).toISOString(),
      status: "queued-local",
      attachments: [],
    })),
  ];
  const chains = resendChains(visualMessages, {
    transientLink: (id) => sendAgainStore?.sentAgainFor(id),
    keepVisible: (message) => Boolean(badgesFor(message)?.length || message.ownReactionRequest ||
      reactions.notices.has(message.id) || hasCancellationAction(message) || cancellationState(message) ||
      (message.sendCancellation && message.sendCancellation.state !== "none") ||
      message.id === searchTargetId || pendingById.has(message.id)),
  });
  const shownMessages = visualMessages.filter((message) => !chains.hidden.has(message.id));
  const registerMessageRow = (message: MessageView, row: HTMLElement | null) => {
    registerRow(message.id, row);
    const chain = chains.byLatest.get(message.id);
    registerResendRows(chain, row, registerRow);
  };
  const timeIsShown = (message: MessageView) => {
    const displayedTime = messageTime(message).displayedTime;
    return Boolean(formatDateTime(displayedTime) && isoDateTime(displayedTime));
  };
  const hiddenTimes = compactTimestamps
    ? hiddenTimeIds(shownMessages, (message) => timeIsShown(message) && !(reactions.notices.has(message.id) && !showsReactionNoticeMeta(message)))
    : new Set<string>();
  const hiddenTimeTitle = (message: MessageView) => hiddenTimes.has(message.id)
    ? formatDateTime(messageTime(message).displayedTime) ?? undefined
    : undefined;
  const isGroup = Boolean(conversation.group) || (conversation.participants?.length ?? 0) > 1;
  const messageGroups = groupMessages(shownMessages);
  const timeline = interleaveGroupNotices(messageGroups, groupNoticeItems(conversation.group?.notices));
  const senderEdges = isGroup
    ? senderRunEdges(timeline, {
      key: (item) => item.id,
      identity: (item) => {
        if (item.kind === "notice") return { kind: "boundary" };
        const message = item.group.messages[0];
        if (message.sender === "self") return { kind: "outgoing" };
        if (reactions.notices.has(message.id)) return { kind: "boundary" };
        return { kind: "incoming", address: message.senderAddress };
      },
      endsRunAfter: (item) => {
        if (!compactTimestamps || item.kind === "notice") return false;
        const message = item.group.messages.at(-1)!;
        return timeIsShown(message) && !hiddenTimes.has(message.id) && (!reactions.notices.has(message.id) || showsReactionNoticeMeta(message));
      },
    })
    : new Map<string, { start: boolean; end: boolean }>();
  const senderRun = (group: ReturnType<typeof groupMessages>[number]) => {
    const message = group.messages[0];
    const edge = senderEdges.get(group.id);
    if (!edge) return undefined;
    const label = senderLabel(message.senderAddress, resolution);
    const actionable = Boolean(message.senderAddress && onOpenSender && !senderLocked);
    const openSender = () => message.senderAddress && onOpenSender?.(message.senderAddress);
    const labelNode = edge.start ? actionable ? (
      <button type="button" className="message-sender-label" data-sender-label={message.senderAddress} data-sender-known={label.isName} aria-label={label.contact ? `Open ${label.text} in Contacts` : `Add ${label.text} to contacts`} onClick={openSender}>{label.text}</button>
    ) : <span className="message-sender-label" data-plain data-sender-unknown={message.senderAddress ? undefined : true} data-sender-label={message.senderAddress} data-sender-known={label.isName} aria-hidden="true" title={label.text}>{label.text}</span> : undefined;
    const avatar = edge.end && showSenderAvatars ? <span className="message-sender-avatar" data-sender-avatar={message.senderAddress} data-clickable={actionable || undefined} aria-hidden="true" onClick={actionable ? openSender : undefined}><ContactAvatar size={24} seed={incomingToneSeed(conversationToneSeed, message.senderAddress)} name={label.isName ? label.text : undefined} photoDataUrl={resolution[message.senderAddress ?? ""]?.photoDataUrl} toneOverrides={toneOverrides} /></span> : undefined;
    return { edge, label, labelNode, avatar };
  };
  return (
    <ReactionHintLayout suppressed={suppressed} capture={captureReactionLayout} restore={restoreReactionLayout}>
    <div
      ref={listRef}
      id="message-list"
      role="log"
      aria-label="Messages"
      tabIndex={-1}
      onScroll={onScroll}
      onWheel={onWheel}
      onFocusCapture={(event) => setFocusedNoticeId((event.target as HTMLElement).closest<HTMLElement>("[data-reaction-notice]")?.dataset.reactionNotice)}
      onBlurCapture={(event) => {
        setFocusedNoticeId((event.relatedTarget as HTMLElement | null)?.closest<HTMLElement>("[data-reaction-notice]")?.dataset.reactionNotice);
      }}
    >
      {searchTargetId && <span id="search-result-message-hint" className="sr-only">Search result message</span>}
      {detachedHistory && <section id="detached-history-status" aria-label="Search history position">
        <span role="status">Viewing messages near a search result.</span>
        {detachedHistory.hasNewer && <button id="history-load-newer-button" className="secondary-button" type="button" disabled={Boolean(detachedHistory.pendingLoad)} onClick={(event) => onLoadNewer(event.currentTarget)}>Load newer messages</button>}
        <button id="history-go-latest-button" className="secondary-button" type="button" disabled={Boolean(detachedHistory.pendingLoad)} onClick={onGoToLatest}>Go to latest</button>
      </section>}
      {(historyLoading || historyError || hasOlderMessages || detachedHistory?.pendingLoad === "newer") && <div id="history-load-band" className="history-load-band" tabIndex={-1} data-history-state={historyLoading || detachedHistory?.pendingLoad === "newer" ? "loading" : historyError ? "error" : "idle"}>
        {historyLoading && !historyError && <span id="history-load-indicator" className="history-load-indicator" role="status">Loading older messages…</span>}
        {detachedHistory?.pendingLoad === "newer" && <span id="history-load-newer-indicator" className="history-load-indicator" role="status">Loading newer messages…</span>}
        {historyError && <div id="history-load-error" className="history-load-error" role="alert">Couldn&apos;t load {historyErrorDirection} messages. <button id="history-retry-button" className="history-divider-action" type="button" aria-label={`Retry loading ${historyErrorDirection} messages`} disabled={historyLoading || Boolean(detachedHistory?.pendingLoad)} onClick={historyErrorDirection === "newer" ? (event) => onLoadNewer(event.currentTarget) : (event) => onLoadOlder(event.currentTarget)}>Retry</button></div>}
        {!historyLoading && !historyError && hasOlderMessages && <button id="history-load-button" className="history-divider-action" type="button" onClick={(event) => onLoadOlder(event.currentTarget)}>Load older messages</button>}
      </div>}
      <div ref={setReactionContent} id="message-list-content" aria-live={suppressHistoryAnnouncements ? "off" : undefined} onKeyDown={roving.onKeyDown}>
        {conversation.messages.filter(message => suppressed.has(message.id)).map(message => <ReactionNotice key={message.id} reaction={message} notice={reactions.notices.get(message.id)!} actorName={actorName} rowRef={row => registerRow(message.id, row)} onOpen={onOpenReactionTarget} suppressedHint />)}
        {historyExhausted && <p id="history-beginning-marker" className="history-load-band" data-history-state="beginning">Beginning of conversation</p>}
        {!visualMessages.length && <p>Start the conversation.</p>}
        {timeline.map((item) => {
          if (item.kind === "notice") return <GroupNotice key={item.id} notice={item.notice} displayName={(address) => displayAddressName(address, resolution)} />;
          const group = item.group;
          const message = group.messages[0];
          const pending = pendingById.get(message.id);
          if (pending) return <PendingSendRow key={pending.id} pending={pending} message={message} attachmentViews={attachmentViews} onOpenExternal={onOpenExternal} />;
          const reactionNotice = reactions.notices.get(message.id);
          const imageRun = group.messages.every((candidate) => (candidate.transport === "mms" || candidate.transport === "rcs") && candidate.attachments.length > 0 && candidate.attachments.every((attachment) => attachment.mediaType.startsWith("image/")));
          const sender = senderRun(group);
          const resendFor = (candidate: MessageView) => {
            const needsMemberReview = candidate.groupHint === "members_changed" || candidate.attachments.some((attachment) => attachment.rcsSendStatus === "members_changed");
            if (needsMemberReview && onEditGroupResend) return () => onEditGroupResend(candidate);
            if (onEditRcsUnsent && candidate.attachments.some((attachment) => attachment.rcsSendStatus === "not_sent")) return () => onEditRcsUnsent(candidate);
            return undefined;
          };
          if (imageRun) return <MediaGrid key={group.id} messages={group.messages} onOpen={onOpenMedia} reactable={reactionEligible} renderBadges={(candidate) => badgeElement(candidate, "overlay-bottom-start")} renderMessageMeta={(candidate) => <MessageMeta message={candidate} resend={chains.byLatest.get(candidate.id)} hideTime={hiddenTimes.has(candidate.id)} onEditResend={resendFor(candidate)} />} renderCaption={(candidate) => <MessageText text={candidate.body} onOpenExternal={onOpenExternal} />} hasMessageAction={hasCancellationAction} renderMessageAction={(candidate) => cancellationState(candidate) === "requested" ? <SendCancellationPending messageId={candidate.id} waitingForPhone /> : cancellationState(candidate) === "too-late" || cancellationState(candidate) === "unknown" || cancellationState(candidate) === "failed" ? <span data-message-cancellation-notice={candidate.id} role="status">{cancellationState(candidate) === "too-late" ? "Cancellation was too late." : cancellationState(candidate) === "unknown" ? "Cancellation status is unknown." : "Could not request cancellation."}</span> : candidate.sender === "self" && candidate.sendCancellation?.canCancel && !candidate.sendRecovery && bridge.cancel_send ? <CancelSendControl messageId={candidate.id} disabled={cancellingMessageIds.has(candidate.id)} onCancel={() => void requestSendCancellation(candidate)} /> : undefined} isMessagePending={(candidate) => cancellationState(candidate) === "requested"} messageRef={(candidate) => (row) => registerMessageRow(candidate, row)} messageTitle={hiddenTimeTitle} senderRun={(candidate) => sender && candidate.id === group.id ? { label: sender.labelNode, avatar: sender.avatar, gutter: showSenderAvatars, ariaName: sender.label.text, start: sender.edge.start, end: sender.edge.end } : undefined} />;
          if (reactionNotice) return <ReactionNotice key={message.id} reaction={message} notice={reactionNotice} actorName={actorName} rowRef={(row) => registerRow(message.id, row)} onOpen={onOpenReactionTarget} busy={pendingReactionId === message.id} feedback={reactionFeedback?.reactionId === message.id ? reactionFeedback.message : undefined} meta={showsReactionNoticeMeta(message) ? <MessageMeta message={message} /> : undefined} />;
          const badges = badgesFor(message);
          const chromeless = message.attachments.length > 0 && message.attachments.every((attachment) => attachment.mediaType.startsWith("video/")) && !message.attachments.some((attachment) => rcsPlaceholderState(attachment));
          const messageCancellationState = cancellationState(message);
          const cancellationPending = messageCancellationState === "requested";
          const cancellationNotice = messageCancellationState === "too-late" ? "Cancellation was too late." : messageCancellationState === "unknown" ? "Cancellation status is unknown." : messageCancellationState === "failed" ? "Could not request cancellation." : undefined;
          const video = chromeless && message.attachments.length === 1 ? message.attachments[0] : undefined;
          const aspectRatio = videoAspectRatio(video?.mediaMeta);
          const bubbleStyle: CSSProperties | undefined = chromeless
            ? aspectRatio ? { "--video-ar": aspectRatio } as CSSProperties : undefined
            : message.sender === "other" ? toneStyle(incomingToneSeed(conversationToneSeed, message.senderAddress), toneOverrides) : undefined;
          // The roving stop announces the reaction hint and the badge group together (ux-design E).
          const describedBy = [
            searchTargetId === message.id ? "search-result-message-hint" : undefined,
            roving.stopId === message.id && reactionEligible(message) ? ["message-reaction-hint", badges ? `reactions-${message.id}` : undefined].filter(Boolean).join(" ") : undefined,
          ].filter(Boolean).join(" ") || undefined;
          const bubble = (
            <article
              ref={(row) => registerMessageRow(message, row)}
              data-message-id={message.id}
              data-resend-attempts={chains.byLatest.get(message.id)?.attempts.map((attempt) => attempt.id).join(" ")}
              data-search-target={searchTargetId === message.id || undefined}
              aria-describedby={describedBy}
              data-reaction-ok={reactionEligible(message) ? "" : undefined}
              onFocus={reactionEligible(message) ? () => roving.onArticleFocus(message.id) : undefined}
              tabIndex={searchTargetId === message.id ? -1 : reactionEligible(message) ? (roving.stopId === message.id ? 0 : -1) : message.ownReactionRequest ? -1 : undefined}
              aria-keyshortcuts={reactionEligible(message) ? "Enter" : undefined}
              className={chromeless ? "message-bubble" : `message-bubble ${message.sender}`}
              data-chromeless={chromeless || undefined}
              data-peppy-tone={(!chromeless && message.sender === "other") || undefined}
              style={bubbleStyle}
              title={hiddenTimeTitle(message)}
            >
              {sender && <span className="sr-only">From {sender.label.text}: </span>}
              {cancellationPending ? <SendCancellationPending messageId={message.id} waitingForPhone /> : <>
                {message.sender === "self" && message.sendCancellation?.canCancel && !message.sendRecovery && bridge.cancel_send && <CancelSendControl messageId={message.id} disabled={cancellingMessageIds.has(message.id)} onCancel={() => void requestSendCancellation(message)} />}
                {!chromeless && message.body && <p><MessageText text={message.body} onOpenExternal={onOpenExternal} /></p>}
                {!chromeless && message.subject && <p className="message-subject">Subject: <bdi>{message.subject}</bdi></p>}
              {message.attachments.map((file) => {
                const showRcsStatus = message.attachments.some((attachment) => attachment.rcsSendStatus && attachment.rcsSendStatus !== "sent");
                const copy = publicCopies[file.id];
                const placeholder = rcsPlaceholderState(file);
                const rcsSlotKey = `${message.id}:${rcsAttachmentIndex(message, file)}`;
                const renderAttachment = () => {
                  if (file.rcsState && file.mediaType.startsWith("image/")) return <MediaTile key={rcsSlotKey} attachment={file} message={message} variant="single" onOpen={() => onOpenMedia({ attachment: file, message })} />;
                  if (placeholder) return <RcsMediaFileRow key={rcsSlotKey} messageId={message.id} attachment={file} attachmentIndex={rcsAttachmentIndex(message, file)} size={file.byteSize > 0 ? fileSize(file.byteSize) : undefined} />;
                  if (originalPending(file)) return <MediaTile key={file.id} attachment={file} message={message} variant="single" onOpen={() => onOpenMedia({ attachment: file, message })} />;
                  const shareable =
                    file.remoteState !== "removed_by_storage_policy" && file.state === "ready" && file.mediaType.startsWith("image/");
                  if (file.mediaType.startsWith("video/") || file.mediaType.startsWith("audio/")) return <div key={file.id} data-attachment-id={file.id} data-rcs-media-state={file.rcsState ?? file.rcsSendStatus} data-rcs-media-direction={rcsDirection(file)}><InlineMedia attachment={file} requestOriginal={requestOriginal} checkOriginal={checkOriginal} releaseMedia={releaseMedia} retryKey={mediaRetryKey} /><RcsAttachmentStatus attachment={file} visible={showRcsStatus} /></div>;
                  const removedRemoteOnly = file.remoteState === "removed_by_storage_policy" && !file.localAvailable;
                  const removedWithLocalCopy = file.remoteState === "removed_by_storage_policy" && file.localAvailable;
                  return (
                    <div
                      key={file.id}
                      data-attachment-id={file.id}
                      data-rcs-media-state={file.rcsState ?? file.rcsSendStatus}
                      data-rcs-media-direction={rcsDirection(file)}
                      className="file-card"
                    >
                      {file.previewUrl && (
                        <img
                          src={file.previewUrl}
                          alt={file.name}
                          onError={(event) => {
                            event.currentTarget.hidden = true;
                          }}
                        />
                      )}
                      {file.mediaType.startsWith("image/") ? (
                        <Image size={16} aria-hidden />
                      ) : (
                        <FileText size={16} aria-hidden />
                      )}
                      <span><bdi>{file.name}</bdi></span>
                      <small>
                        {fileSize(file.byteSize)}{removedRemoteOnly ? "" : ` · ${attachmentState(file)}`}
                      </small>
                      <RcsAttachmentStatus attachment={file} visible={showRcsStatus} />
                      {removedRemoteOnly ? <p data-attachment-storage-removed role="status">{attachmentStorageRemovalText(false, peppyCopy).body}</p> : file.retryable && file.transfer && !rcsDirection(file) && (
                        <button className="secondary-button" onClick={() => onRetryAttachment(file)} aria-label={`Retry ${file.transfer} for ${file.name}`}>
                          Retry {file.transfer}
                        </button>
                      )}
                      {removedWithLocalCopy && <p data-attachment-storage-local-copy role="status">{attachmentStorageRemovalText(true, peppyCopy).body}</p>}
                      {!removedRemoteOnly && (!file.mediaType.startsWith("image/") || file.state === "ready") && (
                        <button className="secondary-button" onClick={() => onSaveAttachment(file)} aria-label={`Save ${file.name}`}>
                          Save {file.name}
                        </button>
                      )}
                      {shareable && !copy && (
                        <button
                          className="secondary-button"
                          onClick={() => onPublish(file)}
                          aria-label={`Create public link for ${file.name}`}
                        >
                          Create public link
                        </button>
                      )}
                      {copy && (
                        <div className="public-copy" data-public-copy-for={file.id}>
                          <label>
                            Public link{" "}
                            <span className="public-link-warning" role="status">
                              <ShieldAlert size={12} aria-hidden /> Anyone with this
                              link can view this copy
                            </span>
                            <input
                              readOnly
                              aria-label={`Public link for ${file.name}`}
                              value={copy.url}
                              onFocus={(event) => event.currentTarget.select()}
                            />
                          </label>
                          <small>{expiryText(copy.expiresInSeconds)}</small>
                        </div>
                      )}
                    </div>
                  );
                };
                return file.rcsState && !file.mediaType.startsWith("image/")
                  ? <RcsAttachmentSlot key={rcsSlotKey} slotId={rcsSlotKey} phase={file.rcsState}>{renderAttachment()}</RcsAttachmentSlot>
                  : renderAttachment();
              })}
                {chromeless && message.body && <p data-message-caption><MessageText text={message.body} onOpenExternal={onOpenExternal} /></p>}
                {chromeless && message.subject && <p className="message-subject">Subject: <bdi>{message.subject}</bdi></p>}
              </>}
              {cancellationNotice && <span data-message-cancellation-notice={message.id} role="status">{cancellationNotice}</span>}
              {message.sender === "other" && message.oneTimeCode && conversation?.folder !== "spam" && (
                <OneTimeCodeButton key={`${message.id}:${message.oneTimeCode}`} variant="message" hookId={message.id} target={{ kind: "message", messageId: message.id }} code={message.oneTimeCode} onCopy={onCopyOneTimeCode} />
              )}
            </article>
          );
          return (
            <div
              key={message.id}
              className={`message-row ${message.sender}`}
              data-message-row={message.id}
              data-sender={message.sender}
              data-sender-gutter={sender && showSenderAvatars ? "" : undefined}
              data-sender-run-start={sender?.edge.start ? "" : undefined}
              data-sender-run-end={sender?.edge.end ? "" : undefined}
            >
              {sender?.labelNode}
              {sender ? <div className="message-sender-line" data-sender-line>{sender.avatar ?? (showSenderAvatars && <span className="message-sender-gutter" aria-hidden="true" />)}{bubble}</div> : bubble}
              {badgeElement(message, "row")}
              <MessageMeta message={message} resend={chains.byLatest.get(message.id)} hideTime={hiddenTimes.has(message.id)} onEditResend={resendFor(message)} onCreateAgain={onCreateGroupAgain ? () => onCreateGroupAgain(message) : undefined} />
            </div>
          );
        })}
        {typingRow && <TypingRow {...typingRow} showSenderAvatars={showSenderAvatars} conversationToneSeed={conversationToneSeed} toneOverrides={toneOverrides} />}
      </div>
    </div>
    {reactionsEnabled && conversation && <MessageReactionLayer
      reaction={reaction}
      barRef={reactionBarRef}
      messages={messagesById}
      reactionSupport={conversation.reactionSupport}
      routeOffline={reactionRouteOffline}
      writesPaused={writesPaused}
      nameOf={(candidate) => actorName({ sender: candidate.sender, senderAddress: candidate.senderAddress })}
      announce={announceReaction}
      openPicker={bridge.open_emoji_picker}
      onSubmit={(candidate, emoji) => submitReaction(candidate.id, emoji)}
    />}
    {/* Outside the log: the hint and both live regions are never announced as list content. */}
    {reactionsEnabled && <span id="message-reaction-hint" className="sr-only">Press Enter to react.</span>}
    {reactionsEnabled && <span id="message-reaction-hint-r" className="sr-only">Press R to react.</span>}
    <ReactionAnnouncer announcement={announcement} />
    </ReactionHintLayout>
  );
}

/** Only an enrolled owner can add a phone; joined devices keep the normal conversation view. */
export function ownerWithoutPhone(snapshot: DesktopSnapshot) {
  return snapshot.connection.state === "connected" && snapshot.encryption.state === "unlocked" && snapshot.gateways.length === 0 && snapshot.deviceRole === "owner";
}
const phoneDeferredKey = (origin: string | undefined) => origin ? `peppy.setup.phoneDeferred:${origin}` : "";
const phoneSetupDeferred = (origin: string | undefined) => {
  const key = phoneDeferredKey(origin);
  return Boolean(key && localStorage.getItem(key));
};

type SettingsAction = "origin" | "unlock" | "account";

export function notificationPersistenceSettings(bridge: Pick<DesktopBridge, "get_notification_persistence" | "set_notification_persistence">):
  { load(): Promise<NotificationPersistenceSettings>; save(value: NotificationPersistence): Promise<void> } | undefined {
  if (!bridge.get_notification_persistence || !bridge.set_notification_persistence) return undefined;
  return {
    load: () => bridge.get_notification_persistence!(),
    save: value => bridge.set_notification_persistence!(value),
  };
}

export function notificationQuickReplySettings(bridge: Pick<DesktopBridge, "get_notification_quick_reply" | "set_notification_quick_reply">):
  { load(): Promise<NotificationQuickReplySettings>; save(enabled: boolean): Promise<void> } | undefined {
  if (!bridge.get_notification_quick_reply || !bridge.set_notification_quick_reply) return undefined;
  return {
    load: () => bridge.get_notification_quick_reply!(),
    save: enabled => bridge.set_notification_quick_reply!(enabled),
  };
}

function SettingsView({
  storageRequest = 0,
  mode,
  configuredOrigin,
  origin,
  onOrigin,
  onAction,
  onOpenAccount,
  encryption,
  canUnlock,
  theme,
  onTheme,
  accentColor,
  onAccentColor,
  accentError,
  navigationPosition,
  onNavigationPosition,
  popoutMode,
  onPopoutMode,
  compactTimestamps,
  onCompactTimestamps,
  attachmentLayout,
  onAttachmentLayout,
  groupSenderAvatars,
  onGroupSenderAvatars,
  showReactions,
  onShowReactions,
  desktop,
  onStartAtLogin,
  notifications,
  filters,
  sources,
  preferences,
  onPreferences,
  onMute,
  onPermission,
  onOpenSettings,
  headsSupported,
  writesPaused,
  pauseNoticeVisible,
  onLock,
  appLock,
  onAppLock,
  browserHost = false,
  accountUrl,
  hostedDeployment = false,
  sessionScopeId,
  canRevealVaultSecret = false,
  serverOffline = false,
  accountReadOnly = false,
  typingShare,
  typingShareError = false,
  onTypingShare,
  diagnostics,
  requestedSection,
  onRequestedSectionConsumed,
  onAddDevice,
  onAccountDeleted,
  onDeletionCanceled,
}: {
  /** Changes each time "Free up space" asks for the storage card; 0 when nothing was requested. */
  storageRequest?: number;
  mode: "hosted" | "self-hosted";
  configuredOrigin?: string;
  origin: string;
  onOrigin(value: string): void;
  onAction(action: Exclude<SettingsAction, "account">): Promise<void>;
  onOpenAccount(): Promise<void>;
  encryption: DesktopSnapshot["encryption"]["state"];
  canUnlock: boolean;
  theme: Theme;
  onTheme(theme: Theme): void;
  accentColor: number | null;
  onAccentColor(accentColor: number | null): void;
  accentError?: string;
  navigationPosition: NavigationPosition;
  onNavigationPosition(position: NavigationPosition): void;
  popoutMode: PopoutMode;
  onPopoutMode(mode: PopoutMode): void;
  compactTimestamps: boolean;
  onCompactTimestamps(enabled: boolean): void;
  attachmentLayout: AttachmentLayout;
  onAttachmentLayout(layout: AttachmentLayout): void;
  groupSenderAvatars: boolean;
  onGroupSenderAvatars(enabled: boolean): void;
  showReactions: boolean;
  onShowReactions(show: boolean): void;
  desktop?: DesktopSnapshot["desktop"];
  onStartAtLogin(enabled: boolean): Promise<void>;
  notifications: DesktopSnapshot["notifications"];
  filters: AppFilter[];
  sources: { id: string; name: string }[];
  preferences: NotificationPreferences;
  onPreferences(preferences: NotificationPreferences): Promise<void>;
  onMute(filter: AppFilter): Promise<void>;
  onPermission(): Promise<NotificationCheck | void>;
  onOpenSettings(): Promise<void>;
  headsSupported: boolean;
  writesPaused: boolean;
  pauseNoticeVisible: boolean;
  onLock?: () => Promise<void>;
  appLock: AppLockStatus | null;
  onAppLock(enabled: boolean): Promise<boolean>;
  browserHost?: boolean;
  accountUrl?: string;
  hostedDeployment?: boolean;
  sessionScopeId?: string;
  canRevealVaultSecret?: boolean;
  serverOffline?: boolean;
  accountReadOnly?: boolean;
  typingShare?: boolean;
  typingShareError?: boolean;
  onTypingShare(enabled: boolean): void;
  /** Undefined when the host lacks any diagnostics method; the section is then absent. */
  diagnostics?: DiagnosticsBridge;
  requestedSection?: "notifications" | "notification-apps" | "devices" | "updates";
  onRequestedSectionConsumed?(): void;
  onAddDevice?(): void;
  onAccountDeleted(result: { outcome: "deleted" | "pending"; provider: BillingProvider; executeAfter?: string | null; cancellable?: boolean }): void;
  onDeletionCanceled(): void;
}) {
  const platform = detectPlatform();
  const [startupStatus, setStartupStatus] = useState<{ state: "on" | "off" | "approval_needed" | "unavailable"; background: boolean }>();
  const [savingStartup, setSavingStartup] = useState(false);
  const [startupError, setStartupError] = useState("");
  const [openLoginItemsError, setOpenLoginItemsError] = useState(false);
  const [billingSummary, setBillingSummary] = useState<{ provider: BillingProvider; manageableIn: BillingManageableIn }>();
  const [purchaseOptions, setPurchaseOptions] = useState<{ appStore: boolean; website: boolean }>();
  const [pendingAction, setPendingAction] = useState<SettingsAction | null>(null);
  const [actionError, setActionError] = useState<{ action: SettingsAction; message: string } | null>(null);
  const [revealError, setRevealError] = useState<string | null>(null);
  const nativeUpdateHost = !browserHost && isTauriUpdateHost();
  const updateDistribution = useUpdateDistribution(nativeUpdateHost);
  const showUpdates = nativeUpdateHost && updateDistribution !== "app_store";
  const settingsContentRef = useRef<HTMLDivElement>(null);
  const pendingSectionRef = useRef<string | null>(null);
  const scrollEndTimer = useRef<number | undefined>(undefined);
  const [activeSection, setActiveSection] = useState("server");
  const persistenceSettings = useMemo(() => notificationPersistenceSettings(bridge), [bridge]);
  const quickReplySettings = useMemo(() => notificationQuickReplySettings(bridge), [bridge]);
  const syncText = syncStatusText(encryption);
  const reloadStartupStatus = useCallback(async () => {
    if (!desktop?.startupSupported) return;
    setStartupStatus(await bridge.startup_status());
  }, [desktop?.startupSupported]);

  useEffect(() => {
    void reloadStartupStatus().catch(() => undefined);
    const refreshStartupStatus = () => void reloadStartupStatus().catch(() => undefined);
    window.addEventListener("focus", refreshStartupStatus);
    return () => window.removeEventListener("focus", refreshStartupStatus);
  }, [reloadStartupStatus]);

  useEffect(() => {
    if (browserHost) return;
    let active = true;
    void Promise.all([bridge.billing_summary(), bridge.purchase_options()]).then(([summary, options]) => {
      if (!active) return;
      setBillingSummary(summary);
      setPurchaseOptions(options);
    }).catch(() => undefined);
    return () => { active = false; };
  }, [browserHost]);

  const runAction = async (action: SettingsAction) => {
    if (pendingAction) return;
    setPendingAction(action);
    setActionError(null);
    try {
      if (action === "account") await onOpenAccount();
      else await onAction(action);
    } catch (error) {
      setActionError({ action, message: errorText(error) });
    } finally {
      setPendingAction(null);
    }
  };
  const actionPending = (action: SettingsAction) => pendingAction === action;
  const actionAlert = (action: SettingsAction) =>
    actionError?.action === action && <p className="settings-error" role="alert">{actionError.message}</p>;
  const connectionLabel = browserHost ? "Fixed server" : mode === "hosted" ? "Peppy Hosted" : "Self-hosted";
  const hasAccount = (!browserHost && mode === "hosted") || Boolean(browserHost && accountUrl);
  // Account and billing hold hosted storage; community vaults only show the server quota.
  useEffect(() => {
    if (storageRequest > 0) document.getElementById(hasAccount ? "settings-account-billing" : "settings-server")?.scrollIntoView?.({ block: "start" });
  }, [storageRequest, hasAccount]);
  const sections = [
    { id: "server", label: "Server" },
    { id: "sync", label: "Sync encryption" },
    { id: "devices", label: "Devices" },
    { id: "notifications", label: "Notifications" },
    { id: "messages", label: "Messages" },
    { id: "theme", label: "Appearance" },
    ...(desktop?.startupSupported ? [{ id: "startup", label: "Startup" }] : []),
    ...(showUpdates ? [{ id: "updates", label: "Updates" }] : []),
    ...(browserHost ? [] : [{ id: "app-lock", label: "App lock" }]),
    ...(hasAccount ? [{ id: "account-billing", label: "Account" }] : []),
    ...(typingShare !== undefined ? [{ id: "rcs-features", label: "RCS Features" }] : []),
    { id: "advanced", label: "Advanced" },
    ...(diagnostics ? [{ id: "diagnostics", label: "Diagnostics" }] : []),
  ];
  const sectionElementId = (id: string) => {
    if (id === "notifications") return "notification-settings";
    return `settings-${id}`;
  };
  const sectionIds = sections.map((section) => section.id).join(",");
  useEffect(() => {
    if (!sections.some((section) => section.id === activeSection)) {
      setActiveSection(sections[0]?.id ?? "server");
    }
  }, [activeSection, sectionIds]);
  const selectSection = useCallback((id: string) => {
    const notificationApps = id === "notification-apps";
    const activeId = notificationApps ? "notifications" : id;
    const section = document.getElementById(notificationApps ? "notification-apps-group" : sectionElementId(activeId)) ??
      document.getElementById(sectionElementId(activeId));
    setActiveSection(activeId);
    pendingSectionRef.current = activeId;
    if (scrollEndTimer.current) window.clearTimeout(scrollEndTimer.current);
    scrollEndTimer.current = window.setTimeout(() => {
      pendingSectionRef.current = null;
    }, 100);
    const content = settingsContentRef.current;
    if (content && section) {
      const margin = Number.parseFloat(getComputedStyle(section).scrollMarginTop) || 0;
      content.scrollTo?.({
        top: content.scrollTop + section.getBoundingClientRect().top - content.getBoundingClientRect().top - content.clientTop - margin,
        behavior: window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ? "instant" : "smooth",
      });
    }
  }, []);

  useEffect(() => {
    if (!requestedSection) return;
    selectSection(requestedSection);
    if (requestedSection === "devices") {
      window.setTimeout(() => {
        const addDevice = document.getElementById("devices-add-device") as HTMLButtonElement | null;
        (addDevice && !addDevice.disabled ? addDevice : document.getElementById("settings-devices-title"))?.focus({ preventScroll: true });
      }, 0);
    }
    onRequestedSectionConsumed?.();
  }, [onRequestedSectionConsumed, requestedSection, selectSection]);

  useEffect(() => {
    const root = settingsContentRef.current;
    if (!root) return;
    const updateActiveSection = () => {
      if (pendingSectionRef.current) return;
      if (root.scrollHeight <= root.clientHeight) return;
      const atBottom = root.scrollHeight > root.clientHeight &&
        root.scrollTop + root.clientHeight >= root.scrollHeight - 1;
      if (atBottom) return setActiveSection(sections.at(-1)?.id ?? "server");
      const rootTop = root.getBoundingClientRect().top;
      const visible = sections.reduce<string>((current, section) => {
        const element = document.getElementById(sectionElementId(section.id));
        return element && element.getBoundingClientRect().top <= rootTop + 24
          ? section.id
          : current;
      }, sections[0]?.id ?? "server");
      setActiveSection(visible);
    };
    const onScroll = () => {
      if (scrollEndTimer.current) window.clearTimeout(scrollEndTimer.current);
      scrollEndTimer.current = window.setTimeout(() => {
        pendingSectionRef.current = null;
      }, 100);
      updateActiveSection();
    };
    root.addEventListener("scroll", onScroll, { passive: true });
    updateActiveSection();
    return () => {
      root.removeEventListener("scroll", onScroll);
      if (scrollEndTimer.current) window.clearTimeout(scrollEndTimer.current);
    };
  }, [sectionIds]);

  return (
    <section id="settings-view" aria-label="Settings" role="region">
      <h1 className="visually-hidden">Settings</h1>
      <div id="settings-layout">
      <nav id="settings-navigation" aria-label="Settings categories">
        <label className="settings-category-selector">
          <span className="visually-hidden">Settings category</span>
          <select value={activeSection} onChange={(event) => selectSection(event.target.value)}>
            {sections.map((section) => <option key={section.id} value={section.id}>{section.label}</option>)}
          </select>
        </label>
        <div id="settings-category-links" className="settings-category-links">
          {sections.map((section) => (
            <a key={section.id} href={`#${sectionElementId(section.id)}`} aria-current={activeSection === section.id ? "location" : undefined} onClick={(event) => { event.preventDefault(); selectSection(section.id); }}>
              {section.label}
            </a>
          ))}
        </div>
      </nav>
      <div id="settings-content" ref={settingsContentRef}>
      <section id="settings-server" className="settings-card" data-settings-section="server">
        <h2>Server</h2>
        <p id="settings-connection-summary" className="settings-connection-summary"><strong>{connectionLabel}</strong><span> · </span><span>{configuredOrigin || "No server configured"}</span></p>
        <McpConnectHelp sessionKey={sessionScopeId ?? "unavailable"} browserHost={browserHost} />
        {browserHost ? <p className="settings-note">This browser host uses its fixed server.</p> : mode === "self-hosted" ? <details className="settings-disclosure" id="settings-server-editor">
          <summary>Change server</summary>
          <div className="settings-control-row">
            <label>Server URL<input aria-label="Server URL" value={origin} onChange={(event) => onOrigin(event.target.value)} placeholder="https://server.example" disabled={Boolean(pendingAction)} /></label>
            <button className="primary-button" disabled={Boolean(pendingAction)} aria-busy={actionPending("origin") || undefined} onClick={() => void runAction("origin")}>Configure server</button>
          </div>
        </details> : null}
        {actionAlert("origin")}
      </section>
      <section id="settings-sync" className="settings-card" data-settings-section="sync">
        <h2>Sync encryption</h2>
        <p className="settings-sync-state">
          {encryption === "unlocked" ? (
            <LockOpen size={14} aria-hidden />
          ) : (
            <Lock size={14} aria-hidden />
          )}
          {syncText}
        </p>
        {canUnlock && (
          <button
            className="secondary-button"
            disabled={Boolean(pendingAction)} aria-busy={actionPending("unlock") || undefined}
            onClick={() => void runAction("unlock")}
          >
            {browserHost ? "Unlock sync" : "Unlock sync natively"}
          </button>
        )}
        {actionAlert("unlock")}
        {canRevealVaultSecret && <>
          <button id="settings-reveal-vault-secret" className="secondary-button" disabled={Boolean(pendingAction)} onClick={async () => {
            setRevealError(null);
            try { await bridge.reveal_vault_secret(); }
            catch (error) { setRevealError(errorCode(error) === "vault-secret-unavailable" ? "This device doesn't have the vault key phrase stored." : FALLBACK_ERROR); }
          }}>Show vault shared secret</button>
          {revealError && <p id="settings-reveal-vault-secret-error" className="settings-error" role="alert">{revealError}</p>}
        </>}
        {encryption === "unlocked" && onLock && (
          <button id="browser-lock-all-tabs" className="secondary-button" onClick={() => void onLock()}>
            Lock now
          </button>
        )}
      </section>
      <DevicesSettings key={`${sessionScopeId ?? "none"}:${encryption}`} serverOffline={serverOffline} accountReadOnly={accountReadOnly} browserHost={browserHost} sessionKey={sessionScopeId ?? "unavailable"} unlocked={encryption === "unlocked"} onAddDevice={onAddDevice} />
      <NotificationSettings
        notifications={notifications}
        filters={filters}
        sources={sources}
        preferences={preferences}
        onPreferences={onPreferences}
        onMute={onMute}
        onPermission={onPermission}
        onOpenSettings={onOpenSettings}
        headsSupported={headsSupported}
        locked={encryption !== "unlocked" || Boolean(appLock?.locked)}
        writesPaused={writesPaused}
        pauseNoticeVisible={pauseNoticeVisible}
        soundSettings={!browserHost && bridge.get_notification_sound && bridge.set_notification_sound
          ? { load: bridge.get_notification_sound, save: bridge.set_notification_sound } : undefined}
        persistenceSettings={persistenceSettings}
        quickReplySettings={!browserHost ? quickReplySettings : undefined}
      />
      <ReactionSettings show={showReactions} onShow={onShowReactions} />
      <section id="settings-theme" className="settings-card" data-settings-section="theme">
        <h2>Appearance</h2>
        <p>Choose how Peppy follows your system appearance.</p>
        <div className="settings-control-row settings-theme-row">
          <label>
            Theme
            <select
              value={theme}
              onChange={(event) => onTheme(event.target.value as Theme)}
            >
              <option>system</option>
              <option>light</option>
              <option>dark</option>
            </select>
          </label>
        </div>
        <div id="settings-accent-color-row" className="settings-control-row settings-theme-row">
          <div className="settings-field">
            <label htmlFor="settings-accent-color-trigger">Accent color</label>
            <TonePicker id="settings-accent-color" variant="field" seed="accent" value={accentColor ?? undefined} automaticIndex={CORAL_ACCENT_INDEX} label="Accent color" defaultLabel="Default (Coral)" swatchColor={accentSwatchColor} describedBy={accentError ? "settings-accent-color-error" : undefined} onSelect={onAccentColor} />
          </div>
          {accentError && <p id="settings-accent-color-error" className="settings-error" role="alert">{accentError}</p>}
        </div>
        <fieldset id="settings-navigation-position" className="settings-option-cards" aria-describedby="settings-navigation-position-hint">
          <legend>Navigation position</legend>
          <p id="settings-navigation-position-hint" className="settings-field-hint">Choose where the navigation buttons appear.</p>
          <div id="settings-navigation-position-options" className="settings-option-card-list">
            <label data-option="side-rail">
              <input
                type="radio"
                name="navigation-position"
                value="side-rail"
                checked={navigationPosition === "side-rail"}
                onChange={() => onNavigationPosition("side-rail")}
              />
              <span>Side rail</span>
            </label>
            <label data-option="title-bar">
              <input
                type="radio"
                name="navigation-position"
                value="title-bar"
                checked={navigationPosition === "title-bar"}
                onChange={() => onNavigationPosition("title-bar")}
              />
              <span>Title bar</span>
            </label>
          </div>
        </fieldset>
        {!browserHost && <fieldset id="settings-popout-mode" className="settings-option-cards" aria-describedby="settings-popout-mode-hint">
          <legend>Pop-out button opens</legend>
          <p id="settings-popout-mode-hint" className="settings-field-hint">Applies to the pop-out button in the conversation header and in the conversation list.</p>
          <div id="settings-popout-mode-options" className="settings-option-card-list">
            <label data-option="chat-bubble">
              <input
                type="radio"
                name="popout-mode"
                value="bubble"
                checked={popoutMode === "bubble"}
                aria-labelledby="settings-popout-mode-bubble-title"
                aria-describedby="settings-popout-mode-bubble-description"
                onChange={() => onPopoutMode("bubble")}
              />
              <span id="settings-popout-mode-bubble-title">Chat bubble</span>
              <span id="settings-popout-mode-bubble-description">Floating bubble that stays on top and expands into the conversation.</span>
            </label>
            <label data-option="window">
              <input
                type="radio"
                name="popout-mode"
                value="window"
                checked={popoutMode === "window"}
                aria-labelledby="settings-popout-mode-window-title"
                aria-describedby="settings-popout-mode-window-description"
                onChange={() => onPopoutMode("window")}
              />
              <span id="settings-popout-mode-window-title">Window</span>
              <span id="settings-popout-mode-window-description">A normal conversation window without a bubble.</span>
            </label>
          </div>
        </fieldset>}
        <div id="settings-compact-timestamps-group">
          <div id="settings-compact-timestamps-row" className="settings-control-row">
            <label htmlFor="settings-compact-timestamps">Show times only after a pause</label>
            <span className="settings-switch">
              <input id="settings-compact-timestamps" type="checkbox" role="switch" checked={compactTimestamps} aria-checked={compactTimestamps} aria-describedby="settings-compact-timestamps-hint" onChange={(event) => onCompactTimestamps(event.target.checked)} />
              <span aria-hidden />
            </span>
          </div>
          <p id="settings-compact-timestamps-hint" className="settings-field-hint">Hides times on messages sent within an hour of the next one.</p>
        </div>
        <fieldset id="settings-attachment-layout" className="settings-option-cards">
          <legend>Attached image layout</legend>
          <div id="settings-attachment-layout-options" className="settings-option-card-list">
            <label data-option="preview-tiles">
              <input type="radio" name="attachment-layout" value="tiles" checked={attachmentLayout === "tiles"} onChange={() => onAttachmentLayout("tiles")} />
              <span>Preview tiles</span>
            </label>
            <label data-option="compact-rows">
              <input type="radio" name="attachment-layout" value="rows" checked={attachmentLayout === "rows"} onChange={() => onAttachmentLayout("rows")} />
              <span>Compact rows</span>
            </label>
          </div>
        </fieldset>
        <div id="settings-group-sender-avatars-group">
          <div id="settings-group-sender-avatars-row" className="settings-control-row">
            <label htmlFor="settings-group-sender-avatars">Show sender avatars in group conversations</label>
            <span className="settings-switch">
              <input id="settings-group-sender-avatars" type="checkbox" role="switch" checked={groupSenderAvatars} aria-checked={groupSenderAvatars} aria-describedby="settings-group-sender-avatars-hint" onChange={(event) => onGroupSenderAvatars(event.target.checked)} />
              <span aria-hidden />
            </span>
          </div>
          <p id="settings-group-sender-avatars-hint" className="settings-field-hint">Shows a photo or initials beside the last message in each stretch from one person. Names always show.</p>
        </div>
      </section>
      {desktop?.startupSupported && <section id="settings-startup" className="settings-card" data-settings-section="startup" data-startup-state={startupStatus?.state ?? (desktop.startAtLogin ? "on" : "off")}>
        <h2>Startup</h2>
        <div id="start-at-login-row" className="settings-control-row"><label htmlFor="start-at-login">{peppyCopy.startup_login_label}</label><span className="settings-switch"><input id="start-at-login" type="checkbox" role="switch" checked={startupStatus ? startupStatus.state === "on" || startupStatus.state === "approval_needed" : desktop.startAtLogin} aria-checked={startupStatus ? startupStatus.state === "on" || startupStatus.state === "approval_needed" : desktop.startAtLogin} disabled={savingStartup || startupStatus?.state === "unavailable"} onChange={async event => {
          setSavingStartup(true); setStartupError("");
          try { await onStartAtLogin(event.target.checked); await reloadStartupStatus(); }
          catch (error) { setStartupError(errorText(error)); }
          finally { setSavingStartup(false); }
        }} /><span aria-hidden /></span></div>
        <div id="startup-open-background-row" className="settings-control-row"><label htmlFor="startup-open-background">{peppyCopy.startup_background_label}</label><span className="settings-switch"><input id="startup-open-background" type="checkbox" role="switch" checked={startupStatus?.background ?? desktop.background} aria-checked={startupStatus?.background ?? desktop.background} disabled={savingStartup || !startupStatus || startupStatus.state === "off" || startupStatus.state === "unavailable"} aria-describedby="startup-open-background-hint" onChange={async event => {
          const background = event.target.checked;
          setSavingStartup(true); setStartupError("");
          try { await bridge.set_open_in_background(background); await reloadStartupStatus(); }
          catch (error) { setStartupError(errorText(error)); await reloadStartupStatus().catch(() => undefined); }
          finally { setSavingStartup(false); }
        }} /><span aria-hidden /></span></div>
        <p id="startup-open-background-hint" className="settings-field-hint">{startupStatus?.state === "off" ? peppyCopy.startup_background_needs_login : platform === "macos" ? peppyCopy.startup_background_hint_menu_bar : peppyCopy.startup_background_hint_notification_area}</p>
        <p id="startup-status" role="status">{startupStatus?.state === "approval_needed" ? platform === "macos" ? peppyCopy.startup_approval_macos : peppyCopy.startup_approval_windows : startupStatus?.state === "unavailable" ? peppyCopy.startup_unavailable : ""}</p>
        {startupStatus?.state === "approval_needed" && platform === "macos" && <button id="startup-open-login-items" className="secondary-button" type="button" disabled={savingStartup} onClick={() => { setOpenLoginItemsError(false); void bridge.open_login_items().catch(() => setOpenLoginItemsError(true)); }}>{peppyCopy.startup_open_login_items_cta}</button>}
        {openLoginItemsError && <p id="startup-open-login-items-error" className="settings-error" role="alert">{peppyCopy.startup_open_login_items_failed}</p>}
        {startupError && <p className="settings-error" role="alert">{startupError}</p>}
      </section>}
      {showUpdates && <UpdatesSettings />}
      {!browserHost && <AppLockSettings status={appLock} onChange={onAppLock} />}
      {hasAccount && <section id="settings-account-billing" className="settings-card" data-settings-section="account-billing">
        <h2>Account &amp; billing</h2>
        {(browserHost || (purchaseOptions?.website && billingSummary && billingSummary.provider !== "apple" && billingSummary.provider !== "google_play")) && <p>{peppyCopy.account_billing_intro_website}</p>}
        {!browserHost && purchaseOptions?.appStore && billingSummary && (billingSummary.provider === "none" || billingSummary.provider === "apple") && <p>{peppyCopy.account_billing_intro_app_store}</p>}
        {billingSummary?.provider === "google_play" && billingSummary.manageableIn === "google_play" && <p id="account-billing-intro-google-play">{peppyCopy.account_billing_intro_google_play}</p>}
        {browserHost && accountUrl && <a id="settings-account-portal-link" className="secondary-button" href={accountUrl} target="_blank" rel="noopener noreferrer">{peppyCopy.account_portal_cta}<span className="visually-hidden"> (opens in a new tab)</span></a>}
        {browserHost ? <>
          <AttachmentStorageSettingsHost sessionScopeId={sessionScopeId} manageStorageUrl={accountUrl} />
          {hostedDeployment && <FeedbackSettings browserHost accountUrl={accountUrl} bridge={bridge} />}
        </> : <HostedAccountSettings sessionScopeId={sessionScopeId} onAccountDeleted={onAccountDeleted} onDeletionCanceled={onDeletionCanceled} />}
      </section>}
      {typingShare !== undefined && <RcsFeaturesSettings typingShare={{ enabled: typingShare, onChange: onTypingShare, error: typingShareError }} />}
      <AdvancedSettings sessionKey={sessionScopeId ?? "unavailable"} unlocked={encryption === "unlocked"} serverOffline={serverOffline} browserHost={browserHost} onOpenDevices={() => { selectSection("devices"); window.setTimeout(() => document.getElementById("mcp-custody-review")?.focus(), 0); }} />
      {diagnostics && <DiagnosticsSettings bridge={diagnostics} />}
      <PrivacySettings browserHost={browserHost} bridge={bridge} />
      </div>
      </div>
    </section>
  );
}

function StatusDetails({
  connection,
  encryption,
  syncProgress,
  serverUnreachable = false,
}: {
  connection?: DesktopSnapshot["connection"];
  encryption?: DesktopSnapshot["encryption"]["state"];
  syncProgress?: DesktopSnapshot["syncProgress"];
  serverUnreachable?: boolean;
}) {
  if (!connection || !encryption)
    return <div className="status-details" data-status-details><div className="status-details-row">Loading status…</div></div>;
  const text = syncStatusText(encryption);
  const icon = encryption === "unlocked"
    ? <LockOpen size={14} aria-hidden />
    : encryption === "mismatch"
      ? <LockKeyhole size={14} aria-hidden />
      : <Lock size={14} aria-hidden />;
  const label = connectionText(connection);
  const history = encryption !== "locked" ? historyStatus(syncProgress) : undefined;
  return (
    <div className="status-details" data-status-details>
      <div className="status-details-row" data-disclosure="sync-state" data-sync-state={encryption} title={text}>
        {icon}<span>{text}</span>
      </div>
      {history && <div className="status-details-row" data-disclosure="history" data-history-phase={history.phase}>
        <History size={14} aria-hidden />
        <dl className="status-details-pair"><dt>{history.phase === "updating" ? UPDATE_STATUS_TERM : "History"}</dt><dd>{history.value}</dd></dl>
      </div>}
      <div className="status-details-row" data-disclosure="carrier-sms" title="Carrier SMS/MMS not end-to-end encrypted">
        <ShieldAlert size={14} aria-hidden />
        <span>Carrier SMS/MMS not end-to-end encrypted</span>
      </div>
      <div id="connection-status" className="status-details-row" data-connection-state={connection.state} data-error-code={connection.errorCode} data-server-unreachable={serverUnreachable || undefined} title={label}>
        <span className="connection-dot" aria-hidden />
        <span>{label}</span>
      </div>
    </div>
  );
}

function ReenrolScreen({ onRetry, onStart, enrollment }: { onRetry(): Promise<void>; onStart(): Promise<void>; enrollment?: ReactNode }) {
  const headingRef = useRef<HTMLHeadingElement>(null);
  const [retrying, setRetrying] = useState(false);
  const [stillUnavailable, setStillUnavailable] = useState(false);
  const [starting, setStarting] = useState(false);

  useEffect(() => { headingRef.current?.focus(); }, []);

  const retry = async () => {
    setRetrying(true);
    setStillUnavailable(false);
    try {
      await onRetry();
    } catch {
      setStillUnavailable(true);
    } finally {
      setRetrying(false);
    }
  };
  const start = async () => {
    setStarting(true);
    try {
      await onStart();
    } finally {
      setStarting(false);
    }
  };

  return <section id="reenrol-screen" data-error-code="database-key-unavailable" aria-labelledby="reenrol-headline">
    <h2 ref={headingRef} id="reenrol-headline" tabIndex={-1}>{peppyCopy.reenrol_headline}</h2>
    <p id="reenrol-body">{peppyCopy.reenrol_body}</p>
    <p id="reenrol-data-kept">{peppyCopy.reenrol_data_kept}</p>
    <p id="reenrol-draft-warning" data-tone="warning"><TriangleAlert size={16} aria-hidden /> {peppyCopy.reenrol_drafts_warning}</p>
    <div className="enrollment-actions">
      <button id="reenrol-start-button" className="primary-button" type="button" disabled={starting} onClick={() => void start()}>{peppyCopy.reenrol_start_cta}</button>
      <button id="reenrol-retry-button" className="secondary-button" type="button" disabled={retrying || starting} onClick={() => void retry()}>{peppyCopy.try_again}</button>
    </div>
    {retrying && <BusyIndicator id="reenrol-retry-busy" label={peppyCopy.production_pairing_checking} />}
    <p id="reenrol-retry-status" role="status">{stillUnavailable ? peppyCopy.reenrol_still_unavailable : ""}</p>
    {enrollment && <div id="reenrol-enrollment">{enrollment}</div>}
  </section>;
}

function AccountDeletionResultDialog({ result, onDone }: { result: { outcome: "deleted" | "pending"; provider: BillingProvider; executeAfter?: string | null; cancellable?: boolean }; onDone(): void }) {
  const deleted = result.outcome === "deleted";
  const processing = !deleted && result.cancellable === false;
  const deadline = result.executeAfter ? Date.parse(result.executeAfter) : NaN;
  const [managementFailed, setManagementFailed] = useState(false);
  const openSubscriptionManagement = async () => {
    setManagementFailed(false);
    try {
      await bridge.open_store_subscriptions();
    } catch {
      setManagementFailed(true);
    }
  };
  const openGooglePlayManagement = async () => {
    setManagementFailed(false);
    try {
      await bridge.open_play_subscriptions();
    } catch {
      setManagementFailed(true);
    }
  };
  return <Modal
    id="account-deletion-result-dialog"
    title={deleted ? peppyCopy.account_deleted_title : processing ? peppyCopy.account_delete_processing_title : peppyCopy.account_delete_pending_title}
    onDismiss={onDone}
    actions={<button id="account-deletion-result-done" className="primary-button" type="button" onClick={onDone}>{peppyCopy.account_deleted_done}</button>}
  >
    <p id="account-deletion-result-body">{deleted ? peppyCopy.account_deleted_body : processing ? peppyCopy.account_delete_pending_status : Number.isFinite(deadline) ? formatCopy(peppyCopy.account_delete_pending_body, formatDeletionDeadline(deadline / 1000)) : peppyCopy.account_delete_pending_body_nodate}</p>
    {result.provider === "apple" && <><p id="account-deletion-result-apple-notice">{peppyCopy.account_deleted_apple_body}</p><button id="account-deletion-result-manage" className="secondary-button" type="button" onClick={() => void openSubscriptionManagement()}>{peppyCopy.store_manage_cta}</button>{managementFailed && <p id="account-deletion-result-management-error" role="alert">{peppyCopy.store_manage_failed}</p>}</>}
    {result.provider === "google_play" && <><p id="account-deletion-result-google-play-notice">{peppyCopy.account_deleted_google_play_body}</p><button id="account-deletion-result-google-play-manage" className="secondary-button" type="button" onClick={() => void openGooglePlayManagement()}>{peppyCopy.google_play_manage_cta}</button>{managementFailed && <p id="account-deletion-result-management-error" role="alert">{peppyCopy.google_play_manage_failed}</p>}</>}
  </Modal>;
}

/* ------------------------------------------------------------------ app */

export function hasInFlightAttachmentPreparation(
  preparations: ReadonlyMap<string, ReadonlySet<unknown>>,
  key: string,
  resolve: (key: string) => string,
) {
  const resolvedKey = resolve(key);
  return Array.from(preparations.keys()).some((preparingKey) =>
    resolve(preparingKey) === resolvedKey && (preparations.get(preparingKey)?.size ?? 0) > 0,
  );
}

export function App({ hostKind = "native", fixedOrigin, accountUrl, hostedDeployment = false }: { hostKind?: "native" | "browser"; fixedOrigin?: string; accountUrl?: string; hostedDeployment?: boolean } = {}) {
  const launchProgress = useLaunchProgress(bridge);
  const platform = detectPlatform();
  const browserHost = hostKind === "browser";
  const narrowBrowserViewport = useNarrowBrowserViewport(browserHost);
  useUpdateActivity(!browserHost);
  useEffect(() => browserHost ? undefined : installHostedSessionRenewal(renewHostedSession), [browserHost]);
  const [composerConversation] = useState(() =>
    typeof window === "undefined"
      ? null
      : readComposerConversation(window.location.search),
  );
  const [headPanelBootstrap] = useState(() =>
    typeof window !== "undefined" && new URLSearchParams(window.location.search).get("head") === "1",
  );
  const [mcpAuthorizationId, setMcpAuthorizationId] = useState(() =>
    typeof window === "undefined" ? null : new URLSearchParams(window.location.search).get("mcp_authorize"),
  );
  const dismissMcpAuthorization = () => {
    const url = new URL(window.location.href);
    url.searchParams.delete("mcp_authorize");
    window.history.replaceState(window.history.state, "", `${url.pathname}${url.search}${url.hash}`);
    setMcpAuthorizationId(null);
  };
  useMaximizeButtonRegion({ bridge, enabled: !browserHost && platform === "windows" && !composerConversation });
  const [snapshot, setSnapshot] = useState<DesktopSnapshot | null>(null);
  const { visibleNotice: updateNoticeVisible } = useDelayedWritesPaused(isWritesPaused(snapshot));
  const lastFocusedControl = useRef<HTMLElement | null>(null);
  useEffect(() => {
    const rememberFocus = (event: FocusEvent) => {
      if (event.target instanceof HTMLElement && event.target !== document.body) lastFocusedControl.current = event.target;
    };
    document.addEventListener("focusin", rememberFocus);
    return () => document.removeEventListener("focusin", rememberFocus);
  }, []);
  useLayoutEffect(() => {
    if (!isWritesPaused(snapshot)) return;
    const previous = lastFocusedControl.current;
    if (!(previous instanceof HTMLButtonElement || previous instanceof HTMLInputElement) || !previous.disabled) return;
    if (document.activeElement !== document.body && document.activeElement !== previous) return;
    const owner = previous.closest("#composer-area") ? document.getElementById("composer-textarea") : previous.closest<HTMLElement>("[data-conversation-row]")?.querySelector<HTMLElement>(".conversation-select")
      ?? previous.closest<HTMLElement>("[role='dialog']")?.querySelector<HTMLElement>("button:not(:disabled)")
      ?? document.getElementById("thread-pane-header")?.querySelector<HTMLElement>("h1")
      ?? mainRef.current;
    if (owner) {
      if (!owner.hasAttribute("tabindex") && owner.tagName === "H1") owner.tabIndex = -1;
      owner.focus();
    }
  }, [isWritesPaused(snapshot)]);
  const [nameChooser, setNameChooser] = useState<{ address: string; conversationId: string; returnFocus: HTMLElement | null }>();
  const [pendingSends, setPendingSends] = useState<PendingSend[]>([]);
  const [cancelledSendRestore, setCancelledSendRestore] = useState<CancelledSendRestore>();
  const cancelledSendSequence = useRef(0);
  const appliedCancelledSendSequence = useRef(0);
  const pendingSendGeneration = useRef(0);
  const pendingSendSessionScope = useRef<string | undefined>(undefined);
  useEffect(() => {
    if (pendingSendSessionScope.current !== undefined && pendingSendSessionScope.current !== snapshot?.sessionScopeId)
      setPendingSends([]);
    pendingSendSessionScope.current = snapshot?.sessionScopeId;
  }, [snapshot?.sessionScopeId]);
  const [conversationTab, setConversationTab] = useState<ConversationTab>("messages");
  const [lightboxItems, setLightboxItems] = useState<GalleryMediaPageItem[] | null>(null);
  const [lightboxAttachmentId, setLightboxAttachmentId] = useState<string>();
  const [lightboxMessageId, setLightboxMessageId] = useState<string>();
  const [lightboxHasNextPage, setLightboxHasNextPage] = useState(false);
  const [lightboxHasPreviousPage, setLightboxHasPreviousPage] = useState(false);
  const [mediaStateHint, setMediaStateHint] = useState(0);
  const pendingSaveIdsRef = useRef(new Set<string>());
  const gallerySessionRef = useRef(0);
  const galleryConversationRef = useRef<string | undefined>(undefined);
  const galleryCacheRef = useRef<{ conversationId?: string; items: GalleryMediaPageItem[]; newerCursor?: Parameters<typeof bridge.media_page>[1]; olderCursor?: Parameters<typeof bridge.media_page>[1] }>({ items: [] });
  const requestOriginal = useCallback((id: string) => bridge.load_media(id), []);
  // Memoized: the provider keys its hydration queue (dedupe and concurrency) on these identities.
  const hydrateMediaPreview = useMemo(() => bridge.hydrate_media_preview ? (messageId: string, id: string) => bridge.hydrate_media_preview!(messageId, id) : undefined, []);
  const copyNativeImage = useMemo(() => bridge.copy_attachment_image ? (id: string) => bridge.copy_attachment_image!(id) : undefined, []);
  const checkOriginal = useCallback((id: string) => bridge.open_media(id), []);
  const releaseMedia = useCallback((id: string, url: string) => { void bridge.close_media(id, url); }, []);
  const [hasOlderMessages, setHasOlderMessages] = useState(false);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [historyError, setHistoryError] = useState<string | null>(null);
  const [historyErrorDirection, setHistoryErrorDirection] = useState<"older" | "newer">("older");
  const [suppressHistoryAnnouncements, setSuppressHistoryAnnouncements] = useState(false);
  const [historyExhausted, setHistoryExhausted] = useState(false);
  const [appLock, setAppLock] = useState<AppLockStatus | null>(null);
  const appLockRef = useRef(appLock);
  const appLockStatusRequest = useRef(0);
  const previousLockCode = useRef<string | undefined>(undefined);
  appLockRef.current = appLock;
  const [mode, setMode] = useState<"hosted" | "self-hosted">(() => browserHost ? "self-hosted" : savedSetupMode());
  const headPanel = Boolean(composerConversation && (snapshot?.head.panel ?? headPanelBootstrap));
  const [selected, setSelected] = useState(composerConversation ?? "");
  useEffect(() => { setNameChooser(undefined); }, [selected]);
  const [attachmentViews, setAttachmentViews] = useState<
    Record<string, AttachmentView>
  >({});
  const [draftViews, setDraftViews] = useState<Record<string, Draft>>({});
  const acknowledgedDraftNoticeRef = useRef<string | undefined>(undefined);
  const [resolvingDraftConflict, setResolvingDraftConflict] = useState(false);
  const resolutionRef = useRef<number | undefined>(undefined);
  const draftSessionGenerationRef = useRef(0);
  const attachmentSessionRef = useRef<string | undefined>(undefined);
  const mergeDraftAttachmentViews = (draft: Draft) => {
    if (!draft.attachments?.length) return;
    setAttachmentViews((current) => ({
      ...current,
      ...Object.fromEntries(draft.attachments!.map((view) => [
        view.id,
        current[view.id] ? retainOriginalPreview(current[view.id], view) : view,
      ])),
    }));
  };
  const rememberHostDraft = (draft: Draft) => {
    setDraftViews((current) => isOlderDraftView(current[draft.id], draft)
      ? current
      : { ...current, [draft.id]: draft });
    mergeDraftAttachmentViews(draft);
  };
  const resetDraftAttachmentCache = (sessionScopeId?: string) => {
    if (attachmentSessionRef.current === sessionScopeId) return;
    attachmentSessionRef.current = sessionScopeId;
    setAttachmentViews({});
    setDraftViews({});
  };
  const pastedAttachmentInFlight = useRef(new Map<string, number>());
  const attachmentPreparationInFlight = useRef(new Map<string, Set<symbol>>());
  const draftStoreGeneration = useRef(0);
  const [publicCopies, setPublicCopies] = useState<Record<string, PublicCopy>>(
    {},
  );
  const [theme, setTheme] = useState<Theme>(() => {
    try {
      const stored = JSON.parse(localStorage.getItem("peppy.layout.v1") ?? "{}");
      return typeof stored === "object" && stored !== null && !Array.isArray(stored)
        && ["light", "dark", "system"].includes(stored.theme as string)
        ? stored.theme as Theme
        : "system";
    } catch {
      return "system";
    }
  });
  const { accentColor, selectAccentColor, error: accentError } = useAccentColor();
  const persistedTheme = useRef(theme);
  useEffect(() => {
    if (theme === persistedTheme.current) return;
    persistedTheme.current = theme;
    persistLayoutPatch({ theme });
  }, [theme]);
  useEffect(() => {
    const themeClasses = ["theme-light", "theme-dark", "theme-system"];
    const root = document.documentElement;
    document.body.classList.remove(...themeClasses);
    root.classList.remove(...themeClasses);
    document.body.classList.add(`theme-${theme}`);
    root.classList.add(`theme-${theme}`);
    return () => {
      document.body.classList.remove(`theme-${theme}`);
      root.classList.remove(`theme-${theme}`);
    };
  }, [theme]);
  useEffect(() => {
    const syncTheme = (event: StorageEvent) => {
      if (event.key !== "peppy.layout.v1" || !event.newValue) return;
      try {
        const layout = JSON.parse(event.newValue);
        if (["light", "dark", "system"].includes(layout?.theme)) {
          persistedTheme.current = layout.theme;
          setTheme(layout.theme);
        }
      } catch {
        // Ignore another window's malformed optional layout data.
      }
    };
    window.addEventListener("storage", syncTheme);
    return () => window.removeEventListener("storage", syncTheme);
  }, []);
  const [navigationPosition, setNavigationPosition] = useState<NavigationPosition>(() => {
    try {
      const stored = JSON.parse(localStorage.getItem("peppy.layout.v1") ?? "{}");
      return typeof stored === "object" && stored !== null && !Array.isArray(stored) &&
          isNavigationPosition(stored.navigationPosition)
        ? stored.navigationPosition
        : "side-rail";
    } catch {
      return "side-rail";
    }
  });
  const [popoutMode, setPopoutMode] = useState<PopoutMode>(() => {
    try {
      const stored = JSON.parse(localStorage.getItem("peppy.layout.v1") ?? "{}");
      return typeof stored === "object" && stored !== null && !Array.isArray(stored) &&
          isPopoutMode(stored.popoutMode)
        ? stored.popoutMode
        : "bubble";
    } catch {
      return "bubble";
    }
  });
  const [compactTimestamps, setCompactTimestamps] = useState(readCompactTimestamps);
  const [shareTyping, setShareTyping] = useState(readShareTyping);
  const [typingShareError, setTypingShareError] = useState(false);
  const typingShareRequest = useRef(0);
  const confirmedTypingShare = useRef(shareTyping);
  const typingLinkAvailable = Boolean(snapshot?.rcsStatus?.gateways.some((gateway) => gateway.state !== "unlinked"));
  useEffect(() => {
    void bridge.set_typing_sharing?.({ enabled: readShareTyping() }).catch(() => undefined);
  }, [bridge.set_typing_sharing, typingLinkAvailable]);
  useEffect(() => {
    const syncShareTyping = (event: StorageEvent) => {
      if (event.key !== "peppy.layout.v1") return;
      const enabled = readShareTyping();
      setShareTyping(enabled);
      void bridge.set_typing_sharing?.({ enabled }).catch(() => undefined);
    };
    window.addEventListener("storage", syncShareTyping);
    return () => window.removeEventListener("storage", syncShareTyping);
  }, [bridge.set_typing_sharing]);
  const [attachmentLayout, setAttachmentLayout] = useState<AttachmentLayout>(readComposerAttachmentLayout);
  const [groupSenderAvatars, setGroupSenderAvatars] = useState(readGroupSenderAvatars);
  useEffect(() => {
    const syncAttachmentLayout = (event: StorageEvent) => {
      if (event.key !== "peppy.layout.v1" && event.key !== null) return;
      setAttachmentLayout(readComposerAttachmentLayout(event.newValue));
    };
    window.addEventListener("storage", syncAttachmentLayout);
    return () => window.removeEventListener("storage", syncAttachmentLayout);
  }, []);
  const [showReactions, setShowReactions] = useShowReactions();
  const [pendingReactionId, setPendingReactionId] = useState<string>();
  const [reactionFeedback, setReactionFeedback] = useState<{ reactionId: string; message: string }>();
  const [pendingToneSeeds, setPendingToneSeeds] = useState<Record<string, true>>({});
  const [toneErrors, setToneErrors] = useState<Record<string, string>>({});
  useEffect(() => {
    setPendingToneSeeds({});
    setToneErrors({});
  }, [snapshot?.encryption.profileFingerprint, snapshot?.encryption.state]);
  const [activeView, setActiveViewNow] = useState<"conversations" | "notifications" | "settings" | "contacts">(
    "conversations",
  );
  const [contactTarget, setContactTarget] = useState<(ContactTarget & { requestId: number })>();
  const [contactsBookCollapsed, setContactsBookCollapsed] = useState(readContactsBookCollapsed);
  const compactContacts = useMediaMatch(COMPACT_BOOK_LIST_QUERY);
  const setContactsBookCollapsedPersisted = (collapsed: boolean) => {
    setContactsBookCollapsed(collapsed);
    persistLayoutPatch({ contactsBookCollapsed: collapsed });
  };
  const contactTargetRequestId = useRef(0);
  const contactNavigation = useRef<ContactNavigationGuard | null>(null);
  const notificationNavigationEpoch = useRef(0);
  const registerContactNavigation = useCallback((guard: ContactNavigationGuard | null) => {
    contactNavigation.current = guard;
  }, []);
  const setActiveView = useCallback((view: typeof activeView) => {
    notificationNavigationEpoch.current += 1;
    if (view === "contacts" && contactNavigation.current) return;
    const navigate = () => {
      if (view !== "contacts") setContactTarget(undefined);
      if (view === "notifications") {
        setNotificationAppFilter(null);
        resetNotificationSearch();
      }
      setActiveViewNow(view);
    };
    if (contactNavigation.current) contactNavigation.current(navigate);
    else navigate();
  }, []);
  // "Free up space" opens Settings and asks it to scroll to its storage card. The request ends when Settings closes.
  const [storageSettingsRequest, setStorageSettingsRequest] = useState(0);
  const openStorageSettings = () => {
    setStorageSettingsRequest((value) => value + 1);
    setActiveView("settings");
  };
  useEffect(() => {
    if (activeView !== "settings") setStorageSettingsRequest(0);
  }, [activeView]);
  const revoked = snapshot?.connection.errorCode === "revoked";
  const settingsOpen = !revoked && activeView === "settings";
  const notificationsOpen = !revoked && activeView === "notifications";
  const notificationsLocked = snapshot?.encryption.state !== "unlocked";
  const contactsOpen = !revoked && activeView === "contacts";
  const [settingsSectionRequested, setSettingsSectionRequested] = useState<"notifications" | "notification-apps" | "devices" | "updates">();
  const [notificationAppFilter, setNotificationAppFilter] = useState<string | null>(null);
  const notificationSearchRequestId = useRef(0);
  const [actionNotice, setActionNotice] = useState<ActionNotice>();
  const [refreshNotice, setRefreshNotice] = useState("");
  const [composerFeedback, setComposerFeedback] = useState<ComposerFeedback>();
  const [draftNotice, setDraftNotice] = useState<DraftNoticeState>();
  const [draftSubscriptionError, setDraftSubscriptionError] = useState<string>();
  const draftNoticeSequence = useRef(0);
  const draftNoticesRef = useRef(new Map<string, DraftNoticeState>());
  // Drafts whose latest store save failed and has not yet been followed by a successful save.
  const saveFailedKeysRef = useRef(new Set<string>());
  const restartDraftSubscriptionRef = useRef<(() => void) | undefined>(undefined);
  const suspendDraftSubscriptionRef = useRef<(() => void) | undefined>(undefined);
  const bindDraftSubscriptionAfterRefreshRef = useRef<(() => void) | undefined>(undefined);
  const draftSubscriptionSessionRef = useRef<string | undefined>(undefined);
  const draftSubscriptionStateRef = useRef<{ session: string; unlocked: boolean } | undefined>(undefined);
  const reviewNoticeSequenceRef = useRef<number | undefined>(undefined);
  const noticeActionOwnedFocusRef = useRef(false);
  const noticeActionBusyRef = useRef(false);
  const [noticeActionBusy, setNoticeActionBusy] = useState(false);
  const [notificationActionNotice, setNotificationActionNotice] = useState<NotificationActionNoticeState | undefined>();
  const [notificationAnnouncement, setNotificationAnnouncement] = useState<NotificationAnnouncement>({ seq: 0, text: "" });
  const announceNotification = useCallback((text: string) => setNotificationAnnouncement(current => ({ seq: current.seq + 1, text })), []);
  const [notificationMuteNotice, setNotificationMuteNotice] = useState<NotificationMuteNoticeState>();
  const [notificationMutePending, setNotificationMutePending] = useState(false);
  const notificationMutePendingRef = useRef(false);
  const notificationMuteRetryRef = useRef<"mute" | "unmute">("mute");
  const notificationMuteGenerationRef = useRef(0);
  // Undo removes the toast that held focus; land on the restored app's newest row instead of the page.
  const focusRestoredNotification = (filter: AppFilter) => {
    const active = document.activeElement;
    if (active && active !== document.body && active !== mainRef.current) return;
    const newest = snapshotRef.current?.notifications
      .filter(notification => notification.target.sourceDeviceId === filter.sourceDeviceId && notification.packageName === filter.packageName)
      .reduce<MirroredNotification | undefined>((latest, notification) => !latest || notification.postedAt > latest.postedAt ? notification : latest, undefined);
    if (!newest) return;
    const { sourceDeviceId, notificationKey, lifetime } = newest.target;
    document.getElementById(`notif-${notificationTuple(sourceDeviceId, notificationKey, lifetime)}`)?.focus();
  };
  const runNotificationMute = (filter: AppFilter, label: string, muted: boolean) => {
    if (isWritesPaused(snapshotRef.current) || notificationsLocked || appLock?.locked || notificationMutePendingRef.current) return;
    const generation = notificationMuteGenerationRef.current;
    notificationMutePendingRef.current = true;
    setNotificationMutePending(true);
    notificationMuteRetryRef.current = muted ? "mute" : "unmute";
    void (async () => {
      try {
        await bridge.set_app_muted(filter.sourceDeviceId, filter.packageName, filter.appName, muted);
        if (generation !== notificationMuteGenerationRef.current) return;
        setNotificationMuteNotice(muted ? { kind: "success", filter, label } : undefined);
        const refreshed = refresh();
        void refreshed.catch(reportRefresh("Could not refresh notifications. "));
        if (!muted) void refreshed.then(() => requestAnimationFrame(() => focusRestoredNotification(filter)), () => undefined);
      } catch (error) {
        if (generation !== notificationMuteGenerationRef.current) return;
        if (errorCode(error) === "update-in-progress") {
          reportAction("Notification settings not changed: ")(error);
          return;
        }
        setNotificationMuteNotice({ kind: "error", filter, label });
      } finally {
        if (generation !== notificationMuteGenerationRef.current) return;
        notificationMutePendingRef.current = false;
        setNotificationMutePending(false);
      }
    })();
  };
  const [notificationFocus, setNotificationFocus] = useState<{ requestId: string; target: NotificationActivationTarget }>();
  const notificationFocusContext = useRef<{ requestId: string; context: ActivationContext; reported?: boolean } | undefined>(undefined);
  const notificationDrain = useRef<ReturnType<typeof createNotificationActivationDrain> | undefined>(undefined);
  const notificationNavigation = useRef<{ requestId: string; sessionGeneration: string; generation: number; epoch: number } | undefined>(undefined);
  const notificationHandler = useRef<(request: NotificationActivation, context: ActivationContext) => Promise<void>>(async () => {});
  const [addingPhone, setAddingPhone] = useState(false);
  const [addDeviceFromSettings, setAddDeviceFromSettings] = useState(false);
  const [initialEnrollmentActive, setInitialEnrollmentActive] = useState(false);
  const [phoneDeferredOrigin, setPhoneDeferredOrigin] = useState<string | null>(null);
  const openAddDevice = () => {
    setAddDeviceFromSettings(true);
    setAddingPhone(true);
    setActiveView("conversations");
  };
  const returnToSettingsIfRequested = () => {
    if (!addDeviceFromSettings) return;
    setAddDeviceFromSettings(false);
    setSettingsSectionRequested("devices");
    setActiveView("settings");
  };
  useEffect(() => {
    if (snapshot?.connection.state === "connected") setInitialEnrollmentActive(false);
  }, [snapshot?.connection.state]);
  // Names of contacts picked from search, shown on chips until the next native snapshot
  // resolves the same addresses. Display only; recipient IDs stay phone addresses.
  const [chosenContacts, setChosenContacts] = useState<ContactResolutionMap>({});
  const chosenContactsScope = useRef(snapshot?.sessionScopeId);
  const rememberContact = useCallback((suggestion: RecipientSuggestion) => {
    if (snapshot?.contactResolution?.[suggestion.id]) return;
    setChosenContacts((current) => ({
      ...current,
      [suggestion.id]: { contactId: "", bookId: "", displayName: suggestion.label, photoDataUrl: suggestion.avatarUrl },
    }));
  }, [snapshot?.contactResolution]);
  useEffect(() => {
    if (chosenContactsScope.current !== snapshot?.sessionScopeId) {
      chosenContactsScope.current = snapshot?.sessionScopeId;
      setChosenContacts({});
      return;
    }
    const resolved = snapshot?.contactResolution;
    if (!resolved) return;
    setChosenContacts((current) => {
      const unresolved = Object.fromEntries(Object.entries(current).filter(([address]) => !resolved[address]));
      return Object.keys(unresolved).length === Object.keys(current).length ? current : unresolved;
    });
  }, [snapshot?.contactResolution, snapshot?.sessionScopeId]);
  const [origin, setOrigin] = useState(() => browserHost ? fixedOrigin ?? "" : "");
  const [loading, setLoading] = useState(true);
  const [reenrolRequired, setReenrolRequired] = useState(false);
  const [reenrolEnrollment, setReenrolEnrollment] = useState(false);
  const reenrolEnrollmentRef = useRef(false);
  reenrolEnrollmentRef.current = reenrolEnrollment;
  const [deletionResult, setDeletionResult] = useState<{ outcome: "deleted" | "pending"; provider: BillingProvider; executeAfter?: string | null; cancellable?: boolean }>();
  const [refreshing, setRefreshing] = useState(false);
  const [sending, setSending] = useState(false);
  const [closingHead, setClosingHead] = useState(false);
  const [lifecyclePending, setLifecyclePending] = useState<string | null>(null);
  const [draftsVersion, rerender] = useReducer((count: number) => count + 1, 0);
  const defaultListWidth = typeof window !== "undefined" && window.innerWidth < 900 ? 240 : 280;
  const [listWidth, setListWidth] = useState(defaultListWidth);
  const [listCollapsed, setListCollapsed] = useState(false);
  const [mobileConversationListOpen, setMobileConversationListOpen] = useState(false);
  const [composerHeight, setComposerHeight] = useState<number | null>(null);
  const [recipientPosition, setRecipientPosition] = useState<RecipientPosition | null>(null);
  const [pendingRecipient, setPendingRecipient] = useState(false);
  const [newComposeKey, setNewComposeKey] = useState<string | null>(null);
  const [groupDetailsOpen, setGroupDetailsOpen] = useState(false);
  const [groupNotice, setGroupNotice] = useState<GroupComposerNotice | null>(null);
  const groupDetailsButtonRef = useRef<HTMLButtonElement>(null);
  const [deleteConfirmationId, setDeleteConfirmationId] = useState<string | null>(null);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [pendingPinId, setPendingPinId] = useState<string | null>(null);
  const [folderView, setFolderView] = useState<ConversationFolder>("inbox");
  const [folderConfirmation, setFolderConfirmation] = useState<{ id: string; name: string; from: ConversationFolder; to: "archived" | "spam"; trigger: HTMLButtonElement }>();
  const [pendingFolderId, setPendingFolderId] = useState<string | null>(null);
  const [pendingFolder, setPendingFolder] = useState<ConversationFolder>();
  const [folderNotice, setFolderNotice] = useState<FolderNotice>();
  const [senderUnblockConfirmation, setSenderUnblockConfirmation] = useState<{ id: string; name: string; canUnblock: boolean; trigger: HTMLButtonElement }>();
  const [pendingSenderUnblockId, setPendingSenderUnblockId] = useState<string | null>(null);
  const [senderUnblockNotice, setSenderUnblockNotice] = useState<SenderUnblockNoticeState>();
  const [overlayHeight, setOverlayHeight] = useState<number | null>(
    typeof ResizeObserver === "undefined" ? 120 : null,
  );
  const [viewportWidth, setViewportWidth] = useState(() =>
    typeof window === "undefined" ? 0 : window.innerWidth,
  );
  const [paneHeight, setPaneHeight] = useState(0);
  const [composerChrome, setComposerChrome] = useState(0);
  const mainRef = useRef<HTMLElement>(null);
  const listWidthLive = useRef(defaultListWidth);
  const listDragWidth = useRef(defaultListWidth);
  const previousListWidth = useRef(defaultListWidth);
  const composerHeightLive = useRef<number | null>(null);
  const persistedComposerHeight = useRef<number | null>(null);
  const persistedHeadComposerHeight = useRef<number | null>(null);
  const paneRef = useRef<HTMLElement>(null);
  const desktopBodyRef = useRef<HTMLDivElement>(null);
  const [threadListHost, setThreadListHost] = useState<HTMLElement | null>(null);
  const messageListRef = useRef<HTMLDivElement>(null);
  const listScrollAttentionAt = useRef(Number.NEGATIVE_INFINITY);
  const [pendingSeenReactions, setPendingSeenReactions] = useState<{ scope: string; ids: ReadonlySet<string> }>();
  const pendingSeenReactionsRef = useRef<typeof pendingSeenReactions>(undefined);
  const [messageListNode, setMessageListRoot] = useState<HTMLDivElement | null>(null);
  const [reactionSeenRevision, setReactionSeenRevision] = useState(0);
  const badgeReadIds = useRef(new Set<string>());
  const nearBottom = useRef(true);
  const seenMessageIds = useRef(new Set<string>());
  const pendingSeenMessageIds = useRef(new Set<string>());

  const selectedRef = useRef(selected);
  const activeViewRef = useRef(activeView);
  activeViewRef.current = activeView;
  const snapshotRef = useRef(snapshot);
  snapshotRef.current = snapshot;
  const sendingRef = useRef(false);
  const inFlightSendRef = useRef<{ settled: Promise<SendSettlement>; resolve(s: SendSettlement): void } | null>(null);
  const closingRef = useRef(false);
  const loadSequence = useRef(0);
  const historySequence = useRef(0);
  const historyLoadingRef = useRef(false);
  const historyErrorRef = useRef<string | null>(null);
  const historyErrorDirectionRef = useRef<"older" | "newer">("older");
  const hasOlderMessagesRef = useRef(false);
  const messageListScrollTopRef = useRef(0);
  const historyFocusPendingRef = useRef(false);
  const mountedRef = useRef(true);
  const focusRestorePendingRef = useRef(false);
  const pinFocusRestoreRef = useRef<PinFocusRestoreIntent | undefined>(undefined);
  const folderChangeInFlightRef = useRef(false);
  const folderSettlingRef = useRef(false);
  const folderSettleTimerRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const refreshInFlightRef = useRef(false);
  const pendingRefreshRef = useRef<RefreshRequest | undefined>(undefined);
  const toneOperationRef = useRef(new Map<string, number>());
  const toneOperationSequence = useRef(0);
  const retryPendingSavesInFlightRef = useRef(false);
  const retryPendingSavesQueuedRef = useRef(false);
  const overlayHeightRef = useRef(overlayHeight);
  overlayHeightRef.current = overlayHeight;
  const lastUnobscuredHeight = useRef<number | null>(null);
  const historyAnchorRef = useRef<{ row: HTMLElement; top: number } | null>(null);
  const detachedHistoryRef = useRef<DetachedHistory | undefined>(undefined);
  const [detachedHistory, setDetachedHistory] = useState<DetachedHistory>();
  const [searchTargetId, setSearchTargetId] = useState<string>();
  const searchReadGate = useRef(false);
  const navigationGeneration = useRef(0);
  const reactionNavigationGeneration = useRef(0);
  const previousReactionViewRef = useRef(activeView);
  const latestRequiredConversationRef = useRef<string | undefined>(undefined);
  const previousSearchSessionKeyRef = useRef("");
  const activateSearchHitHandlerRef = useRef<(hit: LocalSearchHit) => void>(() => undefined);
  const messageRows = useRef(new Map<string, HTMLElement>());
  const reactionHighlightCleanupRef = useRef<() => void>(() => undefined);
  useEffect(() => () => reactionHighlightCleanupRef.current(), []);
  const invalidateReactionNavigation = () => {
    reactionNavigationGeneration.current += 1;
    setPendingReactionId(undefined);
    setReactionFeedback(undefined);
  };
  useEffect(() => {
    if (previousReactionViewRef.current === activeView) return;
    previousReactionViewRef.current = activeView;
    invalidateReactionNavigation();
  }, [activeView]);
  useEffect(() => {
    const intent = pinFocusRestoreRef.current;
    if (!intent || pendingPinId) return;
    pinFocusRestoreRef.current = undefined;
    intent.dispose();
    if (intent.userMovedFocus || document.activeElement !== document.body) return;
    [...document.querySelectorAll<HTMLButtonElement>("[data-pin-conversation-id]")]
      .find((button) => button.dataset.pinConversationId === intent.conversationId && !button.disabled)?.focus();
  }, [pendingPinId]);
  useEffect(() => () => pinFocusRestoreRef.current?.dispose(), []);
  const handlers = useRef({
    rekeyed: (_from: string, _to: string) => {},
    close: () => {},
    collapse: () => {},
    closeHead: () => {},
  });
  const storeRef = useRef<DraftStore | null>(null);
  if (!storeRef.current)
    storeRef.current = new DraftStore(async (input) => {
      const session = currentSearchSessionKey();
      const generation = draftStoreGeneration.current;
      const saved = await bridge.save_draft(input);
      if (mountedRef.current && session === currentSearchSessionKey() && generation === draftStoreGeneration.current)
        rememberHostDraft(saved);
      return saved;
    }, {
      changed: () => rerender(),
      rekeyed: (from, to) => handlers.current.rekeyed(from, to),
      notice: (key, event) => {
        if (event === "remote-update" && !document.hasFocus() && !store.hasUndo(key) && !store.retainedText(key)) return;
        if (event === "save-failed") saveFailedKeysRef.current.add(key);
        // A healthy ordinary save stays quiet; "saved" shows only when it settles an earlier failure on this draft.
        if (event === "saved" && !saveFailedKeysRef.current.delete(key)) return;
        draftNoticeSequence.current += 1;
        const next = { key, event, sequence: draftNoticeSequence.current };
        setDraftNotice((current) => {
          draftNoticesRef.current.set(key, next);
          if (key !== selectedRef.current) return current;
          if (!current) return next;
          if (current.key !== key) return current;
          const priority: Record<DraftNoticeState["event"], number> = {
            "save-failed": 0,
            "stale-send": 1,
            "send-failed": 2,
            moved: 3,
            "remote-update": 4,
            replaced: 4,
            saved: 5,
            loading: 7,
          };
          if (current.event === "save-failed" && event === "saved" && !storeRef.current?.get(key)?.error) return next;
          return priority[current.event] < priority[event] ? current : next;
        });
      },
    }, async (conversationId) => {
      const state = await bridge.load_state(conversationId);
      if (state.activeConversationId !== conversationId || state.encryption.state !== "unlocked") return undefined;
      if (state.draft && state.draft.conversationId !== conversationId)
        throw new Error("Draft lookup returned a different conversation.");
      return state.draft;
    });
  const store = storeRef.current;
  const setDetached = (next: DetachedHistory | undefined) => {
    detachedHistoryRef.current = next;
    setDetachedHistory(next);
  };
  const setHistoryLoadError = (next: string | null, direction: "older" | "newer" = "older") => {
    const nextDirection = next ? direction : "older";
    historyErrorRef.current = next;
    historyErrorDirectionRef.current = nextDirection;
    setHistoryError(next);
    setHistoryErrorDirection(nextDirection);
  };
  const clearDetached = () => {
    const detachedConversationId = detachedHistoryRef.current?.conversationId;
    if (detachedConversationId) latestRequiredConversationRef.current = detachedConversationId;
    navigationGeneration.current += 1;
    loadSequence.current += 1;
    historySequence.current += 1;
    if (historyErrorRef.current && historyErrorDirectionRef.current === "newer")
      setHistoryLoadError(null);
    setDetached(undefined);
    setSearchTargetId(undefined);
    searchReadGate.current = false;
  };
  const currentSearchSessionKey = () => createSearchSessionKey(snapshotRef.current, Boolean(appLockRef.current?.locked));
  const applyAuthoritativeDraft = (draft: Draft) => {
    rememberHostDraft(draft);
    store.applyAuthoritative(draft);
  };
  const adoptHostDraft = (conversationId: string, draft?: Draft) => {
    if (draft) rememberHostDraft(draft);
    store.adopt(conversationId, draft);
  };
  /**
   * A merge redirected a requested conversation to its survivor. The draft moves with it and the selection
   * follows in place; choose() is not called, so scroll, history window and folder view stay as they are.
   */
  const followRequestedRedirect = (redirect: { from: string; to: string }) => {
    store.followMerge(redirect.from, redirect.to);
    if (latestRequiredConversationRef.current === redirect.from) latestRequiredConversationRef.current = redirect.to;
    // An open gallery keeps its loaded items and cursors, and paginates under the survivor's ID from now on.
    if (sameGalleryConversation(galleryCacheRef.current.conversationId, redirect.from)) {
      galleryCacheRef.current = { ...galleryCacheRef.current, conversationId: redirect.to };
    }
    if (sameGalleryConversation(galleryConversationRef.current, redirect.from)) galleryConversationRef.current = redirect.to;
    if (selectedRef.current !== redirect.from) return;
    selectedRef.current = redirect.to;
    setSelected(redirect.to);
  };
  const reconcileMissingRequestedConversation = (requestedConversationId: string | undefined, state: DesktopSnapshot) => {
    if (
      !requestedConversationId ||
      state.activeConversationId === requestedConversationId ||
      state.requestedRedirect?.from === requestedConversationId ||
      state.conversations.some((conversation) => conversation.id === requestedConversationId)
    ) return;
    const missingSlot = store.get(requestedConversationId);
    if (!missingSlot?.id) return;
    adoptHostDraft(requestedConversationId);
    const reconciledSlot = store.get(requestedConversationId);
    if (reconciledSlot && (isDirty(reconciledSlot) || reconciledSlot.held || reconciledSlot.error)) return;
    if (!composerConversation && selectedRef.current === requestedConversationId) {
      choose(state.activeConversationId ?? "");
      setNewComposeKey((current) => current === requestedConversationId ? null : current);
    }
  };

  useEffect(() => {
    if (!draftNotice || reviewNoticeSequenceRef.current !== draftNotice.sequence) return;
    reviewNoticeSequenceRef.current = undefined;
    store.acknowledgeReview(draftNotice.key);
  }, [draftNotice, store]);

  useLayoutEffect(() => {
    if (!noticeActionOwnedFocusRef.current || document.getElementById("draft-notice-action")) return;
    noticeActionOwnedFocusRef.current = false;
    if (document.activeElement === document.body) document.getElementById("composer-textarea")?.focus();
  }, [draftNotice]);

  useEffect(() => {
    if (!draftNotice || draftNotice.persistent) return;
    const durations: Partial<Record<DraftNoticeState["event"], number>> = {
      saved: 2000,
      "remote-update": 4000,
      moved: 5000,
      replaced: 6000,
    };
    const durationMs = durations[draftNotice.event];
    if (!durationMs) return;
    let retryTimer: number | undefined;
    const dismiss = () => {
      const action = document.activeElement?.id === "draft-notice-action";
      if (action) {
        retryTimer = window.setTimeout(dismiss, 250);
        return;
      }
      setDraftNotice((current) => current?.sequence === draftNotice.sequence ? undefined : current);
      const stored = draftNoticesRef.current.get(draftNotice.key);
      if (stored?.sequence === draftNotice.sequence) draftNoticesRef.current.delete(draftNotice.key);
    };
    const timer = window.setTimeout(dismiss, durationMs);
    return () => {
      window.clearTimeout(timer);
      if (retryTimer) window.clearTimeout(retryTimer);
    };
  }, [draftNotice]);

  const captureHistoryAnchor = (fallbackMessageId?: string) => {
    const list = messageListRef.current;
    const listBounds = list?.getBoundingClientRect();
    const unobscuredBottom = (listBounds?.bottom ?? 0) - (overlayHeightRef.current ?? 0);
    const visibleRow = listBounds
      ? [...messageRows.current.values()]
          .map((row) => ({ row, bounds: row.getBoundingClientRect() }))
          .filter(({ row, bounds }) => !row.hasAttribute("data-reaction-marker") && !row.closest("[data-reaction-notice]") && bounds.bottom > listBounds.top && bounds.top < unobscuredBottom)
          .sort((left, right) => left.bounds.top - right.bounds.top)[0]?.row
      : undefined;
    const row = visibleRow ?? (fallbackMessageId ? messageRows.current.get(fallbackMessageId) : undefined);
    return row ? { row, top: row.getBoundingClientRect().top } : null;
  };
  const correctHistoryAnchorScroll = (anchor: { row: HTMLElement; top: number } | null) => {
    const list = messageListRef.current;
    if (!list || !anchor) return;
    const messageId = anchor.row.dataset.messageId ?? anchor.row.dataset.messageRow;
    const row = (messageId && messageRows.current.get(messageId)) || anchor.row;
    list.setAttribute("data-scroll-programmatic", "");
    list.scrollTop += row.getBoundingClientRect().top - anchor.top;
    messageListScrollTopRef.current = list.scrollTop;
    if (typeof requestAnimationFrame === "undefined") list.removeAttribute("data-scroll-programmatic");
    else requestAnimationFrame(() => list.removeAttribute("data-scroll-programmatic"));
  };

  const persistLayout = persistLayoutPatch;
  const changeNavigationPosition = (position: NavigationPosition) => {
    setNavigationPosition(position);
    persistLayout({ navigationPosition: position });
  };
  const changePopoutMode = (mode: PopoutMode) => {
    setPopoutMode(mode);
    persistLayout({ popoutMode: mode });
  };
  const changeCompactTimestamps = (enabled: boolean) => {
    setCompactTimestamps(enabled);
    persistLayout({ compactTimestamps: enabled });
  };
  const changeTypingShare = (enabled: boolean) => {
    const request = ++typingShareRequest.current;
    setShareTyping(enabled);
    setTypingShareError(false);
    if (!persistLayout({ shareTyping: enabled })) {
      setShareTyping(confirmedTypingShare.current);
      setTypingShareError(true);
      return;
    }
    void bridge.set_typing_sharing?.({ enabled }).then(() => {
      confirmedTypingShare.current = enabled;
    }).catch(() => {
      if (request !== typingShareRequest.current) return;
      persistLayout({ shareTyping: confirmedTypingShare.current });
      setShareTyping(confirmedTypingShare.current);
      setTypingShareError(true);
    });
    if (!enabled) typingSender.noteSend();
  };
  const changeAttachmentLayout = (layout: AttachmentLayout) => {
    setAttachmentLayout(layout);
    persistLayout({ composerAttachmentLayout: layout });
  };
  const changeGroupSenderAvatars = (enabled: boolean) => {
    setGroupSenderAvatars(enabled);
    persistLayout({ groupSenderAvatars: enabled });
  };
  const railWidth = navigationPosition === "side-rail" ? RAIL_WIDTH : 0;
  const listMaxForWindow = Math.max(
    LIST_MIN_WIDTH,
    Math.min(
      LIST_MAX_WIDTH,
      viewportWidth - railWidth - 1 - PANE_MIN_WIDTH,
    ),
  );
  const renderedListWidth = Math.min(listWidth, listMaxForWindow);
  const setListSize = (value: number) => {
    if (value < LIST_COLLAPSE_THRESHOLD) {
      setListCollapsed(true);
      persistLayout({ listCollapsed: true });
      return;
    }
    const width = Math.max(LIST_MIN_WIDTH, Math.min(listMaxForWindow, value));
    listWidthLive.current = width;
    previousListWidth.current = width;
    setListWidth(width);
    setListCollapsed(false);
    persistLayout({ listWidth: width, listCollapsed: false });
  };
  const resizeList = (delta: number) => {
    listDragWidth.current += delta;
    setListSize(listDragWidth.current);
  };
  const resizeListTo = (value: number) => {
    listDragWidth.current = value;
    setListSize(value);
  };
  const commitList = () =>
    persistLayout({
      listCollapsed: listDragWidth.current < LIST_COLLAPSE_THRESHOLD,
      listWidth: listWidthLive.current,
    });
  const composerMinimum = () => {
    const constant = document.getElementById("attachment-tray")
      ? COMPOSER_ATTACHMENT_MIN_HEIGHT
      : COMPOSER_MIN_HEIGHT;
    return composerChrome > 0
      ? Math.max(constant, composerChrome + TEXTAREA_LINE_HEIGHT)
      : constant;
  };
  const composerMax = () =>
    Math.max(
      composerMinimum(),
      (paneRef.current?.offsetHeight ?? window.innerHeight) * 0.5,
    );
  const resizeComposer = (delta: number) => {
    const current = composerHeightLive.current ?? composerMinimum();
    const height = Math.max(
      composerMinimum(),
      Math.min(composerMax(), current - delta),
    );
    composerHeightLive.current = height;
    setComposerHeight(height);
  };
  const resizeComposerTo = (value: number) => {
    const height = Math.max(composerMinimum(), Math.min(composerMax(), value));
    composerHeightLive.current = height;
    setComposerHeight(height);
  };
  const commitComposer = () => {
    if (headPanel) {
      persistedHeadComposerHeight.current = composerHeightLive.current;
      persistLayout({ headComposerHeight: composerHeightLive.current });
    } else {
      persistedComposerHeight.current = composerHeightLive.current;
      persistLayout({ composerHeight: composerHeightLive.current });
    }
  };

  useEffect(() => {
    try {
      const saved = JSON.parse(
        localStorage.getItem("peppy.layout.v1") ?? "{}",
      ) as {
        listWidth?: number;
        listCollapsed?: boolean;
        composerHeight?: number | null;
        headComposerHeight?: number | null;
        recipientPosition?: unknown;
      };
      if (typeof saved.listWidth === "number") {
        const width = Math.max(
          LIST_MIN_WIDTH,
          Math.min(LIST_MAX_WIDTH, saved.listWidth),
        );
        listWidthLive.current = width;
        listDragWidth.current = width;
        previousListWidth.current = width;
        setListWidth(width);
      }
      if (typeof saved.listCollapsed === "boolean") setListCollapsed(saved.listCollapsed);
      if (typeof saved.composerHeight === "number") {
        persistedComposerHeight.current = saved.composerHeight;
        resizeComposerTo(saved.composerHeight);
      }
      if (typeof saved.headComposerHeight === "number")
        persistedHeadComposerHeight.current = saved.headComposerHeight;
      setRecipientPosition(
        isRecipientPosition(saved.recipientPosition) ? saved.recipientPosition : null,
      );
    } catch { /* Corrupt persisted layout falls back to defaults. */ }
    const updateWindowMeasurements = () => {
      setViewportWidth(window.innerWidth);
      const height = paneRef.current?.offsetHeight ?? 0;
      setPaneHeight((current) => (current === height ? current : height));
      if (composerHeightLive.current !== null) resizeComposerTo(composerHeightLive.current);
    };
    updateWindowMeasurements();
    window.addEventListener("resize", updateWindowMeasurements);
    return () => window.removeEventListener("resize", updateWindowMeasurements);
  }, []);
  useEffect(() => {
    if (headPanel && persistedHeadComposerHeight.current !== null)
      resizeComposerTo(persistedHeadComposerHeight.current);
  }, [headPanel]);
  useEffect(() => {
    if (composerConversation !== null) return;
    void bridge.set_notification_context(activeView, activeView === "conversations" ? selectedRef.current : undefined).catch(() => {});
  }, [activeView, selected, composerConversation]);
  useEffect(() => installOverlayScrollbars(document), []);
  useLayoutEffect(() => {
    const measureOverlay = () => {
      const height = document.getElementById("composer-area")?.offsetHeight ?? 0;
      setOverlayHeight((current) => (current === height ? current : height));
    };
    const measure = () => {
      const height = paneRef.current?.offsetHeight ?? 0;
      setPaneHeight((current) => (current === height ? current : height));
      const composerArea = document.getElementById("composer-area");
      const textarea = document.getElementById("composer-textarea");
      if (!(composerArea instanceof HTMLElement) || !(textarea instanceof HTMLElement)) return;
      const chrome = measureComposerChrome(composerArea, textarea);
      setComposerChrome((current) => (current === chrome ? current : chrome));
    };
    measure();
    measureOverlay();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      measure();
      measureOverlay();
    });
    if (paneRef.current) observer.observe(paneRef.current);
    const composerArea = document.getElementById("composer-area");
    if (composerArea) observer.observe(composerArea);
    document.querySelectorAll("#composer-field, #attachment-tray, #composer-toolbar").forEach((element) => observer.observe(element));
    return () => observer.disconnect();
  }, [activeView, headPanel, loading, Boolean(snapshot), snapshot?.connection.state, Boolean(selected), draftsVersion]);
  useEffect(() => {
    const root = mainRef.current;
    if (!root) return;
    const focus = () => { root.dataset.windowFocused = "true"; };
    const blur = () => { root.dataset.windowFocused = "false"; };
    root.dataset.windowFocused = document.hasFocus() ? "true" : "false";
    window.addEventListener("focus", focus);
    window.addEventListener("blur", blur);
    return () => {
      window.removeEventListener("focus", focus);
      window.removeEventListener("blur", blur);
    };
  }, []);

  const choose = (key: string) => {
    if (key) setFolderView(snapshotRef.current?.conversations.find((conversation) => conversation.id === key)?.folder ?? "inbox");
    if (selectedRef.current !== key) invalidateReactionNavigation();
    if (detachedHistoryRef.current && detachedHistoryRef.current.conversationId !== key) clearDetached();
    loadSequence.current += 1;
    historySequence.current += 1;
    historyLoadingRef.current = false;
    setHistoryLoading(false);
    setHistoryLoadError(null);
    setHasOlderMessages(false);
    setHistoryExhausted(false);
    hasOlderMessagesRef.current = false;
    messageListScrollTopRef.current = 0;
    historyAnchorRef.current = null;
    selectedRef.current = key;
    setSelected(key);
    if (narrowBrowserViewport) setMobileConversationListOpen(false);
  };
  const setNotice = (message: ReactNode, severity: ActionNotice["severity"] = "error", persistent = false) =>
    setActionNotice(message ? { message, severity, persistent } : undefined);
  const setComposerNotice = (key: string, message: string, severity: ComposerFeedback["severity"] = "error", kind?: ComposerFeedback["kind"]) =>
    setComposerFeedback(message ? { key, message, severity, kind } : undefined);
  const reportRefresh = (prefix: string) => (error: unknown) =>
    setRefreshNotice(`${prefix}${errorText(error)}`);
  const reportAction = (prefix: string) => (error: unknown) => {
    const paused = errorCode(error) === "update-in-progress";
    setNotice(`${prefix}${errorText(error)}`, paused ? "warning" : "error");
    if (paused) void refresh().catch(reportRefresh(""));
  };
  const prepareFocusRestore = (owner: HTMLElement | null = document.activeElement instanceof HTMLElement ? document.activeElement : null) => {
    focusRestorePendingRef.current = Boolean(owner?.closest("#app-notice-banner"));
  };
  const restoreFocusIfPending = () => {
    if (!focusRestorePendingRef.current) return;
    focusRestorePendingRef.current = false;
    const focus = () => {
      if (document.activeElement !== document.body) return;
      (document.getElementById("composer-textarea") ?? document.getElementById("new-conversation-cta"))?.focus();
    };
    if (typeof requestAnimationFrame === "undefined") focus();
    else requestAnimationFrame(focus);
  };
  const retryPendingSaves = async () => {
    if (retryPendingSavesInFlightRef.current) {
      retryPendingSavesQueuedRef.current = true;
      return;
    }
    retryPendingSavesInFlightRef.current = true;
    try {
      do {
        retryPendingSavesQueuedRef.current = false;
        const attempted = new Set<string>();
        while (true) {
          if (!mountedRef.current) return;
          const attachmentId = [...pendingSaveIdsRef.current].find((id) => !attempted.has(id));
          if (!attachmentId) break;
          attempted.add(attachmentId);
          try {
            await bridge.save_attachment(attachmentId);
            if (!mountedRef.current) return;
            pendingSaveIdsRef.current.delete(attachmentId);
          } catch (error) {
            if (errorCode(error) !== "media-pending") pendingSaveIdsRef.current.delete(attachmentId);
          }
        }
      } while (retryPendingSavesQueuedRef.current && mountedRef.current);
    } finally {
      retryPendingSavesInFlightRef.current = false;
    }
  };

  /** Loads host state for one conversation; responses superseded by a newer request are dropped. */
  const performRefresh = async (conversationId = selectedRef.current) => {
    const sequence = ++loadSequence.current;
    const sessionKey = currentSearchSessionKey();
    const requestedConversationId = conversationId && !isLocalDraftKey(conversationId)
      ? conversationId
      : undefined;
    let detached = detachedHistoryRef.current?.conversationId === conversationId
      ? detachedHistoryRef.current
      : undefined;
    const latestRequired = latestRequiredConversationRef.current === conversationId;
    const loadedConversation = snapshotRef.current?.activeConversationId === conversationId
      ? snapshotRef.current.conversations.find((item) => item.id === conversationId)
      : undefined;
    const historyFromMessageId = detached || latestRequired ? undefined : loadedConversation?.messages[0]?.id;
    const state = detached
      ? await bridge.load_state(requestedConversationId, undefined, { metadataOnly: true })
      : historyFromMessageId
      ? await bridge.load_state(requestedConversationId, historyFromMessageId)
      : await bridge.load_state(requestedConversationId);
    const redirect = state.requestedRedirect?.from === conversationId ? state.requestedRedirect : undefined;
    const survivorId = redirect?.to ?? conversationId;
    // A redirected snapshot is written with the redirect itself, so the view never shows a half-applied merge.
    if (
      mountedRef.current &&
      sessionKey === currentSearchSessionKey() &&
      state.encryption.state !== "unlocked" &&
      !redirect
    )
      setSnapshot(state);
    if (
      !mountedRef.current ||
      sequence !== loadSequence.current ||
      sessionKey !== currentSearchSessionKey() ||
      (detached && detachedHistoryRef.current !== detached)
    ) return;
    resetDraftAttachmentCache(state.sessionScopeId);
    // Applies the merge's selection and draft move; it must share a flush with the snapshot write.
    const followRedirect = () => {
      if (redirect) followRequestedRedirect(redirect);
    };
    const detachedIsCurrent = () => Boolean(detached && detachedHistoryRef.current?.generation === detached.generation);

    if (detached && (detached.pendingLoad || !detached.messageIds.length)) {
      const pendingDetached = detached;
      flushSync(() => {
        followRedirect();
        setSnapshot((current) => {
          if (!current || current.activeConversationId !== conversationId) return state;
          const currentConversation = current.conversations.find((item) => item.id === conversationId);
          return {
            ...state,
            activeConversationId: survivorId,
            conversations: state.conversations.map((item) => item.id === survivorId && currentConversation
              ? { ...item, messages: currentConversation.messages, hasOlderMessages: pendingDetached.hasOlder }
              : item),
          };
        });
        if (redirect && detachedIsCurrent()) setDetached({ ...pendingDetached, conversationId: survivorId });
      });
      if (state.activeConversationId) {
        if (state.draft && store.isHydrated(state.activeConversationId)) applyAuthoritativeDraft(state.draft);
        else adoptHostDraft(state.activeConversationId, state.draft);
      }
      reconcileMissingRequestedConversation(requestedConversationId, state);
      setRefreshNotice("");
      return;
    }

    const refreshedWindow = detached
      ? await bridge.load_message_window({ direction: "refresh", conversationId: survivorId, messageIds: detached.messageIds.slice(0, DETACHED_HISTORY_LIMIT) })
      : undefined;
    if (
      !mountedRef.current ||
      sequence !== loadSequence.current ||
      sessionKey !== currentSearchSessionKey() ||
      (detached && detachedHistoryRef.current !== detached)
    ) return;

    const refreshed = state.conversations.find((item) => item.id === state.activeConversationId);
    // Older messages are merged only when this response is for the requested conversation or its survivor.
    const mergesHistory = Boolean(redirect || state.activeConversationId === conversationId);
    let refreshedDetached: DetachedHistory | undefined;
    const shouldPreserveScrollPosition = Boolean(!nearBottom.current && !historyLoadingRef.current);
    if (shouldPreserveScrollPosition) historyAnchorRef.current = captureHistoryAnchor(historyFromMessageId);
    const updateSnapshot = () => setSnapshot((current) => {
      if (detached && refreshedWindow && current?.activeConversationId === conversationId) {
        const currentMessages = current.conversations.find((item) => item.id === conversationId)?.messages ?? [];
        const detachedMessages = mergeDetachedMessages(currentMessages, refreshedWindow.messages, "refresh").messages;
        refreshedDetached = {
          ...detached,
          conversationId: survivorId,
          messageIds: detachedMessages.map((message) => message.id),
          hasOlder: refreshedWindow.hasOlder,
          hasNewer: refreshedWindow.hasNewer,
        };
        return {
          ...state,
          activeConversationId: survivorId,
          conversations: state.conversations.map((item) => item.id === survivorId
            ? { ...item, messages: detachedMessages, hasOlderMessages: refreshedWindow.hasOlder }
            : item),
        };
      }
      const previous = historyFromMessageId && mergesHistory && current && current.activeConversationId === conversationId
        ? current.conversations.find((item) => item.id === conversationId)
        : undefined;
      if (!previous || !refreshed) return state;
      return {
        ...state,
        conversations: state.conversations.map((item) => item.id === refreshed.id
          ? { ...item, messages: mergeAnchoredMessages(previous.messages, refreshed.messages) }
          : item),
      };
    });
    const commitSnapshot = () => {
      followRedirect();
      if (redirect && detachedIsCurrent()) setDetached({ ...detached!, conversationId: survivorId });
      updateSnapshot();
    };
    if (shouldPreserveScrollPosition) {
      flushSync(commitSnapshot);
      correctHistoryAnchorScroll(historyAnchorRef.current);
    } else {
      flushSync(commitSnapshot);
    }
    if (refreshedDetached && detachedIsCurrent()) setDetached(refreshedDetached);
    if (latestRequiredConversationRef.current === survivorId && !detached) {
      latestRequiredConversationRef.current = undefined;
    }
    if (state.activeConversationId) {
      if (state.draft && store.isHydrated(state.activeConversationId)) applyAuthoritativeDraft(state.draft);
      else adoptHostDraft(state.activeConversationId, state.draft);
    }
    reconcileMissingRequestedConversation(requestedConversationId, state);
    setRefreshNotice("");
    if (!composerConversation && !selectedRef.current && state.activeConversationId) choose(state.activeConversationId);
    const responseHasOlder = refreshedDetached?.hasOlder ?? refreshed?.hasOlderMessages ?? false;
    const currentAnchor = snapshotRef.current?.activeConversationId === survivorId
      ? snapshotRef.current.conversations.find((item) => item.id === survivorId)?.messages[0]?.id
      : undefined;
    if (currentAnchor === historyFromMessageId || !currentAnchor || detached) {
      hasOlderMessagesRef.current = responseHasOlder;
      setHasOlderMessages(responseHasOlder);
      if (responseHasOlder) setHistoryExhausted(false);
    }
    bindDraftSubscriptionAfterRefreshRef.current?.();
  };
  const startRefresh = (request: RefreshRequest) => {
    refreshInFlightRef.current = true;
    setRefreshing(true);
    const conversationId = request.explicit ? request.conversationId : selectedRef.current;
    const run = performRefresh(conversationId);
    run.then(
      () => {
        if (!reenrolEnrollmentRef.current) setReenrolRequired(false);
        request.waiters.forEach(({ resolve }) => resolve());
      },
      (error) => {
        if (errorCode(error) === "database-key-unavailable") setReenrolRequired(true);
        request.waiters.forEach(({ reject }) => reject(error));
      },
    ).finally(() => {
      refreshInFlightRef.current = false;
      const next = pendingRefreshRef.current;
      pendingRefreshRef.current = undefined;
      if (mountedRef.current && next) {
        startRefresh(next);
        return;
      }
      if (mountedRef.current) setRefreshing(false);
    });
  };
  const refresh = (conversationId?: string) => new Promise<void>((resolve, reject) => {
    const request = { conversationId, explicit: conversationId !== undefined, waiters: [{ resolve, reject }] };
    if (!refreshInFlightRef.current) {
      startRefresh(request);
      return;
    }
    const pending = pendingRefreshRef.current;
    if (!pending) {
      pendingRefreshRef.current = request;
      return;
    }
    pending.waiters.push({ resolve, reject });
    if (request.explicit) {
      pending.conversationId = request.conversationId;
      pending.explicit = true;
    }
  });

  const loadLatest = async (conversationId: string, unavailableNotice = "") => {
    const generation = ++navigationGeneration.current;
    const sessionKey = currentSearchSessionKey();
    const state = await bridge.load_state(conversationId);
    if (!mountedRef.current || generation !== navigationGeneration.current || sessionKey !== currentSearchSessionKey() || selectedRef.current !== conversationId) return;
    clearDetached();
    const redirect = state.requestedRedirect?.from === conversationId ? state.requestedRedirect : undefined;
    const survivorId = redirect?.to ?? conversationId;
    if (redirect) followRequestedRedirect(redirect);
    if (state.activeConversationId !== survivorId) {
      latestRequiredConversationRef.current = survivorId;
      setSnapshot({ ...state, activeConversationId: undefined });
      choose("");
      setNotice("The selected conversation is unavailable. Retry from search.", "warning");
      return;
    }
    latestRequiredConversationRef.current = undefined;
    nearBottom.current = true;
    setSnapshot(state);
    adoptHostDraft(survivorId, state.draft);
    setNewComposeKey(null);
    setNotice(unavailableNotice);
    const conversation = state.conversations.find(item => item.id === survivorId);
    const hasOlder = conversation?.hasOlderMessages ?? false;
    hasOlderMessagesRef.current = hasOlder;
    setHasOlderMessages(hasOlder);
    setHistoryExhausted(!hasOlder);
  };

  const select = (key: string) => {
    notificationNavigationEpoch.current += 1;
    const previous = selectedRef.current;
    if (key === previous) {
      if (detachedHistoryRef.current?.conversationId === key) void loadLatest(key).catch(reportAction("Could not load the latest messages: "));
      return;
    }
    if (previous) void store.flush(previous);
    choose(key);
    if (newComposeKey && key !== newComposeKey && !isLocalDraftKey(key)) setNewComposeKey(null);
    setNotice("");
    if (!isLocalDraftKey(key)) void refresh(key).catch(reportRefresh(""));
  };

  const flushBeforeNavigation = async (failurePrefix: string) => {
    const activationGeneration = ++navigationGeneration.current;
    const activationSessionKey = currentSearchSessionKey();
    const isCurrent = () => activationGeneration === navigationGeneration.current && activationSessionKey === currentSearchSessionKey();
    const previous = selectedRef.current;
    if (previous) {
      const flushed = await store.flush(previous);
      if (!flushed.ok) {
        setNotice(`${failurePrefix}: ${flushed.error}`);
        return false;
      }
    }
    if (!isCurrent()) return false;
    return { activationGeneration, activationSessionKey, isCurrent };
  };
  const openContactTarget = async (target: ContactTarget & { requestId: number }, failurePrefix = "Search result not opened") => {
    const navigation = await flushBeforeNavigation(failurePrefix);
    if (!navigation || !navigation.isCurrent()) return false;
    resetContactSearch();
    resetConversationSearch();
    setActiveViewNow("contacts");
    setContactTarget(target);
    return true;
  };
  const activateApprovedSearchHit = async (hit: LocalSearchHit) => {
    if (hit.kind === "contact") {
      const requestId = ++contactTargetRequestId.current;
      await openContactTarget({ bookId: hit.bookId, contactId: hit.contactId, requestId });
      return;
    }
    const navigation = await flushBeforeNavigation("Search result not opened");
    if (!navigation) return;
    const { activationGeneration, activationSessionKey } = navigation;
    if (hit.kind === "conversation") {
      setContactTarget(undefined);
      setActiveViewNow("conversations");
      if (hit.conversationId === selectedRef.current && detachedHistoryRef.current) await loadLatest(hit.conversationId);
      else select(hit.conversationId);
      return;
    }

    const generation = activationGeneration;
    const sessionKey = activationSessionKey;
    loadSequence.current += 1;
    setContactTarget(undefined);
    const pending: DetachedHistory = { conversationId: hit.conversationId, messageIds: [], hasOlder: false, hasNewer: false, pendingTarget: hit.messageId, pendingLoad: "around", generation, sessionKey };
    setDetached(pending);
    setSearchTargetId(undefined);
    searchReadGate.current = true;
    nearBottom.current = false;
    choose(hit.conversationId);
    setDetached(pending);
    setActiveViewNow("conversations");
    setNotice("");
    try {
      const [metadata, window] = await Promise.all([
        bridge.load_state(hit.conversationId, undefined, { metadataOnly: true }),
        bridge.load_message_window({ direction: "around", conversationId: hit.conversationId, messageId: hit.messageId }),
      ]);
      if (!mountedRef.current || generation !== navigationGeneration.current || sessionKey !== currentSearchSessionKey() || selectedRef.current !== hit.conversationId || window.conversationId !== hit.conversationId) return;
      const messages = boundedAroundMessages(window.messages, hit.messageId);
      if (!messages.some(message => message.id === hit.messageId)) throw new Error("search-target-missing");
      const next: DetachedHistory = { ...pending, messageIds: messages.map(message => message.id), hasOlder: window.hasOlder, hasNewer: window.hasNewer, pendingLoad: undefined };
      loadSequence.current += 1;
      setDetached(next);
      flushSync(() => {
        setSnapshot({ ...metadata, activeConversationId: hit.conversationId, conversations: metadata.conversations.map(conversation => conversation.id === hit.conversationId ? { ...conversation, messages, hasOlderMessages: window.hasOlder } : conversation) });
        setSearchTargetId(hit.messageId);
      });
      adoptHostDraft(hit.conversationId, metadata.draft);
      setNewComposeKey(null);
      hasOlderMessagesRef.current = window.hasOlder;
      setHasOlderMessages(window.hasOlder);
      setHistoryExhausted(!window.hasOlder);
    } catch {
      if (!mountedRef.current || generation !== navigationGeneration.current || sessionKey !== currentSearchSessionKey() || selectedRef.current !== hit.conversationId) return;
      setDetached(undefined);
      setSearchTargetId(undefined);
      searchReadGate.current = false;
      try {
        await loadLatest(hit.conversationId, "The selected message is no longer available. Showing the latest messages.");
      } catch {
        if (sessionKey === currentSearchSessionKey() && selectedRef.current === hit.conversationId) setNotice("The selected message is unavailable. Retry from search.", "warning");
      }
    }
  };

  activateSearchHitHandlerRef.current = (hit: LocalSearchHit) => {
    notificationNavigationEpoch.current += 1;
    const navigate = () => { void activateApprovedSearchHit(hit); };
    if (contactNavigation.current) contactNavigation.current(navigate);
    else navigate();
  };
  const activateSearchHit = useCallback((hit: LocalSearchHit) => {
    activateSearchHitHandlerRef.current(hit);
  }, []);

  handlers.current.rekeyed = (from, to) => {
    if (selectedRef.current !== from) return;
    choose(to);
    void refresh(to).catch(reportRefresh(""));
  };

  useEffect(() => {
    mountedRef.current = true;
    const unsubscribe = bridge.subscribe(() => {
      setMediaStateHint(current => current + 1);
      void retryPendingSaves();
      void refresh().then(() => notificationDrain.current?.retry()).catch(reportRefresh(""));
    });
    let unsubscribeDrafts: () => void = () => undefined;
    let subscriptionAttempt = 0;
      const installDraftSubscription = () => {
      unsubscribeDrafts();
      const attempt = ++subscriptionAttempt;
        let boundSession: string | undefined;
        const bufferedDrafts: Draft[] = [];
        const bindAfterVerifiedRefresh = () => {
          if (!mountedRef.current || attempt !== subscriptionAttempt || boundSession) return;
          boundSession = currentSearchSessionKey();
          draftSubscriptionSessionRef.current = boundSession;
          setDraftSubscriptionError(undefined);
          bufferedDrafts.splice(0).forEach(applyDraft);
          bindDraftSubscriptionAfterRefreshRef.current = undefined;
        };
      const applyDraft = (draft: Draft) => {
        if (
          mountedRef.current &&
          attempt === subscriptionAttempt &&
          boundSession === currentSearchSessionKey() &&
          snapshotRef.current?.encryption.state === "unlocked" &&
          !appLockRef.current?.locked
        ) applyAuthoritativeDraft(draft);
      };
      const subscription = bridge.subscribe_drafts?.((draft) => {
        if (!boundSession) bufferedDrafts.push(draft);
        else applyDraft(draft);
      }) ?? Object.assign(() => undefined, { ready: Promise.resolve() });
      unsubscribeDrafts = subscription;
      void (async () => {
        try {
          await (subscription.ready ?? Promise.resolve());
        } catch (error) {
          if (mountedRef.current && attempt === subscriptionAttempt) {
            setDraftSubscriptionError(errorText(error));
            draftNoticeSequence.current += 1;
            setDraftNotice({
              key: selectedRef.current,
              event: "send-failed",
              detail: `Draft updates unavailable: ${errorText(error)}`,
              sequence: draftNoticeSequence.current,
              persistent: true,
            });
          }
          if (mountedRef.current && attempt === subscriptionAttempt) setLoading(false);
          return;
        }
          if (!mountedRef.current || attempt !== subscriptionAttempt) return;
          try {
            bindDraftSubscriptionAfterRefreshRef.current = bindAfterVerifiedRefresh;
            // A composer window follows its conversation through a merge; its selection is the survivor after one.
            await refresh(composerConversation ? selectedRef.current || composerConversation : undefined);
            if (!mountedRef.current || attempt !== subscriptionAttempt) return;
        } catch (error) {
          if (mountedRef.current && attempt === subscriptionAttempt) reportRefresh("")(error);
        } finally {
          if (mountedRef.current && attempt === subscriptionAttempt) setLoading(false);
        }
      })();
    };
      const suspendDraftSubscription = () => {
      subscriptionAttempt += 1;
      unsubscribeDrafts();
      unsubscribeDrafts = () => undefined;
        draftSubscriptionSessionRef.current = undefined;
        bindDraftSubscriptionAfterRefreshRef.current = undefined;
      if (snapshotRef.current) setLoading(false);
    };
    restartDraftSubscriptionRef.current = installDraftSubscription;
    suspendDraftSubscriptionRef.current = suspendDraftSubscription;
    installDraftSubscription();
    return () => {
      restartDraftSubscriptionRef.current = undefined;
      suspendDraftSubscriptionRef.current = undefined;
      draftSubscriptionSessionRef.current = undefined;
      bindDraftSubscriptionAfterRefreshRef.current = undefined;
      mountedRef.current = false;
      const pending = pendingRefreshRef.current;
      pendingRefreshRef.current = undefined;
      pending?.waiters.forEach(({ reject }) => reject(new Error("app-unmounted")));
      loadSequence.current += 1;
      historySequence.current += 1;
      navigationGeneration.current += 1;
      detachedHistoryRef.current = undefined;
      searchReadGate.current = false;
      historyAnchorRef.current = null;
      unsubscribe();
      unsubscribeDrafts();
        draftStoreGeneration.current += 1;
       draftSessionGenerationRef.current += 1;
       resolutionRef.current = undefined;
       setResolvingDraftConflict(false);
       pastedAttachmentInFlight.current.clear();
       attachmentSessionRef.current = undefined;
       setAttachmentViews({});
       setDraftViews({});
       store.reset();
       setCancelledSendRestore(undefined);
       draftNoticesRef.current.clear();
    };
  }, []);
  useEffect(() => {
    if (snapshot?.encryption.state !== "locked" && !appLock?.locked) return;
    draftStoreGeneration.current += 1;
    draftSessionGenerationRef.current += 1;
    resolutionRef.current = undefined;
    setResolvingDraftConflict(false);
    pastedAttachmentInFlight.current.clear();
    attachmentSessionRef.current = undefined;
    setAttachmentViews({});
    setDraftViews({});
    store.reset();
    setCancelledSendRestore(undefined);
    saveFailedKeysRef.current.clear();
    draftNoticesRef.current.clear();
    setDraftNotice(undefined);
    setDraftSubscriptionError(undefined);
    historySequence.current += 1;
    historyLoadingRef.current = false;
    historyAnchorRef.current = null;
    hasOlderMessagesRef.current = false;
    setHistoryLoading(false);
    setHistoryLoadError(null);
    setHasOlderMessages(false);
    setHistoryExhausted(false);
    clearDetached();
    setContactTarget(undefined);
  }, [appLock?.locked, snapshot?.encryption.state]);
  const renderedSearchSessionKey = currentSearchSessionKey();
  const notificationRecoveryBridge = useMemo(() => bridge.notification_reply_recoveries && bridge.claim_notification_reply_restore && bridge.release_notification_reply_restore && bridge.ack_notification_reply_restore && bridge.discard_notification_reply
    ? { load: bridge.notification_reply_recoveries, claim: bridge.claim_notification_reply_restore, release: bridge.release_notification_reply_restore, acknowledge: bridge.ack_notification_reply_restore, discard: bridge.discard_notification_reply }
    : undefined, []);
  useEffect(() => {
    if (!snapshot) return;
    const unlocked = snapshot.encryption.state === "unlocked" && !appLock?.locked;
    const previous = draftSubscriptionStateRef.current;
    draftSubscriptionStateRef.current = { session: renderedSearchSessionKey, unlocked };
    if (!unlocked) {
      suspendDraftSubscriptionRef.current?.();
      return;
    }
    const installedSession = draftSubscriptionSessionRef.current;
    const resumedFromLock = previous && !previous.unlocked;
    const replacedUnlockedSession = previous?.unlocked && installedSession && installedSession !== renderedSearchSessionKey;
    if (resumedFromLock || replacedUnlockedSession) restartDraftSubscriptionRef.current?.();
  }, [renderedSearchSessionKey, snapshot?.encryption.state, appLock?.locked]);
  notificationHandler.current = async (request, context) => {
    const session = currentSearchSessionKey();
    if (notificationNavigation.current?.requestId !== request.requestId || notificationNavigation.current.sessionGeneration !== request.sessionGeneration) {
      notificationNavigation.current = { requestId: request.requestId, sessionGeneration: request.sessionGeneration, generation: navigationGeneration.current, epoch: notificationNavigationEpoch.current };
    }
    const generation = notificationNavigation.current.generation;
    const epoch = notificationNavigation.current.epoch;
    let approvedGuard = contactNavigation.current;
    let deferredByGuard = false;
    const valid = async () => {
      if (!await context.validate() || session !== currentSearchSessionKey()) return false;
      if (generation === navigationGeneration.current && epoch === notificationNavigationEpoch.current && approvedGuard === contactNavigation.current) return true;
      await context.complete();
      return false;
    };
    if (request.destination.kind === "main") { await context.complete(); return; }
    const navigate = async () => {
      if (!await valid()) return;
      const previous = selectedRef.current;
      if (previous) {
        const flushed = await store.flush(previous);
        if (!await valid()) return;
        if (!flushed.ok) throw new Error(flushed.error);
      }
      const destination = request.destination;
      if (destination.kind === "mirrored") {
        await refresh();
        if (!await valid()) return;
        setContactTarget(undefined);
        setNotificationActionNotice(undefined);
        setNotice("");
        flushSync(() => {
          setActiveViewNow("notifications");
          setNotificationAppFilter(null);
          setNotificationQuery("");
          notificationFocusContext.current = { requestId: request.requestId, context };
          setNotificationFocus({ requestId: request.requestId, target: destination.target });
        });
        return;
      }
      const conversationId = destination.kind === "conversation" ? destination.conversationId : undefined;
      const state = await bridge.load_state(conversationId);
      if (!await valid() || state.encryption.state !== "unlocked" ||
          createSearchSessionKey(state, false) !== session) return;
      navigationGeneration.current += 1;
      loadSequence.current += 1;
      setContactTarget(undefined);
      setNotificationActionNotice(undefined);
      if (destination.kind !== "conversation") return;
      // A merged-away notification target opens its survivor, with the draft moved along.
      const redirect = state.requestedRedirect?.from === conversationId ? state.requestedRedirect : undefined;
      const targetId = redirect?.to ?? destination.conversationId;
      const found = state.conversations.find(item => item.id === targetId);
      clearDetached();
      if (redirect) store.followMerge(redirect.from, redirect.to);
      flushSync(() => {
        setSnapshot(state);
        setActiveViewNow("conversations");
        choose(found ? targetId : "");
        setNewComposeKey(null);
        setNotice(found ? "" : "This conversation is no longer available.", "warning");
        if (found) {
          adoptHostDraft(targetId, state.draft);
          const hasOlder = found.hasOlderMessages ?? false;
          hasOlderMessagesRef.current = hasOlder;
          setHasOlderMessages(hasOlder);
          setHistoryExhausted(!hasOlder);
        }
      });
      await context.complete();
    };
    const attempt = () => navigate().catch(context.fail);
    if (contactNavigation.current) {
      deferredByGuard = contactNavigation.current(() => {
        if (deferredByGuard) {
          // Commit Discard's dirty-state reset before capturing the approved guard.
          // Subsequent edits must still invalidate this approval.
          flushSync(() => {});
          approvedGuard = contactNavigation.current;
        }
        void attempt();
      }, () => { void context.complete(); }) === true;
    } else await attempt();
  };
  useEffect(() => {
    if (browserHost || composerConversation || headPanelBootstrap) return;
    const drain = createNotificationActivationDrain({
      transport: nativeNotificationActivation,
      ready: () => mountedRef.current && snapshotRef.current?.encryption.state === "unlocked" && !appLockRef.current?.locked,
      handle: (request, context) => notificationHandler.current(request, context),
      error: error => setNotice(`Could not open notification: ${errorText(error)}. Try again by clicking the notification or refreshing.`, "warning"),
      invalidate: () => {
        notificationFocusContext.current = undefined;
        setNotificationFocus(undefined);
      },
    });
    notificationDrain.current = drain;
    return () => { drain.dispose(); notificationDrain.current = undefined; };
  }, [browserHost, composerConversation, headPanelBootstrap]);
  useEffect(() => {
    notificationNavigation.current = undefined;
    notificationDrain.current?.reset();
    notificationMuteGenerationRef.current += 1;
    notificationMutePendingRef.current = false;
    setNotificationMutePending(false);
    setNotificationAppFilter(null);
    resetNotificationSearch();
    resetContactSearch();
    resetConversationSearch();
    setNotificationMuteNotice(undefined);
  }, [renderedSearchSessionKey, loading, appLock?.locked]);
  useEffect(() => {
    if (activeView !== "contacts") resetContactSearch();
    if (activeView !== "conversations") resetConversationSearch();
  }, [activeView]);
  useEffect(() => {
    if (activeView === "notifications") return;
    setNotificationActionNotice(undefined);
    notificationFocusContext.current = undefined;
    setNotificationFocus(undefined);
    setNotificationAppFilter(null);
    resetNotificationSearch();
  }, [activeView]);
  const notificationFocusResult = (requestId: string, found: boolean) => {
    const pending = notificationFocusContext.current;
    if (pending?.requestId !== requestId) {
      if (!found && requestId.startsWith("omni-")) setNotificationActionNotice({ kind: "info", message: "This notification is no longer available." });
      return;
    }
    if (pending.reported) return;
    pending.reported = true;
    void (async () => {
      if (!await pending.context.validate()) return;
      if (!found) setNotificationActionNotice({ kind: "info", message: "This notification is no longer available." });
      await pending.context.complete();
    })().catch(reportAction("Could not open notification: "));
  };
  const [historySearchVisible, setHistorySearchVisible] = useState(false);
  const historyControls = useHistorySearch({
    sessionKey: renderedSearchSessionKey,
    enabled: snapshot?.encryption.state === "unlocked" && !appLock?.locked,
    visible: historySearchVisible,
    bridge,
  });
  useEffect(() => {
    if (!previousSearchSessionKeyRef.current) {
      previousSearchSessionKeyRef.current = renderedSearchSessionKey;
      return;
    }
    const previousSessionKey = previousSearchSessionKeyRef.current;
    if (previousSessionKey === renderedSearchSessionKey) return;
    previousSearchSessionKeyRef.current = renderedSearchSessionKey;
    if (!previousSessionKey.includes(":unavailable:")) {
      draftStoreGeneration.current += 1;
      draftSessionGenerationRef.current += 1;
      resolutionRef.current = undefined;
      setResolvingDraftConflict(false);
      pastedAttachmentInFlight.current.clear();
      store.reset();
      setCancelledSendRestore(undefined);
      saveFailedKeysRef.current.clear();
      draftNoticesRef.current.clear();
      setDraftNotice(undefined);
      setDraftSubscriptionError(undefined);
    }
    invalidateReactionNavigation();
    const detachedConversationId = detachedHistoryRef.current?.conversationId;
    clearDetached();
    setContactTarget(undefined);
    if (detachedConversationId && snapshotRef.current?.encryption.state === "unlocked" && !appLock?.locked) {
      void refresh(detachedConversationId).catch(reportRefresh(""));
    }
  }, [renderedSearchSessionKey]);
  const getAppLockStatus = async (isCurrent: () => boolean = () => true): Promise<AppLockStatus> => {
    const request = ++appLockStatusRequest.current;
    try {
      const status = await bridge.get_app_lock_status();
      if (isCurrent() && request === appLockStatusRequest.current) setAppLock(status);
      return status;
    } catch (error) {
      if (isCurrent() && request === appLockStatusRequest.current) setAppLock(null);
      throw error;
    }
  };
  useEffect(() => {
    if (browserHost) return;
    let active = true;
    void getAppLockStatus(() => active).catch(() => {});
    return () => { active = false; };
  }, [browserHost]);
  const lockCode = snapshot?.connection.errorCode;
  const appLockSnapshotLoaded = snapshot !== null;
  useEffect(() => {
    const previous = previousLockCode.current;
    previousLockCode.current = lockCode;
    const leftChecking = previous === "app-lock-checking" && lockCode !== "app-lock-checking";
    const pendingWithoutCheckingSnapshot = appLock?.pending === true && appLockSnapshotLoaded && lockCode !== "app-lock-checking";
    if (browserHost || (!leftChecking && !pendingWithoutCheckingSnapshot)) return;
    let active = true;
    void getAppLockStatus(() => active).catch(() => {});
    return () => { active = false; };
  }, [appLock?.pending, appLockSnapshotLoaded, browserHost, lockCode]);
  const changeAppLock = async (enabled: boolean): Promise<boolean> => {
    const changed = await bridge.set_app_lock(enabled);
    await getAppLockStatus();
    return changed;
  };
  const unlockApp = async (): Promise<boolean> => {
    const unlocked = await bridge.unlock_app();
    if (unlocked) {
      await getAppLockStatus();
      await refresh();
    }
    return unlocked;
  };
  const retryAppLockCheck = async (): Promise<void> => {
    const request = ++appLockStatusRequest.current;
    const status = await bridge.retry_app_lock_check();
    if (request === appLockStatusRequest.current) setAppLock(status);
    await refresh();
  };

  /* Visible rows are marked seen when focused; scrolling may temporarily hide reaction hints. */
  const slotKey = store.resolve(selected);
  const previousSlotKeyRef = useRef(slotKey);
  const composerDraftKeyRef = useRef(slotKey);
  if (slotKey !== previousSlotKeyRef.current) {
    composerDraftKeyRef.current = nextComposerDraftIdentity(
      previousSlotKeyRef.current,
      composerDraftKeyRef.current,
      slotKey,
      key => store.resolve(key),
    );
    previousSlotKeyRef.current = slotKey;
  }
  const composerDraftKey = composerDraftKeyRef.current;
  useEffect(() => {
    setPendingRecipient(false);
    setComposerFeedback(current => current?.key === slotKey ? current : undefined);
    setDraftNotice(slotKey ? draftNoticesRef.current.get(slotKey) : undefined);
  }, [slotKey]);
  const active = snapshot?.conversations.find(
    (conversation) => conversation.id === slotKey,
  );
  // A merge that followed the view keeps the view's first identity, so view-level resets and remounts skip it.
  const viewIdentityRef = useRef<string | undefined>(undefined);
  const previousViewIdentity = viewIdentityRef.current;
  const viewIdentity = active && previousViewIdentity !== undefined && store.resolve(previousViewIdentity) === store.resolve(active.id)
    ? previousViewIdentity
    : active?.id;
  viewIdentityRef.current = viewIdentity;
  const sameGalleryConversation = (left: string | undefined, right: string | undefined) =>
    left !== undefined && right !== undefined && store.resolve(left) === store.resolve(right);
  useEffect(() => {
    if (!cancelledSendRestore) return;
    const message = active?.messages.find(({ id }) => id === cancelledSendRestore.messageId);
    const settlement = settleCancelledSendRestore(
      cancelledSendRestore,
      message?.sendCancellation?.state,
      store.get(cancelledSendRestore.draftKey),
    );
    if (settlement === "roll-back") {
      void store.edit(cancelledSendRestore.draftKey, cancelledSendRestore.previous);
      setComposerFeedback((feedback) => feedback?.key === cancelledSendRestore.draftKey && feedback.message === CANCELLED_ATTACHMENTS_SKIPPED_NOTICE ? undefined : feedback);
      setCancelledSendRestore(undefined);
    } else if (settlement === "clear") {
      setCancelledSendRestore(undefined);
    }
  }, [active?.messages, cancelledSendRestore]);
  useEffect(() => {
    setConversationTab("messages");
    setLightboxItems(null);
    setLightboxAttachmentId(undefined);
    setLightboxMessageId(undefined);
    gallerySessionRef.current += 1;
    pendingSaveIdsRef.current.clear();
    galleryConversationRef.current = active?.id;
    galleryCacheRef.current = { conversationId: active?.id, items: [] };
  }, [viewIdentity, appLock?.locked, snapshot?.encryption.state]);
  const conversationLoaded =
    Boolean(active) && snapshot?.activeConversationId === slotKey;
  const visiblePendingSendRows = visiblePendingSends(pendingSends, active, Boolean(detachedHistory && detachedHistory.conversationId === active?.id && detachedHistory.hasNewer));
  const trailingRowsKey = visiblePendingSendRows.map(({ id }) => id).join(",");
  const conversationTabsShown = Boolean(composerConversation) || conversationLoaded;
  const seenReactions = useMemo(() => createSeenReactionLedger({ mode: snapshot?.mode ?? "", origin: snapshot?.connection.origin }), [snapshot?.mode, snapshot?.connection.origin]);
  const reactionScopeKey = JSON.stringify([renderedSearchSessionKey, viewIdentity ?? slotKey]);
  const reactionReadEnabled = conversationLoaded && activeView === "conversations" && conversationTab === "messages" && snapshot?.encryption.state === "unlocked" && !appLock?.locked;
  const reactionReadContext = useRef({ key: reactionScopeKey, enabled: reactionReadEnabled });
  reactionReadContext.current = { key: reactionScopeKey, enabled: reactionReadEnabled };
  const canObserveMessages = () => mountedRef.current && selectedRef.current === slotKey && currentSearchSessionKey() === renderedSearchSessionKey
    && reactionReadContext.current.key === reactionScopeKey && reactionReadContext.current.enabled
    && !searchReadGate.current && document.visibilityState === "visible";
  const canReadMessages = () => canObserveMessages() && document.hasFocus();
  const canSeeReactionHints = () => canObserveMessages() && (document.hasFocus() || performance.now() - listScrollAttentionAt.current <= SCROLL_ATTENTION_MS);
  const markReadMessages = (candidateIds: readonly string[]) => {
    if (!canReadMessages()) return;
    const seen = seenMessageIds.current;
    const pending = pendingSeenMessageIds.current;
    const ids = [...new Set(candidateIds)].filter(id => !seen.has(id) && !pending.has(id));
    if (!ids.length) return;
    ids.forEach(id => pending.add(id));
    void bridge.mark_seen(ids).then(() => {
      if (reactionReadContext.current.key === reactionScopeKey) ids.forEach(id => seen.add(id));
    }).catch(() => {}).finally(() => ids.forEach(id => pending.delete(id)));
  };
  const recordSeenReactions = (ids: readonly string[]) => {
    if (!canReadMessages()) return;
    ids.forEach(id => badgeReadIds.current.add(id));
    seenReactions.mark(slotKey, ids);
    markReadMessages(ids);
  };
  useEffect(() => {
    const commitHeld = () => {
      const held = pendingSeenReactionsRef.current;
      if (!held) return;
      if (held.scope !== reactionScopeKey) {
        pendingSeenReactionsRef.current = undefined;
        setPendingSeenReactions(undefined);
      } else if (canReadMessages()) {
        recordSeenReactions([...held.ids]);
        pendingSeenReactionsRef.current = undefined;
        setPendingSeenReactions(undefined);
      }
    };
    // A render that reopens the read gate (such as returning to the Messages tab) can commit a hold kept across focus.
    commitHeld();
    window.addEventListener("focus", commitHeld);
    document.addEventListener("visibilitychange", commitHeld);
    return () => {
      window.removeEventListener("focus", commitHeld);
      document.removeEventListener("visibilitychange", commitHeld);
    };
  });
  useEffect(() => seenReactions.subscribe(() => setReactionSeenRevision(value => value + 1)), [seenReactions]);
  const copyOneTimeCode = async (target: OneTimeCodeTarget, code: string) => {
    await bridge.copy_one_time_code(target, code);
    if (target.kind === "message") seenMessageIds.current.add(target.messageId);
    else void refresh().catch(() => undefined);
  };
  const captureReactionLayout = (): ReactionLayoutSnapshot | null => searchReadGate.current ? null
    : { bottom: nearBottom.current && !detachedHistoryRef.current, anchor: captureHistoryAnchor() };
  const restoreReactionLayout = (layout: ReactionLayoutSnapshot) => {
    if (searchReadGate.current) return;
    if (layout.bottom) stickToBottom();
    else correctHistoryAnchorScroll(layout.anchor);
  };
  const messageIds = conversationLoaded
    ? active!.messages.map((message) => message.id).join(",")
    : "";
  const stickToBottom = (element = messageListRef.current) => {
    if (!element) return;
    element.setAttribute("data-scroll-programmatic", "");
    element.scrollTop = element.scrollHeight;
    messageListScrollTopRef.current = element.scrollTop;
    if (typeof requestAnimationFrame === "undefined") {
      element.removeAttribute("data-scroll-programmatic");
      return;
    }
    requestAnimationFrame(() =>
      element.removeAttribute("data-scroll-programmatic"),
    );
  };
  const isNearMessageListBottom = (element: HTMLDivElement) =>
    element.scrollHeight - element.scrollTop - element.clientHeight <= 48;
  const adjustMessageScrollForGeometry = (element = messageListRef.current) => {
    if (!element) return;
    if (element.clientHeight === 0) {
      lastUnobscuredHeight.current = null;
      messageListScrollTopRef.current = element.scrollTop;
      return;
    }
    if (overlayHeightRef.current === null) return;
    const unobscuredHeight = Math.max(0, element.clientHeight - overlayHeightRef.current);
    const previousUnobscuredHeight = lastUnobscuredHeight.current;
    lastUnobscuredHeight.current = unobscuredHeight;
    if (previousUnobscuredHeight === null) {
      if (nearBottom.current) stickToBottom(element);
      return;
    }
    if (nearBottom.current) {
      stickToBottom(element);
      return;
    }
    const adjustment = previousUnobscuredHeight - unobscuredHeight;
    if (adjustment === 0) return;
    element.setAttribute("data-scroll-programmatic", "");
    element.scrollTop += adjustment;
    messageListScrollTopRef.current = element.scrollTop;
    nearBottom.current = isNearMessageListBottom(element);
    if (typeof requestAnimationFrame === "undefined") element.removeAttribute("data-scroll-programmatic");
    else requestAnimationFrame(() => element.removeAttribute("data-scroll-programmatic"));
  };
  const loadOlderMessages = useCallback(async () => {
    const conversationId = selectedRef.current;
    const current = snapshotRef.current?.conversations.find((item) => item.id === conversationId);
    const beforeMessageId = current?.messages[0]?.id;
    if (!conversationId || isLocalDraftKey(conversationId) || !beforeMessageId || !hasOlderMessagesRef.current || historyLoadingRef.current) {
      historyFocusPendingRef.current = false;
      return;
    }
    const sequence = ++historySequence.current;
    historyLoadingRef.current = true;
    setHistoryLoading(true);
    setHistoryLoadError(null);
    try {
      const detached = detachedHistoryRef.current?.conversationId === conversationId ? detachedHistoryRef.current : undefined;
      if (detached) {
        loadSequence.current += 1;
        setDetached({ ...detached, pendingLoad: "older" });
        const window = await bridge.load_message_window({ direction: "older", conversationId, messageId: beforeMessageId });
        if (!mountedRef.current || sequence !== historySequence.current || selectedRef.current !== conversationId || detached.generation !== detachedHistoryRef.current?.generation || detached.sessionKey !== currentSearchSessionKey()) return;
        if (window.conversationId !== conversationId) {
          const currentDetached = detachedHistoryRef.current;
          if (currentDetached?.pendingLoad === "older") setDetached({ ...currentDetached, pendingLoad: undefined });
          setHistoryLoadError("Conversation changed while loading older messages. Reload and try again.");
          return;
        }
        const existing = snapshotRef.current?.conversations.find(item => item.id === conversationId)?.messages ?? [];
        const merged = mergeDetachedMessages(existing, window.messages, "older");
        const next = { ...detached, messageIds: merged.messages.map(message => message.id), hasOlder: window.hasOlder, hasNewer: detached.hasNewer || window.hasNewer || merged.droppedNewer, pendingLoad: undefined };
        loadSequence.current += 1;
        setDetached(next);
        historyAnchorRef.current = captureHistoryAnchor(beforeMessageId);
        nearBottom.current = false;
        flushSync(() => {
          setSuppressHistoryAnnouncements(true);
          setSnapshot((previous) => previous && previous.activeConversationId === conversationId
            ? { ...previous, conversations: previous.conversations.map(item => item.id === conversationId ? { ...item, messages: merged.messages } : item) }
            : previous);
        });
        correctHistoryAnchorScroll(historyAnchorRef.current);
        hasOlderMessagesRef.current = window.hasOlder;
        setHasOlderMessages(window.hasOlder);
        if (!window.hasOlder) setHistoryExhausted(true);
        if (typeof requestAnimationFrame === "undefined") setSuppressHistoryAnnouncements(false);
        else requestAnimationFrame(() => mountedRef.current && setSuppressHistoryAnnouncements(false));
        return;
      }
      const page: HistoryPage = await bridge.load_history(conversationId, beforeMessageId);
      if (!mountedRef.current || sequence !== historySequence.current || selectedRef.current !== conversationId) return;
      if (page.conversationId !== conversationId) {
        setHistoryLoadError("Conversation changed while loading older messages. Reload and try again.");
        return;
      }
      historyAnchorRef.current = captureHistoryAnchor(beforeMessageId);
      nearBottom.current = false;
      flushSync(() => {
        setSuppressHistoryAnnouncements(true);
        setSnapshot((previous) => {
        if (!previous || previous.activeConversationId !== conversationId) return previous;
        return { ...previous, conversations: previous.conversations.map((item) => item.id === conversationId
          ? { ...item, messages: mergeMessages(page.messages, item.messages) }
          : item) };
        });
      });
      correctHistoryAnchorScroll(historyAnchorRef.current);
      hasOlderMessagesRef.current = page.hasMore;
      setHasOlderMessages(page.hasMore);
      if (!page.hasMore) setHistoryExhausted(true);
      if (typeof requestAnimationFrame === "undefined") setSuppressHistoryAnnouncements(false);
      else requestAnimationFrame(() => mountedRef.current && setSuppressHistoryAnnouncements(false));
    } catch (error) {
      if (sequence === historySequence.current) {
        const currentDetached = detachedHistoryRef.current;
        if (currentDetached?.pendingLoad === "older") setDetached({ ...currentDetached, pendingLoad: undefined });
        setHistoryLoadError(errorText(error));
      }
    } finally {
      if (sequence === historySequence.current) {
        historyLoadingRef.current = false;
        setHistoryLoading(false);
      }
    }
  }, []);
  const loadNewerMessages = useCallback(async (control?: HTMLElement) => {
    const detached = detachedHistoryRef.current;
    const conversationId = selectedRef.current;
    const existing = snapshotRef.current?.conversations.find(item => item.id === conversationId)?.messages ?? [];
    const afterMessageId = existing.at(-1)?.id;
    if (!detached || detached.conversationId !== conversationId || !detached.hasNewer || detached.pendingLoad || !afterMessageId) return;
    const sequence = ++historySequence.current;
    loadSequence.current += 1;
    historyFocusPendingRef.current = control?.id === "history-retry-button" && document.activeElement === control;
    setDetached({ ...detached, pendingLoad: "newer" });
    setHistoryLoadError(null);
    try {
      const window = await bridge.load_message_window({ direction: "newer", conversationId, messageId: afterMessageId });
      if (!mountedRef.current || sequence !== historySequence.current || selectedRef.current !== conversationId || window.conversationId !== conversationId || detached.generation !== detachedHistoryRef.current?.generation || detached.sessionKey !== currentSearchSessionKey()) return;
      const merged = mergeDetachedMessages(existing, window.messages, "newer");
      const next = { ...detached, messageIds: merged.messages.map(message => message.id), hasOlder: detached.hasOlder || window.hasOlder || merged.droppedOlder, hasNewer: window.hasNewer, pendingLoad: undefined };
      loadSequence.current += 1;
      setDetached(next);
      setSnapshot(previous => previous ? { ...previous, conversations: previous.conversations.map(item => item.id === conversationId ? { ...item, messages: merged.messages, hasOlderMessages: next.hasOlder } : item) } : previous);
      hasOlderMessagesRef.current = next.hasOlder;
      setHasOlderMessages(next.hasOlder);
    } catch (error) {
      if (sequence === historySequence.current) {
        setDetached(detached);
        setHistoryLoadError(errorText(error), "newer");
      }
    }
  }, []);
  const canAutoLoadOlderMessages = (element: HTMLDivElement) =>
    element.clientHeight > 0 &&
    element.scrollTop <= element.clientHeight &&
    !element.hasAttribute("data-scroll-programmatic") &&
    !historyLoadingRef.current &&
    !historyErrorRef.current &&
    hasOlderMessagesRef.current &&
    !detachedHistoryRef.current?.pendingLoad;
  const autoLoadOlderMessages = (element: HTMLDivElement) => {
    if (canAutoLoadOlderMessages(element)) void loadOlderMessages();
  };
  const onMessageScroll = () => {
    const element = messageListRef.current;
    if (!element) return;
    const now = performance.now();
    if (!element.hasAttribute("data-scroll-programmatic") && now - listScrollAttentionAt.current <= SCROLL_ATTENTION_MS) listScrollAttentionAt.current = now;
    const previousScrollTop = messageListScrollTopRef.current;
    const currentScrollTop = element.scrollTop;
    messageListScrollTopRef.current = currentScrollTop;
    if (element.clientHeight === 0) return;
    nearBottom.current = isNearMessageListBottom(element);
    if (currentScrollTop < previousScrollTop) autoLoadOlderMessages(element);
  };
  const onMessageWheel = (event: WheelEvent<HTMLDivElement>) => {
    listScrollAttentionAt.current = performance.now();
    const element = messageListRef.current;
    if (!element) return;
    messageListScrollTopRef.current = element.scrollTop;
    if (event.deltaY < 0) autoLoadOlderMessages(element);
  };
  const loadOlderFromManualControl = (control: HTMLElement) => {
    historyFocusPendingRef.current = document.activeElement === control;
    void loadOlderMessages();
  };
  useLayoutEffect(() => {
    if (!historyFocusPendingRef.current) return;
    const band = document.getElementById("history-load-band");
    const list = messageListRef.current;
    const active = document.activeElement;
    const canRestoreFocus = active === document.body || active === band || active === list;
    if (!canRestoreFocus) {
      historyFocusPendingRef.current = false;
      return;
    }
    if (historyLoading || historyError || hasOlderMessages) band?.focus();
    else list?.focus();
    if (!historyLoading) historyFocusPendingRef.current = false;
  }, [historyLoading, historyError, hasOlderMessages]);
  useLayoutEffect(() => {
    lastUnobscuredHeight.current = null;
    if (!conversationLoaded || detachedHistoryRef.current?.conversationId === slotKey || searchReadGate.current) return;
    stickToBottom();
    nearBottom.current = true;
  }, [viewIdentity, conversationLoaded]);
  useLayoutEffect(() => {
    messageRows.current.forEach((candidate, id) => {
      if (id === searchTargetId || candidate.dataset.searchTarget !== "true") return;
      candidate.removeAttribute("data-search-target");
      candidate.removeAttribute("tabindex");
      candidate.removeAttribute("aria-describedby");
    });
    if (!searchTargetId || !conversationLoaded || detachedHistoryRef.current?.pendingTarget !== searchTargetId) return;
    const row = messageRows.current.get(searchTargetId);
    const list = messageListRef.current;
    if (!row || !list) return;
    row.setAttribute("data-search-target", "true");
    row.setAttribute("tabindex", "-1");
    row.setAttribute("aria-describedby", "search-result-message-hint");
    list.setAttribute("data-scroll-programmatic", "");
    row.scrollIntoView?.({ block: "center", behavior: "instant" });
    messageListScrollTopRef.current = list.scrollTop;
    row.focus({ preventScroll: true });
    const current = detachedHistoryRef.current;
    if (current?.pendingTarget === searchTargetId) setDetached({ ...current, pendingTarget: undefined });
    const release = () => {
      messageListScrollTopRef.current = list.scrollTop;
      list.removeAttribute("data-scroll-programmatic");
      searchReadGate.current = false;
      setReactionSeenRevision(value => value + 1);
    };
    if (typeof requestAnimationFrame === "undefined") release();
    else requestAnimationFrame(release);
  }, [searchTargetId, messageIds, conversationLoaded]);
  useLayoutEffect(() => {
    adjustMessageScrollForGeometry();
  }, [overlayHeight]);
  useLayoutEffect(() => {
    const list = messageListNode;
    const content = document.getElementById("message-list-content");
    if (!list || !content || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver((entries) => {
      if (entries.some((entry) => entry.target === list)) {
        adjustMessageScrollForGeometry(list);
      } else if (nearBottom.current && !searchReadGate.current && !detachedHistoryRef.current) {
        stickToBottom(list);
      }
    });
    observer.observe(list);
    observer.observe(content);
    return () => observer.disconnect();
  }, [activeView, slotKey, conversationLoaded, headPanel, messageListNode]);
  useEffect(() => {
    seenMessageIds.current = new Set<string>();
    pendingSeenMessageIds.current = new Set<string>();
    badgeReadIds.current = new Set<string>();
    pendingSeenReactionsRef.current = undefined;
    setPendingSeenReactions(undefined);
  }, [reactionScopeKey]);
  const setMessageListNode = useCallback((node: HTMLDivElement | null) => {
    if (node !== messageListRef.current) lastUnobscuredHeight.current = null;
    messageListRef.current = node;
    setMessageListRoot(node);
    messageListScrollTopRef.current = node?.scrollTop ?? 0;
  }, []);
  const overlayMargin =
    overlayHeight === null ? null : Math.ceil(overlayHeight / 8) * 8;
  useEffect(() => {
    const listNode = messageListRef.current;
    if (!listNode || overlayMargin === null) return;
    let observing = true;
    const markVisible = () => {
      if (!observing) return;
      const ids = [...messageRows.current]
        .filter(([, row]) => !row.hasAttribute("data-reaction-marker") && row.dataset.visible === "true")
        .map(([id]) => id);
      markReadMessages([...ids, ...badgeReadIds.current]);
    };
    // Preserve the last measured visibility across native state refreshes so a rejected hidden-window mark can retry.
    markVisible();
    const observer =
      typeof IntersectionObserver === "undefined" || !reactionReadEnabled
        ? undefined
         : new IntersectionObserver((entries) => {
            if (!observing) return;
            entries.forEach((entry) =>
              entry.target.setAttribute(
                "data-visible",
                String(!entry.target.hasAttribute("data-reaction-marker") && entry.isIntersecting && entry.intersectionRect?.width > 0 && entry.intersectionRect?.height > 0),
              ),
            );
            markVisible();
          }, { root: listNode, rootMargin: `0px 0px -${overlayMargin}px 0px` });
    messageRows.current.forEach((row) => { if (!row.hasAttribute("data-reaction-marker")) observer?.observe(row); });
    window.addEventListener("focus", markVisible);
    document.addEventListener("visibilitychange", markVisible);
    return () => {
      observing = false;
      observer?.disconnect();
      window.removeEventListener("focus", markVisible);
      document.removeEventListener("visibilitychange", markVisible);
    };
  }, [reactionScopeKey, messageIds, reactionReadEnabled, overlayMargin, snapshot, reactionSeenRevision, messageListNode]);
  const registerRow: RowRegistry = (id, row) => {
    if (row) messageRows.current.set(id, row);
    else messageRows.current.delete(id);
  };

  const slot = store.get(slotKey);
  const content = slot?.content ?? blankContent();
  const gateways = snapshot?.gateways ?? [];
  const route = resolveRoute(content, gateways);
  const rcsRoute = useRcsRoute({
    snapshot: snapshot ?? undefined,
    activeConversationId: active?.id,
    gatewayId: route.kind === "route" ? route.gateway.id : undefined,
    locked: Boolean(appLock?.locked),
  });
  const rcsBlock = rcsRoute.block;
  const rcsRetryBlock = rcsDownloadRetryBlock({ snapshot: snapshot ?? undefined, activeConversationId: active?.id, gatewayId: route.kind === "route" ? route.gateway.id : undefined, nowMs: Date.now(), locked: Boolean(appLock?.locked) });
  const mmsRequired = !rcsRoute.current && requiresMms(content, active);
  const routeRecipients = recipientCount(content, active);
  const estimateBytes = draftEstimateBytes(content, attachmentViews);
  const mmsPlan = planDraftMms(content, route, attachmentViews);
  const problem = routeProblem(
    route,
    gateways,
    mmsRequired,
    routeRecipients,
    mmsPlan,
    mmsPlan.kind === "error" ? attachmentViews[mmsPlan.imageId ?? ""]?.name : undefined,
  );
  const isNewConversation = Boolean(slotKey) && (
    isLocalDraftKey(slotKey) ||
    (newComposeKey !== null && store.resolve(newComposeKey) === slotKey) ||
    active?.draftOnly === true
  );
  const pendingRecipientReason = "finish adding the recipient before sending";
  const effectivePendingRecipient = pendingRecipient && isNewConversation;
  const rcsBlockReason = rcsComposerReason(rcsBlock);
  const routeUnavailableReason = effectivePendingRecipient ? pendingRecipientReason : problem ?? rcsBlockReason;
  const draftHydrated = Boolean(slotKey && store.isHydrated(slotKey));
  const composerReadOnly = Boolean(slotKey && !draftHydrated && !isLocalDraftKey(slotKey));
  const conversationSurfaceVisible = Boolean(composerConversation) || activeView === "conversations";
  const latestIncomingMessageId = useMemo(
    () => active?.messages.reduce<string | undefined>((latest, message) => message.sender === "other" ? message.id : latest, undefined),
    [active?.messages],
  );
  /** The newest incoming message's sender. A group member's typing state ends when their message arrives. */
  const latestIncomingSender = useMemo(
    () => active?.messages.reduce<string | undefined>((latest, message) => message.sender === "other" ? message.senderAddress : latest, undefined),
    [active?.messages],
  );
  const typingSender = useTypingSender({
    bridge,
    conversationId: active?.id,
    current: rcsRoute.current,
    existing: Boolean(active && slotKey && !isLocalDraftKey(slotKey) && !active.draftOnly),
    sendSupported: Boolean(slotKey) && draftHydrated && !draftSubscriptionError && !routeUnavailableReason && !sending,
    readOnly: composerReadOnly,
    visible: conversationSurfaceVisible,
    sharingEnabled: shareTyping,
    groupAllowsTyping: groupAllowsTypingSend(active),
  });
  const isGroup = (active?.participants?.length ?? 0) > 1;
  /** A withdrawn group is read-only: it can be read, but not sent to (UX spec §9, Q2). */
  const groupLeft = groupIsReadOnly(active?.group);
  const typing = useTypingDisplay({
    bridge,
    conversationId: active?.id,
    current: rcsRoute.current && conversationSurfaceVisible,
    localUserTyping: typingSender.state === "active",
    incomingMessageId: latestIncomingMessageId,
    incomingSender: latestIncomingSender,
    members: active ? groupMembers(active) : undefined,
    connected: snapshot?.connection.state === "connected",
    direct: !isGroup,
  });

  const edit = (patch: Partial<DraftContent>) => {
    if (!slotKey || deletingId === slotKey || resolvingDraftConflict) return;
    autoAcknowledgeDraftSyncNotice();
    setDraftNotice((notice) => notice?.key === slotKey && notice.event === "stale-send" ? undefined : notice);
    draftNoticesRef.current.delete(slotKey);
    void store.edit(slotKey, patch);
  };
  const commitRecipients = (recipientIds: string[]) => {
    if (!slotKey || resolvingDraftConflict) return;
    autoAcknowledgeDraftSyncNotice();
    const previous = content.recipientIds;
    void store.edit(slotKey, { recipientIds }).then(outcome => {
      const current = store.get(slotKey);
      if (!outcome.ok && current?.error?.code === "recipientless-draft-exists") {
        setNotice("Another blank draft already exists. Your previous recipients were restored.", "warning");
        void store.edit(slotKey, { recipientIds: previous });
      } else if (!outcome.ok && current?.error?.code === "draft-merge-overflow") {
        setNotice("These drafts are too large to combine. Your draft is still available to edit.", "warning");
      }
    });
  };
  const changeRecipientPosition = (position: RecipientPosition) => {
    setRecipientPosition(position);
    persistRecipientPosition(position);
  };

  const startNewMessage = async (recipient?: string) => {
    notificationNavigationEpoch.current += 1;
    const previous = selectedRef.current;
    const previousSlot = previous ? store.get(previous) : undefined;
    if (previousSlot && isDirty(previousSlot)) await store.flush(previous);
    const { key } = store.createNew(recipient);
    const draftKey = store.resolve(key);
    setNewComposeKey(draftKey);
    choose(draftKey);
    setNotice("");
  };
  /** Opens a new conversation draft to the same people, for a withdrawn group that cannot be sent to. */
  const startGroupMmsDraft = async (recipientIds: string[]) => {
    notificationNavigationEpoch.current += 1;
    const previous = selectedRef.current;
    const previousSlot = previous ? store.get(previous) : undefined;
    if (previousSlot && isDirty(previousSlot)) await store.flush(previous);
    const { key } = store.createNew();
    const draftKey = store.resolve(key);
    void store.edit(draftKey, { recipientIds });
    setNewComposeKey(draftKey);
    choose(draftKey);
    setNotice("");
  };
  const focusComposer = () => {
    requestAnimationFrame(() => document.getElementById("composer-textarea")?.focus());
  };
  const startOmniConversation = async (address: string, signal: AbortSignal): Promise<boolean | "deferred"> => {
    if (signal.aborted) return false;
    const generation = ++navigationGeneration.current;
    const sessionKey = currentSearchSessionKey();
    let deferred = false;
    const isCurrent = () => (deferred || !signal.aborted)
      && mountedRef.current
      && generation === navigationGeneration.current
      && sessionKey === currentSearchSessionKey();
    const begin = async (): Promise<boolean> => {
      if (!isCurrent()) return false;
      const previous = selectedRef.current;
      const comparisonAddress = normalizePhoneNumber(address) ?? address.trim();
      const existing = snapshotRef.current?.conversations.find((conversation) =>
        conversation.participants?.length === 1
        && (normalizePhoneNumber(conversation.participants[0]) ?? conversation.participants[0].trim()) === comparisonAddress,
      );
      const localDraft = store.localDrafts().find((draft) =>
        draft.content.recipientIds.length === 1
        && (normalizePhoneNumber(draft.content.recipientIds[0]) ?? draft.content.recipientIds[0].trim()) === comparisonAddress,
      );
      if (localDraft && previous === localDraft.key) {
        setActiveViewNow("conversations");
        select(localDraft.key);
        focusComposer();
        return true;
      }
      if (previous) {
        const flushed = await store.flush(previous);
        if (!isCurrent()) return false;
        if (!flushed.ok) throw new Error(flushed.error);
      }
      if (!isCurrent()) return false;
      if (existing || localDraft) {
        setActiveViewNow("conversations");
        select(existing?.id ?? localDraft!.key);
        focusComposer();
        return true;
      }
      if (!isCurrent()) return false;
      const { key } = store.createNew(address);
      const draftKey = store.resolve(key);
      setActiveViewNow("conversations");
      setNewComposeKey(draftKey);
      choose(draftKey);
      setNotice("");
      focusComposer();
      return true;
    };
    if (!contactNavigation.current) return begin();
    let started: Promise<boolean> | undefined;
    let guardReturned = false;
    const deferredByGuard = contactNavigation.current(
      () => {
        if (!guardReturned) {
          started = begin();
          return;
        }
        deferred = true;
        started = begin().catch((error: unknown) => {
          if (isCurrent()) setNotice(`Could not start the conversation: ${errorText(error)}`);
          return false;
        });
      },
      () => { deferred = false; },
    );
    guardReturned = true;
    return deferredByGuard ? "deferred" : started ?? false;
  };
  const resolveOmniContactNumbers = async (
    hit: Extract<LocalSearchHit, { kind: "contact" }>,
    signal: AbortSignal,
  ) => {
    const result = await findContactTarget(
      { bookId: hit.bookId, contactId: hit.contactId },
      bridge.list_contacts_page,
      signal,
    );
    if (signal.aborted) return [];
    if (result.state === "unavailable") throw { code: "contact-unavailable" };
    if (result.state === "oversized") throw { code: "contact-oversized" };
    if (result.state !== "found") return [];
    return result.contact.phones.filter((phone) => phone.number.trim()).map((phone) => ({
      address: phone.number,
      display: formatPhoneNumber(phone.number),
      label: phone.label,
    }));
  };
  const toggleConversationList = () => {
    if (narrowBrowserViewport) {
      if (activeView !== "conversations") {
        setActiveView("conversations");
        setMobileConversationListOpen(true);
        return;
      }
      if (selected) setMobileConversationListOpen((open) => !open);
      return;
    }
    if (activeView !== "conversations") {
      setActiveView("conversations");
      resizeListTo(previousListWidth.current);
      return;
    }
    resizeListTo(listCollapsed ? previousListWidth.current : 0);
  };
  const startTitlebarMessage = () => {
    const begin = () => {
      setContactTarget(undefined);
      setActiveViewNow("conversations");
      if (listCollapsed) resizeListTo(previousListWidth.current);
      void startNewMessage();
    };
    if (contactNavigation.current) contactNavigation.current(begin);
    else begin();
  };

  const retrySave = async () => {
    if (draftSubscriptionError) {
      setDraftSubscriptionError(undefined);
      restartDraftSubscriptionRef.current?.();
      return;
    }
    if (!slotKey) return;
    const current = store.get(slotKey);
    const session = currentSearchSessionKey();
    const outcome = await store.retry(slotKey);
    if (!mountedRef.current || session !== currentSearchSessionKey() || store.get(slotKey) !== current) return;
    if (!outcome.ok) return;
    // A successful save already replaced the failure with its "saved" event, which then lasts its normal 2s.
    // Only a retry that wrote nothing clears the failure here.
    saveFailedKeysRef.current.delete(slotKey);
    setDraftNotice((notice) => notice?.key === slotKey && notice.event === "save-failed" ? undefined : notice);
  };

  const deleteDraft = async (conversationId: string) => {
    if (!conversationId) return setDeleteConfirmationId(null);
    const conversation = snapshotRef.current?.conversations.find(item => item.id === conversationId);
    const localDraft = isLocalDraftKey(conversationId);
    if (!localDraft && (!conversation?.draftOnly || !conversation.recipientlessDraft)) return;
    let draftSlot = store.get(conversationId);
    if (!draftSlot && !localDraft) {
      try {
        const state = await bridge.load_state(conversationId);
        if (state.activeConversationId === conversationId) adoptHostDraft(conversationId, state.draft);
        draftSlot = store.get(conversationId);
      } catch (error) {
        setNotice(`Draft not deleted: ${errorText(error)}`);
        return;
      }
    }
    if (!draftSlot || draftSlot.content.recipientIds.length) return;
    const hasContent = Boolean(draftSlot.content.text || draftSlot.content.attachmentIds.length);
    if (hasContent && deleteConfirmationId !== conversationId) {
      setDeleteConfirmationId(conversationId);
      return;
    }
    setDeletingId(conversationId);
    try {
      const flushed = await store.flush(conversationId);
      const current = store.get(conversationId);
      if (!flushed.ok && current?.id) throw new Error(flushed.error);
      if (!current || current.content.recipientIds.length) throw new Error("The draft changed elsewhere; reload before deleting it.");
      if (shouldConfirmPostFlushDelete(
        conversationId,
        current.key,
        current.content,
        hasContent,
        deleteConfirmationId,
      )) {
        setDeleteConfirmationId(current.key);
        return;
      }
      if (current.id)
        await bridge.discard_draft({ id: current.id, conversationId: current.conversationId, expectedRevision: current.revision });
      store.remove(conversationId);
      setDeleteConfirmationId(null);
      setNewComposeKey((current) =>
        current && store.resolve(current) === conversationId ? null : current,
      );
      if (selectedRef.current === conversationId) {
        const next = snapshotRef.current?.conversations.find(item => item.id !== conversationId)?.id ?? "";
        choose(next);
      }
      try {
        await refresh();
      } catch (error) {
        setNotice(`Draft deleted, but refresh failed: ${errorText(error)}`, "warning");
        return;
      }
      setNotice("Draft deleted.", "info");
    } catch (error) {
      setNotice(`Draft not deleted: ${errorText(error)}`);
    } finally {
      setDeletingId(null);
    }
  };
  const cancelDeleteDraft = () => {
    setDeleteConfirmationId(null);
  };

  const remainingAttachmentSlots = (draftKey: string) => {
    const attachmentIds = store.get(draftKey)?.content.attachmentIds ?? [];
    const inFlight = pastedAttachmentInFlight.current.get(draftKey) ?? 0;
    return Math.max(0, MAX_DRAFT_ATTACHMENTS - attachmentIds.length - inFlight);
  };
  const beginAttachmentPreparation = (draftKey: string) => {
    const operation = Symbol("attachment preparation");
    const operations = attachmentPreparationInFlight.current.get(draftKey) ?? new Set<symbol>();
    operations.add(operation);
    attachmentPreparationInFlight.current.set(draftKey, operations);
    return operation;
  };
  const finishAttachmentPreparation = (draftKey: string, operation: symbol) => {
    const operations = attachmentPreparationInFlight.current.get(draftKey);
    if (!operations) return;
    operations.delete(operation);
    if (!operations.size && attachmentPreparationInFlight.current.get(draftKey) === operations)
      attachmentPreparationInFlight.current.delete(draftKey);
  };
  const addAttachment = async () => {
    if (!slotKey || resolvingDraftConflict) return;
    autoAcknowledgeDraftSyncNotice();
    const draftKey = slotKey;
    const session = currentSearchSessionKey();
    const generation = draftStoreGeneration.current;
    const operation = beginAttachmentPreparation(draftKey);
    try {
      const picked = await bridge.pick_attachments();
      if (!picked.length) return;
      if (generation !== draftStoreGeneration.current || session !== currentSearchSessionKey()) return;
      const selectedFiles = picked.slice(0, remainingAttachmentSlots(draftKey));
      if (selectedFiles.length < picked.length)
        setComposerNotice(draftKey, `You can attach up to ${MAX_DRAFT_ATTACHMENTS} images.`, "warning", "attachment-feedback");
      if (!selectedFiles.length) return;
      setAttachmentViews((current) => ({
        ...current,
        ...Object.fromEntries(selectedFiles.map((file) => [file.id, file])),
      }));
      const resolvedKey = store.resolve(draftKey);
      const existing = store.get(resolvedKey)?.content.attachmentIds ?? [];
      const outcome = await store.edit(resolvedKey, {
        attachmentIds: [...existing, ...selectedFiles.map((file) => file.id)],
      });
      if (!outcome.ok) setComposerNotice(store.resolve(draftKey), `Couldn't add attachment: ${outcome.error}`, "error", "attachment-feedback");
    } catch (error) {
      setNotice(`Attachment picker: ${errorText(error)}`);
    } finally {
      finishAttachmentPreparation(draftKey, operation);
    }
  };
  const addPastedAttachments = async (files: File[]) => {
    if (!slotKey || !files.length || resolvingDraftConflict) return;
    autoAcknowledgeDraftSyncNotice();
    const draftKey = slotKey;
    const inFlight = pastedAttachmentInFlight.current.get(draftKey) ?? 0;
    const remaining = remainingAttachmentSlots(draftKey);
    const selectedFiles = files.slice(0, remaining);
    if (!selectedFiles.length) {
      setComposerNotice(draftKey, `You can attach up to ${MAX_DRAFT_ATTACHMENTS} images.`, "warning", "attachment-feedback");
      return;
    }
    if (selectedFiles.length < files.length)
      setComposerNotice(draftKey, `You can attach up to ${MAX_DRAFT_ATTACHMENTS} images.`, "warning", "attachment-feedback");
    const session = currentSearchSessionKey();
    const generation = draftStoreGeneration.current;
    const operation = beginAttachmentPreparation(draftKey);
    pastedAttachmentInFlight.current.set(draftKey, inFlight + selectedFiles.length);
    try {
      const pasted = await Promise.all(selectedFiles.map(async (file) => ({
        bytes: await file.arrayBuffer(),
        name: file.name,
        mediaType: file.type,
      })));
      const { views, failures } = await bridge.prepare_pasted_attachments(pasted);
      if (generation !== draftStoreGeneration.current || session !== currentSearchSessionKey()) return;
      const resolvedKey = store.resolve(draftKey);
      setAttachmentViews((current) => ({ ...current, ...Object.fromEntries(views.map((file) => [file.id, file])) }));
      const current = store.get(resolvedKey)?.content.attachmentIds ?? [];
      const available = Math.max(0, MAX_DRAFT_ATTACHMENTS - current.length);
      const outcome = await store.edit(resolvedKey, { attachmentIds: [...current, ...views.slice(0, available).map((file) => file.id)] });
      if (!outcome.ok) setComposerNotice(store.resolve(draftKey), `Couldn't add pasted image: ${outcome.error}`, "error", "attachment-feedback");
      else if (failures.length || views.length > available) setComposerNotice(resolvedKey, [...failures, ...(views.length > available ? [`You can attach up to ${MAX_DRAFT_ATTACHMENTS} images.`] : [])].join(" "), "warning", "attachment-feedback");
    } catch (error) {
      setComposerNotice(store.resolve(draftKey), `Couldn't add pasted image: ${errorText(error)}`, "error", "attachment-feedback");
    } finally {
      finishAttachmentPreparation(draftKey, operation);
      const pending = pastedAttachmentInFlight.current.get(draftKey) ?? selectedFiles.length;
      if (pending <= selectedFiles.length) pastedAttachmentInFlight.current.delete(draftKey);
      else pastedAttachmentInFlight.current.set(draftKey, pending - selectedFiles.length);
    }
  };

  const send = async (options: GroupSendOptions = {}) => {
    if (sendingRef.current || !slotKey || groupLeft) return;
    if (isWritesPaused(snapshotRef.current)) {
      setComposerNotice(slotKey, UPDATE_SEND_FAILURE, "warning");
      return;
    }
    autoAcknowledgeDraftSyncNotice();
    const session = currentSearchSessionKey();
    sendingRef.current = true;
    let resolveSend!: (settlement: SendSettlement) => void;
    const inFlightSend = { settled: new Promise<SendSettlement>((resolve) => { resolveSend = resolve; }), resolve: (settlement: SendSettlement) => resolveSend(settlement) };
    inFlightSendRef.current = inFlightSend;
    setSending(true);
    setGroupNotice(null);
    setComposerNotice(slotKey, "");
    setDraftNotice((notice) => notice?.key === slotKey && notice.event === "stale-send" ? undefined : notice);
    draftNoticesRef.current.delete(slotKey);
    let pendingSendId: string | undefined;
    try {
      let resolvedKey = store.resolve(slotKey);
      let flushed = await store.flush(resolvedKey);
      while (flushed.ok && Boolean(store.get(resolvedKey) && isDirty(store.get(resolvedKey)!))) {
        flushed = await store.flush(resolvedKey);
        resolvedKey = store.resolve(resolvedKey);
      }
      if (!mountedRef.current || session !== currentSearchSessionKey() || !store.get(resolvedKey)) return;
      if (!flushed.ok && flushed.reviewRequired) {
        draftNoticeSequence.current += 1;
        reviewNoticeSequenceRef.current = draftNoticeSequence.current;
        setDraftNotice({ key: resolvedKey, event: "stale-send", sequence: draftNoticeSequence.current, persistent: true });
        return;
      }
      if (!flushed.ok)
        return setComposerNotice(resolvedKey,
          `Not sent: the draft could not be saved (${flushed.error}).`,
        );
      const current = store.get(resolvedKey);
      if (!current?.id)
        return setComposerNotice(resolvedKey, "Not sent: the draft has not been stored yet.");
      const latestSnapshot = snapshotRef.current;
      const latestConversation = latestSnapshot?.conversations.find(
        (conversation) => conversation.id === current.conversationId,
      );
      const latestGateways = latestSnapshot?.gateways ?? [];
      const chosen = resolveRoute(current.content, latestGateways);
      const latestMmsPlan = planDraftMms(current.content, chosen, attachmentViews);
      const blocked = routeProblem(
        chosen,
        latestGateways,
        !currentRcsRoute({ snapshot: latestSnapshot ?? undefined, activeConversationId: current.conversationId, gatewayId: chosen.kind === "route" ? chosen.gateway.id : undefined, nowMs: Date.now(), locked: Boolean(appLockRef.current?.locked) }).current && requiresMms(current.content, latestConversation),
        recipientCount(current.content, latestConversation),
        latestMmsPlan,
        latestMmsPlan.kind === "error" ? attachmentViews[latestMmsPlan.imageId ?? ""]?.name : undefined,
      );
      if (blocked || chosen.kind !== "route")
        return setComposerNotice(current.key, `Not sent: ${blocked ?? "choose a phone and SIM"}.`);
      const rcsBlocked = rcsSendBlock({ snapshot: latestSnapshot ?? undefined, activeConversationId: current.conversationId, gatewayId: chosen.gateway.id, nowMs: Date.now(), locked: Boolean(appLockRef.current?.locked) });
      if (rcsBlocked.kind !== "none") {
        const reason = rcsComposerReason(rcsBlocked)!;
        return setComposerNotice(current.key, `Not sent: ${reason}.`);
      }
      const sendContent = { ...current.content, recipientIds: [...current.content.recipientIds], attachmentIds: [...current.content.attachmentIds] };
      if (isWritesPaused(snapshotRef.current))
        return setComposerNotice(current.key, UPDATE_SEND_FAILURE, "warning");
      const groupSend = sendContent.recipientIds.length > 1 || (latestConversation?.participants?.length ?? 0) > 1;
      // The composer's revision: the caller's (a resend after a refusal), else the latest snapshot's.
      const reviewedRevision = groupNotice?.kind === "members-changed" && groupNotice.key === current.key ? groupNotice.memberRevision : undefined;
      const expectedMemberRevision = groupSend ? options.expectedMemberRevision ?? reviewedRevision ?? latestConversation?.group?.memberRevision : undefined;
      const restoreUnsent = async () => {
        const next = store.get(current.key)?.content ?? blankContent();
        await store.edit(current.key, {
          text: next.text === sendContent.text || next.text.startsWith(`${sendContent.text}\n`)
            ? next.text
            : [sendContent.text, next.text].filter(Boolean).join("\n"),
          attachmentIds: [...new Set([...sendContent.attachmentIds, ...next.attachmentIds])],
          groupName: next.groupName ?? sendContent.groupName,
        });
        store.release(current.key);
      };
      const pendingSend: PendingSend = {
        id: `pending:${current.id}:${++pendingSendGeneration.current}`,
        conversationId: current.conversationId,
        content: sendContent,
        createdAt: Date.now(),
        baselineSelfIds: new Set(latestConversation?.messages.filter((message) => message.sender === "self").map((message) => message.id)),
      };
      pendingSendId = pendingSend.id;
      setPendingSends((sends) => [...sends, pendingSend]);
      if (!detachedHistoryRef.current) nearBottom.current = true;
      store.hold(current.key);
      await store.edit(current.key, { text: "", attachmentIds: [], groupName: undefined });
      const sentSlot = store.get(current.key);
      if (!sentSlot || !mountedRef.current || session !== currentSearchSessionKey()) return;
      const sentGeneration = sentSlot.generation;
      try {
        const result = await bridge.send_draft({
          id: current.id,
          conversationId: current.conversationId,
          ...sendContent,
          groupName: groupSend ? normalizeGroupName(sendContent.groupName) : undefined,
          expectedRevision: current.revision,
          gatewayId: chosen.gateway.id,
          simId: chosen.gateway.simId,
          ...(expectedMemberRevision === undefined ? {} : { expectedMemberRevision }),
          ...(options.groupCreateConfirmDuplicate ? { groupCreateConfirmDuplicate: true } : {}),
        });
        if (!mountedRef.current || session !== currentSearchSessionKey() || !store.get(current.key)) return;
        if (!result.accepted) {
          await restoreUnsent();
          if (result.refusedAttachmentIds?.length) {
            const refused = new Set(result.refusedAttachmentIds);
            const first = sendContent.attachmentIds.find((id) => refused.has(id));
            const attachment = first ? attachmentViews[first] : undefined;
            const heic = attachment && (attachment.mediaType === "image/heic" || attachment.mediaType === "image/heif" || /\.hei[cf]$/i.test(attachment.name));
            const copy = result.reason === "too_large"
              ? peppyCopy.rcs_media_refused_size.replace("{name}", attachment?.name ?? "This file").replace("{limit}", "32 MiB")
              : heic ? peppyCopy.rcs_media_refused_heic.replace("{name}", attachment?.name ?? "This file")
                : peppyCopy.rcs_media_refused_type.replace("{name}", attachment?.name ?? "This file");
            setAttachmentViews((views) => Object.fromEntries(Object.entries(views).map(([id, view]) => [id, refused.has(id) ? { ...view, error: result.reason, state: "failed" } : view])));
            return setComposerNotice(current.key, `${copy} ${peppyCopy.rcs_composer_draft_kept}`, "error", "attachment-feedback");
          }
          const refusal = groupComposerNotice(result, current.key);
          if (refusal) return setGroupNotice(refusal);
          return setComposerNotice(current.key,
            result.reason ?? `${STATUS_LABEL[result.status]}. The draft was preserved.`,
          );
        }
        const postSendContent = store.get(current.key)?.content;
        store.markSent(current.key, sentGeneration, result.revision, postSendContent);
        inFlightSend.resolve("sent");
        setNewComposeKey(null);
        if (result.reason === "create_not_rcs") setGroupNotice({ kind: "create-not-rcs", key: store.resolve(current.key) });
        if (detachedHistoryRef.current?.conversationId === current.conversationId) clearDetached();
      } catch (error) {
        if (!mountedRef.current || session !== currentSearchSessionKey() || !store.get(current.key)) return;
        const staleSend = errorCode(error) === "draft-moved" || errorCode(error) === "stale-draft";
        if (staleSend) {
          const details = typeof error === "object" && error !== null && "details" in error
            ? (error as { details?: { conversationId?: unknown; draftId?: unknown } }).details
            : undefined;
          const destinationId = typeof details?.conversationId === "string" ? details.conversationId : current.conversationId;
          const expectedDraftId = typeof details?.draftId === "string" ? details.draftId : current.id;
          const verificationSequence = loadSequence.current;
          const movedToAnotherConversation = destinationId !== current.conversationId;
          try {
            const state = await bridge.load_state(destinationId);
            if (!mountedRef.current || session !== currentSearchSessionKey() || !store.get(current.key)) return;
            if (verificationSequence !== loadSequence.current) {
              await restoreUnsent();
              draftNoticeSequence.current += 1;
              const retryNotice = { key: current.key, event: "save-failed" as const, sequence: draftNoticeSequence.current, persistent: true };
              draftNoticesRef.current.set(current.key, retryNotice);
              setDraftNotice(retryNotice);
              return;
            }
            const verifiedHost = state.activeConversationId === destinationId
              && state.encryption.state === "unlocked"
              && createSearchSessionKey(state, false) === session
              && state.draft?.id === expectedDraftId
              && state.draft.conversationId === destinationId
              ? state.draft
              : undefined;
            if (!verifiedHost) {
              await restoreUnsent();
              draftNoticeSequence.current += 1;
              setDraftNotice({ key: current.key, event: "save-failed", sequence: draftNoticeSequence.current, persistent: true });
              return;
            }
            const nextContent = store.get(current.key)?.content ?? blankContent();
            const outcome = store.reconcileForReview(current.key, verifiedHost, [sendContent, nextContent]);
            if (!outcome.ok) {
              const reviewKey = outcome.key ?? store.resolve(current.key);
              if (outcome.reviewRequired) {
                draftNoticeSequence.current += 1;
                reviewNoticeSequenceRef.current = draftNoticeSequence.current;
                setDraftNotice({
                  key: reviewKey,
                  event: movedToAnotherConversation ? "moved" : "stale-send",
                  sequence: draftNoticeSequence.current,
                  persistent: true,
                });
                return;
              }
              return setComposerNotice(reviewKey, `Not sent: ${outcome.error}`);
            }
            return;
          } catch (lookupError) {
            if (!mountedRef.current || session !== currentSearchSessionKey() || !store.get(current.key)) return;
            await restoreUnsent();
            draftNoticeSequence.current += 1;
            setDraftNotice({ key: current.key, event: "save-failed", sequence: draftNoticeSequence.current, persistent: true });
            return;
          }
        }
        await restoreUnsent();
        if (errorCode(error) === "update-in-progress") {
          void refresh().catch(reportRefresh(""));
          return setComposerNotice(current.key, UPDATE_SEND_FAILURE, "warning");
        }
        return setComposerNotice(current.key, `Send failed: ${errorText(error)}`);
      }
      await refresh(current.conversationId).catch(reportRefresh(""));
    } finally {
      inFlightSend.resolve("unsent");
      if (inFlightSendRef.current === inFlightSend) inFlightSendRef.current = null;
      if (pendingSendId) setPendingSends((sends) => sends.filter((pending) => pending.id !== pendingSendId));
      sendingRef.current = false;
      setSending(false);
    }
  };

  const publish = async (attachment: AttachmentView) => {
    try {
      const copy = await bridge.publish_attachment(attachment.id);
      if (!copy)
        return setNotice("Public link not created; nothing was shared.", "warning");
      setPublicCopies((current) => ({ ...current, [attachment.id]: copy }));
    } catch (error) {
      setNotice(`Public link: ${errorText(error)}`);
    }
  };
  const retryAttachment = async (attachment: AttachmentView) => {
    try {
      await bridge.retry_attachment(attachment.id);
      setNotice(`Retrying ${attachment.transfer ?? "attachment"} transfer.`, "info");
    } catch (error) {
      setNotice(`Attachment retry: ${errorText(error)}`);
    }
  };
  const saveAttachment = async (attachment: AttachmentView) => {
    try {
      if (!await bridge.save_attachment(attachment.id))
        setNotice("Attachment save cancelled.", "info");
    } catch (error) {
      if (errorCode(error) === "media-pending") {
        pendingSaveIdsRef.current.add(attachment.id);
        setNotice(<>Downloading <IsolatedText text={attachment.name} /> before saving.</>, "info");
      } else setNotice(`Attachment save: ${errorText(error)}`);
    }
  };
  const saveGalleryAttachment = async (attachment: AttachmentView): Promise<GallerySaveOutcome> => {
    try {
      if (!await bridge.save_attachment(attachment.id)) return "cancelled";
      return "saved";
    } catch (error) {
      if (errorCode(error) === "media-pending") {
        pendingSaveIdsRef.current.add(attachment.id);
        return "pending";
      }
      throw error;
    }
  };
  const publishGalleryAttachment = async (attachment: AttachmentView): Promise<PublicCopy | null> => {
    const copy = await bridge.publish_attachment(attachment.id);
    if (copy) setPublicCopies((current) => ({ ...current, [attachment.id]: copy }));
    return copy;
  };

  /** Closing never discards an unsaved draft: a failed save keeps the window open. */
  const flushForClose = async (): Promise<CloseOutcome> => {
    const inFlightSend = inFlightSendRef.current;
    if (inFlightSend) {
      const outcome = await waitForSend(inFlightSend.settled, CLOSE_SEND_WAIT_MS);
      if (outcome !== "sent") return { ok: false, reason: outcome === "unsent" ? "send-unsent" : "still-sending" };
    }
    const reviewedKeys = new Set<string>();
    for (;;) {
      const outcome = await store.flushAll();
      if (!outcome.ok && outcome.key && store.get(store.resolve(outcome.key))?.held) return { ok: false, reason: "still-sending" };
      if (outcome.ok || !outcome.reviewRequired || !outcome.key || reviewedKeys.has(outcome.key)) return outcome;
      // The store reports reviewRequired only for a clean, durably adopted host row.
      reviewedKeys.add(outcome.key);
      store.acknowledgeReview(outcome.key);
    }
  };
  const showFlushFailure = (outcome: Extract<CloseOutcome, { ok: false }>, prefix: string, sendNotices: { "still-sending": string; "send-unsent"?: string } = CLOSE_SEND_NOTICES) => {
    if ("reason" in outcome) {
      const notice = sendNotices[outcome.reason];
      if (notice) setNotice(notice, "warning");
      return;
    }
    if (outcome.key) {
      setActiveView("conversations");
      if (outcome.key !== selectedRef.current) choose(outcome.key);
    }
    if (outcome.key) {
      draftNoticeSequence.current += 1;
      const notice = { key: outcome.key, event: "save-failed" as const, sequence: draftNoticeSequence.current, persistent: true };
      draftNoticesRef.current.set(outcome.key, notice);
      setDraftNotice(notice);
    }
    setNotice(`${prefix} (${outcome.error}).`);
  };
  const closeAfterSave = async (close: () => Promise<void>) => {
    if (closingRef.current) return;
    closingRef.current = true;
    try {
      const outcome = await flushForClose();
      if (!outcome.ok)
        return showFlushFailure(outcome, "The window stayed open because the draft was not saved");
      await close();
    } catch (error) {
      setNotice(errorText(error));
    } finally {
      closingRef.current = false;
    }
  };
  handlers.current.close = () =>
    void closeAfterSave(() => bridge.close_composer());
  handlers.current.collapse = () =>
    void closeAfterSave(() => bridge.close_composer());
  handlers.current.closeHead = () => {
    if (closingRef.current) return;
    closingRef.current = true;
    setClosingHead(true);
    void flushForClose().then(async outcome => {
      if (!outcome.ok) {
        showFlushFailure(outcome, "The window stayed open because the draft was not saved");
        return;
      }
      await bridge.close_head_panel();
    }).catch(error => {
      setNotice(errorText(error));
    }).finally(() => {
      closingRef.current = false;
      setClosingHead(false);
    });
  };

  useEffect(() => bridge.subscribe_lifecycle(request => {
    setLifecyclePending(request.id);
    void flushForClose().then(async outcome => {
      if (!outcome.ok) {
        showFlushFailure(outcome, "The window stayed open because a draft was not saved");
        await bridge.acknowledge_lifecycle(request.id, false);
        return;
      }
      await bridge.acknowledge_lifecycle(request.id, true);
    }).catch(async error => {
      setNotice(errorText(error));
      await bridge.acknowledge_lifecycle(request.id, false).catch(() => {});
    });
  }), [store]);

  useEffect(() => bridge.subscribe_lifecycle_finished(result => {
    setLifecyclePending(current => current === result.id ? null : current);
  }), []);

  const popoutConversation = async (conversationId: string) => {
    const outcome = await store.flush(conversationId);
    if (!outcome.ok) return setNotice(`Not opened: the draft could not be saved (${outcome.error}).`);
    const resolvedId = store.resolve(conversationId);
    if (isLocalDraftKey(resolvedId)) return setNotice("Not opened: the draft has not been stored yet.");
    try {
      if (popoutMode === "window") {
        await bridge.open_composer(resolvedId);
      } else {
        const result = await bridge.popout_conversation(resolvedId);
        if (result.warning) setNotice(result.warning);
      }
    } catch (error) {
      setNotice(`${popoutMode === "window" ? "Conversation window" : "Floating conversation"}: ${errorText(error)}`);
    }
  };
  const toggleConversationPin = async (conversationId: string, pinned: boolean) => {
    if (pendingPinId || isWritesPaused(snapshotRef.current) || snapshotRef.current?.encryption.state !== "unlocked" || appLockRef.current?.locked) return;
    const origin = document.activeElement;
    if (origin instanceof HTMLElement && origin.dataset.pinConversationId === conversationId) {
      pinFocusRestoreRef.current?.dispose();
      const intent = watchFocusIntent(origin);
      if (intent) pinFocusRestoreRef.current = Object.assign(intent, { conversationId });
    }
    setPendingPinId(conversationId);
    try {
      try {
        await bridge.set_conversation_pinned(conversationId, pinned);
      } catch (error) {
        const paused = errorCode(error) === "update-in-progress";
        setNotice(`Conversation pin not changed: ${errorText(error)}`, paused ? "warning" : "error");
        if (paused) void refresh().catch(reportRefresh(""));
        return;
      }
      try {
        await refresh();
      } catch (error) {
        setNotice(`${pinned ? "Pinned" : "Unpinned"}, but the list could not refresh: ${errorText(error)}`);
      }
    } finally {
      if (mountedRef.current) setPendingPinId(null);
    }
  };
  const conversationFolderName = (conversationId: string) => {
    const conversation = snapshotRef.current?.conversations.find((item) => item.id === conversationId);
    return conversation
      ? displayConversation(conversation.name, conversation.participants, resolution, conversation.id, conversation.recipientlessDraft).name
      : "conversation";
  };
  const focusAfterFolderChange = (conversationId: string, fallbackConversationId?: string, trigger?: HTMLButtonElement) => {
    const activeElement = document.activeElement;
    const confirmation = document.getElementById("conversation-folder-confirmation");
    const notice = document.getElementById("conversation-folder-notice");
    const senderConfirmation = document.getElementById("sender-unblock-confirmation");
    const senderNotice = document.getElementById("sender-unblock-notice");
    if (activeElement !== document.body && !confirmation?.contains(activeElement) && !notice?.contains(activeElement) && !senderConfirmation?.contains(activeElement) && !senderNotice?.contains(activeElement) && !trigger?.contains(activeElement)) return;
    if (trigger?.isConnected && !trigger.disabled) {
      trigger.focus();
      return;
    }
    const buttons = [...document.querySelectorAll<HTMLButtonElement>("[data-conversation-id]")];
    const target = buttons.find((button) => button.dataset.conversationId === conversationId);
    const fallback = buttons.find((button) => button.dataset.conversationId === fallbackConversationId);
    if (target && !target.disabled) target.focus();
    else if (fallback && !fallback.disabled) fallback.focus();
    else if (buttons[0] && !buttons[0].disabled) buttons[0].focus();
    else document.getElementById("conversation-folder-trigger")?.focus();
  };
  const restoreFolderFocus = (conversationId: string, fallbackConversationId?: string, trigger?: HTMLButtonElement) => new Promise<void>((resolve) => {
    requestAnimationFrame(() => {
      focusAfterFolderChange(conversationId, fallbackConversationId, trigger);
      resolve();
    });
  });
  const startFolderSettle = () => {
    folderSettlingRef.current = true;
    clearTimeout(folderSettleTimerRef.current);
    folderSettleTimerRef.current = setTimeout(() => {
      folderSettlingRef.current = false;
    }, FOLDER_SETTLE_MS);
  };
  const applyFolderChange = async (conversationId: string, from: ConversationFolder, to: ConversationFolder, trigger?: HTMLButtonElement) => {
    if (folderChangeInFlightRef.current || isWritesPaused(snapshotRef.current) || snapshotRef.current?.encryption.state !== "unlocked" || appLockRef.current?.locked) return;
    folderChangeInFlightRef.current = true;
    const name = conversationFolderName(conversationId);
    const leavesCurrentView = from === folderView && to !== folderView;
    const movedIndex = conversations.findIndex((conversation) => conversation.id === conversationId);
    const fallbackConversationId = leavesCurrentView
      ? conversations[movedIndex + 1]?.id ?? conversations[movedIndex - 1]?.id
      : undefined;
    setPendingFolderId(conversationId);
    setPendingFolder(to);
    // The notice appears only once the change settles, so its Undo or Retry is never inert.
    let nextNotice: FolderNotice | undefined;
    try {
      try {
        await bridge.set_conversation_folder(conversationId, to);
      } catch (error) {
        if (errorCode(error) === "update-in-progress") {
          reportAction("Conversation folder not changed: ")(error);
          return;
        }
        nextNotice = { kind: "error", conversationId, name, from, to };
        await restoreFolderFocus(conversationId, fallbackConversationId, trigger);
        return;
      }
      try {
        await refresh();
      } catch {
        // The folder change succeeded, so retain its Undo notice when the snapshot refresh fails.
      }
      nextNotice = { kind: "success", conversationId, name, from, to };
      if (leavesCurrentView && selectedRef.current === conversationId) choose("");
      await restoreFolderFocus(conversationId, fallbackConversationId, leavesCurrentView ? undefined : trigger);
    } finally {
      if (mountedRef.current) {
        setPendingFolderId(null);
        setPendingFolder(undefined);
        setFolderConfirmation(undefined);
        if (nextNotice) {
          setSenderUnblockNotice(undefined);
          setFolderNotice(nextNotice);
        }
        startFolderSettle();
      }
      folderChangeInFlightRef.current = false;
    }
  };
  const requestFolderChange = (conversationId: string, to: ConversationFolder, trigger: HTMLButtonElement) => {
    if (folderChangeInFlightRef.current || isWritesPaused(snapshotRef.current) || snapshotRef.current?.encryption.state !== "unlocked" || appLockRef.current?.locked) return;
    const conversation = snapshotRef.current?.conversations.find((item) => item.id === conversationId);
    const from = conversation?.folder ?? "inbox";
    if (to === "inbox" && from === "spam" && conversation?.folderReason === "blocked") {
      setSenderUnblockConfirmation({ id: conversationId, name: conversationFolderName(conversationId), canUnblock: Boolean(conversation.canUnblock), trigger });
      return;
    }
    if (folderSettlingRef.current) return;
    if (to === "inbox") {
      void applyFolderChange(conversationId, from, to, trigger);
      return;
    }
    if (folderConfirmation?.id === conversationId && folderConfirmation.to === to) {
      setFolderConfirmation(undefined);
      return;
    }
    setFolderConfirmation({ id: conversationId, name: conversationFolderName(conversationId), from, to, trigger });
  };
  const requestSenderUnblock = async (conversationId: string, name: string, trigger?: HTMLButtonElement) => {
    if (pendingSenderUnblockId || isWritesPaused(snapshotRef.current) || snapshotRef.current?.encryption.state !== "unlocked" || appLockRef.current?.locked) return;
    setPendingSenderUnblockId(conversationId);
    try {
      await bridge.request_sender_unblock(conversationId);
      try {
        await refresh();
      } catch {
        // The unblock request succeeded, so retain its success notice when refresh fails.
      }
      setSenderUnblockConfirmation(undefined);
      setFolderNotice(undefined);
      setSenderUnblockNotice({ kind: "success", id: conversationId, name });
      // Keep the selected conversation open so its pending unblock notice remains visible.
      await restoreFolderFocus(conversationId, conversations.find((conversation) => conversation.id !== conversationId)?.id, trigger);
    } catch (error) {
      if (errorCode(error) === "update-in-progress") {
        reportAction("Unblock not requested: ")(error);
        return;
      }
      setSenderUnblockConfirmation(undefined);
      setFolderNotice(undefined);
      setSenderUnblockNotice({ kind: "error", id: conversationId, name });
    } finally {
      if (mountedRef.current) setPendingSenderUnblockId(null);
    }
  };
  useEffect(() => () => clearTimeout(folderSettleTimerRef.current), []);
  useEffect(() => {
    if (!folderConfirmation) return;
    const conversation = snapshot?.conversations.find((item) => item.id === folderConfirmation.id);
    if (appLock?.locked || snapshot?.encryption.state !== "unlocked" || !folderConfirmation.trigger.isConnected || !conversation || (conversation.folder ?? "inbox") !== folderConfirmation.from)
      setFolderConfirmation(undefined);
  }, [appLock?.locked, folderConfirmation, snapshot]);
  useEffect(() => {
    if (!senderUnblockConfirmation) return;
    const conversation = snapshot?.conversations.find((item) => item.id === senderUnblockConfirmation.id);
    if (appLock?.locked || snapshot?.encryption.state !== "unlocked" || !senderUnblockConfirmation.trigger.isConnected || conversation?.folderReason !== "blocked")
      setSenderUnblockConfirmation(undefined);
  }, [appLock?.locked, senderUnblockConfirmation, snapshot]);

  const runSetupAction = async (
    action: "origin" | "unlock",
    onConfigured?: () => void,
  ) => {
    if (action === "origin") {
      await bridge.configure_server(origin);
      onConfigured?.();
    }
    else await bridge.unlock_sync();
    await refresh();
  };
  const lock = bridge.lock_sync?.bind(bridge);
  const lockSync = lock ? async () => {
    const saved = await flushForClose();
    if (!saved.ok) {
      showFlushFailure(saved, "Not locked: the draft could not be saved", { "still-sending": "Not locked: your message is still sending. Try again once it finishes.", "send-unsent": "Not locked: your message was not sent." });
      return;
    }
    try {
      historySequence.current += 1;
      historyLoadingRef.current = false;
      historyAnchorRef.current = null;
      messageListScrollTopRef.current = 0;
      hasOlderMessagesRef.current = false;
      setHistoryLoading(false);
      setHistoryLoadError(null);
      setHasOlderMessages(false);
      await lock();
      await refresh();
    } catch (error) {
      if (errorCode(error) === "recovery-clear-failed") {
        setNotice("Sync is locked, but this browser may still remember your Vault key phrase.");
        await refresh();
      } else setNotice(`Could not lock sync: ${errorText(error)}`);
    }
  } : undefined;

  useEffect(() => {
    if (!composerConversation) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !event.isComposing)
        handlers.current.collapse();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const resolution = useMemo<ContactResolutionMap>(() => ({ ...chosenContacts, ...(snapshot?.contactResolution ?? {}) }), [snapshot?.contactResolution, chosenContacts]);
  const typingLabel = groupTypingLabel(typing.participants, (address) => displayAddressName(address, resolution));
  const conversationToneSeed = active ? conversationAvatar({ id: active.id, name: active.name, participants: active.participants, recipientless: active.recipientlessDraft }).seed : "";
  const typingRow = useMemo<TypingRowData | undefined>(() => {
    const detached = detachedHistory?.conversationId === active?.id;
    const hasVisualMessages = Boolean(active?.messages.some(message => message.sendCancellation?.state !== "confirmed") || visiblePendingSendRows.length);
    if (!active || !conversationLoaded || !typing.visible || !hasVisualMessages || detached || groupLeft) return undefined;
    if (isGroup) return {
      scope: "group",
      label: typingLabel,
      typers: typing.participants.map((address) => ({
        address,
        name: resolution[address]?.displayName,
        photoDataUrl: resolution[address]?.photoDataUrl,
        toneSeed: incomingToneSeed(conversationToneSeed, address),
      })),
    };
    const address = latestIncomingSender ?? "";
    return {
      scope: "direct",
      label: peppyCopy.typing_indicator_direct,
      typers: [{ address, name: resolution[address]?.displayName, photoDataUrl: resolution[address]?.photoDataUrl, toneSeed: incomingToneSeed(conversationToneSeed, address) }],
    };
  }, [active, conversationLoaded, conversationToneSeed, detachedHistory, groupLeft, isGroup, latestIncomingSender, resolution, typing, typingLabel, visiblePendingSendRows]);
  const typingRowsKey = typingRow ? `${typingRow.scope}:${typingRow.typers.map(({ address }) => address).join(",")}` : "";
  useLayoutEffect(() => {
    if (nearBottom.current && !searchReadGate.current && !detachedHistoryRef.current) stickToBottom();
  }, [messageIds, trailingRowsKey, typingRowsKey]);
  const conversationRows = useMemo<Conversation[]>(() => {
    const combined = [
      ...store.localDrafts().map((local) => {
      const recipientIds = local.content.recipientIds;
      return {
        id: local.key,
        name: recipientIds.map((id) => displayAddressName(id, resolution)).join(", ") || "New message",
        avatar: avatarFor(
          conversationAvatar({ id: local.key, name: "", participants: recipientIds, recipientless: recipientIds.length === 0 }),
          recipientIds,
          resolution,
        ),
        preview: "Not saved yet",
        unread: 0,
        draftOnly: true,
        pinned: false,
        folder: "inbox" as const,
        recipientlessDraft: recipientIds.length === 0,
      };
    }),
    ...(snapshot?.conversations ?? []).map(({ id, name, preview, unread, participants, recipientlessDraft, draftOnly, pinned, folder, folderReason, unblock, canUnblock }) => ({
      id,
      ...displayConversation(name, participants, resolution, id, recipientlessDraft),
      preview,
      unread,
      recipientlessDraft,
      draftOnly,
      pinned,
      folder: folder ?? "inbox",
      folderReason,
      unblock,
      canUnblock,
    })),
    ];
    return [...combined.filter((conversation) => conversation.pinned && !conversation.draftOnly), ...combined.filter((conversation) => !conversation.pinned || conversation.draftOnly)];
  }, [draftsVersion, resolution, snapshot?.conversations, store]);
  const conversations = useMemo(() => conversationRows.filter((conversation) => (conversation.folder ?? "inbox") === folderView), [conversationRows, folderView]);
  const folderCounts = useMemo<Record<ConversationFolder, number>>(() => conversationRows.reduce<Record<ConversationFolder, number>>((counts, conversation) => {
    counts[conversation.folder ?? "inbox"] += 1;
    return counts;
  }, { inbox: 0, archived: 0, spam: 0 }), [conversationRows]);
  const folderUnread = useMemo<Record<ConversationFolder, number>>(() => (snapshot?.conversations ?? []).reduce<Record<ConversationFolder, number>>((counts, conversation) => {
    counts[conversation.folder ?? "inbox"] += conversation.unread;
    return counts;
  }, { inbox: 0, archived: 0, spam: 0 }), [snapshot?.conversations]);
  const searchLocal = useCallback(async (request: LocalSearchRequest) => {
    const page = await bridge.search_local(request);
    return {
      ...page,
      items: page.items.map((hit) => {
        if (hit.kind !== "message") return hit;
        const conversation = snapshotRef.current?.conversations.find(({ id }) => id === hit.conversationId);
        if (!conversation) return hit;
        const context = displayConversation(
          conversation.name,
          conversation.participants,
          resolution,
          conversation.id,
          conversation.recipientlessDraft,
        ).name;
        if (hit.title === "Message") return { ...hit, title: context };
        if (hit.title === context || hit.title === conversation.name) return hit;
        return { ...hit, title: `${context} · ${hit.title}` };
      }),
    };
  }, [resolution]);
  const notificationDisplayTitle = useCallback((title: string) => resolution[title]?.displayName ?? title, [resolution]);
  const openNotificationSearch = async (target: NotificationActivationTarget | undefined, query = "") => {
    const navigation = await flushBeforeNavigation("Search result not opened");
    if (!navigation || !navigation.isCurrent()) return;
    flushSync(() => {
      setContactTarget(undefined);
      setNotificationAppFilter(null);
      resetNotificationSearch();
      setNotificationQuery(query);
      setActiveViewNow("notifications");
      if (target) setNotificationFocus({ requestId: `omni-${++notificationSearchRequestId.current}`, target });
    });
  };
  const navigateNotificationSearch = (target: NotificationActivationTarget | undefined, query?: string) => {
    notificationNavigationEpoch.current += 1;
    const open = () => { void openNotificationSearch(target, query); };
    if (contactNavigation.current) contactNavigation.current(open);
    else open();
  };
  const notificationDismissals = useNotificationDismissals({
    bridge,
    notifications: snapshot?.notifications ?? EMPTY_NOTIFICATIONS,
    refresh: () => refresh(),
    writesPaused: isWritesPaused(snapshot),
    announce: announceNotification,
    onBulkFailure: notice => setNotificationActionNotice(notice && { kind: "error", ...notice }),
    label: row => ({ title: notificationDisplayTitle(row.title), appName: notificationAppName(row.appName, row.packageName) }),
    singleAnnouncement: row => {
      const sources = snapshot?.gateways ?? [];
      const id = row.target.sourceDeviceId;
      if (!shownDismissalBlocks(snapshot?.notificationDismissalBlocks ?? [], sources).some(block => block.sourceDeviceId === id)) return undefined;
      return blockedDismissAnnouncement(notificationDisplayTitle(row.title), notificationAppName(row.appName, row.packageName), phoneNameOf(id, sources, [id]));
    },
  });
  const hiddenNotificationKeys = notificationDismissals.hiddenKeys;
  const notificationSearchRows = useMemo(
    () => (snapshot?.notifications ?? []).filter(notification => !hiddenNotificationKeys.has(notificationKey(notification.target))),
    [snapshot?.notifications, hiddenNotificationKeys],
  );
  const {
    notificationQuery,
    setNotificationQuery,
    notificationResults,
    resetSearch: resetNotificationSearch,
    scope: notificationSearchScope,
  } = useNotificationSearch({
    active: notificationsOpen,
    locked: notificationsLocked || Boolean(appLock?.locked),
    notifications: notificationSearchRows,
    filters: snapshot?.appFilters ?? [],
    sources: snapshot?.gateways ?? [],
    displayTitle: notificationDisplayTitle,
    onOpenHit: target => navigateNotificationSearch(target),
    onShowAll: query => navigateNotificationSearch(undefined, query),
    onUnavailable: () => {
      setNotificationActionNotice({ kind: "info", message: "This notification is no longer available." });
      navigateNotificationSearch(undefined);
    },
  });
  // Dismiss all: up to 100 visible, dismissible, not-hidden rows, newest first. Same set the toolbar counts.
  const dismissibleNotificationRows = dismissibleNotifications(snapshot?.notifications ?? [], snapshot?.appFilters ?? [], notificationsLocked, hiddenNotificationKeys);
  const dismissibleNotificationsCount = dismissibleNotificationRows.length;
  const dismissAllNotifications = () => {
    if (!notificationDismissals.dismiss(newestDismissTargets(dismissibleNotificationRows), { single: false })) return;
    setNotificationAppFilter(null);
    setNotificationQuery("");
  };
  const { query: contactQuery, setQuery: setContactQuery, scope: contactSearchScope, resetSearch: resetContactSearch } = useScopedSearch({
    active: contactsOpen,
    locked: !snapshot || snapshot.encryption.state === "locked" || Boolean(appLock?.locked),
    label: "Contacts",
    placeholder: "Search contacts",
  });
  const { scope: conversationSearchScope, resetSearch: resetConversationSearch } = useScopedSearch({
    active: !revoked && activeView === "conversations",
    locked: !snapshot || snapshot.encryption.state === "locked" || Boolean(appLock?.locked),
    label: "Conversations",
    placeholder: "Search conversations and messages",
    mode: "results",
  });
  // Only one tab is active, so at most one scope exists.
  const titleSearchScope = notificationSearchScope ?? contactSearchScope ?? conversationSearchScope;

  const accountIdentityKey = snapshot ? `${snapshot.connection.origin ?? ""}|${snapshot.sessionScopeId ?? ""}` : null;
  const accountNotice = useAccountNotice(accountIdentityKey);
  const setupCode = snapshot?.connection.errorCode;
  const nativeCredentialed = snapshot?.mode === "native" && !["server-required", "credentials-required", "revoked"].includes(setupCode ?? "");
  const enrolled = Boolean(snapshot && (nativeCredentialed || (browserHost && snapshot.mode === "browser" && snapshot.encryption.state !== "preview")));
  const [ribbonOpenFailed, setRibbonOpenFailed] = useState(false);
  const [ribbonBilling, setRibbonBilling] = useState<(RibbonBillingFacts & { identityKey: string }) | null>(null);
  const accountNoticeKind = accountNotice.notice?.kind ?? null;

  /* A failed open belongs to the account that showed it. */
  useEffect(() => {
    setRibbonOpenFailed(false);
  }, [accountIdentityKey]);

  /* Native and enrolled only. Refetches when a notice appears or changes kind, so a failed read is not kept for the session. */
  useEffect(() => {
    if (browserHost || !enrolled || !accountIdentityKey) return;
    let active = true;
    const identityKey = accountIdentityKey;
    void Promise.allSettled([bridge.billing_summary(), bridge.purchase_options()]).then(([summary, options]) => {
      if (!active) return;
      setRibbonBilling({
        identityKey,
        provider: summary.status === "fulfilled" ? summary.value.provider : null,
        websitePurchase: options.status === "fulfilled" ? options.value.website : null,
      });
    });
    return () => { active = false; };
  }, [browserHost, enrolled, accountIdentityKey, accountNoticeKind]);
  /* Facts read for another account are unresolved, in the same render as the identity change. */
  const ribbonFacts = ribbonBilling?.identityKey === accountIdentityKey ? ribbonBilling : null;

  /* Locked snapshots carry no conversations, contacts, or notifications, so nothing else renders. */
  if (!browserHost && (lockCode === "app-locked" || lockCode === "secure-store-unavailable" || lockCode === "app-lock-checking"))
    return <>
      {mcpAuthorizationId && snapshot && <div id="mcp-authorization-waiting" className="app-notice-row" role="status" data-mcp-authorization-waiting="locked"><p>An AI client is waiting for owner approval. Unlock Peppy to continue.</p><button className="app-notice-dismiss" type="button" onClick={dismissMcpAuthorization}>Dismiss</button></div>}
      <AppLockScreen
        state={lockCode === "app-lock-checking" ? "checking" : lockCode === "secure-store-unavailable" ? "storage-unavailable" : "locked"}
        onUnlock={unlockApp}
        onRetry={retryAppLockCheck}
      />
    </>;

  if (loading && composerConversation)
    return <main ref={mainRef} id="composer-shell" data-platform={platform}><StartupScreen variant="composer" progress={launchProgress} /></main>;

  const canUnlock = snapshot?.mode === "native"
    ? snapshot.encryption.state !== "unlocked" && nativeCredentialed
    : browserHost && snapshot?.mode === "browser" && (snapshot.encryption.state === "locked" || snapshot.encryption.state === "mismatch");
  const canRecoverNativeSync = canUnlock || nativeCredentialed;
  const needsServerSetup = snapshot?.connection.errorCode === "server-required" || snapshot?.connection.errorCode === "credentials-required";
  const showSetup = Boolean(snapshot && (revoked || addingPhone || initialEnrollmentActive || (!selected && (needsServerSetup || (ownerWithoutPhone(snapshot) && phoneDeferredOrigin !== snapshot.connection.origin && !phoneSetupDeferred(snapshot.connection.origin)) || (browserHost && snapshot.mode === "browser" && snapshot.encryption.state === "preview")))));
  const narrowConversationListMode = narrowBrowserViewport && !showSetup && activeView === "conversations";
  const showingMobileConversationList = narrowConversationListMode && (!selected || mobileConversationListOpen);
  const hideThreadList = settingsOpen || notificationsOpen || contactsOpen || (narrowConversationListMode ? !showingMobileConversationList : listCollapsed);
  const threadListToastHost = hideThreadList ? undefined : threadListHost;
  const serverOffline = Boolean(snapshot && enrolled && isServerUnreachable(snapshot.connection));
  const accountNoticeShown = enrolled ? accountNotice.notice : null;
  const ribbonActionContext: RibbonActionContext = { browserHost, billing: ribbonFacts };
  const systemRibbon = resolveSystemRibbon(serverOffline, accountNoticeShown, Date.now(), ribbonActionContext);
  const accountReadOnly = accountNoticeShown?.severity === "error";
  /* Only the hosted browser app links to the account website, and only while the account can send. */
  const titlebarAccountUrl = browserHost && hostedDeployment && enrolled && !showSetup && accountNotice.accessLevel === "read_write" ? accountUrl : undefined;
  const openRibbonAction = () => {
    const opened = systemRibbon?.actionTarget === "appStore"
      ? bridge.open_store_subscriptions().then(() => true, () => false)
      : openAccountManageUrl(systemRibbon?.actionUrl ?? null, browserHost, bridge.open_external_url);
    void opened.then(success => setRibbonOpenFailed(!success));
  };
  const attachments: Attachment[] = content.attachmentIds.map((id) => {
    const attachment = attachmentViews[id] ?? { id, name: "Attached file", state: "pending" };
    if (!rcsRoute.current) return attachment;
    const mediaType = attachment.mediaType?.toLowerCase() ?? "";
    const heic = mediaType === "image/heic" || mediaType === "image/heif" || (!mediaType && /\.hei[cf]$/i.test(attachment.name));
    const block = heic
      ? { kind: "heic" as const, text: peppyCopy.rcs_media_chip_heic }
      : attachment.error === "not_allowed_mime"
        ? { kind: "type" as const, text: peppyCopy.rcs_media_chip_type }
      : attachment.byteSize > 32 * 1024 * 1024 || attachment.error === "too_large"
        ? { kind: "size" as const, text: peppyCopy.rcs_media_chip_size.replace("{limit}", "32 MiB") }
        : undefined;
    return { ...attachment, block };
  });
  const activeDraftView = slot?.id ? draftViews[slot.id] : undefined;
  const failedDraftAttachment = attachments.find((attachment) =>
    attachment.state === "failed" || ["upload-failed", "download-failed", "unavailable"].includes(attachment.draftState ?? ""),
  );
  const draftConflict = Boolean(activeDraftView?.sync?.conflicts.length);
  const syncedOffline = Boolean(serverOffline && activeDraftView?.sync?.sendRequiresConnection);
  const draftSendBlock = attachments.some((attachment) => attachment.block) ? peppyCopy.rcs_media_reason_blocked
    : draftConflict ? "resolve the draft conflict before sending"
    : failedDraftAttachment ? `image unavailable: ${failedDraftAttachment.name}`
    : syncedOffline ? "Reconnect to check this draft before sending."
    : undefined;
  const activeDisplay = active
    ? displayConversation(active.name, active.participants, resolution, active.id, active.recipientlessDraft)
    : undefined;
  const toneOverrides = snapshot?.toneOverrides ?? {};
  const saveTone = async (seed: string, paletteIndex: number | null) => {
    const current = snapshotRef.current;
    if (!mountedRef.current || !current || isWritesPaused(current) || current.encryption.state !== "unlocked" || appLockRef.current?.locked) return;
    if (toneOperationRef.current.has(seed)) return;
    const operation = ++toneOperationSequence.current;
    const session = currentSearchSessionKey();
    toneOperationRef.current.set(seed, operation);
    setPendingToneSeeds((pending) => ({ ...pending, [seed]: true }));
    setToneErrors((errors) => ({ ...errors, [seed]: "" }));
    try {
      await bridge.set_conversation_tone(seed, paletteIndex);
      try {
        const refreshed = await bridge.load_state(undefined, undefined, { metadataOnly: true });
        const responseSession = createSearchSessionKey(refreshed, Boolean(appLockRef.current?.locked));
        if (mountedRef.current && toneOperationRef.current.get(seed) === operation && session === currentSearchSessionKey() && responseSession === session) {
          const hasOverride = Object.hasOwn(refreshed.toneOverrides ?? {}, seed);
          setSnapshot((currentSnapshot) => {
            if (!currentSnapshot) return currentSnapshot;
            const next = { ...(currentSnapshot.toneOverrides ?? {}) };
            if (hasOverride) next[seed] = refreshed.toneOverrides![seed];
            else delete next[seed];
            return { ...currentSnapshot, toneOverrides: next };
          });
        }
      } catch {
        if (mountedRef.current && toneOperationRef.current.get(seed) === operation && session === currentSearchSessionKey()) setToneErrors((errors) => ({ ...errors, [seed]: "Color saved; waiting for the latest state." }));
      }
    } catch (error) {
      if (errorCode(error) === "update-in-progress") {
        reportAction("Color not changed: ")(error);
        return;
      }
      if (mountedRef.current && toneOperationRef.current.get(seed) === operation && session === currentSearchSessionKey()) setToneErrors((errors) => ({ ...errors, [seed]: "Couldn't save the color. Try again." }));
    } finally {
      if (toneOperationRef.current.get(seed) !== operation) return;
      toneOperationRef.current.delete(seed);
      setPendingToneSeeds((pending) => {
        if (!pending[seed]) return pending;
        const { [seed]: _, ...remaining } = pending;
        return remaining;
      });
    }
  };
  const actorName: ActorName = (actor) => {
    if (actor.sender === "self") return "You";
    if (!actor.senderAddress) return activeDisplay?.name ?? "Someone";
    if ((active?.participants?.length ?? 0) > 1) return senderLabel(actor.senderAddress, resolution).text;
    const contactName = resolution[actor.senderAddress]?.displayName;
    if (contactName) return contactName;
    const isSoleParticipant = active?.participants?.length === 1 && active.participants[0] === actor.senderAddress;
    return isSoleParticipant && activeDisplay?.name ? activeDisplay.name : actor.senderAddress;
  };
  const openSender = (address: string) => {
    const label = senderLabel(address, resolution);
    if ((!label.contact && resolution[address]) || snapshot?.contactNameConflicts?.includes(address)) {
      setNameChooser({ address, conversationId: active?.id ?? "", returnFocus: document.activeElement instanceof HTMLElement ? document.activeElement : null });
      return;
    }
    const requestId = ++contactTargetRequestId.current;
    const target: ContactTarget & { requestId: number } = label.contact
      ? { ...label.contact, requestId }
      : { kind: "create", phone: address, requestId };
    void openContactTarget(target, "Sender not opened");
  };
  const addConversationContactAddress = conversationLoaded
    ? conversationContactAddress({ conversation: active, resolution, books: snapshot?.contactBooks, conflicts: snapshot?.contactNameConflicts })
    : undefined;
  const chooserAddress = conversationLoaded && !active?.group && active?.participants?.length === 1 ? active.participants[0] : undefined;
  const chooserState = chooserAddress && (snapshot?.contactNameConflicts?.includes(chooserAddress) ? "conflict" : resolution[chooserAddress]?.nameBasis);
  const chooserOpen = nameChooser?.conversationId === active?.id;
  const closeNameChooser = () => {
    setNameChooser(undefined);
    nameChooser?.returnFocus?.focus();
  };
  const revealReactionTarget = (row: HTMLElement) => {
    reactionHighlightCleanupRef.current();
    let addedTabIndex = false;
    const clearHighlight = () => {
      window.clearTimeout(timer);
      row.removeAttribute("data-reaction-highlight");
      if (addedTabIndex) row.removeAttribute("tabindex");
      reactionHighlightCleanupRef.current = () => undefined;
    };
    row.scrollIntoView?.({ block: "center" });
    row.setAttribute("data-reaction-highlight", "true");
    if (!row.hasAttribute("tabindex")) {
      row.setAttribute("tabindex", "-1");
      addedTabIndex = true;
    }
    row.focus({ preventScroll: true });
    // Keep this duration aligned with the reaction-highlight-fade keyframes.
    const timer = window.setTimeout(clearHighlight, REACTION_HIGHLIGHT_MS);
    reactionHighlightCleanupRef.current = clearHighlight;
  };
  const openReactionTarget = async (reaction: MessageView, notice: ReactionNoticeInfo) => {
    const requestGeneration = ++reactionNavigationGeneration.current;
    setPendingReactionId(undefined);
    setReactionFeedback(undefined);
    const loadedTarget = notice.targetId ? messageRows.current.get(notice.targetId) : undefined;
    if (loadedTarget) return revealReactionTarget(loadedTarget);

    const conversationId = selectedRef.current;
    if (!conversationId) return;
    const navigationAtStart = navigationGeneration.current;
    const sessionAtStart = currentSearchSessionKey();
    const isCurrent = () => mountedRef.current
      && reactionNavigationGeneration.current === requestGeneration
      && navigationGeneration.current === navigationAtStart
      && currentSearchSessionKey() === sessionAtStart
      && selectedRef.current === conversationId
      && activeViewRef.current === "conversations";
    setPendingReactionId(reaction.id);
    try {
      const result = await locateReactionTarget(searchLocal, { conversationId, reaction, notice });
      if (!isCurrent()) return;
      if (result.state === "candidate") {
        const window = await bridge.load_message_window({ direction: "around", conversationId, messageId: result.hit.messageId });
        if (!isCurrent()) return;
        const target = window.conversationId === conversationId
          ? window.messages.find((message) => message.id === result.hit.messageId)
          : undefined;
        if (!target || !quoteMatches(normalizeReactionText(target.body), notice)) {
          setReactionFeedback({ reactionId: reaction.id, message: REACTION_NOT_FOUND });
          return;
        }
      }
      if (result.state === "found" || result.state === "candidate") activateSearchHit(result.hit);
      else setReactionFeedback({ reactionId: reaction.id, message: reactionUnavailableCopy(result) });
    } catch {
      if (isCurrent()) setReactionFeedback({ reactionId: reaction.id, message: REACTION_NOT_FOUND });
    } finally {
      if (reactionNavigationGeneration.current === requestGeneration) setPendingReactionId(undefined);
    }
  };
  const title = active?.group
    ? groupTitle({ name: active.group.name, memberNames: groupMembers(active).map((part) => displayAddressName(part, resolution)) })
    : activeDisplay?.name ??
      (isLocalDraftKey(slotKey)
        ? content.recipientIds.map((id) => displayAddressName(id, resolution)).join(", ") || "New message"
        : undefined);
  const participantsLine = active?.participants?.map((part) => displayAddressName(part, resolution)).join(", ");
  const groupBarState = groupNoticeState(active?.group);
  const groupRcsLinked = Boolean(content.gatewayId && snapshot?.rcsStatus?.gateways.some((gateway) => gateway.gatewayId === content.gatewayId && gateway.state === "linked" && gateway.current && gateway.healthy));
  /** Copies definitely unsent RCS content into the draft. Nothing is sent. */
  const restoreUnsentRcs = async (message: MessageView) => {
    if (!slotKey || !active) return;
    const unsentAttachments = message.attachments.filter((attachment) => attachment.rcsSendStatus === "not_sent" || attachment.rcsSendStatus === "members_changed");
    const partialRcs = message.attachments.some((attachment) => attachment.rcsSendStatus !== undefined);
    const captionCarrier = message.attachments.find((attachment) => rcsAttachmentIndex(message, attachment) === 0);
    const restoreCaption = Boolean(message.body) && (!partialRcs || Boolean(captionCarrier && unsentAttachments.includes(captionCarrier)));
    const draftKey = store.resolve(slotKey);
    const draft = store.get(draftKey);
    if (!draft) return;
    const recoveryId = `${message.id}\u0000${captionCarrier?.id ?? "body"}`;
    setAttachmentViews((views) => ({ ...views, ...Object.fromEntries(unsentAttachments.map((attachment) => [attachment.id, attachment])) }));
    await store.recoverRcsContent(draftKey, recoveryId, {
      caption: restoreCaption ? message.body : undefined,
      attachmentIds: unsentAttachments.map((attachment) => attachment.id),
    });
  };
  const restoreCancelledSend = (message: MessageView, sequence: number) => {
    if (!slotKey || !active || sequence <= appliedCancelledSendSequence.current) return;
    appliedCancelledSendSequence.current = sequence;
    const draftKey = store.resolve(slotKey);
    const before = store.get(draftKey);
    const generationBefore = before?.generation;
    const previous = before
      ? { text: before.content.text, attachmentIds: [...before.content.attachmentIds] }
      : { text: "", attachmentIds: [] };
    const { skippedAttachments, ...restored } = restorableContent(message);
    const restoredAttachments = message.attachments.filter((attachment) => restored.attachmentIds.includes(attachment.id));
    setAttachmentViews((views) => ({ ...views, ...Object.fromEntries(restoredAttachments.map((attachment) => [attachment.id, attachment])) }));
    void store.edit(draftKey, restored);
    const after = store.get(draftKey);
    if (!after || after.generation === generationBefore) return;
    setCancelledSendRestore({
      messageId: message.id,
      draftKey,
      sequence,
      generation: after.generation,
      restored,
      previous,
    });
    if (skippedAttachments > 0)
      setComposerNotice(draftKey, CANCELLED_ATTACHMENTS_SKIPPED_NOTICE, "warning", "attachment-feedback");
  };
  const editGroupResend = (message: MessageView) => {
    if (!slotKey || groupLeft || !active) return;
    void restoreUnsentRcs(message);
    setGroupNotice({ kind: "members-changed", key: slotKey, memberRevision: active.group?.memberRevision, members: active.group?.members });
  };
  // The unconfirmed create's body goes back into the composer; the user confirms with "Create it again" there.
  const createGroupAgainFromMessage = (message: MessageView) => {
    if (!slotKey || groupLeft || !active) return;
    edit({ text: [message.body, content.text].filter(Boolean).join("\n") });
    setGroupNotice({ kind: "create-unconfirmed", key: slotKey });
  };
  const composerGroupNotice = groupNotice && groupNotice.key === slotKey ? groupNotice : null;
  const maxAutoGrowHeight = paneHeight
    ? Math.max(
        TEXTAREA_LINE_HEIGHT,
        Math.min(
          TEXTAREA_MAX_HEIGHT,
          Math.floor(paneHeight * 0.5) - composerChrome,
        ),
      )
    : undefined;
  const mmsLimitShown =
    mmsRequired && route.kind === "route" && estimateBytes > 0;
  const mmsLimitBytes = route.kind === "route" ? route.gateway.mmsMaxBytes ?? DEFAULT_MMS_LIMIT_BYTES : 0;
  const mmsHeader = mmsLimitShown
    ? mmsPlan.kind === "split"
      ? { text: `Sends as ${mmsPlan.parts} messages`, tone: "default" as const }
      : route.kind === "route" && mmsPlan.kind !== "error" && (route.gateway.mmsContentVersion ?? 0) >= 4 && estimateBytes > mmsLimitBytes
        ? { text: fileSize(estimateBytes), tone: "default" as const }
        : mmsHeaderSize(estimateBytes, mmsLimitBytes)
    : undefined;
  const unavailableReason = effectivePendingRecipient ? pendingRecipientReason : groupLeft ? peppyCopy.group_composer_reason_left : draftSendBlock ?? problem ?? rcsBlockReason;
  const mainWindowRouteRecovery = headPanel && !effectivePendingRecipient && !settingUpGatewayFor(route, gateways) && (
    (route.kind === "none" && gateways.length > 0) || route.kind === "stale"
  );
  const composerUnavailableReason = mainWindowRouteRecovery
    ? `${unavailableReason} in the main window`
    : unavailableReason;
  const composerResizeHandle = (
    <ResizeHandle
      id="handle-h2"
      className="resize-handle resize-handle-vertical composer-resize-grip"
      direction="vertical"
      ariaLabel="Resize composer"
      value={composerHeight ?? composerMinimum()}
      min={composerMinimum()}
      max={composerMax()}
      valueUnit="pixels"
      onResize={resizeComposer}
      onResizeTo={resizeComposerTo}
      onResizeEnd={commitComposer}
      onDoubleClick={() => {
        composerHeightLive.current = null;
        persistedComposerHeight.current = null;
        setComposerHeight(null);
        if (headPanel) persistLayout({ headComposerHeight: null });
        else persistLayout({ composerHeight: null });
      }}
    />
  );
  const noticeCandidates: DraftNoticeCandidate[] = [];
  if (draftSubscriptionError) noticeCandidates.push({ kind: "save-failed", detail: `Draft updates unavailable: ${draftSubscriptionError}`, sequence: Number.MAX_SAFE_INTEGER });
  if (slotKey && !store.isHydrated(slotKey) && !isLocalDraftKey(slotKey)) noticeCandidates.push({ kind: "loading", sequence: 0 });
  if (slotKey && store.saveState(slotKey) === "saving") noticeCandidates.push({ kind: "saving", sequence: 1 });
  if (slot?.error) noticeCandidates.push({ kind: "save-failed", errorCode: slot.error.code, sequence: 2 });
  const retainedTextAvailable = Boolean(slotKey && store.retainedText(slotKey));
  if (draftNotice?.key === slotKey) {
    const undoAvailable = store.hasUndo(slotKey);
    if (draftNotice.event === "send-failed") noticeCandidates.push({ kind: "send-failed", detail: draftNotice.detail ?? "Draft updates are unavailable.", sequence: draftNotice.sequence });
    else if (draftNotice.event === "moved") noticeCandidates.push({ kind: "moved", detail: activeDisplay?.name ?? "this conversation", sequence: draftNotice.sequence, undoAvailable, retainedTextAvailable });
    else noticeCandidates.push({ kind: draftNotice.event, sequence: draftNotice.sequence, undoAvailable, retainedTextAvailable });
  } else if (retainedTextAvailable) {
    noticeCandidates.push({ kind: "remote-update", sequence: -1, retainedTextAvailable: true });
  }
  if (composerFeedback?.key === slotKey) noticeCandidates.push({ kind: composerFeedback.kind ?? "send-failed", detail: composerFeedback.message, sequence: draftNoticeSequence.current + 1 });
  const visibleDraftNotice = selectDraftNotice(noticeCandidates);
  const localSaveState = slotKey ? store.saveState(slotKey) : "idle";
  const hasDraftConflicts = Boolean(activeDraftView?.sync?.conflicts.length);
  const draftSyncLocalSaveState = visibleDraftNotice?.kind === "save-failed" ? "idle" : localSaveState;
  const groupStatusItems = groupComposerStatusItems({
    creating: sending && isNewConversation && content.recipientIds.length > 1,
    notice: composerGroupNotice?.kind === "members-changed"
      ? { kind: "members-changed", memberRevision: composerGroupNotice.memberRevision ?? active?.group?.memberRevision ?? "", members: composerGroupNotice.members }
      : composerGroupNotice ? { kind: composerGroupNotice.kind } : null,
    groupLeft,
    displayName: (address) => displayAddressName(address, resolution),
    canResend: active?.group?.state === "live" || active?.group?.state === "paused",
    checking: active?.group?.state === "paused",
    writesPaused: isWritesPaused(snapshot),
    onResend: () => void send({ expectedMemberRevision: composerGroupNotice?.kind === "members-changed" ? composerGroupNotice.memberRevision : undefined }),
    onReview: () => setGroupDetailsOpen(true),
    onCreateAgain: () => void send({ groupCreateConfirmDuplicate: true }),
    onStartNewMms: () => void startGroupMmsDraft(active ? groupMembers(active) : []),
  });
  const draftBlockKind = attachments.some((attachment) => attachment.block)
    ? "attachment" as const
    : draftConflict ? "conflict" as const
    : failedDraftAttachment ? "failed-attachment" as const
    : syncedOffline ? "offline" as const
    : undefined;
  const statusItems: InlineStatusItem[] = composerStatusItems({
    writesPaused: isWritesPaused(snapshot),
    showUpdateNotice: updateNoticeVisible,
    pendingRecipientReason: effectivePendingRecipient ? pendingRecipientReason : undefined,
    groupLeft,
    draftSendBlock,
    draftBlockKind,
    routeProblem: problem,
    routeProblemKind: settingUpGatewayFor(route, gateways) ? "setting-up" : undefined,
    rcsBlock,
    mainWindowRecovery: mainWindowRouteRecovery,
    groupItems: groupStatusItems,
    composerFeedback: composerFeedback?.key === slotKey ? composerFeedback : undefined,
  });
  const resolveDraftConflict = async (branchId: string | null): Promise<DraftResolutionResult> => {
    if (resolutionRef.current !== undefined) return { ok: false, message: "A version is already being applied." };
    if (!activeDraftView?.id || !slotKey)
      return { ok: false, message: "The draft changed. Review the latest versions before choosing." };
    const resolvedKey = store.resolve(slotKey);
    if (hasInFlightAttachmentPreparation(attachmentPreparationInFlight.current, resolvedKey, key => store.resolve(key))) {
      return { ok: false, message: "Wait for images to finish preparing before choosing a version." };
    }
    const operation = Date.now();
    resolutionRef.current = operation;
    setResolvingDraftConflict(true);
    const session = currentSearchSessionKey();
    const sessionGeneration = draftSessionGenerationRef.current;
    const before = store.get(resolvedKey);
    const draftId = activeDraftView.id;
    const revision = before?.revision;
    const generation = before?.generation;
    const flushed = await store.flush(resolvedKey);
    const current = store.get(resolvedKey);
    const valid = mountedRef.current && resolutionRef.current === operation
      && sessionGeneration === draftSessionGenerationRef.current && session === currentSearchSessionKey()
      && revision !== undefined && current?.id === draftId && current.generation === generation;
    if (!valid) {
      if (resolutionRef.current === operation) {
        resolutionRef.current = undefined;
        setResolvingDraftConflict(false);
      }
      return { ok: false, message: "The draft changed. Review the latest versions before choosing." };
    }
    if (!flushed.ok) {
      if (resolutionRef.current === operation) {
        resolutionRef.current = undefined;
        setResolvingDraftConflict(false);
      }
      return { ok: false, message: flushed.error };
    }
    try {
      const resolved = await bridge.resolve_draft_conflict({ draftId, keepBranchId: branchId });
      if (!mountedRef.current || resolutionRef.current !== operation || sessionGeneration !== draftSessionGenerationRef.current || session !== currentSearchSessionKey())
        return { ok: false, message: "The draft changed. Review the latest versions before choosing." };
      applyAuthoritativeDraft(resolved);
      if (store.resolve(selectedRef.current ?? "") === resolvedKey)
        document.getElementById("composer-textarea")?.focus();
      return { ok: true };
    } catch (error) {
      if (mountedRef.current && resolutionRef.current === operation && sessionGeneration === draftSessionGenerationRef.current && session === currentSearchSessionKey())
        return { ok: false, message: errorText(error) };
      return { ok: false, message: "The draft changed. Review the latest versions before choosing." };
    } finally {
      if (resolutionRef.current === operation) {
        resolutionRef.current = undefined;
        if (mountedRef.current && sessionGeneration === draftSessionGenerationRef.current) setResolvingDraftConflict(false);
      }
    }
  };
  const acknowledgeDraftSyncNotice = (restoreFocus = true) => {
    if (!activeDraftView?.id || !slotKey) return;
    const draftId = activeDraftView.id;
    const key = slotKey;
    const generation = draftSessionGenerationRef.current;
    const session = currentSearchSessionKey();
    void bridge.acknowledge_draft_notice(draftId)
      .then(() => {
        if (!mountedRef.current || generation !== draftSessionGenerationRef.current || session !== currentSearchSessionKey() || selectedRef.current !== key) return;
        setDraftViews((current) => current[draftId]
        ? { ...current, [draftId]: { ...current[draftId], sync: current[draftId].sync ? { ...current[draftId].sync!, notice: null } : undefined } }
        : current);
        acknowledgedDraftNoticeRef.current = undefined;
        if (restoreFocus) document.getElementById("composer-textarea")?.focus();
      })
      .catch((error) => {
        if (mountedRef.current && generation === draftSessionGenerationRef.current && session === currentSearchSessionKey() && selectedRef.current === key)
          setComposerNotice(key, `Couldn't dismiss draft notice: ${errorText(error)}`);
      });
  };
  const autoAcknowledgeDraftSyncNotice = () => {
    const draftId = activeDraftView?.id;
    const notice = activeDraftView?.sync?.notice;
    if (!draftId || !notice) {
      acknowledgedDraftNoticeRef.current = undefined;
      return;
    }
    const noticeKey = `${draftId}:${notice}`;
    if (acknowledgedDraftNoticeRef.current === noticeKey) return;
    acknowledgedDraftNoticeRef.current = noticeKey;
    acknowledgeDraftSyncNotice(false);
  };
  const onDraftNoticeAction = (action: "retry" | "save-new" | "discard-local" | "undo" | "copy") => {
    if (!slotKey || noticeActionBusyRef.current) return;
    noticeActionOwnedFocusRef.current = document.activeElement?.id === "draft-notice-action";
    noticeActionBusyRef.current = true;
    setNoticeActionBusy(true);
    const run = async () => {
      if (action === "retry") await retrySave();
      else if (action === "save-new") await store.saveAsNew(slotKey);
      else if (action === "discard-local" && store.discardLocalChanges(slotKey)) {
        draftNoticesRef.current.delete(slotKey);
        setDraftNotice(undefined);
        if (selectedRef.current === slotKey && !isLocalDraftKey(slotKey)) void refresh(slotKey).catch(reportRefresh(""));
      }
      else if (action === "undo") {
        const outcome = await store.undo(slotKey);
        if (outcome.ok) {
          draftNoticesRef.current.delete(slotKey);
          setDraftNotice(undefined);
        }
      } else {
        const text = store.retainedText(slotKey);
        if (!text) return;
        try {
          if (!navigator.clipboard?.writeText) throw new Error("Clipboard is unavailable.");
          await navigator.clipboard.writeText(text);
          store.clearUndo(slotKey);
          draftNoticesRef.current.delete(slotKey);
          setDraftNotice(undefined);
        } catch {
          setNotice("Could not copy retained text. It is still available to copy.", "warning");
        }
      }
    };
    void run().finally(() => {
      noticeActionBusyRef.current = false;
      if (mountedRef.current) setNoticeActionBusy(false);
      window.setTimeout(() => {
        if (!noticeActionOwnedFocusRef.current || document.getElementById("draft-notice-action")) return;
        noticeActionOwnedFocusRef.current = false;
        if (document.activeElement === document.body) document.getElementById("composer-textarea")?.focus();
      });
    });
  };
  const composer = (
            <section
      id="composer-area"
      aria-label="Composer"
      aria-busy={sending}
      data-user-sized={composerHeight !== null || undefined}
      style={composerHeight === null ? undefined : { height: Math.max(composerHeight, composerMinimum()) }}
    >
      {composerResizeHandle}
      <Composer
        draft={content.text}
        attachments={attachments}
        sendSupported={Boolean(slotKey) && draftHydrated && !draftSubscriptionError && !unavailableReason && !sending && !resolvingDraftConflict && !isWritesPaused(snapshot)}
        hideSmsCounter={rcsRoute.current || rcsBlock.kind !== "none"}
        typingSendState={typingSender.state}
        unavailableReason={isWritesPaused(snapshot) ? UPDATE_SEND_REASON : resolvingDraftConflict ? "resolving draft conflict…" : sending ? "sending…" : draftSubscriptionError ? "draft updates unavailable; retry setup" : composerReadOnly ? "draft is loading" : composerUnavailableReason}
        readOnly={composerReadOnly || resolvingDraftConflict || groupLeft}
        placeholder={composerReadOnly ? "Loading draft…" : undefined}
        onDraftChange={(text) => { typingSender.noteInput(text); edit({ text }); }}
        onSend={() => { typingSender.noteSend(); void send(); }}
        onAddAttachment={slotKey && !composerReadOnly && !resolvingDraftConflict ? () => void addAttachment() : undefined}
        onPasteImages={slotKey && !composerReadOnly && !resolvingDraftConflict ? files => void addPastedAttachments(files) : undefined}
        onRemoveAttachment={slotKey && !composerReadOnly && !resolvingDraftConflict ? (id) => edit({ attachmentIds: content.attachmentIds.filter((attachmentId) => attachmentId !== id) }) : undefined}
        onRetryAttachment={composerReadOnly || resolvingDraftConflict ? undefined : id => {
          const session = currentSearchSessionKey();
          const sessionGeneration = draftSessionGenerationRef.current;
          const key = slotKey;
          const previous = attachmentViews[id];
          const retryState = previous?.transfer === "download" || previous?.draftState === "download-failed" ? "downloading" : "uploading";
          setAttachmentViews(current => current[id] ? { ...current, [id]: { ...current[id], draftState: retryState } } : current);
          void bridge.retry_attachment(id).then(() => {
            if (mountedRef.current && key === selectedRef.current && sessionGeneration === draftSessionGenerationRef.current && session === currentSearchSessionKey()) void refresh();
          }).catch(error => {
            if (mountedRef.current && key === selectedRef.current && sessionGeneration === draftSessionGenerationRef.current && session === currentSearchSessionKey()) {
              setAttachmentViews(current => previous ? { ...current, [id]: previous } : current);
              setComposerNotice(store.resolve(slotKey ?? ""), `Couldn't retry attachment: ${errorText(error)}`, "error", "attachment-feedback");
            }
          });
        }}
        attachmentLayout={attachmentLayout}
        draftKey={slotKey}
        draftIdentity={composerDraftKey}
        attachmentHeaderDetail={rcsRoute.current ? <span>{peppyCopy.rcs_media_tray_limit.replace("{limit}", "32 MiB")}</span> : mmsHeader && route.kind === "route" ? <span className="mms-size-header-detail" data-mms-size-tone={mmsHeader.tone}>
          <span>{mmsHeader.text}</span>
          <MmsSizeHelp
            estimatedSize={fileSize(estimateBytes)}
            limitLabel={route.gateway.mmsLimitSource === "carrier" ? "Carrier limit" : "Default limit"}
            limitSize={fileSize(mmsLimitBytes)}
            limitKilobytes={Math.round(mmsLimitBytes / 1024)}
            recipientLimit={Math.min(route.gateway.mmsMaxRecipients ?? 20, 20)}
            resizingImages={mmsPlan.kind !== "error" && (route.gateway.mmsContentVersion ?? 0) >= 4 && estimateBytes > mmsLimitBytes}
          />
        </span> : undefined}
        maxAttachments={MAX_DRAFT_ATTACHMENTS}
        inlineStatusSlot={<>
          <DraftNotice notice={visibleDraftNotice} noticeKey={slotKey} onAction={onDraftNoticeAction} busy={sending || noticeActionBusy} />
          <DraftSyncStatus
            draft={activeDraftView}
            local={content}
            attachments={attachmentViews}
            offline={serverOffline}
            localSaveState={draftSyncLocalSaveState}
            onResolveConflict={resolveDraftConflict}
            onAcknowledgeNotice={acknowledgeDraftSyncNotice}
            onAutoAcknowledgeNotice={() => acknowledgeDraftSyncNotice(false)}
          />
        </>}
        statusItems={statusItems}
        statusLabels={{ more: (count) => peppyCopy.composer_status_more_sr.replace("{count}", String(count)) }}
        statusTrailingSlot={!browserHost && !composerConversation && notificationRecoveryBridge && (
            <NotificationReplyRecovery
              bridge={notificationRecoveryBridge}
              conversationId={active?.id}
              sessionKey={renderedSearchSessionKey}
              refreshSignal={snapshot}
              unlocked={snapshot?.encryption.state === "unlocked" && !appLock?.locked}
              draftText={content.text}
              onAppend={async body => {
                const session = renderedSearchSessionKey;
                if (!slotKey || deletingId === slotKey || resolvingDraftConflict || appLockRef.current?.locked || snapshotRef.current?.encryption.state !== "unlocked" || session !== currentSearchSessionKey()) throw new Error("Draft is unavailable.");
                const currentDraft = store.get(slotKey);
                const text = currentDraft?.content.text && body ? `${currentDraft.content.text}\n${body}` : `${currentDraft?.content.text ?? ""}${body}`;
                if (new TextEncoder().encode(text).byteLength > 64 * 1024) throw new DefinitivePreEditRefusal();
                autoAcknowledgeDraftSyncNotice();
                const outcome = await store.edit(slotKey, { text });
                if (!outcome.ok) throw new Error(outcome.error);
              }}
            />
        )}
        composerName={title}
        composerUserSized={composerHeight !== null}
        maxAutoGrowHeight={maxAutoGrowHeight}
        platform={platform}
        gatewaySlot={snapshot ? (headPanel ? (
          <HeadRoutePill content={content} gateways={gateways} accountReadOnly={accountReadOnly} rcsBlock={rcsBlock} />
        ) : (
          <GatewaySelector
          accountReadOnly={accountReadOnly}
            gateways={gateways}
            content={content}
            problem={unavailableReason}
            rcsBlock={rcsBlock}
            onSelect={(gateway) =>
              edit({ gatewayId: gateway.id, simId: gateway.simId })
            }
          />
        )) : null}
      />
    </section>
  );
  const recipientPanel = isNewConversation ? (
    <RecipientPanel
      key={newComposeKey ?? composerDraftKey}
      recipients={content.recipientIds.map((id) => ({
        id,
        label: resolution[id]?.displayName ?? id,
        avatarUrl: resolution[id]?.photoDataUrl,
        avatarSeed: seedKey(id),
        avatarName: resolution[id]?.displayName,
      }))}
      onCommit={commitRecipients}
      searchContacts={snapshot ? (query) => searchRecipientSuggestions(query, content.gatewayId) : undefined}
      onSuggestionChosen={rememberContact}
      position={recipientPosition}
      onPositionChange={changeRecipientPosition}
      onPendingChange={setPendingRecipient}
      bottomOffset={8}
      hint="Separate numbers with commas. SMS goes to one recipient; group messages need an MMS-capable route."
      nameField={<GroupNameField value={content.groupName ?? ""} onChange={(groupName) => edit({ groupName })} recipientCount={content.recipientIds.length} rcsLinked={groupRcsLinked} />}
    />
  ) : null;
  const recipientRail = recipientPanel ? (
    <div id="recipient-rail-layer">{recipientPanel}</div>
  ) : null;
  const toGalleryItem = (item: Awaited<ReturnType<typeof bridge.media_page>>["items"][number]): GalleryMediaPageItem => {
    const message = active?.messages.find((candidate) => candidate.id === item.messageId) ?? { id: item.messageId, revision: item.cursor.localOrder, sender: item.senderAddress ? "other" : "self", senderAddress: item.senderAddress, body: "", timestamp: item.timestamp, attachments: [] };
    const mediaMeta = item.attachment.mediaMeta ?? item.mediaMeta;
    return { attachment: { ...item.attachment, mediaMeta }, message, cursor: item.cursor };
  };
  const openGalleryItem = async (item: GalleryMediaPageItem) => {
    const conversationId = active?.id;
    if (!conversationId) return;
    if (composerConversation && bridge.open_media_viewer) {
      try {
        await bridge.open_media_viewer(conversationId, item.attachment.id, item.message.id);
      } catch {
        setNotice("Could not open media viewer.");
      }
      return;
    }
    const session = gallerySessionRef.current;
    const isCurrent = () => gallerySessionRef.current === session && sameGalleryConversation(galleryConversationRef.current, conversationId);
    try {
      const page = await bridge.media_page_around(conversationId, item.attachment.id, item.message.id, 40);
      if (!isCurrent()) return;
      const uniqueItems = (candidates: GalleryMediaPageItem[]) => [...new Map(candidates.map(candidate => [`${candidate.message.id}:${candidate.attachment.id}`, candidate])).values()];
      const items = uniqueItems([...page.newer.map(toGalleryItem).reverse(), toGalleryItem(page.target), ...page.older.map(toGalleryItem)]);
      if (!items.some(candidate => candidate.attachment.id === item.attachment.id && candidate.message.id === item.message.id)) return;
      galleryCacheRef.current = { conversationId, items, newerCursor: page.newerCursor, olderCursor: page.olderCursor };
      setLightboxItems(items);
      setLightboxHasNextPage(Boolean(page.olderCursor));
      setLightboxHasPreviousPage(Boolean(page.newerCursor));
      setLightboxAttachmentId(item.attachment.id);
      setLightboxMessageId(item.message.id);
    } catch {
      if (isCurrent()) setNotice("Could not load conversation media.");
    }
  };
  const goToLatest = () => { if (active?.id) void loadLatest(active.id).catch(reportAction("Could not load the latest messages: ")); };
  const messages = (
    <MediaActionsProvider publicCopies={publicCopies} onSave={saveGalleryAttachment} onPublish={publishGalleryAttachment} onRetry={(id) => bridge.retry_attachment(id)} requestDownload={(id) => bridge.request_attachment_download(id)} requestMediaSync={bridge.media_sync_request ? attachmentIds => bridge.media_sync_request!(attachmentIds) : undefined} loadOriginal={requestOriginal} releaseMedia={releaseMedia} canCopyImage={browserHost && clipboardImageSupported()} copyNativeImage={copyNativeImage} hydrateMediaPreview={hydrateMediaPreview}>
      <RcsMediaActionsProvider retryBlock={rcsRetryBlock} retryDownload={(messageId, attachmentIndex) => bridge.retryRcsMediaDownload(messageId, attachmentIndex)} openStorageSettings={openStorageSettings}>
      <TypingAnnouncer text={!groupLeft && typing.announcement ? isGroup ? typingLabel : peppyCopy.typing_announce_direct : ""} />
      <div id="tabpanel-messages" role={conversationTabsShown ? "tabpanel" : undefined} aria-labelledby={conversationTabsShown ? "tab-messages" : undefined} hidden={conversationTab !== "messages"}>
      <MessageList
      key={reactionScopeKey}
      conversation={active}
       pendingSends={visiblePendingSendRows}
       typingRow={typingRow}
      reactionRouteOffline={Boolean(active?.reactionGatewayId) && snapshot?.gateways.find((gateway) => gateway.id === active?.reactionGatewayId)?.online !== true}
      attachmentViews={attachmentViews}
      loading={Boolean(active) && !conversationLoaded}
      registerRow={registerRow}
      publicCopies={publicCopies}
      onPublish={(file) => void publish(file)}
      onRetryAttachment={(file) => void retryAttachment(file)}
      onSaveAttachment={(file) => void saveAttachment(file)}
      onCopyOneTimeCode={copyOneTimeCode}
      listRef={setMessageListNode}
      onScroll={onMessageScroll}
      onWheel={onMessageWheel}
      hasOlderMessages={hasOlderMessages}
      historyLoading={historyLoading}
      historyError={historyError}
      historyErrorDirection={historyErrorDirection}
      onLoadOlder={loadOlderFromManualControl}
      suppressHistoryAnnouncements={suppressHistoryAnnouncements}
      historyExhausted={historyExhausted}
      onStartConversation={!composerConversation && !slotKey ? () => void startNewMessage() : undefined}
      onOpenMedia={(item) => void openGalleryItem({ attachment: item.attachment, message: item.message })}
      requestOriginal={requestOriginal}
      checkOriginal={checkOriginal}
      releaseMedia={releaseMedia}
      mediaRetryKey={mediaStateHint}
      onOpenExternal={browserHost ? undefined : bridge.open_external_url}
      searchTargetId={searchTargetId}
      detachedHistory={detachedHistory?.conversationId === active?.id ? detachedHistory : undefined}
      onLoadNewer={(control) => void loadNewerMessages(control)}
      onGoToLatest={goToLatest}
      compactTimestamps={compactTimestamps}
      showSenderAvatars={groupSenderAvatars}
      onOpenSender={!composerConversation ? openSender : undefined}
      senderLocked={!snapshot || snapshot.encryption.state !== "unlocked" || Boolean(appLock?.locked)}
      resolution={resolution}
      showReactions={showReactions}
      actorName={actorName}
      onOpenReactionTarget={(reaction, notice) => { void openReactionTarget(reaction, notice); }}
      writesPaused={isWritesPaused(snapshot)}
      pendingReactionId={pendingReactionId}
      reactionFeedback={reactionFeedback}
      seenReactions={seenReactions}
      pendingSeenReactionIds={pendingSeenReactions?.scope === reactionScopeKey ? pendingSeenReactions.ids : EMPTY_REACTION_IDS}
      captureReactionLayout={captureReactionLayout}
      restoreReactionLayout={restoreReactionLayout}
      badgeObservation={{ root: messageListNode, obscuredBottom: overlayMargin, enabled: reactionReadEnabled && showReactions, canRecord: canSeeReactionHints, onSeen: ids => {
        if (!canSeeReactionHints()) return;
        if (canReadMessages()) recordSeenReactions(ids);
        else {
          const held = pendingSeenReactionsRef.current;
          const next = { scope: reactionScopeKey, ids: new Set([...(held?.scope === reactionScopeKey ? held.ids : []), ...ids]) };
          pendingSeenReactionsRef.current = next;
          setPendingSeenReactions(next);
        }
      } }}
      toneOverrides={toneOverrides}
      onEditGroupResend={editGroupResend}
      onEditRcsUnsent={restoreUnsentRcs}
      onCreateGroupAgain={createGroupAgainFromMessage}
      takeSendCancellationSequence={() => ++cancelledSendSequence.current}
      onSendCancellationRequested={restoreCancelledSend}
    />
      </div>
      {conversationTab === "media" && <div id="tabpanel-media" role="tabpanel" aria-labelledby="tab-media">
        {active && <MediaPanel key={viewIdentity} loadPage={async (cursor) => {
          const page = await bridge.media_page(active.id, cursor as Parameters<typeof bridge.media_page>[1], 40);
          return { items: page.items.map(toGalleryItem), nextCursor: page.nextCursor };
        }} onOpen={(item) => void openGalleryItem(item)} refreshKey={mediaStateHint} />}
      </div>}
      {lightboxItems && lightboxAttachmentId && <MediaLightbox items={lightboxItems} initialAttachmentId={lightboxAttachmentId} initialMessageId={lightboxMessageId} onDismiss={() => { setLightboxItems(null); setLightboxAttachmentId(undefined); setLightboxMessageId(undefined); }} requestOriginal={requestOriginal} checkOriginal={checkOriginal} saveMedia={async (id) => { await bridge.save_attachment(id); }} releaseMedia={releaseMedia} hasNextPage={lightboxHasNextPage} hasPreviousPage={lightboxHasPreviousPage} retryKey={mediaStateHint} loadAdjacentPage={async (direction) => {
        const cache = galleryCacheRef.current;
        const cursor = direction === "previous" ? cache.newerCursor : cache.olderCursor;
        if (!cache.conversationId || !cursor) return undefined;
        const session = gallerySessionRef.current;
        const page = direction === "previous"
          ? await bridge.media_page_newer(cache.conversationId, cursor, 40)
          : await bridge.media_page(cache.conversationId, cursor, 40);
        if (gallerySessionRef.current !== session || !sameGalleryConversation(galleryConversationRef.current, cache.conversationId)) return undefined;
        const pageItems = [...new Map(page.items.map(toGalleryItem).map(candidate => [`${candidate.message.id}:${candidate.attachment.id}`, candidate])).values()].filter((candidate) => !cache.items.some((known) => known.attachment.id === candidate.attachment.id && known.message.id === candidate.message.id));
        const orderedItems = direction === "previous" ? pageItems.reverse() : pageItems;
        cache.items = direction === "previous" ? [...orderedItems, ...cache.items] : [...cache.items, ...orderedItems];
        if (direction === "previous") {
          cache.newerCursor = page.nextCursor;
          setLightboxHasPreviousPage(Boolean(page.nextCursor));
        } else {
          cache.olderCursor = page.nextCursor;
          setLightboxHasNextPage(Boolean(page.nextCursor));
        }
        return { items: orderedItems, hasNextPage: Boolean(page.nextCursor), cursor: page.nextCursor };
      }} />}
      </RcsMediaActionsProvider>
    </MediaActionsProvider>
  );
  const panelStatus = snapshot
    ? {
        tone: statusTone(snapshot.connection, snapshot.encryption.state, serverOffline),
        summary: statusSummary(snapshot.connection, snapshot.encryption.state),
        details: <StatusDetails connection={snapshot.connection} encryption={snapshot.encryption.state} syncProgress={snapshot.syncProgress} serverUnreachable={serverOffline} />,
      }
    : {
        tone: "neutral" as const,
        summary: "Loading status",
        details: <StatusDetails />,
      };
  const mutedNotificationKeys = new Set((snapshot?.appFilters ?? []).filter((filter) => filter.muted).map((filter) => notificationMuteKey(filter.sourceDeviceId, filter.packageName)));
  const notificationUnread = snapshot?.notifications.filter((notification) => !notification.seen && !notification.dismissalPending && !hiddenNotificationKeys.has(notificationKey(notification.target)) && !mutedNotificationKeys.has(notificationMuteKey(notification.target.sourceDeviceId, notification.packageName))).length ?? 0;
  const contactsPending = snapshot?.contactsPendingCount ?? snapshot?.contactBooks?.reduce((count, book) => count + book.pendingEditCount, 0) ?? 0;
  const searchDisabled = !snapshot || snapshot.encryption.state !== "unlocked" || Boolean(appLock?.locked);
  const searchSessionKey = renderedSearchSessionKey;
  const composerConnectionDot = snapshot
    ? <ConnectionDot state={snapshot.connection.state} label={`Connection: ${connectionText(snapshot.connection)}`} serverUnreachable={serverOffline} />
    : null;
  const appNotices: AppNotice[] = [
    ...(refreshNotice ? [{ source: "refresh" as const, severity: "error" as const, message: refreshNotice, persistent: true }] : []),
    ...(actionNotice ? [{ source: "action" as const, ...actionNotice }] : []),
  ];
  /* Progress is shown only for enrolled, unlocked UI; locked and setup states never render it. */
  const syncProgressEligible = Boolean(snapshot && enrolled && !showSetup && snapshot.encryption.state !== "locked" && !appLock?.locked);
  const syncTooLarge = snapshot?.connection.errorCode === "snapshot-too-large";
  const syncProgressBanner = syncProgressEligible
    ? syncTooLarge
      ? <SyncTooLargeNotice browserHost={browserHost} onCheckForUpdates={() => { setSettingsSectionRequested("updates"); setActiveView("settings"); }} onReload={() => void bridge.retry_snapshot_import().finally(() => window.location.reload())} />
      : <SyncProgress progress={snapshot?.syncProgress} />
    : null;
  const appNoticeBanner = <AppNoticeBanner
    notices={appNotices}
    onDismiss={(source) => {
      if (source !== "action") return;
      prepareFocusRestore();
      setNotice("");
      restoreFocusIfPending();
    }}
    onRetry={() => {
      prepareFocusRestore();
      void refresh().then(restoreFocusIfPending).catch(reportRefresh(""));
    }}
    retrying={refreshing}
  />;
  if (composerConversation) {
    return (
      <main
        ref={mainRef}
        id="composer-shell"
        tabIndex={-1}
        className={`theme-${theme}${headPanel ? " head-panel-shell" : ""}`}
        data-accent={accentAttribute(accentColor)}
        data-bridge-mode={snapshot?.mode ?? "unavailable"}
        data-writes-paused={isWritesPaused(snapshot) || undefined}
        data-platform={platform}
        inert={lifecyclePending ? true : undefined}
        aria-busy={lifecyclePending ? "true" : undefined}
      >
        {isWritesPaused(snapshot) && <p id="update-paused-description" className="sr-only">{UPDATE_PAUSED_DESCRIPTION}</p>}
        <section
          id="conversation-pane"
          className="conversation-pane"
          aria-label="Conversation"
          ref={paneRef}
        >
          {!headPanel && <AppTitlebar
            isComposer platform={platform} title={revoked ? "Peppy" : title}
            viewSwitch={revoked ? undefined : <ConversationViewTabs value={conversationTab} onChange={setConversationTab} />}
            status={composerConnectionDot}
            onMinimize={browserHost ? undefined : () => {}} onMaximize={browserHost ? undefined : () => {}} onClose={browserHost ? undefined : handlers.current.close}
          />}
          {headPanel && <header id="head-panel-header" aria-label={revoked ? "Peppy" : `Floating conversation with ${title ?? "Conversation"}`} data-tauri-drag-region>
            <strong data-tauri-drag-region>{revoked ? "Peppy" : title ?? "Conversation"}</strong>
            {!revoked && <ConversationViewTabs value={conversationTab} onChange={setConversationTab} />}
            <span className="head-panel-status">{composerConnectionDot}</span>
            <button id="head-panel-collapse" type="button" aria-label="Collapse to bubble" title="Collapse" onClick={handlers.current.collapse}><Minus size={14} strokeWidth={1.75} aria-hidden /></button>
            <button id="head-panel-close" type="button" aria-label={closingHead ? "Closing…" : "Close bubble"} title="Close" disabled={closingHead} aria-busy={closingHead || undefined} onClick={handlers.current.closeHead}><X size={14} strokeWidth={1.75} aria-hidden /></button>
          </header>}
          {systemRibbon && (headPanel
            ? <SystemRibbonStrip model={systemRibbon} />
            : <SystemRibbon model={systemRibbon} openFailed={ribbonOpenFailed} onAction={openRibbonAction} onDismiss={accountNotice.dismiss} />)}
          {!headPanel && <SystemRibbonAnnouncement model={systemRibbon} />}
          {appNoticeBanner}
          <div
            id="conversation-stage"
            style={overlayHeight === null ? undefined : { "--composer-overlay-height": `${overlayHeight}px` } as CSSProperties}
          >
            {revoked ? <p id="composer-recovery-notice" role="status">Reconnect this computer in the main Peppy window.</p> : <>
              <SendAgainHost conversation={active} gateways={gateways} routeLabel={sendAgainRouteLabel(gateways, accountReadOnly)} serverOffline={serverOffline} writesPaused={isWritesPaused(snapshot)} onUpdateInProgress={reportAction("Message not sent: ")} onGoToLatest={goToLatest}>{messages}</SendAgainHost>
              {recipientRail}
              {composer}
            </>}
          </div>
        </section>
      </main>
    );
  }

  return (
    <ToneOverridesProvider overrides={toneOverrides}>
    <>
    <main
      ref={mainRef}
      id="desktop-shell"
      tabIndex={-1}
      className={`theme-${theme}`}
      data-accent={accentAttribute(accentColor)}
      data-bridge-mode={snapshot?.mode ?? "unavailable"}
      data-writes-paused={isWritesPaused(snapshot) || undefined}
      data-platform={platform}
      data-navigation={navigationPosition}
      data-browser-setup={browserHost && showSetup ? "true" : undefined}
      data-narrow-browser={narrowBrowserViewport || undefined}
      inert={lifecyclePending ? true : undefined}
      aria-busy={lifecyclePending ? "true" : undefined}
    >
      {isWritesPaused(snapshot) && <p id="update-paused-description" className="sr-only">{UPDATE_PAUSED_DESCRIPTION}</p>}
      <AppTitlebar
        accountUrl={titlebarAccountUrl}
        onMinimize={browserHost ? undefined : () => void bridge.window("minimize")}
        onMaximize={browserHost ? undefined : () => void bridge.window("maximize")}
        onClose={browserHost ? undefined : () => void closeAfterSave(() => bridge.window("close"))}
        platform={platform}
        nativeWindowControls={!browserHost && platform === "macos"}
        search={<OmniSearch showContactBook={(snapshot?.contactBooks?.length ?? 0) > 1} onSearch={searchLocal} onActivate={activateSearchHit} resolveContactNumbers={resolveOmniContactNumbers} onStartConversation={startOmniConversation} sessionKey={searchSessionKey} disabled={searchDisabled} refreshKey={mediaStateHint} historySearch={historyControls.historySearch} onSearchFurtherBack={historyControls.onSearchFurtherBack} onCancelHistorySearch={historyControls.onCancelHistorySearch} onOpenChange={setHistorySearchVisible} scope={titleSearchScope} notificationResults={titleSearchScope ? undefined : notificationResults} />}
        onToggleSidebar={activeView === "contacts" && !compactContacts ? () => setContactsBookCollapsedPersisted(!contactsBookCollapsed) : toggleConversationList}
        sidebarExpanded={activeView === "contacts" && !compactContacts ? !contactsBookCollapsed : narrowConversationListMode ? showingMobileConversationList : activeView === "conversations" && !listCollapsed}
        sidebarControls={activeView === "contacts" && !compactContacts ? "contacts-book-list" : "thread-list"}
        sidebarLabel={activeView === "contacts" && !compactContacts ? "Toggle contact books" : undefined}
        onNewMessage={startTitlebarMessage}
        newMessageDisabled={loading || snapshot?.encryption.state !== "unlocked" || Boolean(appLock?.locked)}
        actions={snapshot && enrolled && !showSetup ? <>
          <MarkAllReadAction
            key={currentSearchSessionKey()}
            unreadCount={snapshot.conversations.reduce((total, conversation) => (conversation.folder ?? "inbox") === "inbox" ? total + conversation.unread : total, 0)}
            enabled={snapshot.encryption.state === "unlocked" && !appLock?.locked && !loading}
            markAllRead={() => bridge.mark_all_read()}
            refresh={() => refresh()}
            toastHost={threadListToastHost}
            onFocusFallback={() => {
              const titlebarNewMessage = document.querySelector<HTMLButtonElement>("[data-action='new-message']");
              const omniInput = document.querySelector<HTMLInputElement>("#titlebar-search input");
              const fallback = document.getElementById("thread-list") ?? document.getElementById("conversation-pane");
              if (titlebarNewMessage && !titlebarNewMessage.disabled) titlebarNewMessage.focus();
              else if (omniInput && !omniInput.disabled) omniInput.focus();
              else fallback?.focus();
            }}
          />
          {snapshot.deviceRole === "owner" && snapshot.gateways.length === 0 && <button
            id="titlebar-connect-phone"
            type="button"
            aria-label="Connect a phone"
            title={snapshot.connection.state !== "connected" || snapshot.encryption.state !== "unlocked" || appLock?.locked
              ? "Connect a phone (available when connected and unlocked)"
              : "Connect a phone"}
            disabled={snapshot.connection.state !== "connected" || snapshot.encryption.state !== "unlocked" || Boolean(appLock?.locked)}
            onClick={() => setAddingPhone(true)}
          >
            <Smartphone size={16} strokeWidth={1.75} aria-hidden />
            <span className="titlebar-action-dot" aria-hidden />
          </button>}
        </> : undefined}
        navigation={navigationPosition === "title-bar" ? (
          <NavButtons
            orientation="titlebar"
            activeView={activeView}
            onView={setActiveView}
            onToggleList={toggleConversationList}
            listCollapsed={narrowConversationListMode ? !showingMobileConversationList : listCollapsed}
            threadListId="thread-list"
            notificationUnread={notificationUnread}
            contactsPending={contactsPending}
          />
        ) : undefined}
        status={navigationPosition === "title-bar" ? (
          <StatusPopover tone={panelStatus.tone} summary={panelStatus.summary}>
            {panelStatus.details}
          </StatusPopover>
        ) : undefined}
      />
      {systemRibbon && <SystemRibbon model={systemRibbon} openFailed={ribbonOpenFailed} onAction={openRibbonAction} onDismiss={accountNotice.dismiss} />}
      <SystemRibbonAnnouncement model={systemRibbon} />
      {appNoticeBanner}
      {syncProgressBanner}
      <div ref={desktopBodyRef} id="desktop-body" className="desktop-layout">
        {navigationPosition === "side-rail" && <Panel
          activeView={activeView}
          onView={setActiveView}
          onToggleList={toggleConversationList}
          listCollapsed={narrowConversationListMode ? !showingMobileConversationList : listCollapsed}
          threadListId="thread-list"
          status={panelStatus}
          notificationUnread={notificationUnread}
          contactsPending={contactsPending}
        />}
        {loading || (!snapshot && draftSubscriptionError) ? (
          <section id="desktop-loading-state">
            <StartupScreen variant="main" progress={launchProgress} error={draftSubscriptionError ? {
              title: "Couldn't open your messages",
              message: "Draft updates aren't available. Try again to finish loading.",
              onRetry: () => {
                setLoading(true);
                setDraftSubscriptionError(undefined);
                restartDraftSubscriptionRef.current?.();
              },
            } : undefined} />
          </section>
        ) : <>
        <aside
          id="thread-list"
          aria-label="Thread list"
          tabIndex={-1}
          data-collapsed={!showingMobileConversationList && listCollapsed || undefined}
          data-mobile-list={showingMobileConversationList || undefined}
          hidden={hideThreadList}
          ref={setThreadListHost}
          style={
            hideThreadList
              ? { display: "none" }
              : showingMobileConversationList
                ? { width: "auto", flex: "1 1 0" }
                : { width: renderedListWidth, flexBasis: renderedListWidth }
          }
        >
            <ConversationFolderSwitcher
              value={folderView}
              counts={folderCounts}
              unread={folderUnread}
              onChange={setFolderView}
            />
            <ConversationList
            conversations={conversations}
            actionsPaused={isWritesPaused(snapshot)}
            selectedId={slotKey}
            onSelect={select}
              onPopout={id => void popoutConversation(id)}
              popoutMode={popoutMode}
              onTogglePin={snapshot?.encryption.state === "unlocked" && !appLock?.locked ? (id, pinned) => void toggleConversationPin(id, pinned) : undefined}
              onRequestFolderChange={snapshot?.encryption.state === "unlocked" && !appLock?.locked ? requestFolderChange : undefined}
              onDelete={id => void deleteDraft(id)}
              onCancelDelete={cancelDeleteDraft}
              deleteConfirmationId={deleteConfirmationId ?? undefined}
              deletingId={deletingId ?? undefined}
              deleteWarningId={listCollapsed ? undefined : "conversation-delete-draft-warning"}
              pendingPinId={pendingPinId ?? undefined}
              pendingFolderId={pendingFolderId ?? undefined}
              pendingFolder={pendingFolder}
              confirmingFolderId={folderConfirmation?.id ?? senderUnblockConfirmation?.id}
              confirmingFolder={folderConfirmation?.to}
              emptyLabel={folderView === "archived" ? "No archived conversations" : folderView === "spam" ? "No spam conversations" : undefined}
          />
              {folderConfirmation && <ConversationFolderConfirmation
              kind={folderConfirmation.to}
              name={folderConfirmation.name}
              trigger={folderConfirmation.trigger}
              pending={Boolean(pendingFolderId)}
              writesPaused={isWritesPaused(snapshot)}
              pauseNoticeVisible={updateNoticeVisible}
              onConfirm={() => {
                void applyFolderChange(folderConfirmation.id, folderConfirmation.from, folderConfirmation.to, folderConfirmation.trigger);
              }}
              onCancel={() => setFolderConfirmation(undefined)}
            />}
            {folderNotice && <ConversationFolderNotice
              notice={folderNotice}
              pending={Boolean(pendingFolderId)}
              writesPaused={isWritesPaused(snapshot)}
              toastHost={threadListToastHost}
              onUndo={() => void applyFolderChange(folderNotice.conversationId, folderNotice.to, folderNotice.from)}
              onRetry={() => void applyFolderChange(folderNotice.conversationId, folderNotice.from, folderNotice.to)}
              onDismiss={() => setFolderNotice(undefined)}
            />}
            <SenderUnblockNotice
              notice={senderUnblockNotice}
              writesPaused={isWritesPaused(snapshot)}
              pending={Boolean(pendingSenderUnblockId) || snapshot?.encryption.state !== "unlocked" || Boolean(appLock?.locked)}
              toastHost={threadListToastHost}
              onRetry={() => senderUnblockNotice && void requestSenderUnblock(senderUnblockNotice.id, senderUnblockNotice.name)}
              onDismiss={() => setSenderUnblockNotice(undefined)}
            />
        </aside>
        {!settingsOpen && !notificationsOpen && !contactsOpen && !narrowConversationListMode && <ResizeHandle
          id="handle-h1"
          direction="horizontal"
          ariaLabel="Resize thread list"
          value={listCollapsed ? 0 : renderedListWidth}
          min={LIST_MIN_WIDTH}
          max={listMaxForWindow}
          valueUnit="pixels"
          onResizeStart={(position) => {
            const bodyLeft =
              desktopBodyRef.current?.getBoundingClientRect().left ?? 0;
            listDragWidth.current = listCollapsed
              ? position - (bodyLeft + railWidth)
              : renderedListWidth;
          }}
          onResize={resizeList}
          onResizeTo={resizeListTo}
          onResizeEnd={commitList}
          collapsed={listCollapsed ? "before" : undefined}
          collapsible={{ side: "before", restoreValue: previousListWidth.current }}
        />}
        <section
          id="conversation-pane"
          className="conversation-pane"
          aria-label={settingsOpen ? "Settings pane" : notificationsOpen ? "Notifications pane" : contactsOpen ? "Contacts pane" : "Conversation"}
          data-view={activeView}
          data-mobile-list={showingMobileConversationList || undefined}
          ref={paneRef}
        >
          {activeView === "conversations" && <header id="thread-pane-header">
            {activeDisplay && (
              <TonePicker id="thread-tone-picker" seed={activeDisplay.avatar.seed} value={toneOverrides[activeDisplay.avatar.seed]} automaticIndex={paletteIndex(activeDisplay.avatar.seed)} disabled={!snapshot || snapshot.encryption.state === "locked" || Boolean(appLock?.locked) || isWritesPaused(snapshot)} disabledReason={isWritesPaused(snapshot) ? UPDATE_PAUSED_DESCRIPTION : undefined} describedBy={isWritesPaused(snapshot) ? "update-paused-description" : undefined} pending={Boolean(pendingToneSeeds[activeDisplay.avatar.seed])} error={toneErrors[activeDisplay.avatar.seed]} onSelect={(index) => { void saveTone(activeDisplay.avatar.seed, index); }} renderTrigger={(props) => <button {...props} id="thread-contact-avatar" aria-label="Change conversation color"><ContactAvatar {...activeDisplay.avatar} toneOverrides={toneOverrides} size={36} /></button>} />
            )}
            <div className="header-copy">
              <div className="header-copy-details">
                <div data-header-title-row><b data-header-title title={title}>{revoked ? "Set up Peppy" : title ?? (showSetup ? "Set up Peppy" : "Peppy")}</b>{!revoked && !headPanel && rcsRoute.current && <RcsBadge encryptionReported={active?.rcsEncryption === true} paused={active?.group?.state === "paused"} />}{!revoked && !headPanel && active?.group && !rcsRoute.current && active.group.state !== "withdrawn" && <MmsGroupBadge />}</div>
                {active?.group
                  ? <small data-conversation-participants>{groupSubtitle({ name: active.group.name, memberNames: groupMembers(active).map((part) => displayAddressName(part, resolution)) })}</small>
                  : participantsLine && participantsLine !== title && <small data-conversation-participants>{participantsLine}</small>}
              </div>
            </div>
            <div id="conversation-header-actions" className="header-actions" hidden={revoked}>
              {isGroup && active?.group && !headPanel && <button
                id="header-group-details"
                ref={groupDetailsButtonRef}
                type="button"
                aria-label={peppyCopy.group_details_open_label}
                title={peppyCopy.group_details_open_label}
                aria-haspopup="dialog"
                aria-expanded={groupDetailsOpen}
                disabled={Boolean(appLock?.locked)}
                onClick={() => setGroupDetailsOpen(true)}
              >
                <Info size={16} aria-hidden />
              </button>}
              {conversationLoaded && <ConversationViewTabs value={conversationTab} onChange={setConversationTab} />}
              {(chooserState || chooserOpen) && <div className="contact-source-anchor" data-contact-source-anchor>
                {chooserState && <button id="conversation-contact-chooser" type="button" className="link-button" data-name-state={chooserState} aria-haspopup="dialog" aria-expanded={Boolean(chooserOpen)} onClick={event => chooserOpen ? closeNameChooser() : setNameChooser({ address: chooserAddress!, conversationId: active!.id, returnFocus: event.currentTarget })}>
                  {chooserState === "conflict" ? "Choose contact" : chooserState === "pinned" ? "Name chosen" : "Contact sources"}
                </button>}
                {chooserOpen && nameChooser && <ContactNameChooser key={`${nameChooser.conversationId}:${nameChooser.address}`} address={nameChooser.address} state={snapshot?.contactNameConflicts?.includes(nameChooser.address) ? "conflict" : resolution[nameChooser.address]?.nameBasis ?? "conflict"} books={snapshot?.contactBooks ?? []} bridge={bridge} onClose={closeNameChooser} onRefresh={() => { void refresh(active?.id).catch(reportRefresh("")); }} onOpenContact={candidate => { setNameChooser(undefined); void openContactTarget({ bookId: candidate.bookId, contactId: candidate.contactId, requestId: ++contactTargetRequestId.current }); }} />}
              </div>}
              {addConversationContactAddress && <button
                id="add-conversation-contact"
                aria-label={`Add ${formatPhoneNumber(addConversationContactAddress)} to contacts`}
                title={`Add ${formatPhoneNumber(addConversationContactAddress)} to contacts`}
                onClick={() => openSender(addConversationContactAddress)}
              >
                <UserPlus size={16} aria-hidden />
              </button>}
              {active?.folder === "spam" && <button id="header-not-spam" aria-label="Not spam" title={isWritesPaused(snapshot) ? UPDATE_PAUSED_DESCRIPTION : snapshot?.encryption.state !== "unlocked" || appLock?.locked ? "Unavailable while locked" : active.folderReason === "blocked" ? "Not spam (unblocks this number on your phone)" : "Not spam"} aria-haspopup={active.folderReason === "blocked" ? "dialog" : undefined} aria-expanded={active.folderReason === "blocked" ? senderUnblockConfirmation?.id === active.id : undefined} aria-controls={active.folderReason === "blocked" && senderUnblockConfirmation?.id === active.id ? "sender-unblock-confirmation" : undefined} aria-describedby={isWritesPaused(snapshot) ? "update-paused-description" : undefined} disabled={isWritesPaused(snapshot) || snapshot?.encryption.state !== "unlocked" || Boolean(appLock?.locked) || Boolean(pendingSenderUnblockId) || Boolean(pendingFolderId)} onClick={event => requestFolderChange(active.id, "inbox", event.currentTarget)}><ShieldCheck size={16} aria-hidden /></button>}
              {!browserHost && conversationLoaded && (
                <button id="header-popout-conversation"
                  aria-label={popoutMode === "bubble" ? "Open as chat bubble" : "Open in new window"}
                  title={popoutMode === "bubble" ? "Open as chat bubble" : "Open in new window"}
                  onClick={() => void popoutConversation(slotKey)}
                >
                  <ExternalLink size={16} aria-hidden />
                </button>
              )}
              {slotKey && ((active?.recipientlessDraft && active.draftOnly) || (Boolean(newComposeKey) && content.recipientIds.length === 0)) && <button
                id="conversation-delete-draft"
                className={deleteConfirmationId === slotKey ? "confirming" : undefined}
                aria-label={deleteConfirmationId === slotKey ? "Confirm delete draft" : "Delete draft"}
                aria-describedby={deleteConfirmationId === slotKey ? "conversation-delete-draft-warning" : undefined}
                title={deleteConfirmationId === slotKey ? "Confirm delete draft" : "Delete draft"}
                disabled={deletingId === slotKey}
                onBlur={() => deleteConfirmationId === slotKey && cancelDeleteDraft()}
                onKeyDown={event => {
                  if (event.key !== "Escape") return;
                  event.preventDefault();
                  event.stopPropagation();
                  cancelDeleteDraft();
                }}
                onClick={() => void deleteDraft(slotKey)}
              >
               <Trash2 size={16} aria-hidden />
               </button>}
              {listCollapsed && deleteConfirmationId === slotKey && <span id="conversation-delete-draft-warning" data-delete-draft-warning role="status">Deleting this draft permanently removes its text and attachments.</span>}
            </div>
          </header>}
          {!settingsOpen && !notificationsOpen && !contactsOpen && active && <ConversationSpamNotice folder={active.folder} folderReason={active.folderReason} unblock={active.unblock} />}
          {!settingsOpen && !notificationsOpen && !contactsOpen && active && <ConversationRcsNotice historyGap={active.rcsHistoryGap} group={groupBarState ? { state: groupBarState } : undefined} />}
          {groupDetailsOpen && active?.group && activeDisplay && <GroupDetailsDialog
            group={{ name: active.group.name, members: groupMembers(active), state: active.group.state }}
            contactName={(address) => resolution[address]?.displayName}
            toneSeed={activeDisplay.avatar.seed}
            onOpenContact={(address) => openSender(address)}
            onDismiss={() => setGroupDetailsOpen(false)}
            opener={groupDetailsButtonRef}
          />}
          {senderUnblockConfirmation && <SenderUnblockConfirmation
            name={senderUnblockConfirmation.name}
            canUnblock={senderUnblockConfirmation.canUnblock}
            trigger={senderUnblockConfirmation.trigger}
            pending={pendingSenderUnblockId === senderUnblockConfirmation.id}
            writesPaused={isWritesPaused(snapshot)}
            pauseNoticeVisible={updateNoticeVisible}
            onConfirm={() => void requestSenderUnblock(senderUnblockConfirmation.id, senderUnblockConfirmation.name, senderUnblockConfirmation.trigger)}
            onCancel={() => setSenderUnblockConfirmation(undefined)}
          />}
          <NotificationMuteNotice
            notice={notificationMuteNotice}
            pending={notificationMutePending}
            writesPaused={isWritesPaused(snapshot)}
            errorAction={notificationMuteRetryRef.current}
            onUndo={() => {
              if (notificationMuteNotice) runNotificationMute(notificationMuteNotice.filter, notificationMuteNotice.label, false);
            }}
            onRetry={() => {
              if (notificationMuteNotice) runNotificationMute(notificationMuteNotice.filter, notificationMuteNotice.label, notificationMuteRetryRef.current === "mute");
            }}
            onDismiss={() => setNotificationMuteNotice(undefined)}
          />
          {mcpAuthorizationId && snapshot && !(snapshot.encryption.state === "unlocked" && snapshot.deviceRole === "owner" && snapshot.sessionScopeId) && <div id="mcp-authorization-waiting" className="app-notice-row" role="status" data-mcp-authorization-waiting={snapshot.encryption.state === "unlocked" ? "non-owner" : "locked"}><p>{snapshot.encryption.state === "unlocked" ? "An AI client is waiting for owner approval. Approve it from the vault owner device." : "An AI client is waiting for owner approval. Unlock Peppy to continue."}</p><button className="app-notice-dismiss" type="button" onClick={dismissMcpAuthorization}>Dismiss</button></div>}
          {mcpAuthorizationId && snapshot?.encryption.state === "unlocked" && snapshot.deviceRole === "owner" && snapshot.sessionScopeId && <McpAuthorizationConsent key={`${snapshot.sessionScopeId}:${mcpAuthorizationId}`} authorizationId={mcpAuthorizationId} browserHost={browserHost} sessionKey={snapshot.sessionScopeId} focusFallback={mainRef} onClose={dismissMcpAuthorization} />}
          {settingsOpen ? (
            <SettingsView
              storageRequest={storageSettingsRequest}
              mode={mode}
              configuredOrigin={browserHost ? fixedOrigin ?? snapshot?.connection.origin : snapshot?.connection.origin}
              origin={origin}
              onOrigin={setOrigin}
              onAction={runSetupAction}
              onOpenAccount={() => bridge.hosted_open_billing()}
              sessionScopeId={snapshot?.sessionScopeId}
              encryption={snapshot?.encryption.state ?? "locked"}
              canUnlock={canRecoverNativeSync}
              theme={theme}
              onTheme={setTheme}
              accentColor={accentColor}
              onAccentColor={selectAccentColor}
              accentError={accentError}
              navigationPosition={navigationPosition}
              onNavigationPosition={changeNavigationPosition}
              popoutMode={popoutMode}
              onPopoutMode={changePopoutMode}
              compactTimestamps={compactTimestamps}
              onCompactTimestamps={changeCompactTimestamps}
              attachmentLayout={attachmentLayout}
              onAttachmentLayout={changeAttachmentLayout}
              groupSenderAvatars={groupSenderAvatars}
              onGroupSenderAvatars={changeGroupSenderAvatars}
              showReactions={showReactions}
              onShowReactions={setShowReactions}
              desktop={snapshot?.desktop}
              onStartAtLogin={async enabled => {
                try { await bridge.set_start_at_login(enabled); await refresh(); }
                catch (error) { await refresh().catch(() => {}); throw error; }
              }}
              notifications={snapshot?.notifications ?? []}
              filters={snapshot?.appFilters ?? []}
              sources={snapshot?.gateways ?? []}
              preferences={snapshot?.notificationPreferences ?? { messageBanners: true, mirroredBanners: true, preview: "full", muteWithHeads: true }}
              onPreferences={async preferences => {
                try { await bridge.set_notification_preferences(preferences); await refresh(); }
                catch (error) { if (errorCode(error) === "update-in-progress") reportAction("Notification settings not changed: ")(error); throw error; }
              }}
               onMute={async filter => {
                try { await bridge.set_app_muted(filter.sourceDeviceId, filter.packageName, filter.appName, filter.muted); await refresh(); }
                catch (error) { if (errorCode(error) === "update-in-progress") reportAction("Notification settings not changed: ")(error); throw error; }
              }}
              onPermission={() => bridge.request_notification_permission()}
              onOpenSettings={() => bridge.open_notification_settings()}
              headsSupported={snapshot?.head.enabled === true}
              writesPaused={isWritesPaused(snapshot)}
              pauseNoticeVisible={updateNoticeVisible}
               onLock={browserHost ? lockSync : undefined}
               appLock={appLock}
               onAppLock={changeAppLock}
               browserHost={browserHost}
              accountUrl={accountUrl}
              hostedDeployment={hostedDeployment}
              canRevealVaultSecret={!browserHost && snapshot?.deviceRole === "owner" && snapshot.connection.state === "connected"}
               serverOffline={serverOffline}
               accountReadOnly={accountReadOnly}
               typingShare={typingLinkAvailable && bridge.send_typing ? shareTyping : undefined}
               typingShareError={typingShareError}
               onTypingShare={changeTypingShare}
               diagnostics={hasDiagnosticsBridge(bridge) ? bridge : undefined}
                requestedSection={settingsSectionRequested}
                onRequestedSectionConsumed={() => setSettingsSectionRequested(undefined)}
                onAddDevice={snapshot?.deviceRole === "owner" ? openAddDevice : undefined}
                onAccountDeleted={result => {
                  setDeletionResult(result);
                  void refresh().catch(() => undefined);
                }}
                onDeletionCanceled={() => { void refresh().catch(() => undefined); }}
             />
          ) : notificationsOpen ? (
            <NotificationsView
              actionNotice={notificationActionNotice}
              onDismissActionNotice={() => setNotificationActionNotice(undefined)}
              focusRequest={notificationFocus}
              onFocusResult={notificationFocusResult}
              notifications={snapshot?.notifications ?? []}
              displayTitle={notificationDisplayTitle}
              filters={snapshot?.appFilters ?? []}
              sources={snapshot?.gateways ?? []}
              locked={notificationsLocked}
              writesPaused={isWritesPaused(snapshot)}
              hiddenKeys={hiddenNotificationKeys}
              failures={notificationDismissals.failures}
              onRetryDismiss={key => { setNotificationActionNotice(undefined); notificationDismissals.retry(key); }}
              dismissalBlocks={snapshot?.notificationDismissalBlocks ?? []}
              announcement={notificationAnnouncement}
              onAnnounce={announceNotification}
              dismissAll={{
                label: dismissibleNotificationsCount > 100 ? "Dismiss up to 100" : "Dismiss all",
                disabled: !dismissibleNotificationsCount || isWritesPaused(snapshot),
                disabledReason: isWritesPaused(snapshot) ? UPDATE_PAUSED_DESCRIPTION : undefined,
                onDismissAll: dismissAllNotifications,
              }}
              onDismiss={target => {
                setNotificationActionNotice(undefined);
                notificationDismissals.dismiss([target], { single: true });
              }}
              onMute={filter => {
                if (notificationsLocked || appLock?.locked || notificationMutePendingRef.current) return;
                setNotificationActionNotice(undefined);
                const visible = visibleNotifications(snapshot?.notifications ?? [], snapshot?.appFilters ?? [], notificationsLocked);
                const sourceIds = [...new Set(visible.map(notification => notification.target.sourceDeviceId))];
                const label = sourceIds.length > 1
                  ? `${notificationAppName(filter.appName, filter.packageName)} on ${notificationSourceName(filter.sourceDeviceId, snapshot?.gateways ?? [], sourceIds)}`
                  : notificationAppName(filter.appName, filter.packageName);
                runNotificationMute(filter, label, true);
              }}
              onCopyOneTimeCode={copyOneTimeCode}
              onSeen={targets => {
                return bridge.mark_notifications_seen(targets).then(() => refresh()).catch(error => {
                  setNotificationActionNotice({ kind: "error", message: `Could not mark notifications as seen. ${errorText(error)}` });
                  throw error;
                });
              }}
              onSettings={() => { setSettingsSectionRequested("notifications"); setActiveView("settings"); }}
              onManageApps={() => { setSettingsSectionRequested("notification-apps"); setActiveView("settings"); }}
              onDismissMany={targets => {
                setNotificationActionNotice(undefined);
                notificationDismissals.dismiss(targets, { single: false });
              }}
              query={notificationQuery}
              onClearQuery={() => setNotificationQuery("")}
              appFilter={notificationAppFilter}
              onAppFilter={setNotificationAppFilter}
            />
          ) : contactsOpen ? (
            <ContactsView books={snapshot?.contactBooks ?? []} sync={snapshot?.contactSync} serverConnected={snapshot?.connection.state === "connected"} writesPaused={isWritesPaused(snapshot)} pauseNoticeVisible={updateNoticeVisible} onUpdateInProgress={() => { void refresh().catch(reportRefresh("")); }} toneOverrides={toneOverrides} toneDisabled={!snapshot || snapshot.encryption.state === "locked" || Boolean(appLock?.locked) || isWritesPaused(snapshot)} pendingToneSeeds={pendingToneSeeds} toneErrors={toneErrors} onSelectTone={(seed, index) => { void saveTone(seed, index); }} target={contactTarget} onTargetConsumed={(requestId) => setContactTarget(current => current?.requestId === requestId ? undefined : current)} onNavigationGuard={registerContactNavigation} bookCollapsed={contactsBookCollapsed} onBookCollapsedChange={setContactsBookCollapsedPersisted} query={contactQuery} onClearQuery={() => setContactQuery("")} />
          ) : (
            <>
              {reenrolRequired && !revoked ? (
                <ReenrolScreen
                  onRetry={() => refresh()}
                  onStart={async () => setReenrolEnrollment(true)}
                  enrollment={reenrolEnrollment ? <SetupLanding
                    reenrol
                    mode={mode}
                    onMode={setMode}
                    enrolledWithoutPhone={false}
                    onExit={() => setReenrolEnrollment(false)}
                    onJoined={async () => { setReenrolEnrollment(false); await refresh(); }}
                    hostKind={browserHost ? "browser" : "native"}
                  /> : undefined}
                />
              ) : showSetup && snapshot ? (
                <SetupLanding
                  mode={mode}
                  onMode={setMode}
                  enrolledWithoutPhone={!revoked && ownerWithoutPhone(snapshot)}
                  enrollmentEstablished={!revoked && snapshot.deviceRole === "owner" && snapshot.encryption.state === "unlocked"}
                  addingPhone={!revoked && addingPhone}
                  connectionPending={snapshot.connection.errorCode === "connecting"}
                  onExit={() => { setAddingPhone(false); returnToSettingsIfRequested(); setInitialEnrollmentActive(false); }}
                  onDeferPhone={async () => {
                    const key = phoneDeferredKey(snapshot.connection.origin);
                    if (key) localStorage.setItem(key, "1");
                     setPhoneDeferredOrigin(snapshot.connection.origin ?? null);
                     setInitialEnrollmentActive(false);
                     await refresh();
                    if (addingPhone) setAddingPhone(false);
                    returnToSettingsIfRequested();
                  }}
                  onJoined={async () => { await refresh(); setAddingPhone(false); returnToSettingsIfRequested(); setInitialEnrollmentActive(false); }}
                  onInitialEnrollmentStart={() => setInitialEnrollmentActive(true)}
                  fixedOrigin={browserHost ? fixedOrigin : undefined}
                  accountUrl={browserHost ? accountUrl : undefined}
                  newIdentity={browserHost && snapshot.encryption.state === "preview"}
                  hostKind={browserHost ? "browser" : "native"}
                />
              ) : (
                <>
                  <div
                    id="conversation-stage"
                    style={
                      !slotKey
                        ? { "--composer-overlay-height": "0px" } as CSSProperties
                        : overlayHeight === null
                          ? undefined
                          : { "--composer-overlay-height": `${overlayHeight}px` } as CSSProperties
                    }
                  >
                    <SendAgainHost conversation={active} gateways={gateways} routeLabel={sendAgainRouteLabel(gateways, accountReadOnly)} serverOffline={serverOffline} writesPaused={isWritesPaused(snapshot)} onUpdateInProgress={reportAction("Message not sent: ")} onGoToLatest={goToLatest}>{messages}</SendAgainHost>
                    {recipientRail}
                    {slotKey && composer}
                  </div>
                </>
              )}
            </>
          )}
        </section>
        </>}
      </div>
    </main>
    {deletionResult && <AccountDeletionResultDialog result={deletionResult} onDone={() => {
      const focusTarget = deletionResult.outcome === "deleted"
        ? document.querySelector<HTMLElement>("#setup-panel h2, #onboarding-view h2")
        : document.getElementById("hosted-account-delete-status");
      setDeletionResult(undefined);
      requestAnimationFrame(() => focusTarget?.focus());
    }} />}
    </>
    </ToneOverridesProvider>
  );
}
