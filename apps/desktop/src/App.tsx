import {
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
} from "react";
import { flushSync } from "react-dom";
import type { GatewayAvailability } from "@peppy/desktop-ui/bridge-contracts";
import {
  AppTitlebar,
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
  RecipientPicker,
  RecipientPanel,
  ResizeHandle,
  ShieldAlert,
  Smartphone,
  SquarePen,
  StatusPopover,
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
  formatDateTime,
  isoDateTime,
  seedKey,
  type RecipientPosition,
  type RecipientSuggestion,
} from "@peppy/desktop-ui";
import "./settings.css";
import "./vibrancy.css";
import {
  bridge,
  type AttachmentView,
  type ConversationView,
  type DesktopSnapshot,
  type GatewayView,
  type MessageStatus,
  type MessageView,
  type HistoryPage,
  type AppFilter,
  type NotificationPreferences,
  type PublicCopy,
  type ContactResolutionMap,
  type AppLockStatus,
} from "./bridge";
import { notificationMuteKey, NotificationsView } from "./Notifications";
import { NotificationSettings } from "./NotificationSettings";
import { ContactsView, type ContactNavigationGuard } from "./Contacts";
import { DevicesSettings } from "./DevicesSettings";
import { SetupLanding, savedSetupMode } from "./SetupLanding";
import { AppLockScreen, AppLockSettings } from "./AppLock";
import { UpdatesSettings } from "./UpdatesSettings";
import { isTauriUpdateHost, useUpdateActivity } from "./updates";
import { connectionText, isServerUnreachable, statusSummary, statusTone, syncStatusText } from "./status";
import { ServerOfflineAnnouncement, ServerOfflineBanner } from "./ServerOfflineBanner";
import { peppyCopy } from "./generated/peppyCopy";
import { MediaLightbox } from "./media/MediaLightbox";
import { MediaPanel, type MediaPageItem as GalleryMediaPageItem } from "./media/MediaPanel";
import { MediaGrid } from "./media/MediaGrid";
import { InlineMedia } from "./media/InlineMedia";
import { groupMessages } from "./media/grouping";
import { GatewayPicker } from "./GatewayPicker";
import { persistLayoutPatch, persistRecipientPosition } from "./layout-persistence";
import { FALLBACK_ERROR, errorCode, errorText } from "./bridgeErrors";
import {
  DraftStore,
  blankContent,
  isDirty,
  isLocalDraftKey,
  shouldConfirmPostFlushDelete,
  type DraftContent,
  type DraftSlot,
  type SaveOutcome,
} from "./drafts/DraftStore";

export { errorText } from "./bridgeErrors";
export {
  DraftStore,
  shouldConfirmPostFlushDelete,
  type DraftSlot,
  type SaveOutcome,
} from "./drafts/DraftStore";

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
  const previews = new Map(previous.attachments.map((attachment) => [attachment.id, attachment.previewUrl]));
  return {
    ...refreshed,
    attachments: refreshed.attachments.map((attachment) =>
      attachment.previewUrl || attachment.state !== "ready" || !attachment.mediaType.startsWith("image/") || !previews.get(attachment.id)
        ? attachment
        : { ...attachment, previewUrl: previews.get(attachment.id) },
    ),
  };
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
    default: return "";
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
  const simLabels = new Map<string, string>();
  gateways.filter(gateway => gateway.simId).forEach((gateway, index) => {
    const key = routeKey(gateway.id, gateway.simId);
    if (!simLabels.has(key)) simLabels.set(key, `SIM ${index + 1}`);
  });
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
  const simLabel = (gateway: GatewayView) => gateway.simId ? (simLabels.get(routeKey(gateway.id, gateway.simId)) ?? gateway.simId) : "No SMS SIM";
  const selectedGateway = route.kind === "route" ? route.gateway : undefined;
  const selectedAvailability = selectedGateway && gatewayAvailability(selectedGateway);
  const statusHint = selectedGateway && selectedAvailability === "delayed" ? `${selectedGateway.name} last synced ${selectedGateway.lastSeenAgeSeconds !== undefined ? formatAge(selectedGateway.lastSeenAgeSeconds) : "recently"} ago. Message will wait for the phone to wake up.` : undefined;
  const presenceHint = statusHint ?? (selectedGateway && (availabilityProblem(selectedGateway) ?? (selectedAvailability === "unknown" ? `${selectedGateway.name}'s status is unknown.` : undefined)));
  return (
    <section id="gateway-selector" aria-label="Phone and SIM" aria-describedby={statusHint || presenceHint ? "gateway-presence-hint" : undefined} data-route-state={problem ? "blocked" : selectedAvailability === "delayed" ? "delayed" : "ready"}>
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
        describedBy={statusHint || presenceHint ? "gateway-presence-hint" : undefined}
      />
      <span id="gateway-presence-hint" className="visually-hidden" role="status" aria-live="polite">{statusHint ?? presenceHint}</span>
    </section>
  );
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

function MessageMeta({ message }: { message: MessageView }) {
  const { displayedTime, deliveryTimeFallback, deliveryTimeUnavailable } = messageTime(message);
  const formattedTime = formatDateTime(displayedTime);
  const isoTime = isoDateTime(displayedTime);
  const deliveryConfirmed = message.status === "delivery-confirmed";
  const deliveryLabel = deliveryTimeFallback ? "Delivery confirmed; sent time shown because delivery time is unavailable" : deliveryTimeUnavailable ? "Delivery confirmed; time unavailable" : "Delivery confirmed";
  const phoneHistory = message.sender === "self" && !message.status && Boolean(formattedTime);
  if (!message.status && !formattedTime) return null;
  return <div className="message-meta" data-message-meta={message.id} data-status={message.status}>
    {!deliveryConfirmed && message.status && <span className="message-meta-status">{["failed-before-submit", "failed-confirmed"].includes(message.status) ? <X size={10} aria-hidden /> : message.status === "unknown" ? <TriangleAlert size={10} aria-hidden /> : message.status === "sent" ? <Check size={10} aria-hidden /> : <Clock size={10} aria-hidden />}{STATUS_LABEL[message.status]}</span>}
    {!deliveryConfirmed && message.status && formattedTime && <span className="message-meta-separator" aria-hidden>·</span>}
    {formattedTime && isoTime && <time dateTime={isoTime}>{formattedTime}</time>}
    {deliveryConfirmed && <span className="message-meta-status" role="img" aria-label={deliveryLabel} title={deliveryLabel}><CheckCheck size={10} aria-hidden /></span>}
    {phoneHistory && <span className="message-meta-status" role="img" aria-label="From phone history" title="From phone history" data-message-phone-icon={message.id}><Smartphone size={10} aria-hidden /></span>}
  </div>;
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
  hasOlderMessages,
  historyLoading,
  historyError,
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
  hasOlderMessages: boolean;
  historyLoading: boolean;
  historyError: string | null;
  onLoadOlder(): void;
  suppressHistoryAnnouncements: boolean;
  historyExhausted: boolean;
  onStartConversation?: () => void;
  noSelectionFeedback?: ReactNode;
  onOpenMedia(item: { attachment: AttachmentView; message: MessageView }): void;
  requestOriginal(id: string): Promise<{ state: "ready"; url: string } | { state: "pending" }>;
  checkOriginal(id: string): Promise<{ state: "ready"; url: string } | { state: "pending" }>;
  releaseMedia(id: string, url: string): void;
  mediaRetryKey: unknown;
}) {
  if (loading)
    return (
      <div
        ref={listRef}
        id="message-list"
        role="log"
        aria-label="Messages"
        aria-busy="true"
        onScroll={onScroll}
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
        onScroll={onScroll}
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
  return (
    <div
      ref={listRef}
      id="message-list"
      role="log"
      aria-label="Messages"
      onScroll={onScroll}
    >
      {hasOlderMessages && <div id="history-page-sentinel" aria-hidden="true" />}
      {(historyLoading || historyError || hasOlderMessages) && <div id="history-load-band" aria-busy={historyLoading || undefined} data-history-state={historyLoading ? "loading" : historyError ? "error" : "idle"}>
        {historyLoading && !historyError && <span id="history-load-indicator" role="status">Loading older messages…</span>}
        {historyError && <div id="history-load-error" role="alert">Couldn&apos;t load older messages. <button id="history-retry-button" className="secondary-button" type="button" aria-label="Retry loading older messages" disabled={historyLoading} onClick={onLoadOlder}>Retry</button></div>}
        {!historyLoading && !historyError && hasOlderMessages && <button id="history-load-button" className="secondary-button" type="button" onClick={onLoadOlder}>Load older messages</button>}
      </div>}
      <div id="message-list-content" aria-live={suppressHistoryAnnouncements ? "off" : undefined}>
        {historyExhausted && <p id="history-beginning-marker">Beginning of conversation</p>}
        {!conversation.messages.length && <p>Start the conversation.</p>}
        {groupMessages(conversation.messages).map((group) => {
          const message = group.messages[0];
          const imageRun = group.messages.every((candidate) => candidate.transport === "mms" && candidate.attachments.length > 0 && candidate.attachments.every((attachment) => attachment.mediaType.startsWith("image/")));
          if (imageRun) return <MediaGrid key={group.id} messages={group.messages} onOpen={onOpenMedia} renderMessageMeta={(candidate) => <MessageMeta message={candidate} />} renderAttachmentActions={(attachment) => {
            const copy = publicCopies[attachment.id];
            return <details data-gallery-sharing={attachment.id}><summary>Attachment actions</summary>{!copy && <button className="secondary-button" type="button" onClick={() => onPublish(attachment)} aria-label={`Create public link for ${attachment.name}`}>Create public link</button>}{copy && <div className="public-copy" data-public-copy-for={attachment.id}><label>Public link <span className="public-link-warning" role="status"><ShieldAlert size={12} aria-hidden /> Anyone with this link can view this copy</span><input readOnly aria-label={`Public link for ${attachment.name}`} value={copy.url} onFocus={(event) => event.currentTarget.select()} /></label><small>{expiryText(copy.expiresInSeconds)}</small></div>}</details>;
          }} messageRef={(candidate) => (row) => registerRow(candidate.id, row)} />;
          return (
            <div
              key={message.id}
              className={`message-row ${message.sender}`}
              data-message-row={message.id}
              data-sender={message.sender}
            >
              <article
                ref={(row) => registerRow(message.id, row)}
                data-message-id={message.id}
                className={`message-bubble ${message.sender}`}
              >
                {message.body && <p>{message.body}</p>}
                {message.subject && <p className="message-subject">Subject: {message.subject}</p>}
                {message.attachments.map((file) => {
                  const copy = publicCopies[file.id];
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
              <MessageMeta message={message} />
            </div>
          );
        })}
      </div>
    </div>
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
  desktop,
  onStartAtLogin,
  notifications,
  filters,
  sources,
  preferences,
  onPreferences,
  onMute,
  onPermission,
  onLock,
  appLock,
  onAppLock,
  browserHost = false,
  accountUrl,
  canRevealVaultSecret = false,
  serverOffline = false,
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
  desktop?: DesktopSnapshot["desktop"];
  onStartAtLogin(enabled: boolean): Promise<void>;
  notifications: DesktopSnapshot["notifications"];
  filters: AppFilter[];
  sources: { id: string; name: string }[];
  preferences: NotificationPreferences;
  onPreferences(preferences: NotificationPreferences): Promise<void>;
  onMute(filter: AppFilter): Promise<void>;
  onPermission(): Promise<"granted" | "denied" | "unknown" | void>;
  onLock?: () => Promise<void>;
  appLock: AppLockStatus | null;
  onAppLock(enabled: boolean): Promise<boolean>;
  browserHost?: boolean;
  accountUrl?: string;
  canRevealVaultSecret?: boolean;
  serverOffline?: boolean;
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
    { id: "theme", label: "Appearance" },
    ...(desktop?.startupSupported ? [{ id: "startup", label: "Startup" }] : []),
    ...(nativeUpdateHost ? [{ id: "updates", label: "Updates" }] : []),
    ...(browserHost ? [] : [{ id: "app-lock", label: "App lock" }]),
    ...(hasAccount ? [{ id: "account-billing", label: "Account" }] : []),
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
  const selectSection = (id: string) => {
    const section = document.getElementById(sectionElementId(id));
    setActiveSection(id);
    pendingSectionRef.current = id;
    if (scrollEndTimer.current) window.clearTimeout(scrollEndTimer.current);
    scrollEndTimer.current = window.setTimeout(() => {
      pendingSectionRef.current = null;
    }, 100);
    section?.scrollIntoView?.({
      block: "start",
      behavior: window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth",
    });
  };

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
            catch (error) { setRevealError(errorCode(error) === "vault-secret-unavailable" ? "This device doesn't have the vault shared secret stored." : FALLBACK_ERROR); }
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
      />
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
        {!browserHost ? <button className="secondary-button" disabled={Boolean(pendingAction)} aria-busy={actionPending("account") || undefined} onClick={() => void runAction("account")}>Open account &amp; billing</button> : <a id="settings-account-billing-link" href={accountUrl} target="_blank" rel="noopener noreferrer" className="secondary-button">Open account &amp; billing<span className="visually-hidden"> (opens in a new tab)</span></a>}
        {actionAlert("account")}
      </section>}
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
  useUpdateActivity(!browserHost);
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
  const checkOriginal = useCallback((id: string) => bridge.open_media(id), []);
  const releaseMedia = useCallback((id: string, url: string) => { void bridge.close_media(id, url); }, []);
  const [hasOlderMessages, setHasOlderMessages] = useState(false);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [historyError, setHistoryError] = useState<string | null>(null);
  const [suppressHistoryAnnouncements, setSuppressHistoryAnnouncements] = useState(false);
  const [historyExhausted, setHistoryExhausted] = useState(false);
  const [appLock, setAppLock] = useState<AppLockStatus | null>(null);
  const [mode, setMode] = useState<"hosted" | "self-hosted">(() => browserHost ? "self-hosted" : savedSetupMode());
  const headPanel = Boolean(composerConversation && (snapshot?.head.panel ?? headPanelBootstrap));
  const [selected, setSelected] = useState(composerConversation ?? "");
  const [attachmentViews, setAttachmentViews] = useState<
    Record<string, AttachmentView>
  >({});
  const [publicCopies, setPublicCopies] = useState<Record<string, PublicCopy>>(
    {},
  );
  const [theme, setTheme] = useState<Theme>("system");
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
  const [activeView, setActiveViewNow] = useState<"conversations" | "notifications" | "settings" | "contacts">(
    "conversations",
  );
  const contactNavigation = useRef<ContactNavigationGuard | null>(null);
  const registerContactNavigation = useCallback((guard: ContactNavigationGuard | null) => {
    contactNavigation.current = guard;
  }, []);
  const setActiveView = useCallback((view: typeof activeView) => {
    if (view === "contacts" && contactNavigation.current) return;
    const navigate = () => setActiveViewNow(view);
    if (contactNavigation.current) contactNavigation.current(navigate);
    else navigate();
  }, []);
  const settingsOpen = activeView === "settings";
  const notificationsOpen = activeView === "notifications";
  const contactsOpen = activeView === "contacts";
  const notificationSettingsRequested = useRef(false);
  const [notice, setNotice] = useState("");
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
  const [sending, setSending] = useState(false);
  const [closingHead, setClosingHead] = useState(false);
  const [lifecyclePending, setLifecyclePending] = useState<string | null>(null);
  const [draftsVersion, rerender] = useReducer((count: number) => count + 1, 0);
  const defaultListWidth = typeof window !== "undefined" && window.innerWidth < 900 ? 240 : 280;
  const [listWidth, setListWidth] = useState(defaultListWidth);
  const [listCollapsed, setListCollapsed] = useState(false);
  const [composerHeight, setComposerHeight] = useState<number | null>(null);
  const [recipientPosition, setRecipientPosition] = useState<RecipientPosition | null>(null);
  const [pendingRecipient, setPendingRecipient] = useState(false);
  const [newComposeKey, setNewComposeKey] = useState<string | null>(null);
  const [deleteConfirmationId, setDeleteConfirmationId] = useState<string | null>(null);
  const [deletingId, setDeletingId] = useState<string | null>(null);
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
  const nearBottom = useRef(true);
  const seenMessageIds = useRef(new Set<string>());
  const pendingSeenMessageIds = useRef(new Set<string>());

  const selectedRef = useRef(selected);
  const snapshotRef = useRef(snapshot);
  snapshotRef.current = snapshot;
  const sendingRef = useRef(false);
  const closingRef = useRef(false);
  const loadSequence = useRef(0);
  const historySequence = useRef(0);
  const historyLoadingRef = useRef(false);
  const historySentinelArmed = useRef(true);
  const historyUserScrolled = useRef(false);
  const hasOlderMessagesRef = useRef(false);
  const mountedRef = useRef(true);
  const overlayHeightRef = useRef(overlayHeight);
  overlayHeightRef.current = overlayHeight;
  const lastUnobscuredHeight = useRef<number | null>(null);
  const historyAnchorRef = useRef<{ row: HTMLElement; top: number } | null>(null);
  const messageRows = useRef(new Map<string, HTMLElement>());
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

  const persistLayout = persistLayoutPatch;
  const changeNavigationPosition = (position: NavigationPosition) => {
    setNavigationPosition(position);
    persistLayout({ navigationPosition: position });
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
  useEffect(() => {
    if (activeView === "settings" && notificationSettingsRequested.current) {
      notificationSettingsRequested.current = false;
      document.getElementById("notification-settings")?.scrollIntoView?.({ block: "start" });
    }
  }, [activeView]);
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
    loadSequence.current += 1;
    historySequence.current += 1;
    historyLoadingRef.current = false;
    setHistoryLoading(false);
    setHistoryError(null);
    setHasOlderMessages(false);
    setHistoryExhausted(false);
    hasOlderMessagesRef.current = false;
    historySentinelArmed.current = true;
    historyUserScrolled.current = false;
    historyAnchorRef.current = null;
    selectedRef.current = key;
    setSelected(key);
  };
  const report = (prefix: string) => (error: unknown) =>
    setNotice(`${prefix}${errorText(error)}`);
  const retryPendingSaves = async () => {
    for (const attachmentId of pendingSaveIdsRef.current) {
      try {
        const media = await bridge.open_media(attachmentId);
        if (media.state === "pending") continue;
        await bridge.save_attachment(attachmentId);
        pendingSaveIdsRef.current.delete(attachmentId);
      } catch (error) {
        if (errorCode(error) !== "media-pending") pendingSaveIdsRef.current.delete(attachmentId);
      }
    }
  };

  /** Loads host state for one conversation; responses superseded by a newer request are dropped. */
  const refresh = async (conversationId = selectedRef.current) => {
    const sequence = ++loadSequence.current;
    const loadedConversation = snapshotRef.current?.activeConversationId === conversationId
      ? snapshotRef.current.conversations.find((item) => item.id === conversationId)
      : undefined;
    const historyFromMessageId = loadedConversation?.messages[0]?.id;
    const requestedConversationId = conversationId && !isLocalDraftKey(conversationId)
      ? conversationId
      : undefined;
    const state = historyFromMessageId
      ? await bridge.load_state(requestedConversationId, historyFromMessageId)
      : await bridge.load_state(requestedConversationId);
    if (!mountedRef.current || sequence !== loadSequence.current) return;
    setSnapshot((current) => {
      const previous = historyFromMessageId && current && current.activeConversationId === state.activeConversationId
        ? current.conversations.find((item) => item.id === state.activeConversationId)
        : undefined;
      const refreshed = state.conversations.find((item) => item.id === state.activeConversationId);
      if (!previous || !refreshed) return state;
      return {
        ...state,
        conversations: state.conversations.map((item) => item.id === refreshed.id
          ? { ...item, messages: mergeAnchoredMessages(previous.messages, refreshed.messages) }
          : item),
      };
    });
    const activeConversation = state.conversations.find((item) => item.id === state.activeConversationId);
    if (state.activeConversationId)
      store.adopt(state.activeConversationId, state.draft);
    if (
      !composerConversation &&
      !selectedRef.current &&
      state.activeConversationId
    )
      choose(state.activeConversationId);
    const responseHasOlder = activeConversation?.hasOlderMessages ?? false;
    const currentAnchor = snapshotRef.current?.activeConversationId === conversationId
      ? snapshotRef.current.conversations.find((item) => item.id === conversationId)?.messages[0]?.id
      : undefined;
    if (currentAnchor === historyFromMessageId || !currentAnchor) {
      hasOlderMessagesRef.current = responseHasOlder;
      setHasOlderMessages(responseHasOlder);
    }
  };

  const select = (key: string) => {
    const previous = selectedRef.current;
    if (key === previous) return;
    if (previous) void store.flush(previous);
    choose(key);
    if (newComposeKey && key !== newComposeKey && !isLocalDraftKey(key)) setNewComposeKey(null);
    setNotice("");
    if (!isLocalDraftKey(key)) void refresh(key).catch(report(""));
  };

  handlers.current.rekeyed = (from, to) => {
    if (selectedRef.current !== from) return;
    choose(to);
    void refresh(to).catch(report(""));
  };
  handlers.current.unavailable = (from, to, message) => {
    if (selectedRef.current === from && to) choose(to);
    else if (selectedRef.current === from) choose("");
    setNotice(message ?? "Draft changed elsewhere.");
    if (to) void refresh(to).catch(report(""));
  };

  useEffect(() => {
    mountedRef.current = true;
    void refresh(composerConversation ?? undefined)
      .catch(report(""))
      .finally(() => setLoading(false));
    const unsubscribe = bridge.subscribe(() => {
      setMediaStateHint(current => current + 1);
      void retryPendingSaves();
      void refresh().catch(report(""));
    });
    return () => {
      mountedRef.current = false;
      loadSequence.current += 1;
      historySequence.current += 1;
      historyAnchorRef.current = null;
      unsubscribe();
    };
  }, []);
  useEffect(() => {
    if (snapshot?.encryption.state !== "locked" && !appLock?.locked) return;
    historySequence.current += 1;
    historyLoadingRef.current = false;
    historyAnchorRef.current = null;
    hasOlderMessagesRef.current = false;
    setHistoryLoading(false);
    setHistoryError(null);
    setHasOlderMessages(false);
    setHistoryExhausted(false);
  }, [appLock?.locked, snapshot?.encryption.state]);
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
  const messageIds = conversationLoaded
    ? active!.messages.map((message) => message.id).join(",")
    : "";
  const stickToBottom = (element = messageListRef.current) => {
    if (!element) return;
    element.setAttribute("data-scroll-programmatic", "");
    element.scrollTop = element.scrollHeight;
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
  const onMessageScroll = () => {
    const element = messageListRef.current;
    if (!element || element.clientHeight === 0) return;
    if (!element.hasAttribute("data-scroll-programmatic")) historyUserScrolled.current = true;
    nearBottom.current = isNearMessageListBottom(element);
  };
  const adjustMessageScrollForGeometry = (element = messageListRef.current) => {
    if (!element) return;
    if (element.clientHeight === 0) {
      lastUnobscuredHeight.current = null;
      historyUserScrolled.current = false;
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
    nearBottom.current = isNearMessageListBottom(element);
    if (typeof requestAnimationFrame === "undefined") element.removeAttribute("data-scroll-programmatic");
    else requestAnimationFrame(() => element.removeAttribute("data-scroll-programmatic"));
  };
  const loadOlderMessages = useCallback(async () => {
    const conversationId = selectedRef.current;
    const current = snapshotRef.current?.conversations.find((item) => item.id === conversationId);
    const beforeMessageId = current?.messages[0]?.id;
    if (!conversationId || isLocalDraftKey(conversationId) || !beforeMessageId || !hasOlderMessagesRef.current || historyLoadingRef.current) return;
    const sequence = ++historySequence.current;
    historySentinelArmed.current = false;
    historyLoadingRef.current = true;
    setHistoryLoading(true);
    setHistoryError(null);
    try {
      const page: HistoryPage = await bridge.load_history(conversationId, beforeMessageId);
      if (!mountedRef.current || sequence !== historySequence.current || selectedRef.current !== conversationId) return;
      if (page.conversationId !== conversationId) {
        setHistoryError("Conversation changed while loading older messages. Reload and try again.");
        return;
      }
      const list = messageListRef.current;
      const listBounds = list?.getBoundingClientRect();
      const unobscuredBottom = (listBounds?.bottom ?? 0) - (overlayHeightRef.current ?? 0);
      const visibleRow = listBounds
        ? [...messageRows.current.values()]
            .map((row) => ({ row, bounds: row.getBoundingClientRect() }))
            .filter(({ bounds }) => bounds.bottom > listBounds.top && bounds.top < unobscuredBottom)
            .sort((left, right) => left.bounds.top - right.bounds.top)[0]?.row
        : undefined;
      const anchorRow = visibleRow ?? messageRows.current.get(beforeMessageId) ?? null;
      historyAnchorRef.current = anchorRow ? { row: anchorRow, top: anchorRow.getBoundingClientRect().top } : null;
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
      const anchor = historyAnchorRef.current;
      if (list && anchor) {
        list.setAttribute("data-scroll-programmatic", "");
        list.scrollTop += anchor.row.getBoundingClientRect().top - anchor.top;
        if (typeof requestAnimationFrame === "undefined") list.removeAttribute("data-scroll-programmatic");
        else requestAnimationFrame(() => list.removeAttribute("data-scroll-programmatic"));
      }
      hasOlderMessagesRef.current = page.hasMore;
      setHasOlderMessages(page.hasMore);
      historyUserScrolled.current = false;
      if (!page.hasMore) setHistoryExhausted(true);
      if (typeof requestAnimationFrame === "undefined") setSuppressHistoryAnnouncements(false);
      else requestAnimationFrame(() => mountedRef.current && setSuppressHistoryAnnouncements(false));
    } catch (error) {
      if (sequence === historySequence.current) setHistoryError(errorText(error));
    } finally {
      if (sequence === historySequence.current) {
        historyLoadingRef.current = false;
        setHistoryLoading(false);
      }
    }
  }, []);
  useEffect(() => {
    const list = messageListRef.current;
    const sentinel = document.getElementById("history-page-sentinel");
    if (!list || !sentinel || !hasOlderMessages || typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver(([entry]) => {
      if (!entry.isIntersecting) {
        historySentinelArmed.current = true;
        return;
      }
      if (historyUserScrolled.current && historySentinelArmed.current) void loadOlderMessages();
    }, { root: list, rootMargin: "200px 0px 0px 0px" });
    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [hasOlderMessages, loadOlderMessages, messageIds]);
  useLayoutEffect(() => {
    lastUnobscuredHeight.current = null;
    if (!conversationLoaded) return;
    stickToBottom();
    nearBottom.current = true;
  }, [slotKey, conversationLoaded]);
  useLayoutEffect(() => {
    if (nearBottom.current) stickToBottom();
  }, [messageIds]);
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
      } else if (nearBottom.current) {
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
  }, [slotKey]);
  const setMessageListNode = useCallback((node: HTMLDivElement | null) => {
    if (node !== messageListRef.current) lastUnobscuredHeight.current = null;
    messageListRef.current = node;
  }, []);
  const overlayMargin =
    overlayHeight === null ? null : Math.ceil(overlayHeight / 8) * 8;
  useEffect(() => {
    const listNode = messageListRef.current;
    if (!listNode || overlayMargin === null) return;
    const markVisible = () => {
      if (!document.hasFocus() || document.visibilityState !== "visible")
        return;
      const ids = [...messageRows.current]
        .filter(([id, row]) => row.dataset.visible === "true" && !seenMessageIds.current.has(id) && !pendingSeenMessageIds.current.has(id))
        .map(([id]) => id);
      if (!ids.length) return;
      const visibleSlot = slotKey;
      ids.forEach(id => pendingSeenMessageIds.current.add(id));
      void bridge.mark_seen(ids).then(() => {
        if (selectedRef.current === visibleSlot) ids.forEach(id => seenMessageIds.current.add(id));
      }).catch(report("")).finally(() => ids.forEach(id => pendingSeenMessageIds.current.delete(id)));
    };
    // Preserve the last measured visibility across native state refreshes so a rejected hidden-window mark can retry.
    markVisible();
    const observer =
      typeof IntersectionObserver === "undefined"
        ? undefined
        : new IntersectionObserver((entries) => {
            entries.forEach((entry) =>
              entry.target.setAttribute(
                "data-visible",
                String(entry.isIntersecting),
              ),
            );
            markVisible();
          }, { root: listNode, rootMargin: `0px 0px -${overlayMargin}px 0px` });
    messageRows.current.forEach((row) => observer?.observe(row));
    window.addEventListener("focus", markVisible);
    document.addEventListener("visibilitychange", markVisible);
    return () => {
      observer?.disconnect();
      window.removeEventListener("focus", markVisible);
      document.removeEventListener("visibilitychange", markVisible);
    };
  }, [slotKey, messageIds, activeView, overlayMargin, snapshot]);
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
        setNotice("Another blank draft already exists. Your previous recipients were restored.");
        void store.edit(slotKey, { recipientIds: previous });
      } else if (!outcome.ok && current?.error?.code === "draft-merge-overflow") {
        setNotice("These drafts are too large to combine. Your draft is still available to edit.");
      }
    });
  };
  const changeRecipientPosition = (position: RecipientPosition) => {
    setRecipientPosition(position);
    persistRecipientPosition(position);
  };

  const startNewMessage = async (recipient?: string) => {
    const previous = selectedRef.current;
    const previousSlot = previous ? store.get(previous) : undefined;
    if (previousSlot && isDirty(previousSlot)) await store.flush(previous);
    const { key } = store.createNew(recipient);
    const draftKey = store.resolve(key);
    setNewComposeKey(draftKey);
    choose(draftKey);
    setNotice("");
  };
  const showConversationList = () => {
    if (activeView !== "conversations") setActiveView("conversations");
    if (listCollapsed) resizeListTo(previousListWidth.current);
  };
  const toggleConversationList = () => {
    if (activeView !== "conversations") {
      setActiveView("conversations");
      resizeListTo(previousListWidth.current);
      return;
    }
    resizeListTo(listCollapsed ? previousListWidth.current : 0);
  };
  const startTitlebarMessage = () => {
    showConversationList();
    void startNewMessage();
  };

  const retrySave = async () => {
    const current = store.get(slotKey);
    if (!current) return;
    if (current.error?.code === "stale-draft" && current.conversationId) {
      try {
        const state = await bridge.load_state(current.conversationId);
        store.rebase(
          slotKey,
          state.activeConversationId === current.conversationId
            ? state.draft
            : undefined,
        );
      } catch (error) {
        return setNotice(errorText(error));
      }
    }
    await store.flush(slotKey);
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
      setNotice("Deleting this draft permanently removes its text and attachments. Select Confirm delete draft to continue.");
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
        setNotice("Deleting this draft permanently removes its text and attachments. Select Confirm delete draft to continue.");
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
        setNotice(`Draft deleted, but refresh failed: ${errorText(error)}`);
        return;
      }
      setNotice("Draft deleted.");
    } catch (error) {
      setNotice(`Draft not deleted: ${errorText(error)}`);
    } finally {
      setDeletingId(null);
    }
  };
  const cancelDeleteDraft = () => {
    setDeleteConfirmationId(null);
    setNotice("");
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
    setNotice("");
    try {
      if (active?.replyBlockedReason)
        return setNotice(`Not sent: ${active.replyBlockedReason}.`);
      const flushed = await store.flush(slotKey);
      if (!flushed.ok)
        return setNotice(
          `Not sent: the draft could not be saved (${flushed.error}).`,
        );
      const current = store.get(slotKey);
      if (!current?.id)
        return setNotice("Not sent: the draft has not been stored yet.");
      const latestSnapshot = snapshotRef.current;
      const latestConversation = latestSnapshot?.conversations.find(
        (conversation) => conversation.id === current.conversationId,
      );
      if (latestConversation?.replyBlockedReason)
        return setNotice(`Not sent: ${latestConversation.replyBlockedReason}.`);
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
        return setNotice(`Not sent: ${blocked ?? "choose a phone and SIM"}.`);
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
          return setNotice(
            `${STATUS_LABEL[result.status]}: ${result.reason ?? "The draft was preserved."}`,
          );
        }
        store.markSent(current.key, sentGeneration);
        setNewComposeKey(null);
      } catch (error) {
        store.release(current.key);
        if (errorCode(error) === "draft-moved") {
          const destination = store.retireUnavailable(current.key, error);
          if (destination) {
            choose(destination);
            void refresh(destination).catch(report(""));
          }
          return setNotice("Draft moved elsewhere. Review the retained text, then send again.");
        }
        return setNotice(`Send failed: ${errorText(error)}`);
      }
      await refresh(current.conversationId).catch(report(""));
    } finally {
      sendingRef.current = false;
      setSending(false);
    }
  };

  const publish = async (attachment: AttachmentView) => {
    try {
      const copy = await bridge.publish_attachment(attachment.id);
      if (!copy)
        return setNotice("Public link not created; nothing was shared.");
      setPublicCopies((current) => ({ ...current, [attachment.id]: copy }));
    } catch (error) {
      setNotice(`Public link: ${errorText(error)}`);
    }
  };
  const retryAttachment = async (attachment: AttachmentView) => {
    try {
      await bridge.retry_attachment(attachment.id);
      setNotice(`Retrying ${attachment.transfer ?? "attachment"} transfer.`);
    } catch (error) {
      setNotice(`Attachment retry: ${errorText(error)}`);
    }
  };
  const saveAttachment = async (attachment: AttachmentView) => {
    try {
      if (!await bridge.save_attachment(attachment.id))
        setNotice("Attachment save cancelled.");
    } catch (error) {
      if (errorCode(error) === "media-pending") {
        pendingSaveIdsRef.current.add(attachment.id);
        setNotice(`Downloading ${attachment.name} before saving.`);
      } else setNotice(`Attachment save: ${errorText(error)}`);
    }
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
      hasOlderMessagesRef.current = false;
      setHistoryLoading(false);
      setHistoryError(null);
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
  const conversations = useMemo<Conversation[]>(() => [
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
        recipientlessDraft: recipientIds.length === 0,
      };
    }),
    ...(snapshot?.conversations ?? []).map(({ id, name, preview, unread, participants, recipientlessDraft, draftOnly }) => ({
      id,
      ...displayConversation(name, participants, resolution, id, recipientlessDraft),
      preview,
      unread,
      recipientlessDraft,
      draftOnly,
    })),
  ], [draftsVersion, resolution, snapshot?.conversations, store]);

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
  const enrolled = Boolean(snapshot && (nativeCredentialed || (browserHost && snapshot.mode === "browser" && snapshot.encryption.state !== "preview")));
  const serverOffline = Boolean(snapshot && enrolled && isServerUnreachable(snapshot.connection));
  const attachments: Attachment[] = content.attachmentIds.map(
    (id) =>
      attachmentViews[id] ?? { id, name: "Attached file", state: "pending" },
  );
  const activeDisplay = active
    ? displayConversation(active.name, active.participants, resolution, active.id, active.recipientlessDraft)
    : undefined;
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
  const blockedReason = active?.replyBlockedReason ?? (effectivePendingRecipient ? pendingRecipientReason : problem);
  const delayedStatus = route.kind === "route" && gatewayAvailability(route.gateway) === "delayed"
    ? `${route.gateway.name} last synced ${route.gateway.lastSeenAgeSeconds !== undefined ? formatAge(route.gateway.lastSeenAgeSeconds) : "recently"} ago. Message will wait for the phone to wake up.`
    : undefined;
  const statusActive = Boolean(blockedReason || delayedStatus || mmsLimitShown || notice);
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
  const draftFeedback = slot?.error || recoveries.length ? (
    <>
      {slot?.error && <DraftRecovery
        error={slot.error.message}
        onRetry={() => void retrySave()}
      />}
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
          void store.edit(recovery.destination!, recovery.content).then(outcome => {
            if (outcome.ok) store.discardRecovery(recovery.id);
            else setNotice(`Retained text was not applied: ${outcome.error}`);
          });
         }}>Replace draft with retained content</button> : <button className="secondary-button" type="button" onClick={() => {
          const { key } = store.createNew();
          setNewComposeKey(key);
          choose(key);
          store.discardRecovery(recovery.id);
          void store.edit(key, recovery.content);
        }}>Restore in new draft</button>}
        {recovery.content.text && <button className="secondary-button" type="button" onClick={() => {
          void navigator.clipboard?.writeText(recovery.content.text).then(
            () => setNotice("Recoverable text copied."),
            () => setNotice("Could not copy recoverable text; select it above to copy."),
          );
        }}>Copy retained text</button>}
        <button className="secondary-button" type="button" onClick={() => store.discardRecovery(recovery.id)}>Discard retained text</button>
      </section>)}
    </>
  ) : undefined;
  const noSelectionFeedback = notice || draftFeedback ? (
    <>
      {notice && <p id="no-selection-notice" role="status">{notice}</p>}
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
        sendSupported={Boolean(slotKey) && !problem && !active?.replyBlockedReason && !effectivePendingRecipient && !sending}
        unavailableReason={sending ? "sending…" : active?.replyBlockedReason ?? (effectivePendingRecipient ? pendingRecipientReason : problem)}
        onDraftChange={(text) => edit({ text })}
        onSend={() => void send()}
        onAddAttachment={slotKey ? () => void addAttachment() : undefined}
        onRemoveAttachment={slotKey ? (id) => edit({ attachmentIds: content.attachmentIds.filter((attachmentId) => attachmentId !== id) }) : undefined}
        status={notice}
        statusActive={statusActive}
        bannerSlot={draftFeedback}
        statusSlot={
          <>
            <p id="reply-blocked-reason" data-reply-blocked role="status" hidden={!active?.replyBlockedReason}>
              Replies unavailable: {active?.replyBlockedReason}
            </p>
            <span id="unavailable-hint" hidden={!blockedReason}>
              Sending unavailable: {blockedReason}
            </span>
            <p id="gateway-status-hint" role="status" hidden={!delayedStatus}>{delayedStatus}</p>
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
        gatewaySlot={snapshot ? (
          <GatewaySelector
            gateways={gateways}
            content={content}
            problem={blockedReason}
            onSelect={(gateway) =>
              edit({ gatewayId: gateway.id, simId: gateway.simId })
            }
          />
        ) : null}
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
    return { attachment: { ...item.attachment, mediaMeta, durationMs: mediaMeta?.durationMs ? Number(mediaMeta.durationMs) : undefined }, message, cursor: item.cursor };
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
    <>
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
      conversation={active}
      loading={Boolean(active) && !conversationLoaded}
      registerRow={registerRow}
      publicCopies={publicCopies}
      onPublish={(file) => void publish(file)}
      onRetryAttachment={(file) => void retryAttachment(file)}
      onSaveAttachment={(file) => void saveAttachment(file)}
      listRef={setMessageListNode}
      onScroll={onMessageScroll}
      hasOlderMessages={hasOlderMessages}
      historyLoading={historyLoading}
      historyError={historyError}
      onLoadOlder={() => void loadOlderMessages()}
      suppressHistoryAnnouncements={suppressHistoryAnnouncements}
      historyExhausted={historyExhausted}
      onStartConversation={!composerConversation && !slotKey ? () => void startNewMessage() : undefined}
      noSelectionFeedback={!composerConversation && !slotKey ? noSelectionFeedback : undefined}
      onOpenMedia={(item) => void openGalleryItem({ attachment: item.attachment, message: item.message })}
      requestOriginal={requestOriginal}
      checkOriginal={checkOriginal}
      releaseMedia={releaseMedia}
      mediaRetryKey={mediaStateHint}
    />
      </div>
      {conversationTab === "media" && <div id="tabpanel-media" role="tabpanel" aria-labelledby="tab-media">
        {active && <MediaPanel key={active.id} loadPage={async (cursor) => {
          const page = await bridge.media_page(active.id, cursor as Parameters<typeof bridge.media_page>[1], 40);
          return { items: page.items.map(toGalleryItem), nextCursor: page.nextCursor };
        }} onOpen={(item) => void openGalleryItem(item)} />}
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
    </>
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
  const composerConnectionDot = snapshot
    ? <ConnectionDot state={snapshot.connection.state} label={`Connection: ${connectionText(snapshot.connection)}`} serverUnreachable={serverOffline} />
    : null;
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
    <main
      ref={mainRef}
      id="desktop-shell"
      className={`theme-${theme}`}
      data-bridge-mode={snapshot?.mode ?? "unavailable"}
      data-platform={platform}
      data-navigation={navigationPosition}
      inert={lifecyclePending ? true : undefined}
      aria-busy={loading || lifecyclePending ? "true" : undefined}
    >
      <AppTitlebar
        onMinimize={browserHost ? undefined : () => void bridge.window("minimize")}
        onMaximize={browserHost ? undefined : () => void bridge.window("maximize")}
        onClose={browserHost ? undefined : () => void closeAfterSave(() => bridge.window("close"))}
        platform={platform}
        onToggleSidebar={toggleConversationList}
        sidebarExpanded={activeView === "conversations" && !listCollapsed}
        sidebarControls="thread-list"
        onNewMessage={navigationPosition === "title-bar" || activeView !== "conversations" || listCollapsed ? startTitlebarMessage : undefined}
        navigation={navigationPosition === "title-bar" ? (
          <NavButtons
            orientation="titlebar"
            activeView={activeView}
            onView={setActiveView}
            onToggleList={() =>
              resizeListTo(listCollapsed ? previousListWidth.current : 0)
            }
            listCollapsed={listCollapsed}
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
      <div ref={desktopBodyRef} id="desktop-body" className="desktop-layout">
        {navigationPosition === "side-rail" && <Panel
          activeView={activeView}
          onView={setActiveView}
          onToggleList={() =>
            resizeListTo(listCollapsed ? previousListWidth.current : 0)
          }
          listCollapsed={listCollapsed}
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
          data-collapsed={listCollapsed || undefined}
          hidden={settingsOpen || notificationsOpen || contactsOpen}
          style={
            listCollapsed || settingsOpen || notificationsOpen || contactsOpen
              ? { display: "none" }
              : { width: renderedListWidth, flexBasis: renderedListWidth }
          }
        >
          <div
            id="thread-list-toolbar"
            role="toolbar"
            aria-label="Thread list toolbar"
          >
            <RecipientPicker
              recipients={conversations.filter(
                (item) => !isLocalDraftKey(item.id),
              )}
              onChange={(ids) => ids[0] && select(ids[0])}
              onNewRecipient={startNewMessage}
              searchContacts={snapshot ? (query) => searchRecipientSuggestions(query) : undefined}
            />
            {navigationPosition === "side-rail" && <button
              aria-label="New message"
              title="New message"
              data-action="new-message"
              onClick={() => void startNewMessage()}
            >
              <MessageSquarePlus size={16} strokeWidth={1.75} aria-hidden />
            </button>}
          </div>
            <ConversationList
            conversations={conversations}
            selectedId={slotKey}
            onSelect={select}
              onPopout={id => void popoutConversation(id)}
              onDelete={id => void deleteDraft(id)}
              onCancelDelete={cancelDeleteDraft}
              deleteConfirmationId={deleteConfirmationId ?? undefined}
              deletingId={deletingId ?? undefined}
          />
        </aside>
        {!settingsOpen && !notificationsOpen && !contactsOpen && <ResizeHandle
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
          ref={paneRef}
        >
          <header id="thread-pane-header">
            {!settingsOpen && !notificationsOpen && !contactsOpen && activeDisplay && (
              <span id="thread-contact-avatar">
                <ContactAvatar {...activeDisplay.avatar} size={36} />
              </span>
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
            </div>
          </header>
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
              desktop={snapshot?.desktop}
              onStartAtLogin={async enabled => {
                try { await bridge.set_start_at_login(enabled); await refresh(); }
                catch (error) { setNotice(`Start at login was not changed: ${errorText(error)}`); await refresh().catch(() => {}); throw error; }
              }}
              notifications={snapshot?.notifications ?? []}
              filters={snapshot?.appFilters ?? []}
              sources={snapshot?.gateways ?? []}
              preferences={snapshot?.notificationPreferences ?? { messageBanners: true, mirroredBanners: true, preview: "full" }}
              onPreferences={async preferences => { await bridge.set_notification_preferences(preferences); await refresh(); }}
               onMute={async filter => { await bridge.set_app_muted(filter.sourceDeviceId, filter.packageName, filter.appName, filter.muted); await refresh(); }}
               onPermission={() => bridge.request_notification_permission()}
               onLock={browserHost ? lockSync : undefined}
               appLock={appLock}
               onAppLock={changeAppLock}
               browserHost={browserHost}
              accountUrl={accountUrl}
              canRevealVaultSecret={!browserHost && snapshot?.deviceRole === "owner" && snapshot.connection.state === "connected"}
               serverOffline={serverOffline}
             />
          ) : notificationsOpen ? (
            <NotificationsView
              notifications={snapshot?.notifications ?? []}
              displayTitle={(title) => resolution[title]?.displayName ?? title}
              filters={snapshot?.appFilters ?? []}
              sources={snapshot?.gateways ?? []}
              locked={snapshot?.encryption.state !== "unlocked"}
              onDismiss={target => void bridge.dismiss_notification(target).then(() => refresh()).catch(report("Could not dismiss notification. "))}
              onDismissAll={() => void bridge.dismiss_all_notifications().then(() => refresh()).catch(report("Could not dismiss notifications. "))}
              onMute={filter => void bridge.set_app_muted(filter.sourceDeviceId, filter.packageName, filter.appName, true).then(() => refresh()).catch(report("Could not mute app. "))}
              onSeen={targets => bridge.mark_notifications_seen(targets).then(() => refresh()).catch(error => { report("")(error); throw error; })}
              onSettings={() => { notificationSettingsRequested.current = true; setActiveView("settings"); }}
            />
          ) : contactsOpen ? (
            <ContactsView books={snapshot?.contactBooks ?? []} sync={snapshot?.contactSync} onNavigationGuard={registerContactNavigation} />
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
  );
}
