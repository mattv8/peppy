import {
  Component,
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
import type { GatewayAvailability, LocalSearchHit, LocalSearchRequest } from "@peppy/desktop-ui/bridge-contracts";
import {
  AppTitlebar,
  OmniSearch,
  Check,
  CheckCheck,
  CheckCircle,
  Clock,
  Composer,
  ConnectionDot,
  ContactAvatar,
  ConversationList,
  ExternalLink,
  FileText,
  Image,
  Lock,
  LockKeyhole,
  LockOpen,
  MessageSquare,
  MessageSquarePlus,
  Minus,
  NavButtons,
  type NavigationPosition,
  Panel,
  RecipientPanel,
  ResizeHandle,
  ShieldAlert,
  Smartphone,
  SquarePen,
  StatusPopover,
  SyncProgress,
  TriangleAlert,
  Trash2,
  X,
  type Attachment,
  type AvatarIdentity,
  type Conversation,
  conversationAvatar,
  detectPlatform,
  installOverlayScrollbars,
  isRecipientPosition,
  isNavigationPosition,
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
} from "@peppy/desktop-ui";
import "./settings.css";
import "./vibrancy.css";
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
  type NotificationCheck,
  type NotificationPreferences,
  type PublicCopy,
  type ContactResolutionMap,
  type AppLockStatus,
  type Draft,
} from "./bridge";
import { dismissibleNotifications, notificationMuteKey, NotificationsView } from "./Notifications";
import { createNotificationActivationDrain, nativeNotificationActivation, type ActivationContext } from "./notificationActivation";
import type { NotificationActivation, NotificationActivationTarget } from "@peppy/generated/notification-activation";
import { NotificationSettings } from "./NotificationSettings";
import { ContactsView, type ContactNavigationGuard, type ContactTarget } from "./Contacts";
import { findContactTarget } from "./contact-target";
import { useHistorySearch } from "./useHistorySearch";
import { DevicesSettings } from "./DevicesSettings";
import { HostedAccountSettings } from "./HostedAccountSettings";
import { PrivacySettings } from "./PrivacySettings";
import { SetupLanding, savedSetupMode } from "./SetupLanding";
import { AppLockScreen, AppLockSettings } from "./AppLock";
import { UpdatesSettings } from "./UpdatesSettings";
import { isTauriUpdateHost, useUpdateActivity } from "./updates";
import { connectionText, isServerUnreachable, statusSummary, statusTone, syncStatusText } from "./status";
import { ServerOfflineAnnouncement, ServerOfflineBanner } from "./ServerOfflineBanner";
import { AppNoticeBanner, type AppNotice } from "./AppNoticeBanner";
import { MarkAllReadAction } from "./MarkAllReadAction";
import { peppyCopy } from "./generated/peppyCopy";
import { MediaLightbox } from "./media/MediaLightbox";
import { MediaPanel, type MediaPageItem as GalleryMediaPageItem } from "./media/MediaPanel";
import { MediaGrid } from "./media/MediaGrid";
import { MediaTile } from "./media/MediaTile";
import { originalPending } from "./media/mediaState";
import { MediaActionsProvider, type SaveOutcome as GallerySaveOutcome } from "./media/MediaActionsContext";
import { clipboardImageSupported } from "./media/clipboard";
import { InlineMedia } from "./media/InlineMedia";
import { MessageText } from "./message-links";
import { groupMessages } from "./media/grouping";
import { DETACHED_HISTORY_LIMIT, boundedAroundMessages, mergeDetachedMessages, searchSessionKey as createSearchSessionKey, type DetachedHistory } from "./search/detachedHistory";

import { GatewayPicker } from "./GatewayPicker";
import { installHostedSessionRenewal } from "./hostedSessionRenewal";
import { persistLayoutPatch, persistRecipientPosition, readCompactTimestamps, readGroupSenderAvatars } from "./layout-persistence";
import { senderLabel, senderRunEdges } from "./sender/attribution";
import { hiddenTimeIds } from "./timestamp-bursts";
import { createSeenReactionLedger, linkReactions, locateReactionTarget, NO_REACTIONS, ReactionBadges, ReactionNotice, ReactionSettings, useSeenBadges, useShowReactions, type ActorName, type ReactionNoticeInfo, type ReactionTargetResult, type SeenReactionLedger } from "./reactions";
import { normalizeReactionText, quoteMatches } from "./reactions/parse";
import { FALLBACK_ERROR, errorCode, errorText } from "./bridgeErrors";
import {
  DraftStore,
  blankContent,
  isDirty,
  isLocalDraftKey,
  shouldConfirmPostFlushDelete,
  type DraftContent,
  type DraftRecovery,
  type DraftSlot,
  type SaveOutcome,
} from "./drafts/DraftStore";

type RefreshRequest = {
  conversationId?: string;
  explicit: boolean;
  waiters: Array<{ resolve: () => void; reject: (error: unknown) => void }>;
};

type ActionNotice = Omit<AppNotice, "source">;
type ComposerFeedback = Pick<AppNotice, "message" | "severity"> & { key: string };
type StoredDraftConflict = { key: string; draft?: Draft; generation: number; revision: string };
type PinFocusRestoreIntent = {
  conversationId: string;
  origin: HTMLElement;
  userMovedFocus: boolean;
  dispose(): void;
};

export { errorText } from "./bridgeErrors";
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

/** Native contact phone matches as UI suggestions; the phone address is the recipient ID. */
export async function searchRecipientSuggestions(query: string, sourceDeviceId?: string): Promise<RecipientSuggestion[]> {
  const found = await bridge.search_contact_recipients(query, sourceDeviceId);
  return found.map((match) => ({
    id: match.address,
    rowId: JSON.stringify([match.bookId, match.contactId, match.phoneId]),
    label: match.displayName,
    detail: [match.label, match.normalized ? formatPhoneNumber(match.address) : match.number].filter(Boolean).join(" · "),
    avatarUrl: match.avatarUrl,
    avatarSeed: seedKey(match.address),
    avatarName: match.displayName,
  }));
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
};

const TIMELESS_OUTGOING_STATUSES = new Set<MessageStatus>(["queued-local", "server-accepted"]);

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

function expiryText(seconds: number): string {
  if (seconds <= 0)
    return "No expiry was reported; revoke it on the server when it is no longer needed.";
  if (seconds < 3600) return `Expires in ${Math.ceil(seconds / 60)} min.`;
  if (seconds < 172800) return `Expires in ${Math.round(seconds / 3600)} h.`;
  return `Expires in ${Math.round(seconds / 86400)} days.`;
}

function fileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function attachmentState(file: AttachmentView): string {
  if (file.state === "failed")
    return `Failed${file.error ? `: ${file.error}` : ""}`;
  return file.state === "uploading"
    ? "Uploading…"
    : file.state.charAt(0).toUpperCase() + file.state.slice(1);
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

/** Gateway and SIM together identify a route; one device can expose several SIMs. */
const routeKey = (gatewayId: string, simId: string) =>
  `${encodeURIComponent(gatewayId)} ${encodeURIComponent(simId)}`;

type RouteChoice =
  | { kind: "route"; gateway: GatewayView; implicit: boolean }
  | { kind: "stale"; gatewayId: string; simId: string }
  | { kind: "none" };

function gatewayAvailability(gateway: GatewayView): GatewayAvailability { return gateway.availability ?? "unknown"; }
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
function availabilityLabel(gateway: GatewayView): string {
  switch (gatewayAvailability(gateway)) {
    case "delayed": return gateway.lastSeenAgeSeconds !== undefined ? `Last seen ${formatAge(gateway.lastSeenAgeSeconds)} ago` : "Last seen";
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
function availabilityProblem(gateway: GatewayView): string | undefined {
  switch (gatewayAvailability(gateway)) {
    case "offline": return `${gateway.name} isn't responding. Sending is paused until it reconnects.`;
    case "never_connected": return `${gateway.name} has never connected. Finish setup on the phone to enable sending.`;
    case "no_sms_sim": return `No default SMS SIM is set on ${gateway.name}. Open the phone's SIM settings to fix this.`;
    case "sms_permission_needed": return `Peppy needs SMS permission on ${gateway.name}. Open Peppy on the phone to grant it.`;
    case "sms_unavailable": return `SMS is unavailable on ${gateway.name}. Check carrier settings on the phone.`;
    case "unsupported_platform": return "Can't send SMS through Peppy on this iPhone. Select an Android phone to send.";
    case "capabilities_unknown": return `${gateway.name}'s capabilities are unknown. Sending is paused until the phone reports in.`;
    default: return undefined;
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
  if (content.gatewayId && content.simId) {
    const gateway = gateways.find(
      (item) => item.id === content.gatewayId && item.simId === content.simId,
    );
    return gateway
      ? { kind: "route", gateway, implicit: false }
      : { kind: "stale", gatewayId: content.gatewayId, simId: content.simId };
  }
  return gateways.length === 1
    ? { kind: "route", gateway: gateways[0], implicit: true }
    : { kind: "none" };
}

function routeProblem(
  route: RouteChoice,
  gateways: GatewayView[],
  requiresMms: boolean,
  recipientCount: number,
  estimatedBytes: number,
): string | undefined {
  if (route.kind === "none")
    return gateways.length
      ? "choose a phone and SIM"
      : "no phone is configured";
  if (route.kind === "stale")
    return "the selected phone/SIM is no longer reported; choose another route";
  const availability = availabilityProblem(route.gateway);
  if (availability) return availability;
  if (!requiresMms && !route.gateway.supportsSms)
    return "this phone/SIM does not support SMS";
  if (requiresMms && !route.gateway.supportsMms)
    return "this phone/SIM does not support MMS attachments";
  const contentVersion = route.gateway.mmsContentVersion;
  if (requiresMms && (!Number.isSafeInteger(contentVersion) || (contentVersion ?? 0) < 2))
    return "this phone/SIM needs MMS content version 2";
  const recipientLimit = Math.min(route.gateway.mmsMaxRecipients ?? 20, 20);
  if (requiresMms && recipientCount > recipientLimit)
    return `this MMS route supports up to ${recipientLimit} recipients`;
  if (requiresMms && route.gateway.mmsMaxBytes !== undefined && estimatedBytes > route.gateway.mmsMaxBytes)
    return `the MMS estimate exceeds the ${fileSize(route.gateway.mmsMaxBytes)} limit`;
  return undefined;
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

/* ------------------------------------------------------------------ presentational pieces */

type Theme = "light" | "dark" | "system";

function GatewaySelector({
  gateways,
  content,
  problem,
  onSelect,
}: {
  gateways: GatewayView[];
  content: DraftContent;
  problem?: string;
  onSelect(gateway: GatewayView): void;
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
  const presenceHint = selectedGateway && (availabilityProblem(selectedGateway) ?? (selectedAvailability === "unknown" ? `${selectedGateway.name}'s status is unknown.` : undefined));
  return (
    <section id="gateway-selector" aria-label="Phone and SIM" aria-describedby={presenceHint ? "gateway-presence-hint" : undefined} data-route-state={problem ? "blocked" : selectedAvailability === "delayed" ? "delayed" : "ready"}>
      <GatewayPicker
        options={[
          ...(route.kind === "stale"
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
            simLabel: simLabel(item),
            statusLabel: availabilityLabel(item),
            simulated: item.simulated,
            availability: gatewayAvailability(item),
            disabled: gatewayAvailability(item) === "unsupported_platform",
            gatewayId: item.id,
            simId: item.simId,
          })),
        ]}
        value={value}
        placeholder={gateways.length ? "Choose phone and SIM" : "No phone"}
        disabled={!gateways.length}
        onChange={choose}
        describedBy={presenceHint ? "gateway-presence-hint" : undefined}
      />
      <span id="gateway-presence-hint" className="visually-hidden" role="status" aria-live="polite">{presenceHint}</span>
    </section>
  );
}

function headRoutePresentation(route: RouteChoice, gateways: GatewayView[]): { sim?: string; state: string; label: string } {
  if (route.kind === "none") {
    const state = gateways.length ? "No SIM chosen" : "No phone";
    return { state, label: state };
  }
  if (route.kind === "stale") return { state: "SIM unavailable", label: "SIM unavailable" };
  const simLabel = simLabels(gateways).get(routeKey(route.gateway.id, route.gateway.simId)) ?? route.gateway.simId;
  if (gatewayAvailability(route.gateway) === "no_sms_sim") return { state: "No SMS SIM", label: "No SMS SIM" };
  let state: string;
  switch (gatewayAvailability(route.gateway)) {
    case "connected": state = "Online"; break;
    case "delayed": state = route.gateway.lastSeenAgeSeconds === undefined ? "Last seen" : `Seen ${formatAge(route.gateway.lastSeenAgeSeconds)} ago`; break;
    case "offline": state = "Not responding"; break;
    default: state = availabilityLabel(route.gateway);
  }
  return { sim: simLabel, state, label: `${simLabel} · ${state}` };
}

function HeadRoutePill({ content, gateways }: { content: DraftContent; gateways: GatewayView[] }) {
  const route = resolveRoute(content, gateways);
  const { sim, state, label } = headRoutePresentation(route, gateways);
  const tooltip = route.kind === "route"
    ? `${route.gateway.name}: choose this phone and SIM in the main window.`
    : "Choose a phone and SIM in the main window.";
  const description = route.kind === "route"
    ? `${route.gateway.name}, ${sim ?? state}. Phone and SIM are selected in the main window.`
    : "Choose a phone and SIM in the main window.";
  return <section id="head-panel-route-section" aria-label="Phone and SIM" aria-describedby="head-panel-route-description" data-route-state={route.kind === "route" ? gatewayAvailability(route.gateway) : route.kind}>
    <span id="head-panel-route" className="head-panel-route" title={tooltip} data-gateway-availability={route.kind === "route" ? gatewayAvailability(route.gateway) : undefined}>{sim && <span data-head-route-sim>{sim}</span>}{sim && <span data-head-route-separator>·</span>}<span data-head-route-state>{state}</span></span>
    <span id="head-panel-route-description" className="visually-hidden">{description}</span>
    <span className="visually-hidden" role="status" aria-live="polite">{label}</span>
  </section>;
}

function DraftRecovery({ error, onRetry }: { error: string; onRetry(): void }) {
  return (
    <section id="draft-recovery" role="alert" aria-label="Unsaved draft">
      <p>
        Draft not saved: {error} Your text is still here; sending and closing
        stay blocked until it is saved.
      </p>
      <button onClick={onRetry}>Retry save</button>
    </section>
  );
}

type RowRegistry = (id: string, row: HTMLElement | null) => void;

function MessageMeta({ message, hideTime = false }: { message: MessageView; hideTime?: boolean }) {
  const { displayedTime, deliveryTimeFallback, deliveryTimeUnavailable } = messageTime(message);
  const formattedTime = formatDateTime(displayedTime);
  const isoTime = isoDateTime(displayedTime);
  const deliveryConfirmed = message.status === "delivery-confirmed";
  const phoneHistory = message.sender === "self" && !message.status && Boolean(formattedTime);
  const compactStatus = message.status && ["gateway-persisted", "preparing", "submitted", "sent"].includes(message.status);
  const statusLabel = message.status ? STATUS_LABEL[message.status] : undefined;
  const validTime = Boolean(formattedTime && isoTime);
  const showTime = validTime && !hideTime;
  const deliveryLabel = deliveryTimeFallback
    ? hideTime
      ? "Delivery confirmed; delivery time unavailable"
      : "Delivery confirmed; sent time shown because delivery time is unavailable"
    : deliveryTimeUnavailable
      ? "Delivery confirmed; time unavailable"
      : "Delivery confirmed";
  const statusIcon = message.status === "sent" ? <Check size={10} aria-hidden /> : <Clock size={10} aria-hidden />;
  const showTextStatus = !deliveryConfirmed && Boolean(message.status) && (!compactStatus || !validTime);
  const showCompactStatus = !deliveryConfirmed && Boolean(compactStatus && validTime);
  if (!showTextStatus && !showTime && !showCompactStatus && !deliveryConfirmed && !phoneHistory) return null;
  return <div className="message-meta" data-message-meta={message.id} data-status={message.status}>
    {showTextStatus && <span className="message-meta-status">{["failed-before-submit", "failed-confirmed"].includes(message.status!) ? <X size={10} aria-hidden /> : message.status === "unknown" ? <TriangleAlert size={10} aria-hidden /> : statusIcon}{statusLabel}</span>}
    {showTextStatus && showTime && <span className="message-meta-separator" aria-hidden>·</span>}
    {showTime && <time dateTime={isoTime}>{formattedTime}</time>}
    {showCompactStatus && <span className="message-meta-status" role="img" aria-label={statusLabel} title={statusLabel}>{statusIcon}</span>}
    {deliveryConfirmed && <span className="message-meta-status" role="img" aria-label={deliveryLabel} title={deliveryLabel}><CheckCheck size={10} aria-hidden /></span>}
    {phoneHistory && <span className="message-meta-status" role="img" aria-label="From phone history" title="From phone history" data-message-phone-icon={message.id}><Smartphone size={10} aria-hidden /></span>}
  </div>;
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
  noSelectionFeedback,
  onOpenMedia,
  requestOriginal,
  checkOriginal,
  releaseMedia,
  mediaRetryKey,
  onOpenExternal,
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
  actorName,
  onOpenReactionTarget,
  pendingReactionId,
  reactionFeedback,
  seenReactions,
  badgeObservation,
  captureReactionLayout,
  restoreReactionLayout,
  toneOverrides = {},
}: {
  conversation?: ConversationView;
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
  noSelectionFeedback?: ReactNode;
  onOpenMedia(item: { attachment: AttachmentView; message: MessageView }): void;
  requestOriginal(id: string): Promise<{ state: "ready"; url: string } | { state: "pending" }>;
  checkOriginal(id: string): Promise<{ state: "ready"; url: string } | { state: "pending" }>;
  releaseMedia(id: string, url: string): void;
  mediaRetryKey: unknown;
  onOpenExternal?: (url: string) => Promise<void>;
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
  actorName: ActorName;
  onOpenReactionTarget(reaction: MessageView, notice: ReactionNoticeInfo): void;
  pendingReactionId?: string;
  reactionFeedback?: { reactionId: string; message: string };
  seenReactions: SeenReactionLedger;
  badgeObservation: { root: HTMLElement | null; obscuredBottom: number | null; enabled: boolean; canRecord(): boolean; onSeen(ids: readonly string[]): void };
  captureReactionLayout(): ReactionLayoutSnapshot | null;
  restoreReactionLayout(snapshot: ReactionLayoutSnapshot): void;
  toneOverrides?: Record<string, number>;
}) {
  const reactions = useMemo(() => showReactions && conversation ? linkReactions(conversation.messages) : NO_REACTIONS, [showReactions, conversation?.messages]);
  const [focusedNoticeId, setFocusedNoticeId] = useState<string>();
  const chipRef = useSeenBadges({
    ...badgeObservation,
    badges: [...reactions.badges].flatMap(([id, badges]) => badges.map(badge => ({ key: JSON.stringify([id, badge.emoji]), reactionIds: badge.reactionIds }))),
  });
  const suppressed = new Set(conversation?.messages.filter(message => reactions.notices.has(message.id)
    && seenReactions.has(conversation.id, message.id) && !showsReactionNoticeMeta(message)
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
          {noSelectionFeedback && <div id="no-selection-feedback">{noSelectionFeedback}</div>}
        </div>
      </div>
    );
  const conversationToneSeed = conversationAvatar({ id: conversation.id, name: conversation.name, participants: conversation.participants, recipientless: conversation.recipientlessDraft }).seed;
  const visualMessages = conversation.messages.filter(message => !suppressed.has(message.id));
  const timeIsShown = (message: MessageView) => {
    const displayedTime = messageTime(message).displayedTime;
    return Boolean(formatDateTime(displayedTime) && isoDateTime(displayedTime));
  };
  const hiddenTimes = compactTimestamps
    ? hiddenTimeIds(visualMessages, (message) => timeIsShown(message) && !(reactions.notices.has(message.id) && !showsReactionNoticeMeta(message)))
    : new Set<string>();
  const hiddenTimeTitle = (message: MessageView) => hiddenTimes.has(message.id)
    ? formatDateTime(messageTime(message).displayedTime) ?? undefined
    : undefined;
  const isGroup = (conversation.participants?.length ?? 0) > 1;
  const messageGroups = groupMessages(visualMessages);
  const senderEdges = isGroup
    ? senderRunEdges(messageGroups, {
      key: (group) => group.id,
      identity: (group) => {
        const message = group.messages[0];
        if (message.sender === "self") return { kind: "outgoing" };
        if (reactions.notices.has(message.id)) return { kind: "boundary" };
        return { kind: "incoming", address: message.senderAddress };
      },
      endsRunAfter: (group) => {
        if (!compactTimestamps) return false;
        const message = group.messages.at(-1)!;
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
    ) : <span className="message-sender-label" data-plain data-sender-label={message.senderAddress} data-sender-known={label.isName} aria-hidden="true" title={label.text}>{label.text}</span> : undefined;
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
      <div id="message-list-content" aria-live={suppressHistoryAnnouncements ? "off" : undefined}>
        {conversation.messages.filter(message => suppressed.has(message.id)).map(message => <ReactionNotice key={message.id} reaction={message} notice={reactions.notices.get(message.id)!} actorName={actorName} rowRef={row => registerRow(message.id, row)} onOpen={onOpenReactionTarget} suppressedHint />)}
        {historyExhausted && <p id="history-beginning-marker" className="history-load-band" data-history-state="beginning">Beginning of conversation</p>}
        {!conversation.messages.length && <p>Start the conversation.</p>}
        {messageGroups.map((group) => {
          const message = group.messages[0];
          const reactionNotice = reactions.notices.get(message.id);
          const imageRun = group.messages.every((candidate) => candidate.transport === "mms" && candidate.attachments.length > 0 && candidate.attachments.every((attachment) => attachment.mediaType.startsWith("image/")));
          const sender = senderRun(group);
          if (imageRun) return <MediaGrid key={group.id} messages={group.messages} onOpen={onOpenMedia} renderMessageMeta={(candidate) => <MessageMeta message={candidate} hideTime={hiddenTimes.has(candidate.id)} />} renderCaption={(candidate) => <MessageText text={candidate.body} onOpenExternal={onOpenExternal} />} messageRef={(candidate) => (row) => registerRow(candidate.id, row)} messageTitle={hiddenTimeTitle} senderRun={(candidate) => sender && candidate.id === group.id ? { label: sender.labelNode, avatar: sender.avatar, gutter: showSenderAvatars, ariaName: sender.label.text, start: sender.edge.start, end: sender.edge.end } : undefined} />;
          if (reactionNotice) return <ReactionNotice key={message.id} reaction={message} notice={reactionNotice} actorName={actorName} rowRef={(row) => registerRow(message.id, row)} onOpen={onOpenReactionTarget} busy={pendingReactionId === message.id} feedback={reactionFeedback?.reactionId === message.id ? reactionFeedback.message : undefined} meta={showsReactionNoticeMeta(message) ? <MessageMeta message={message} /> : undefined} />;
          const badges = reactions.badges.get(message.id);
          const bubble = (
            <article
              ref={(row) => registerRow(message.id, row)}
              data-message-id={message.id}
              data-search-target={searchTargetId === message.id || undefined}
              aria-describedby={searchTargetId === message.id ? "search-result-message-hint" : undefined}
              tabIndex={searchTargetId === message.id ? -1 : undefined}
              className={`message-bubble ${message.sender}`}
              data-peppy-tone={message.sender === "other" || undefined}
              style={message.sender === "other" ? toneStyle(incomingToneSeed(conversationToneSeed, message.senderAddress), toneOverrides) : undefined}
              title={hiddenTimeTitle(message)}
            >
              {sender && <span className="sr-only">From {sender.label.text}: </span>}
              {message.body && <p><MessageText text={message.body} onOpenExternal={onOpenExternal} /></p>}
              {message.subject && <p className="message-subject">Subject: {message.subject}</p>}
              {message.attachments.map((file) => {
                const copy = publicCopies[file.id];
                if (originalPending(file)) return <MediaTile key={file.id} attachment={file} message={message} variant="single" onOpen={() => onOpenMedia({ attachment: file, message })} />;
                const shareable =
                  file.state === "ready" && file.mediaType.startsWith("image/");
                if (file.mediaType.startsWith("video/") || file.mediaType.startsWith("audio/")) return <InlineMedia key={file.id} attachment={file} requestOriginal={requestOriginal} checkOriginal={checkOriginal} releaseMedia={releaseMedia} retryKey={mediaRetryKey} />;
                return (
                  <div
                    key={file.id}
                    data-attachment-id={file.id}
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
                    <span>{file.name}</span>
                    <small>
                      {fileSize(file.byteSize)} · {attachmentState(file)}
                    </small>
                    {file.retryable && file.transfer && (
                      <button className="secondary-button" onClick={() => onRetryAttachment(file)} aria-label={`Retry ${file.transfer} for ${file.name}`}>
                        Retry {file.transfer}
                      </button>
                    )}
                    {(!file.mediaType.startsWith("image/") || file.state === "ready") && (
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
              })}
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
              {badges && <ReactionBadges messageId={message.id} badges={badges} actorName={actorName} chipRef={chipRef} />}
              <MessageMeta message={message} hideTime={hiddenTimes.has(message.id)} />
            </div>
          );
        })}
      </div>
    </div>
    </ReactionHintLayout>
  );
}

/** Only an enrolled owner can add a phone; joined devices keep the normal conversation view. */
function ownerWithoutPhone(snapshot: DesktopSnapshot) {
  return snapshot.connection.state === "connected" && snapshot.encryption.state === "unlocked" && snapshot.gateways.length === 0 && snapshot.deviceRole === "owner";
}
const phoneDeferredKey = (origin: string | undefined) => origin ? `peppy.setup.phoneDeferred:${origin}` : "";
const phoneSetupDeferred = (origin: string | undefined) => {
  const key = phoneDeferredKey(origin);
  return Boolean(key && localStorage.getItem(key));
};

type SettingsAction = "origin" | "unlock" | "account";

function SettingsView({
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
  navigationPosition,
  onNavigationPosition,
  compactTimestamps,
  onCompactTimestamps,
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
  onLock,
  appLock,
  onAppLock,
  browserHost = false,
  accountUrl,
  canRevealVaultSecret = false,
  serverOffline = false,
  requestedSection,
  onRequestedSectionConsumed,
}: {
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
  navigationPosition: NavigationPosition;
  onNavigationPosition(position: NavigationPosition): void;
  compactTimestamps: boolean;
  onCompactTimestamps(enabled: boolean): void;
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
  onLock?: () => Promise<void>;
  appLock: AppLockStatus | null;
  onAppLock(enabled: boolean): Promise<boolean>;
  browserHost?: boolean;
  accountUrl?: string;
  canRevealVaultSecret?: boolean;
  serverOffline?: boolean;
  requestedSection?: string;
  onRequestedSectionConsumed?(): void;
}) {
  const [savingStartup, setSavingStartup] = useState(false);
  const [startupError, setStartupError] = useState("");
  const [pendingAction, setPendingAction] = useState<SettingsAction | null>(null);
  const [actionError, setActionError] = useState<{ action: SettingsAction; message: string } | null>(null);
  const [revealError, setRevealError] = useState<string | null>(null);
  const nativeUpdateHost = !browserHost && isTauriUpdateHost();
  const settingsContentRef = useRef<HTMLDivElement>(null);
  const pendingSectionRef = useRef<string | null>(null);
  const scrollEndTimer = useRef<number | undefined>(undefined);
  const [activeSection, setActiveSection] = useState("server");
  const syncText = syncStatusText(encryption);

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
  const sections = [
    { id: "server", label: "Server" },
    { id: "sync", label: "Sync encryption" },
    { id: "devices", label: "Devices" },
    { id: "notifications", label: "Notifications" },
    { id: "messages", label: "Messages" },
    { id: "theme", label: "Appearance" },
    ...(desktop?.startupSupported ? [{ id: "startup", label: "Startup" }] : []),
    ...(nativeUpdateHost ? [{ id: "updates", label: "Updates" }] : []),
    ...(browserHost ? [] : [{ id: "app-lock", label: "App lock" }]),
    ...(hasAccount ? [{ id: "account-billing", label: "Account" }] : []),
    { id: "privacy", label: "Privacy" },
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
    const section = document.getElementById(sectionElementId(id));
    setActiveSection(id);
    pendingSectionRef.current = id;
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
      <div id="settings-layout">
      <nav id="settings-navigation" aria-label="Settings categories">
        <label className="settings-category-selector">
          <span className="visually-hidden">Settings category</span>
          <select value={activeSection} onChange={(event) => selectSection(event.target.value)}>
            {sections.map((section) => <option key={section.id} value={section.id}>{section.label}</option>)}
          </select>
        </label>
        <div className="settings-category-links">
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
      <DevicesSettings serverOffline={serverOffline} />
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
        soundSettings={!browserHost && bridge.get_notification_sound && bridge.set_notification_sound
          ? { load: bridge.get_notification_sound, save: bridge.set_notification_sound } : undefined}
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
        <fieldset id="settings-navigation-position" aria-describedby="settings-navigation-position-hint">
          <legend>Navigation position</legend>
          <p id="settings-navigation-position-hint" className="settings-field-hint">Choose where the navigation buttons appear.</p>
          <label className="settings-check">
            <input
              type="radio"
              name="navigation-position"
              value="side-rail"
              checked={navigationPosition === "side-rail"}
              onChange={() => onNavigationPosition("side-rail")}
            />
            Side rail
          </label>
          <label className="settings-check">
            <input
              type="radio"
              name="navigation-position"
              value="title-bar"
              checked={navigationPosition === "title-bar"}
              onChange={() => onNavigationPosition("title-bar")}
            />
            Title bar
          </label>
        </fieldset>
        <div id="settings-compact-timestamps-group">
          <label className="settings-check">
            <input id="settings-compact-timestamps" type="checkbox" checked={compactTimestamps} aria-describedby="settings-compact-timestamps-hint" onChange={(event) => onCompactTimestamps(event.target.checked)} />
            Show times only after a pause
          </label>
          <p id="settings-compact-timestamps-hint" className="settings-field-hint">Hides times on messages sent within an hour of the next one.</p>
        </div>
        <div id="settings-group-sender-avatars-group">
          <label className="settings-check">
            <input id="settings-group-sender-avatars" type="checkbox" checked={groupSenderAvatars} aria-describedby="settings-group-sender-avatars-hint" onChange={(event) => onGroupSenderAvatars(event.target.checked)} />
            Show sender avatars in group conversations
          </label>
          <p id="settings-group-sender-avatars-hint" className="settings-field-hint">Shows a photo or initials beside the last message in each stretch from one person. Names always show.</p>
        </div>
      </section>
      {desktop?.startupSupported && <section id="settings-startup" className="settings-card" data-settings-section="startup">
        <h2>Startup</h2>
        <label className="settings-check"><input id="start-at-login" type="checkbox" checked={desktop.startAtLogin} disabled={savingStartup} onChange={async event => {
          setSavingStartup(true); setStartupError("");
          try { await onStartAtLogin(event.target.checked); }
          catch (error) { setStartupError(errorText(error)); }
          finally { setSavingStartup(false); }
        }} /> Start at login</label>
        {startupError && <p className="settings-error" role="alert">Start at login was not changed: {startupError}</p>}
      </section>}
      {nativeUpdateHost && <UpdatesSettings />}
      {!browserHost && <AppLockSettings status={appLock} onChange={onAppLock} />}
      {hasAccount && <section id="settings-account-billing" className="settings-card" data-settings-section="account-billing">
        <h2>Account &amp; billing</h2>
        <p>Manage your Peppy subscription and account on Peppy&rsquo;s website.</p>
        {!browserHost && <HostedAccountSettings />}
        {!browserHost ? <button className="secondary-button" disabled={Boolean(pendingAction)} aria-busy={actionPending("account") || undefined} onClick={() => void runAction("account")}>Open account &amp; billing</button> : <a id="settings-account-billing-link" href={accountUrl} target="_blank" rel="noopener noreferrer" className="secondary-button">Open account &amp; billing<span className="visually-hidden"> (opens in a new tab)</span></a>}
        {actionAlert("account")}
      </section>}
      <PrivacySettings browserHost={browserHost} />
      </div>
      </div>
    </section>
  );
}

function StatusDetails({
  connection,
  encryption,
  serverUnreachable = false,
}: {
  connection?: DesktopSnapshot["connection"];
  encryption?: DesktopSnapshot["encryption"]["state"];
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
  return (
    <div className="status-details" data-status-details>
      <div className="status-details-row" data-disclosure="sync-state" data-sync-state={encryption} title={text}>
        {icon}<span>{text}</span>
      </div>
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

/* ------------------------------------------------------------------ app */

export function App({ hostKind = "native", fixedOrigin, accountUrl }: { hostKind?: "native" | "browser"; fixedOrigin?: string; accountUrl?: string } = {}) {
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
  const [snapshot, setSnapshot] = useState<DesktopSnapshot | null>(null);
  const [conversationTab, setConversationTab] = useState<"messages" | "media">("messages");
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
  appLockRef.current = appLock;
  const [mode, setMode] = useState<"hosted" | "self-hosted">(() => browserHost ? "self-hosted" : savedSetupMode());
  const headPanel = Boolean(composerConversation && (snapshot?.head.panel ?? headPanelBootstrap));
  const [selected, setSelected] = useState(composerConversation ?? "");
  const [attachmentViews, setAttachmentViews] = useState<
    Record<string, AttachmentView>
  >({});
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
  const persistedTheme = useRef(theme);
  useEffect(() => {
    if (theme === persistedTheme.current) return;
    persistedTheme.current = theme;
    persistLayoutPatch({ theme });
  }, [theme]);
  useEffect(() => {
    const themeClasses = ["theme-light", "theme-dark", "theme-system"];
    document.body.classList.remove(...themeClasses);
    document.body.classList.add(`theme-${theme}`);
    return () => document.body.classList.remove(`theme-${theme}`);
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
  const [compactTimestamps, setCompactTimestamps] = useState(readCompactTimestamps);
  const [groupSenderAvatars, setGroupSenderAvatars] = useState(readGroupSenderAvatars);
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
  const [contactsHeaderHost, setContactsHeaderHost] = useState<HTMLDivElement | null>(null);
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
      setActiveViewNow(view);
    };
    if (contactNavigation.current) contactNavigation.current(navigate);
    else navigate();
  }, []);
  const settingsOpen = activeView === "settings";
  const notificationsOpen = activeView === "notifications";
  const notificationsLocked = snapshot?.encryption.state !== "unlocked";
  const dismissibleNotificationsCount = dismissibleNotifications(snapshot?.notifications ?? [], snapshot?.appFilters ?? [], notificationsLocked).length;
  const contactsOpen = activeView === "contacts";
  const [notificationSettingsRequested, setNotificationSettingsRequested] = useState(false);
  const dismissingNotificationsRef = useRef(false);
  const [actionNotice, setActionNotice] = useState<ActionNotice>();
  const [refreshNotice, setRefreshNotice] = useState("");
  const [composerFeedback, setComposerFeedback] = useState<ComposerFeedback>();
  const [notificationActionNotice, setNotificationActionNotice] = useState("");
  const [notificationFocus, setNotificationFocus] = useState<{ requestId: string; target: NotificationActivationTarget }>();
  const notificationFocusContext = useRef<{ requestId: string; context: ActivationContext; reported?: boolean } | undefined>(undefined);
  const notificationDrain = useRef<ReturnType<typeof createNotificationActivationDrain> | undefined>(undefined);
  const notificationNavigation = useRef<{ requestId: string; sessionGeneration: string; generation: number; epoch: number } | undefined>(undefined);
  const notificationHandler = useRef<(request: NotificationActivation, context: ActivationContext) => Promise<void>>(async () => {});
  const [dismissingNotifications, setDismissingNotifications] = useState(false);
  const [addingPhone, setAddingPhone] = useState(false);
  const [initialEnrollmentActive, setInitialEnrollmentActive] = useState(false);
  const [phoneDeferredOrigin, setPhoneDeferredOrigin] = useState<string | null>(null);
  useEffect(() => {
    if (snapshot?.connection.state === "connected") setInitialEnrollmentActive(false);
  }, [snapshot?.connection.state]);
  // Names of contacts picked from search, shown on chips until the next native snapshot
  // resolves the same addresses. Display only; recipient IDs stay phone addresses.
  const [chosenContacts, setChosenContacts] = useState<ContactResolutionMap>({});
  const rememberContact = useCallback((suggestion: RecipientSuggestion) => {
    setChosenContacts((current) => ({
      ...current,
      [suggestion.id]: { contactId: "", bookId: "", displayName: suggestion.label, photoDataUrl: suggestion.avatarUrl },
    }));
  }, []);
  const [origin, setOrigin] = useState(() => browserHost ? fixedOrigin ?? "" : "");
  const [loading, setLoading] = useState(true);
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
  const [deleteConfirmationId, setDeleteConfirmationId] = useState<string | null>(null);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [pendingPinId, setPendingPinId] = useState<string | null>(null);
  const [storedConflict, setStoredConflict] = useState<StoredDraftConflict | null>(null);
  const [resolutionPendingKey, setResolutionPendingKey] = useState<string | null>(null);
  const [conflictFeedback, setConflictFeedback] = useState<string>();
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
  const messageListRef = useRef<HTMLDivElement>(null);
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
  const conflictFocusIntentRef = useRef<StoredDraftConflict | null>(null);
  const pinFocusRestoreRef = useRef<PinFocusRestoreIntent | undefined>(undefined);
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
  const recoveryRestoreSequence = useRef(0);
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
  useLayoutEffect(() => {
    const intent = conflictFocusIntentRef.current;
    if (!intent || intent !== storedConflict) return;
    conflictFocusIntentRef.current = null;
    document.getElementById("draft-conflict-comparison-title")?.focus();
  }, [storedConflict]);
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
    unavailable: (_from: string, _to?: string, _message?: string) => {},
    close: () => {},
    collapse: () => {},
    closeHead: () => {},
  });
  const storeRef = useRef<DraftStore | null>(null);
  if (!storeRef.current)
    storeRef.current = new DraftStore((input) => bridge.save_draft(input), {
      changed: () => rerender(),
      rekeyed: (from, to) => handlers.current.rekeyed(from, to),
      unavailable: (from, to, message) => handlers.current.unavailable(from, to, message),
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
  const changeCompactTimestamps = (enabled: boolean) => {
    setCompactTimestamps(enabled);
    persistLayout({ compactTimestamps: enabled });
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
      const chrome = composerArea.offsetHeight - textarea.offsetHeight;
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
    return () => observer.disconnect();
  }, [activeView, headPanel, loading, Boolean(snapshot), snapshot?.connection.state, Boolean(selected)]);
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
  const setNotice = (message: string, severity: ActionNotice["severity"] = "error", persistent = false) =>
    setActionNotice(message ? { message, severity, persistent } : undefined);
  const setComposerNotice = (key: string, message: string, severity: ComposerFeedback["severity"] = "error") =>
    setComposerFeedback(message ? { key, message, severity } : undefined);
  const reportRefresh = (prefix: string) => (error: unknown) =>
    setRefreshNotice(`${prefix}${errorText(error)}`);
  const reportAction = (prefix: string) => (error: unknown) =>
    setNotice(`${prefix}${errorText(error)}`);
  const prepareFocusRestore = (owner: HTMLElement | null = document.activeElement instanceof HTMLElement ? document.activeElement : null) => {
    focusRestorePendingRef.current = Boolean(owner?.closest("#app-notice-banner, #draft-recovery"));
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
            const media = await bridge.open_media(attachmentId);
            if (!mountedRef.current) return;
            if (media.state === "pending") continue;
            await bridge.save_attachment(attachmentId);
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
    const detached = detachedHistoryRef.current?.conversationId === conversationId
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
    if (
      mountedRef.current &&
      sessionKey === currentSearchSessionKey() &&
      state.encryption.state !== "unlocked"
    )
      setSnapshot(state);
    if (
      !mountedRef.current ||
      sequence !== loadSequence.current ||
      sessionKey !== currentSearchSessionKey() ||
      (detached && detachedHistoryRef.current !== detached)
    ) return;

    if (detached && (detached.pendingLoad || !detached.messageIds.length)) {
      setSnapshot((current) => {
        if (!current || current.activeConversationId !== conversationId) return state;
        const currentConversation = current.conversations.find((item) => item.id === conversationId);
        return {
          ...state,
          activeConversationId: conversationId,
          conversations: state.conversations.map((item) => item.id === conversationId && currentConversation
            ? { ...item, messages: currentConversation.messages, hasOlderMessages: detached.hasOlder }
            : item),
        };
      });
      if (state.activeConversationId) store.adopt(state.activeConversationId, state.draft);
      setRefreshNotice("");
      return;
    }

    const refreshedWindow = detached
      ? await bridge.load_message_window({ direction: "refresh", conversationId, messageIds: detached.messageIds.slice(0, DETACHED_HISTORY_LIMIT) })
      : undefined;
    if (
      !mountedRef.current ||
      sequence !== loadSequence.current ||
      sessionKey !== currentSearchSessionKey() ||
      (detached && detachedHistoryRef.current !== detached)
    ) return;

    const refreshed = state.conversations.find((item) => item.id === state.activeConversationId);
    let refreshedDetached: DetachedHistory | undefined;
    const shouldPreserveScrollPosition = Boolean(!nearBottom.current && !historyLoadingRef.current);
    if (shouldPreserveScrollPosition) historyAnchorRef.current = captureHistoryAnchor(historyFromMessageId);
    const updateSnapshot = () => setSnapshot((current) => {
      if (detached && refreshedWindow && current?.activeConversationId === conversationId) {
        const currentMessages = current.conversations.find((item) => item.id === conversationId)?.messages ?? [];
        const detachedMessages = mergeDetachedMessages(currentMessages, refreshedWindow.messages, "refresh").messages;
        refreshedDetached = {
          ...detached,
          messageIds: detachedMessages.map((message) => message.id),
          hasOlder: refreshedWindow.hasOlder,
          hasNewer: refreshedWindow.hasNewer,
        };
        return {
          ...state,
          activeConversationId: conversationId,
          conversations: state.conversations.map((item) => item.id === conversationId
            ? { ...item, messages: detachedMessages, hasOlderMessages: refreshedWindow.hasOlder }
            : item),
        };
      }
      const previous = historyFromMessageId && current && current.activeConversationId === state.activeConversationId
        ? current.conversations.find((item) => item.id === state.activeConversationId)
        : undefined;
      if (!previous || !refreshed) return state;
      return {
        ...state,
        conversations: state.conversations.map((item) => item.id === refreshed.id
          ? { ...item, messages: mergeAnchoredMessages(previous.messages, refreshed.messages) }
          : item),
      };
    });
    if (shouldPreserveScrollPosition) {
      flushSync(updateSnapshot);
      correctHistoryAnchorScroll(historyAnchorRef.current);
    } else {
      flushSync(updateSnapshot);
    }
    if (refreshedDetached && detachedHistoryRef.current === detached) setDetached(refreshedDetached);
    if (latestRequiredConversationRef.current === conversationId && !detached) {
      latestRequiredConversationRef.current = undefined;
    }
    if (state.activeConversationId) store.adopt(state.activeConversationId, state.draft);
    setRefreshNotice("");
    if (!composerConversation && !selectedRef.current && state.activeConversationId) choose(state.activeConversationId);
    const responseHasOlder = refreshedDetached?.hasOlder ?? refreshed?.hasOlderMessages ?? false;
    const currentAnchor = snapshotRef.current?.activeConversationId === conversationId
      ? snapshotRef.current.conversations.find((item) => item.id === conversationId)?.messages[0]?.id
      : undefined;
    if (currentAnchor === historyFromMessageId || !currentAnchor || detached) {
      hasOlderMessagesRef.current = responseHasOlder;
      setHasOlderMessages(responseHasOlder);
      if (responseHasOlder) setHistoryExhausted(false);
    }
  };
  const startRefresh = (request: RefreshRequest) => {
    refreshInFlightRef.current = true;
    setRefreshing(true);
    const conversationId = request.explicit ? request.conversationId : selectedRef.current;
    const run = performRefresh(conversationId);
    run.then(
      () => request.waiters.forEach(({ resolve }) => resolve()),
      (error) => request.waiters.forEach(({ reject }) => reject(error)),
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
    if (state.activeConversationId !== conversationId) {
      latestRequiredConversationRef.current = conversationId;
      setSnapshot({ ...state, activeConversationId: undefined });
      choose("");
      setNotice("The selected conversation is unavailable. Retry from search.", "warning");
      return;
    }
    latestRequiredConversationRef.current = undefined;
    nearBottom.current = true;
    setSnapshot(state);
    if (state.activeConversationId === conversationId) store.adopt(conversationId, state.draft);
    setNewComposeKey(null);
    setNotice(unavailableNotice);
    const conversation = state.conversations.find(item => item.id === conversationId);
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
      store.adopt(hit.conversationId, metadata.draft);
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
  handlers.current.unavailable = (from, to, message) => {
    if (selectedRef.current === from && to) choose(to);
    else if (selectedRef.current === from) choose("");
    if (to) void refresh(to).catch(reportRefresh(""));
  };

  useEffect(() => {
    mountedRef.current = true;
    void refresh(composerConversation ?? undefined)
      .catch(reportRefresh(""))
      .finally(() => setLoading(false));
    const unsubscribe = bridge.subscribe(() => {
      setMediaStateHint(current => current + 1);
      setStoredConflict(null);
      void retryPendingSaves();
      void refresh().then(() => notificationDrain.current?.retry()).catch(reportRefresh(""));
    });
    const unsubscribeDrafts = bridge.subscribe_drafts?.((draft) => {
      if (mountedRef.current && snapshotRef.current?.encryption.state === "unlocked" && !appLockRef.current?.locked) {
        setStoredConflict(null);
        store.applyAuthoritative(draft);
      }
    }) ?? (() => undefined);
    return () => {
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
    };
  }, []);
  useEffect(() => {
    if (snapshot?.encryption.state !== "locked" && !appLock?.locked) return;
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
        setNotificationActionNotice("");
        setNotice("");
        flushSync(() => {
          setActiveViewNow("notifications");
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
      setNotificationActionNotice("");
      if (destination.kind !== "conversation") return;
      const found = state.conversations.find(item => item.id === conversationId);
      clearDetached();
      flushSync(() => {
        setSnapshot(state);
        setActiveViewNow("conversations");
        choose(found ? destination.conversationId : "");
        setNewComposeKey(null);
        setNotice(found ? "" : "This conversation is no longer available.", "warning");
        if (found) {
          store.adopt(destination.conversationId, state.draft);
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
  }, [renderedSearchSessionKey, loading, appLock?.locked]);
  useEffect(() => {
    if (activeView === "notifications") return;
    notificationFocusContext.current = undefined;
    setNotificationFocus(undefined);
  }, [activeView]);
  const notificationFocusResult = (requestId: string, found: boolean) => {
    const pending = notificationFocusContext.current;
    if (pending?.requestId !== requestId || pending.reported) return;
    pending.reported = true;
    void (async () => {
      if (!await pending.context.validate()) return;
      if (!found) setNotificationActionNotice("This notification is no longer available.");
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
    if (previousSearchSessionKeyRef.current === renderedSearchSessionKey) return;
    previousSearchSessionKeyRef.current = renderedSearchSessionKey;
    invalidateReactionNavigation();
    const detachedConversationId = detachedHistoryRef.current?.conversationId;
    clearDetached();
    setContactTarget(undefined);
    if (detachedConversationId && snapshotRef.current?.encryption.state === "unlocked" && !appLock?.locked) {
      void refresh(detachedConversationId).catch(reportRefresh(""));
    }
  }, [renderedSearchSessionKey]);
  useEffect(() => {
    if (browserHost) return;
    void bridge.get_app_lock_status().then(setAppLock).catch(() => setAppLock(null));
  }, [browserHost]);
  const changeAppLock = async (enabled: boolean): Promise<boolean> => {
    const changed = await bridge.set_app_lock(enabled);
    setAppLock(await bridge.get_app_lock_status());
    return changed;
  };
  const unlockApp = async (): Promise<boolean> => {
    const unlocked = await bridge.unlock_app();
    if (unlocked) {
      setAppLock(await bridge.get_app_lock_status());
      await refresh();
    }
    return unlocked;
  };

  /* Only rows that are actually visible while the window is focused are marked seen. */
  const slotKey = store.resolve(selected);
  useEffect(() => {
    setPendingRecipient(false);
    setComposerFeedback(current => current?.key === slotKey ? current : undefined);
  }, [slotKey]);
  const active = snapshot?.conversations.find(
    (conversation) => conversation.id === slotKey,
  );
  useEffect(() => {
    setConversationTab("messages");
    setLightboxItems(null);
    setLightboxAttachmentId(undefined);
    setLightboxMessageId(undefined);
    gallerySessionRef.current += 1;
    pendingSaveIdsRef.current.clear();
    galleryConversationRef.current = active?.id;
    galleryCacheRef.current = { conversationId: active?.id, items: [] };
  }, [active?.id, appLock?.locked, snapshot?.encryption.state]);
  const conversationLoaded =
    Boolean(active) && snapshot?.activeConversationId === slotKey;
  const seenReactions = useMemo(() => createSeenReactionLedger({ mode: snapshot?.mode ?? "", origin: snapshot?.connection.origin }), [snapshot?.mode, snapshot?.connection.origin]);
  const reactionScopeKey = JSON.stringify([renderedSearchSessionKey, slotKey]);
  const reactionReadEnabled = conversationLoaded && activeView === "conversations" && conversationTab === "messages" && snapshot?.encryption.state === "unlocked" && !appLock?.locked;
  const reactionReadContext = useRef({ key: reactionScopeKey, enabled: reactionReadEnabled });
  reactionReadContext.current = { key: reactionScopeKey, enabled: reactionReadEnabled };
  const canReadMessages = () => mountedRef.current && selectedRef.current === slotKey && currentSearchSessionKey() === renderedSearchSessionKey
    && reactionReadContext.current.key === reactionScopeKey && reactionReadContext.current.enabled
    && !searchReadGate.current && document.hasFocus() && document.visibilityState === "visible";
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
  useEffect(() => seenReactions.subscribe(() => setReactionSeenRevision(value => value + 1)), [seenReactions]);
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
    const previousScrollTop = messageListScrollTopRef.current;
    const currentScrollTop = element.scrollTop;
    messageListScrollTopRef.current = currentScrollTop;
    if (element.clientHeight === 0) return;
    nearBottom.current = isNearMessageListBottom(element);
    if (currentScrollTop < previousScrollTop) autoLoadOlderMessages(element);
  };
  const onMessageWheel = (event: WheelEvent<HTMLDivElement>) => {
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
  }, [slotKey, conversationLoaded]);
  useLayoutEffect(() => {
    if (nearBottom.current && !searchReadGate.current && !detachedHistoryRef.current) stickToBottom();
  }, [messageIds]);
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
    const list = messageListRef.current;
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
  }, [activeView, slotKey, conversationLoaded, headPanel]);
  useEffect(() => {
    seenMessageIds.current = new Set<string>();
    pendingSeenMessageIds.current = new Set<string>();
    badgeReadIds.current = new Set<string>();
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
  const recoveries = store.recoveriesFor(slotKey || undefined);
  const content = slot?.content ?? blankContent();
  const gateways = snapshot?.gateways ?? [];
  const route = resolveRoute(content, gateways);
  const mmsRequired = requiresMms(content, active);
  const routeRecipients = recipientCount(content, active);
  const estimateBytes = draftEstimateBytes(content, attachmentViews);
  const problem = routeProblem(
    route,
    gateways,
    mmsRequired,
    routeRecipients,
    estimateBytes,
  );
  const isNewConversation = Boolean(slotKey) && (
    isLocalDraftKey(slotKey) ||
    (newComposeKey !== null && store.resolve(newComposeKey) === slotKey) ||
    active?.draftOnly === true
  );

  const edit = (patch: Partial<DraftContent>) => {
    if (slotKey && deletingId !== slotKey) void store.edit(slotKey, patch);
  };
  const commitRecipients = (recipientIds: string[]) => {
    if (!slotKey) return;
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
      bridge.list_contacts,
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

  const restoreRecovery = async (recovery: DraftRecovery, feedbackKey: string) => {
    const destination = recovery.destination;
    if (!destination) return;
    const requestSequence = ++recoveryRestoreSequence.current;
    const sessionKey = currentSearchSessionKey();
    const initialSlot = store.get(destination);
    const initialGeneration = initialSlot?.generation;
    const initialRevision = initialSlot?.revision;
    try {
      const state = await bridge.load_state(destination);
      if (
        !mountedRef.current ||
        requestSequence !== recoveryRestoreSequence.current ||
        sessionKey !== currentSearchSessionKey() ||
        !store.recoveriesFor(destination).some((candidate) => candidate.id === recovery.id)
      ) return;
      const currentSlot = store.get(destination);
      const slotChanged = initialSlot
        ? currentSlot !== initialSlot || currentSlot.generation !== initialGeneration || currentSlot.revision !== initialRevision
        : Boolean(currentSlot && (isDirty(currentSlot) || currentSlot.id !== state.draft?.id || currentSlot.revision !== (state.draft?.revision ?? "0")));
      if (
        slotChanged ||
        state.activeConversationId !== destination ||
        (state.draft && state.draft.conversationId !== destination)
      ) {
        setComposerNotice(feedbackKey, "Retained text was not applied because the destination draft changed or could not be verified.");
        return;
      }
      store.adopt(destination, state.draft);
      const outcome = await store.edit(destination, recovery.content);
      if (outcome.ok) store.discardRecovery(recovery.id);
      else setComposerNotice(feedbackKey, `Retained text was not applied: ${outcome.error}`);
    } catch (error) {
      if (
        mountedRef.current &&
        requestSequence === recoveryRestoreSequence.current &&
        sessionKey === currentSearchSessionKey()
      ) setComposerNotice(feedbackKey, `Retained text was not applied: ${errorText(error)}`);
    }
  };

  const retrySave = async () => {
    const current = store.get(slotKey);
    if (!current) return;
    if (current.error?.code === "stale-draft") {
      if (!current.conversationId) return;
      setResolutionPendingKey(current.key);
      setConflictFeedback(undefined);
      try {
        const state = await bridge.load_state(current.conversationId);
        const currentSlot = store.get(current.key);
        if (!mountedRef.current) return;
        if (currentSlot !== current || state.activeConversationId !== current.conversationId || (state.draft && state.draft.conversationId !== current.conversationId)) {
          setConflictFeedback("The saved draft could not be verified. Your local draft remains unchanged.");
          return;
        }
        const conflict = { key: current.key, draft: state.draft, generation: current.generation, revision: current.revision };
        conflictFocusIntentRef.current = conflict;
        setStoredConflict(conflict);
      } catch (error) {
        setConflictFeedback(errorText(error));
      } finally {
        if (mountedRef.current) setResolutionPendingKey(null);
      }
      return;
    }
    const outcome = await store.flush(slotKey);
    if (outcome.ok) setComposerNotice(slotKey, "");
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
        if (state.activeConversationId === conversationId) store.adopt(conversationId, state.draft);
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

  const addAttachment = async () => {
    if (!slotKey) return;
    try {
      const picked = await bridge.pick_attachments();
      if (!picked.length) return;
      setAttachmentViews((current) => ({
        ...current,
        ...Object.fromEntries(picked.map((file) => [file.id, file])),
      }));
      const existing = store.get(slotKey)?.content.attachmentIds ?? [];
      void store.edit(slotKey, {
        attachmentIds: [...existing, ...picked.map((file) => file.id)],
      });
    } catch (error) {
      setNotice(`Attachment picker: ${errorText(error)}`);
    }
  };

  const send = async () => {
    if (sendingRef.current || !slotKey) return;
    sendingRef.current = true;
    setSending(true);
    setComposerNotice(slotKey, "");
    try {
      const flushed = await store.flush(slotKey);
      const resolvedKey = store.resolve(slotKey);
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
      const blocked = routeProblem(
        chosen,
        latestGateways,
        requiresMms(current.content, latestConversation),
        recipientCount(current.content, latestConversation),
        draftEstimateBytes(current.content, attachmentViews),
      );
      if (blocked || chosen.kind !== "route")
        return setComposerNotice(current.key, `Not sent: ${blocked ?? "choose a phone and SIM"}.`);
      const sentGeneration = current.generation;
      store.hold(current.key);
      try {
        const result = await bridge.send_draft({
          id: current.id,
          conversationId: current.conversationId,
          ...current.content,
          expectedRevision: current.revision,
          gatewayId: chosen.gateway.id,
          simId: chosen.gateway.simId,
        });
        if (!result.accepted) {
          store.release(current.key);
          return setComposerNotice(current.key,
            `${STATUS_LABEL[result.status]}: ${result.reason ?? "The draft was preserved."}`,
          );
        }
        store.markSent(current.key, sentGeneration);
        setNewComposeKey(null);
        if (detachedHistoryRef.current?.conversationId === current.conversationId) clearDetached();
      } catch (error) {
        store.release(current.key);
        if (errorCode(error) === "draft-moved") {
          const destination = store.retireUnavailable(current.key, error);
          if (destination) {
            choose(destination);
            void refresh(destination).catch(reportRefresh(""));
          }
          return;
        }
        return setComposerNotice(current.key, `Send failed: ${errorText(error)}`);
      }
      await refresh(current.conversationId).catch(reportRefresh(""));
    } finally {
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
        setNotice(`Downloading ${attachment.name} before saving.`, "info");
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
  const closeAfterSave = async (close: () => Promise<void>) => {
    if (closingRef.current) return;
    closingRef.current = true;
    try {
      const outcome = await store.flushAll();
      if (!outcome.ok)
        return setNotice(
          `The window stayed open because the draft was not saved (${outcome.error}).`,
        );
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
    void store.flushAll().then(async outcome => {
      if (!outcome.ok) {
        setNotice(`The window stayed open because the draft was not saved (${outcome.error}).`);
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
    void store.flushAll().then(async outcome => {
      if (!outcome.ok) {
        setNotice(`The window stayed open because a draft was not saved (${outcome.error}).`);
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

  const openComposerWindow = async (conversationId?: string) => {
    if (conversationId) {
      const outcome = await store.flush(conversationId);
      if (!outcome.ok)
        return setNotice(
          `Not opened: the draft could not be saved (${outcome.error}).`,
        );
    }
    try {
      await bridge.open_composer(conversationId);
    } catch (error) {
      setNotice(`Composer window: ${errorText(error)}`);
    }
  };
  const popoutConversation = async (conversationId: string) => {
    const outcome = await store.flush(conversationId);
    if (!outcome.ok) return setNotice(`Not opened: the draft could not be saved (${outcome.error}).`);
    const resolvedId = store.resolve(conversationId);
    if (isLocalDraftKey(resolvedId)) return setNotice("Not opened: the draft has not been stored yet.");
    try {
      const result = await bridge.popout_conversation(resolvedId);
      if (result.warning) setNotice(result.warning);
    } catch (error) { setNotice(`Floating conversation: ${errorText(error)}`); }
  };
  const toggleConversationPin = async (conversationId: string, pinned: boolean) => {
    if (pendingPinId || snapshotRef.current?.encryption.state !== "unlocked" || appLockRef.current?.locked) return;
    const origin = document.activeElement;
    if (origin instanceof HTMLElement && origin.dataset.pinConversationId === conversationId) {
      pinFocusRestoreRef.current?.dispose();
      function watchFocus(event: FocusEvent) {
        if (event.target instanceof HTMLElement && event.target !== document.body && event.target !== intent.origin)
          intent.userMovedFocus = true;
      }
      function watchPointer(event: PointerEvent) {
        if (!(event.target instanceof Node) || !intent.origin.contains(event.target)) intent.userMovedFocus = true;
      }
      const intent: PinFocusRestoreIntent = {
        conversationId,
        origin,
        userMovedFocus: false,
        dispose: () => {
          document.removeEventListener("focusin", watchFocus);
          document.removeEventListener("pointerdown", watchPointer);
        },
      };
      document.addEventListener("focusin", watchFocus);
      document.addEventListener("pointerdown", watchPointer);
      pinFocusRestoreRef.current = intent;
    }
    setPendingPinId(conversationId);
    try {
      try {
        await bridge.set_conversation_pinned(conversationId, pinned);
      } catch (error) {
        setNotice(`Conversation pin not changed: ${errorText(error)}`);
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
    const saved = await store.flushAll();
    if (!saved.ok) {
      setNotice(`Not locked: the draft could not be saved (${saved.error}).`);
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
      setNotice(`Could not lock sync: ${errorText(error)}`);
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

  const resolution = useMemo<ContactResolutionMap>(() => ({ ...(snapshot?.contactResolution ?? {}), ...chosenContacts }), [snapshot?.contactResolution, chosenContacts]);
  const conversations = useMemo<Conversation[]>(() => {
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
        recipientlessDraft: recipientIds.length === 0,
      };
    }),
    ...(snapshot?.conversations ?? []).map(({ id, name, preview, unread, participants, recipientlessDraft, draftOnly, pinned }) => ({
      id,
      ...displayConversation(name, participants, resolution, id, recipientlessDraft),
      preview,
      unread,
      recipientlessDraft,
      draftOnly,
      pinned,
    })),
    ];
    return [...combined.filter((conversation) => conversation.pinned && !conversation.draftOnly), ...combined.filter((conversation) => !conversation.pinned || conversation.draftOnly)];
  }, [draftsVersion, resolution, snapshot?.conversations, store]);
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

  /* Locked snapshots carry no conversations, contacts, or notifications, so nothing else renders. */
  const lockCode = snapshot?.connection.errorCode;
  if (!browserHost && (lockCode === "app-locked" || lockCode === "secure-store-unavailable"))
    return (
      <AppLockScreen
        storageUnavailable={lockCode === "secure-store-unavailable"}
        onUnlock={unlockApp}
        onRetry={() => refresh()}
      />
    );

  if (loading && composerConversation)
    return <main ref={mainRef} id="composer-shell" aria-busy="true" data-platform={platform}>Loading messaging state…</main>;

  const setupCode = snapshot?.connection.errorCode;
  const nativeCredentialed = snapshot?.mode === "native" && !["server-required", "credentials-required", "revoked"].includes(setupCode ?? "");
  const canUnlock = snapshot?.mode === "native"
    ? snapshot.encryption.state !== "unlocked" && nativeCredentialed
    : browserHost && snapshot?.mode === "browser" && (snapshot.encryption.state === "locked" || snapshot.encryption.state === "mismatch");
  const canRecoverNativeSync = canUnlock || nativeCredentialed;
  const needsServerSetup = snapshot?.connection.errorCode === "server-required" || snapshot?.connection.errorCode === "credentials-required";
  const showSetup = Boolean(snapshot && (addingPhone || initialEnrollmentActive || (!selected && (needsServerSetup || setupCode === "revoked" || (ownerWithoutPhone(snapshot) && phoneDeferredOrigin !== snapshot.connection.origin && !phoneSetupDeferred(snapshot.connection.origin)) || (browserHost && snapshot.mode === "browser" && snapshot.encryption.state === "preview")))));
  const narrowConversationListMode = narrowBrowserViewport && !showSetup && activeView === "conversations";
  const showingMobileConversationList = narrowConversationListMode && (!selected || mobileConversationListOpen);
  const hideThreadList = settingsOpen || notificationsOpen || contactsOpen || (narrowConversationListMode ? !showingMobileConversationList : listCollapsed);
  const enrolled = Boolean(snapshot && (nativeCredentialed || (browserHost && snapshot.mode === "browser" && snapshot.encryption.state !== "preview")));
  const serverOffline = Boolean(snapshot && enrolled && isServerUnreachable(snapshot.connection));
  const attachments: Attachment[] = content.attachmentIds.map(
    (id) =>
      attachmentViews[id] ?? { id, name: "Attached file", state: "pending" },
  );
  const activeDisplay = active
    ? displayConversation(active.name, active.participants, resolution, active.id, active.recipientlessDraft)
    : undefined;
  const toneOverrides = snapshot?.toneOverrides ?? {};
  const saveTone = async (seed: string, paletteIndex: number | null) => {
    const current = snapshotRef.current;
    if (!mountedRef.current || !current || current.encryption.state !== "unlocked" || appLockRef.current?.locked) return;
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
    } catch {
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
    const requestId = ++contactTargetRequestId.current;
    const target: ContactTarget & { requestId: number } = label.contact
      ? { ...label.contact, requestId }
      : { kind: "create", phone: address, requestId };
    void openContactTarget(target, "Sender not opened");
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
  const title =
    activeDisplay?.name ??
    (isLocalDraftKey(slotKey)
      ? content.recipientIds.map((id) => displayAddressName(id, resolution)).join(", ") || "New message"
      : undefined);
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
  const pendingRecipientReason = "finish adding the recipient before sending";
  const effectivePendingRecipient = pendingRecipient && isNewConversation;
  const unavailableReason = effectivePendingRecipient ? pendingRecipientReason : problem;
  const mainWindowRouteRecovery = headPanel && !effectivePendingRecipient && (
    (route.kind === "none" && gateways.length > 0) || route.kind === "stale"
  );
  const composerUnavailableReason = mainWindowRouteRecovery
    ? `${unavailableReason} in the main window`
    : unavailableReason;
  const statusActive = Boolean(unavailableReason || mmsLimitShown);
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
  const draftFeedback = slot?.error || recoveries.length || composerFeedback?.key === slotKey ? (
    <>
      {slot?.error?.code === "stale-draft" ? (
        <section id="draft-recovery" data-draft-recovery="conflict" aria-label="Draft conflict">
          <p role="alert">Draft changed elsewhere. Review both versions before choosing. Sending and closing stay blocked until the chosen version is saved.</p>
          {conflictFeedback && <p role="status">{conflictFeedback}</p>}
          {!storedConflict || storedConflict.key !== slotKey || storedConflict.generation !== slot?.generation || storedConflict.revision !== slot?.revision ? <button type="button" disabled={resolutionPendingKey === slotKey} onClick={() => void retrySave()}>{resolutionPendingKey === slotKey ? "Loading saved draft…" : "Review saved draft"}</button> : <div data-draft-conflict-comparison role="group" aria-labelledby="draft-conflict-comparison-title">
            <p id="draft-conflict-comparison-title" tabIndex={-1}>Compare the saved and local drafts before choosing.</p>
            <p id="draft-conflict-saved-label">Saved version</p>
            <pre aria-labelledby="draft-conflict-saved-label">{storedConflict.draft?.text ?? "No saved draft"}</pre>
            <p id="draft-conflict-local-label">Local version</p>
            <pre aria-labelledby="draft-conflict-local-label">{store.get(slotKey)?.content.text}</pre>
            <div data-draft-conflict-actions><button className="secondary-button" type="button" disabled={resolutionPendingKey === slotKey} onClick={() => {
            prepareFocusRestore();
            setResolutionPendingKey(slotKey);
            void (async () => {
              const current = store.get(slotKey);
              if (!current) return;
              const state = await bridge.load_state(current.conversationId);
              const reviewed = storedConflict;
              const latest = store.get(slotKey);
              if (!latest || latest !== current || latest.generation !== reviewed.generation || latest.revision !== reviewed.revision || state.activeConversationId !== current.conversationId || state.draft?.id !== reviewed.draft?.id || state.draft?.revision !== reviewed.draft?.revision) {
                setStoredConflict(null);
                setConflictFeedback("The saved draft changed. Review it again before choosing.");
                return;
              }
              await store.resolveUseStored(current.key, state.draft);
              setStoredConflict(null);
              restoreFocusIfPending();
            })().catch(error => setConflictFeedback(errorText(error))).finally(() => mountedRef.current && setResolutionPendingKey(null));
            }}>Use saved draft</button>
            <button type="button" disabled={resolutionPendingKey === slotKey} onClick={() => {
            prepareFocusRestore();
            setResolutionPendingKey(slotKey);
            void (async () => {
              const current = store.get(slotKey);
              if (!current) return { ok: false, error: "This draft was deleted or moved elsewhere." } as const;
              const state = await bridge.load_state(current.conversationId);
              const reviewed = storedConflict;
              const latest = store.get(slotKey);
              if (!latest || latest !== current || latest.generation !== reviewed.generation || latest.revision !== reviewed.revision || state.activeConversationId !== current.conversationId || state.draft?.id !== reviewed.draft?.id || state.draft?.revision !== reviewed.draft?.revision) {
                setStoredConflict(null);
                setConflictFeedback("The saved draft changed. Review it again before choosing.");
                return { ok: false, error: "The saved draft changed. Review it again before choosing." } as const;
              }
              return store.resolveKeepLocal(current.key, state.draft);
            })().then(outcome => {
              setStoredConflict(null);
              if (!outcome.ok) setConflictFeedback(`Draft not saved: ${outcome.error}`);
              else restoreFocusIfPending();
              }).finally(() => {
                if (mountedRef.current) setResolutionPendingKey(null);
              });
            }}>{resolutionPendingKey === slotKey ? "Saving local draft…" : "Replace saved draft with local version"}</button></div>
          </div>}
        </section>
      ) : slot?.error && <DraftRecovery error={slot.error.message} onRetry={() => void retrySave()} />}
      {composerFeedback?.key === slotKey && <p role={composerFeedback.severity === "error" ? "alert" : "status"} data-composer-feedback={slotKey}>{composerFeedback.message}</p>}
      {recoveries.map(recovery => <section
        key={recovery.id}
        id={`draft-recovery-${recovery.id.replace(/[^A-Za-z0-9_-]/g, "-")}`}
        data-draft-recovery={recovery.id}
        aria-label="Recoverable unsaved draft"
        role="status"
      >
        <p>{recovery.message}</p>
        {recovery.content.text && <pre>{recovery.content.text}</pre>}
        {recovery.destination ? <button className="secondary-button" type="button" onClick={() => {
          void restoreRecovery(recovery, slotKey);
         }}>Replace draft with retained content</button> : <button className="secondary-button" type="button" onClick={() => {
          const { key } = store.createNew();
          setNewComposeKey(key);
          choose(key);
          store.discardRecovery(recovery.id);
          void store.edit(key, recovery.content);
        }}>Restore in new draft</button>}
        {recovery.content.text && <button className="secondary-button" type="button" onClick={() => {
          void navigator.clipboard?.writeText(recovery.content.text).then(
            () => setComposerNotice(slotKey, "Recoverable text copied.", "info"),
            () => setComposerNotice(slotKey, "Could not copy recoverable text; select it above to copy."),
          );
        }}>Copy retained text</button>}
        <button className="secondary-button" type="button" onClick={() => store.discardRecovery(recovery.id)}>Discard retained text</button>
      </section>)}
    </>
  ) : undefined;
  const noSelectionFeedback = draftFeedback ? (
    <>
      {draftFeedback}
    </>
  ) : undefined;
  const composer = (
            <section
      id="composer-area"
      aria-label="Composer"
      aria-busy={sending}
      data-user-sized={composerHeight !== null || undefined}
      style={composerHeight === null ? undefined : { height: composerHeight }}
    >
      {composerResizeHandle}
      <Composer
        draft={content.text}
        attachments={attachments}
        sendSupported={Boolean(slotKey) && !unavailableReason && !sending && slot?.error?.code !== "stale-draft"}
        unavailableReason={sending ? "sending…" : slot?.error?.code === "stale-draft" ? "review the draft conflict before sending" : composerUnavailableReason}
        onDraftChange={(text) => edit({ text })}
        onSend={() => void send()}
        onAddAttachment={slotKey ? () => void addAttachment() : undefined}
        onRemoveAttachment={slotKey ? (id) => edit({ attachmentIds: content.attachmentIds.filter((attachmentId) => attachmentId !== id) }) : undefined}
        statusActive={statusActive}
        bannerSlot={draftFeedback}
        statusSlot={
          <>
            <span id="unavailable-hint" hidden={!unavailableReason}>
              Sending unavailable: {composerUnavailableReason}
            </span>
            {mmsLimitShown && route.kind === "route" && (
              <span className="mms-limit" data-mms-limit-source={route.gateway.mmsLimitSource ?? "fallback"}>
                {fileSize(estimateBytes)} used · {fileSize(route.gateway.mmsMaxBytes ?? 300 * 1024)} limit ({route.gateway.mmsLimitSource === "carrier" ? "carrier" : "fallback"}) · up to {Math.min(route.gateway.mmsMaxRecipients ?? 20, 20)} recipients
              </span>
            )}
          </>
        }
        composerName={title}
        composerUserSized={composerHeight !== null}
        maxAutoGrowHeight={maxAutoGrowHeight}
        platform={platform}
        gatewaySlot={snapshot ? (headPanel ? (
          <HeadRoutePill content={content} gateways={gateways} />
        ) : (
          <GatewaySelector
            gateways={gateways}
            content={content}
            problem={unavailableReason}
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
      key={newComposeKey ?? slotKey}
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
    const session = gallerySessionRef.current;
    const isCurrent = () => gallerySessionRef.current === session && galleryConversationRef.current === conversationId;
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
  const messages = (
    <MediaActionsProvider publicCopies={publicCopies} onSave={saveGalleryAttachment} onPublish={publishGalleryAttachment} onRetry={(id) => bridge.retry_attachment(id)} requestDownload={(id) => bridge.request_attachment_download(id)} requestMediaSync={bridge.media_sync_request ? attachmentIds => bridge.media_sync_request!(attachmentIds) : undefined} loadOriginal={requestOriginal} releaseMedia={releaseMedia} canCopyImage={browserHost && clipboardImageSupported()} copyNativeImage={copyNativeImage} hydrateMediaPreview={hydrateMediaPreview}>
      <div id="conversation-tabs" role="tablist" aria-label="Conversation views" onKeyDown={(event) => {
        if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
        const next = conversationTab === "messages" ? "media" : "messages";
        setConversationTab(next);
        document.getElementById(next === "messages" ? "tab-messages" : "tab-media")?.focus();
      }}>
        <button id="tab-messages" type="button" role="tab" tabIndex={conversationTab === "messages" ? 0 : -1} aria-selected={conversationTab === "messages"} aria-controls="tabpanel-messages" onClick={() => setConversationTab("messages")}>Messages</button>
        <button id="tab-media" type="button" role="tab" tabIndex={conversationTab === "media" ? 0 : -1} aria-selected={conversationTab === "media"} aria-controls="tabpanel-media" onClick={() => setConversationTab("media")}>Media</button>
      </div>
      <div id="tabpanel-messages" role="tabpanel" aria-labelledby="tab-messages" hidden={conversationTab !== "messages"}>
      <MessageList
      key={reactionScopeKey}
      conversation={active}
      loading={Boolean(active) && !conversationLoaded}
      registerRow={registerRow}
      publicCopies={publicCopies}
      onPublish={(file) => void publish(file)}
      onRetryAttachment={(file) => void retryAttachment(file)}
      onSaveAttachment={(file) => void saveAttachment(file)}
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
      noSelectionFeedback={!composerConversation && !slotKey ? noSelectionFeedback : undefined}
      onOpenMedia={(item) => void openGalleryItem({ attachment: item.attachment, message: item.message })}
      requestOriginal={requestOriginal}
      checkOriginal={checkOriginal}
      releaseMedia={releaseMedia}
      mediaRetryKey={mediaStateHint}
      onOpenExternal={browserHost ? undefined : bridge.open_external_url}
      searchTargetId={searchTargetId}
      detachedHistory={detachedHistory?.conversationId === active?.id ? detachedHistory : undefined}
      onLoadNewer={(control) => void loadNewerMessages(control)}
      onGoToLatest={() => active?.id && void loadLatest(active.id).catch(reportAction("Could not load the latest messages: "))}
      compactTimestamps={compactTimestamps}
      showSenderAvatars={groupSenderAvatars}
      onOpenSender={!composerConversation ? openSender : undefined}
      senderLocked={!snapshot || snapshot.encryption.state !== "unlocked" || Boolean(appLock?.locked)}
      resolution={resolution}
      showReactions={showReactions}
      actorName={actorName}
      onOpenReactionTarget={(reaction, notice) => { void openReactionTarget(reaction, notice); }}
      pendingReactionId={pendingReactionId}
      reactionFeedback={reactionFeedback}
      seenReactions={seenReactions}
      captureReactionLayout={captureReactionLayout}
      restoreReactionLayout={restoreReactionLayout}
      badgeObservation={{ root: messageListNode, obscuredBottom: overlayMargin, enabled: reactionReadEnabled && showReactions, canRecord: canReadMessages, onSeen: ids => {
        if (!canReadMessages()) return;
        ids.forEach(id => badgeReadIds.current.add(id));
        seenReactions.mark(slotKey, ids);
        markReadMessages(ids);
      } }}
      toneOverrides={toneOverrides}
    />
      </div>
      {conversationTab === "media" && <div id="tabpanel-media" role="tabpanel" aria-labelledby="tab-media">
        {active && <MediaPanel key={active.id} loadPage={async (cursor) => {
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
        if (gallerySessionRef.current !== session || galleryConversationRef.current !== cache.conversationId) return undefined;
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
    </MediaActionsProvider>
  );
  const panelStatus = snapshot
    ? {
        tone: statusTone(snapshot.connection, snapshot.encryption.state, serverOffline),
        summary: statusSummary(snapshot.connection, snapshot.encryption.state),
        details: <StatusDetails connection={snapshot.connection} encryption={snapshot.encryption.state} serverUnreachable={serverOffline} />,
      }
    : {
        tone: "neutral" as const,
        summary: "Loading status",
        details: <StatusDetails />,
      };
  const mutedNotificationKeys = new Set((snapshot?.appFilters ?? []).filter((filter) => filter.muted).map((filter) => notificationMuteKey(filter.sourceDeviceId, filter.packageName)));
  const notificationUnread = snapshot?.notifications.filter((notification) => !notification.seen && !notification.dismissalPending && !mutedNotificationKeys.has(notificationMuteKey(notification.target.sourceDeviceId, notification.packageName))).length ?? 0;
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
  const syncProgressBanner = syncProgressEligible ? <SyncProgress progress={snapshot?.syncProgress} /> : null;
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
        className={`theme-${theme}${headPanel ? " head-panel-shell" : ""}`}
        data-bridge-mode={snapshot?.mode ?? "unavailable"}
        data-platform={platform}
        inert={lifecyclePending ? true : undefined}
        aria-busy={lifecyclePending ? "true" : undefined}
      >
        <section
          id="conversation-pane"
          className="conversation-pane"
          aria-label="Conversation"
          ref={paneRef}
        >
          {!headPanel && <AppTitlebar
            isComposer platform={platform} title={title}
            status={composerConnectionDot}
            onMinimize={browserHost ? undefined : () => {}} onMaximize={browserHost ? undefined : () => {}} onClose={browserHost ? undefined : handlers.current.close}
          />}
          {headPanel && <header id="head-panel-header" aria-label={`Floating conversation with ${title ?? "Conversation"}`} data-tauri-drag-region>
            <strong data-tauri-drag-region>{title ?? "Conversation"}</strong>
            <span className="head-panel-status">{composerConnectionDot}</span>
            <button id="head-panel-collapse" type="button" aria-label="Collapse to bubble" title="Collapse" onClick={handlers.current.collapse}><Minus size={14} strokeWidth={1.75} aria-hidden /></button>
            <button id="head-panel-close" type="button" aria-label={closingHead ? "Closing…" : "Close bubble"} title="Close" disabled={closingHead} aria-busy={closingHead || undefined} onClick={handlers.current.closeHead}><X size={14} strokeWidth={1.75} aria-hidden /></button>
          </header>}
          {appNoticeBanner}
          <div
            id="conversation-stage"
            style={overlayHeight === null ? undefined : { "--composer-overlay-height": `${overlayHeight}px` } as CSSProperties}
          >
            {messages}
            {recipientRail}
            {composer}
          </div>
        </section>
      </main>
    );
  }

  return (
    <ToneOverridesProvider overrides={toneOverrides}>
    <main
      ref={mainRef}
      id="desktop-shell"
      className={`theme-${theme}`}
      data-bridge-mode={snapshot?.mode ?? "unavailable"}
      data-platform={platform}
      data-navigation={navigationPosition}
      data-browser-setup={browserHost && showSetup ? "true" : undefined}
      data-narrow-browser={narrowBrowserViewport || undefined}
      inert={lifecyclePending ? true : undefined}
      aria-busy={loading || lifecyclePending ? "true" : undefined}
    >
      <AppTitlebar
        onMinimize={browserHost ? undefined : () => void bridge.window("minimize")}
        onMaximize={browserHost ? undefined : () => void bridge.window("maximize")}
        onClose={browserHost ? undefined : () => void closeAfterSave(() => bridge.window("close"))}
        platform={platform}
        search={<OmniSearch onSearch={searchLocal} onActivate={activateSearchHit} resolveContactNumbers={resolveOmniContactNumbers} onStartConversation={startOmniConversation} sessionKey={searchSessionKey} disabled={searchDisabled} refreshKey={mediaStateHint} historySearch={historyControls.historySearch} onSearchFurtherBack={historyControls.onSearchFurtherBack} onCancelHistorySearch={historyControls.onCancelHistorySearch} onOpenChange={setHistorySearchVisible} />}
        onToggleSidebar={toggleConversationList}
        sidebarExpanded={narrowConversationListMode ? showingMobileConversationList : activeView === "conversations" && !listCollapsed}
        sidebarControls="thread-list"
        onNewMessage={startTitlebarMessage}
        newMessageDisabled={loading || snapshot?.encryption.state !== "unlocked" || Boolean(appLock?.locked)}
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
        {loading ? (
          <section id="desktop-loading-state">Loading messaging state…</section>
        ) : <>
        <aside
          id="thread-list"
          aria-label="Thread list"
          tabIndex={-1}
          data-collapsed={!showingMobileConversationList && listCollapsed || undefined}
          data-mobile-list={showingMobileConversationList || undefined}
          hidden={hideThreadList}
          style={
            hideThreadList
              ? { display: "none" }
              : showingMobileConversationList
                ? { width: "auto", flex: "1 1 0" }
                : { width: renderedListWidth, flexBasis: renderedListWidth }
          }
        >
            <MarkAllReadAction
              key={currentSearchSessionKey()}
              unreadCount={snapshot?.conversations.reduce((total, conversation) => total + conversation.unread, 0) ?? 0}
              enabled={snapshot?.encryption.state === "unlocked" && !appLock?.locked}
              markAllRead={() => bridge.mark_all_read()}
              refresh={() => refresh()}
              onFocusFallback={() => {
                const titlebarNewMessage = document.querySelector<HTMLButtonElement>("[data-action='new-message']");
                const omniInput = document.querySelector<HTMLInputElement>("#titlebar-search input");
                const fallback = document.getElementById("thread-list") ?? document.getElementById("conversation-pane");
                if (titlebarNewMessage && !titlebarNewMessage.disabled) titlebarNewMessage.focus();
                else if (omniInput && !omniInput.disabled) omniInput.focus();
                else fallback?.focus();
              }}
            />
            <ConversationList
            conversations={conversations}
            selectedId={slotKey}
            onSelect={select}
              onPopout={id => void popoutConversation(id)}
              onTogglePin={snapshot?.encryption.state === "unlocked" && !appLock?.locked ? (id, pinned) => void toggleConversationPin(id, pinned) : undefined}
              onDelete={id => void deleteDraft(id)}
              onCancelDelete={cancelDeleteDraft}
              deleteConfirmationId={deleteConfirmationId ?? undefined}
              deletingId={deletingId ?? undefined}
              deleteWarningId={listCollapsed ? undefined : "conversation-delete-draft-warning"}
              pendingPinId={pendingPinId ?? undefined}
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
          <header id="thread-pane-header">
            {!settingsOpen && !notificationsOpen && !contactsOpen && activeDisplay && (
              <TonePicker id="thread-tone-picker" seed={activeDisplay.avatar.seed} value={toneOverrides[activeDisplay.avatar.seed]} automaticIndex={paletteIndex(activeDisplay.avatar.seed)} scope={activeDisplay.avatar.kind === "group" ? "group" : "contact"} disabled={!snapshot || snapshot.encryption.state === "locked" || Boolean(appLock?.locked)} pending={Boolean(pendingToneSeeds[activeDisplay.avatar.seed])} error={toneErrors[activeDisplay.avatar.seed]} onSelect={(index) => { void saveTone(activeDisplay.avatar.seed, index); }} renderTrigger={(props) => <button {...props} id="thread-contact-avatar" aria-label="Change conversation color"><ContactAvatar {...activeDisplay.avatar} toneOverrides={toneOverrides} size={36} /></button>} />
            )}
            <div className="header-copy">
              {settingsOpen ? (
                <h1 data-header-title>Settings</h1>
              ) : notificationsOpen ? (
                <h1 data-header-title>Notifications</h1>
              ) : contactsOpen ? (
                <h1 data-header-title>Contacts</h1>
              ) : (
                <div className="header-copy-details">
                  <b data-header-title>{title ?? (showSetup ? "Set up Peppy" : "Peppy")}</b>
                  {active?.participants && <small data-conversation-participants>{active.participants.map((part) => displayAddressName(part, resolution)).join(", ")}</small>}
                </div>
              )}
            </div>
            {contactsOpen && <div id="contacts-header-slot" className="header-actions" ref={setContactsHeaderHost} />}
            <div className="header-actions" hidden={settingsOpen || notificationsOpen || contactsOpen}>
              {!browserHost && <button
                id="new-composer-window"
                aria-label="New message window"
                title="New message window"
                onClick={() => void openComposerWindow()}
              >
                <SquarePen size={16} aria-hidden />
              </button>}
              {snapshot?.deviceRole === "owner" && (
                <button id="add-phone-action" aria-label="Add phone" title="Add phone" onClick={() => setAddingPhone(true)}>
                  <Smartphone size={16} aria-hidden />
                </button>
              )}
              {!browserHost && conversationLoaded && (
                <button id="header-popout-conversation"
                  aria-label="Open as floating conversation"
                  title="Open as floating conversation"
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
            {notificationsOpen && <button
              id="notifications-dismiss-all"
              className="btn"
              disabled={notificationsLocked || !dismissibleNotificationsCount || dismissingNotifications}
              onClick={() => {
                if (dismissingNotificationsRef.current) return;
                dismissingNotificationsRef.current = true;
                setDismissingNotifications(true);
                setNotificationActionNotice("");
                void bridge.dismiss_all_notifications()
                  .then(() => refresh())
                  .catch(error => setNotificationActionNotice(`Could not dismiss notifications. ${errorText(error)}`))
                  .finally(() => {
                    dismissingNotificationsRef.current = false;
                    setDismissingNotifications(false);
                  });
              }}
            >
              {dismissibleNotificationsCount > 100 ? "Dismiss up to 100" : "Dismiss all"}
            </button>}
          </header>
          {notificationsOpen && notificationActionNotice && <p id="notifications-action-notice" role="status">{notificationActionNotice}</p>}
          <ServerOfflineAnnouncement offline={serverOffline} />
          {serverOffline && <ServerOfflineBanner />}
          {settingsOpen ? (
            <SettingsView
              mode={mode}
              configuredOrigin={browserHost ? fixedOrigin ?? snapshot?.connection.origin : snapshot?.connection.origin}
              origin={origin}
              onOrigin={setOrigin}
              onAction={runSetupAction}
              onOpenAccount={() => bridge.hosted_open_billing()}
              encryption={snapshot?.encryption.state ?? "locked"}
              canUnlock={canRecoverNativeSync}
              theme={theme}
              onTheme={setTheme}
              navigationPosition={navigationPosition}
              onNavigationPosition={changeNavigationPosition}
              compactTimestamps={compactTimestamps}
              onCompactTimestamps={changeCompactTimestamps}
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
              onPreferences={async preferences => { await bridge.set_notification_preferences(preferences); await refresh(); }}
               onMute={async filter => { await bridge.set_app_muted(filter.sourceDeviceId, filter.packageName, filter.appName, filter.muted); await refresh(); }}
              onPermission={() => bridge.request_notification_permission()}
              onOpenSettings={() => bridge.open_notification_settings()}
              headsSupported={snapshot?.head.enabled === true}
               onLock={browserHost ? lockSync : undefined}
               appLock={appLock}
               onAppLock={changeAppLock}
               browserHost={browserHost}
              accountUrl={accountUrl}
              canRevealVaultSecret={!browserHost && snapshot?.deviceRole === "owner" && snapshot.connection.state === "connected"}
               serverOffline={serverOffline}
               requestedSection={notificationSettingsRequested ? "notifications" : undefined}
               onRequestedSectionConsumed={() => setNotificationSettingsRequested(false)}
             />
          ) : notificationsOpen ? (
            <NotificationsView
              focusRequest={notificationFocus}
              onFocusResult={notificationFocusResult}
              notifications={snapshot?.notifications ?? []}
              displayTitle={(title) => resolution[title]?.displayName ?? title}
              filters={snapshot?.appFilters ?? []}
              sources={snapshot?.gateways ?? []}
              locked={notificationsLocked}
              onDismiss={target => {
                setNotificationActionNotice("");
                void bridge.dismiss_notification(target).then(() => refresh()).catch(error => setNotificationActionNotice(`Could not dismiss notification. ${errorText(error)}`));
              }}
              onMute={filter => {
                setNotificationActionNotice("");
                void bridge.set_app_muted(filter.sourceDeviceId, filter.packageName, filter.appName, true).then(() => refresh()).catch(error => setNotificationActionNotice(`Could not mute app. ${errorText(error)}`));
              }}
              onSeen={targets => {
                return bridge.mark_notifications_seen(targets).then(() => refresh()).catch(error => {
                  setNotificationActionNotice(errorText(error));
                  throw error;
                });
              }}
              onSettings={() => { setNotificationSettingsRequested(true); setActiveView("settings"); }}
            />
          ) : contactsOpen ? (
            <ContactsView books={snapshot?.contactBooks ?? []} sync={snapshot?.contactSync} toneOverrides={toneOverrides} toneDisabled={!snapshot || snapshot.encryption.state === "locked" || Boolean(appLock?.locked)} pendingToneSeeds={pendingToneSeeds} toneErrors={toneErrors} onSelectTone={(seed, index) => { void saveTone(seed, index); }} target={contactTarget} onTargetConsumed={(requestId) => setContactTarget(current => current?.requestId === requestId ? undefined : current)} onNavigationGuard={registerContactNavigation} headerHost={contactsHeaderHost} />
          ) : (
            <>
              {showSetup && snapshot ? (
                <SetupLanding
                  mode={mode}
                  onMode={setMode}
                  enrolledWithoutPhone={ownerWithoutPhone(snapshot)}
                  enrollmentEstablished={snapshot.deviceRole === "owner" && snapshot.encryption.state === "unlocked"}
                  addingPhone={addingPhone}
                  connectionPending={snapshot.connection.errorCode === "connecting"}
                  onExit={() => { setAddingPhone(false); setInitialEnrollmentActive(false); }}
                  onDeferPhone={async () => {
                    const key = phoneDeferredKey(snapshot.connection.origin);
                    if (key) localStorage.setItem(key, "1");
                     setPhoneDeferredOrigin(snapshot.connection.origin ?? null);
                     setInitialEnrollmentActive(false);
                     await refresh();
                    if (addingPhone) setAddingPhone(false);
                  }}
                  onJoined={async () => { await refresh(); setAddingPhone(false); setInitialEnrollmentActive(false); }}
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
                    {messages}
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
    </ToneOverridesProvider>
  );
}
