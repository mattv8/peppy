import type { KeyboardEvent, PointerEvent, ReactElement, ReactNode } from "react";
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import {
  Archive,
  ArchiveRestore,
  Check,
  Bell,
  Clock,
  ExternalLink,
  GripVertical,
  Info,
  Loader,
  LoaderCircle,
  Maximize2,
  MessageCircle,
  MessageSquarePlus,
  Minus,
  Paperclip,
  PanelLeft,
  Pin,
  PinOff,
  SendHorizontal,
  Settings,
  ShieldAlert,
  ShieldCheck,
  Smartphone,
  Square,
  SquarePen,
  Trash2,
  Users,
  X,
} from "lucide-react";
import {
  formatPhoneInput,
  formatPhoneNumber,
  normalizePhoneNumber,
} from "./phone";
import { conversationAvatar, seedKey, type AvatarIdentity } from "./avatar";
import { ContactAvatar } from "./ContactAvatar";
export { BusyIndicator } from "./BusyIndicator";
import type { ConversationFolder } from "./bridge-contracts";
export { TonePicker, TONE_PRESETS, type TonePickerProps } from "./TonePicker";
export { ToneOverridesProvider, incomingToneSeed, toneIndex, toneStyle, useToneOverrides, useToneStyle, type ToneOverrides } from "./tone";
export { OmniSearch, type OmniSearchProps } from "./OmniSearch";
export { ResizeHandle, type ResizeHandleProps } from "./ResizeHandle";
export { SyncProgress, type SyncProgressProps } from "./SyncProgress";
export { AttachmentStorageSettings, attachmentStorageRemovalText, type AttachmentStorageCopy, type AttachmentStorageSettingsProps, type StoragePurchaseProps, type StoragePurchaseState } from "./AttachmentStorageSettings";
export { BillingElsewhereNotice, type BillingElsewhereCopy, type BillingElsewhereNoticeProps, type BillingElsewhereVariant } from "./BillingElsewhereNotice";
export { StorePlanOptions, type StorePlanOptionsCopy, type StorePlanOptionsProps, type StorePlanState } from "./StorePlanOptions";
export { Modal, type ModalProps } from "./Modal";
export { installOverlayScrollbars, OVERLAY_SCROLL_HIDE_DELAY } from "./overlayScroll";
export {
  formatPhoneInput,
  formatPhoneNumber,
  isValidPhoneNumber,
  normalizePhoneNumber,
} from "./phone";
import "./styles.css";
import "./avatar-palette.generated.css";
export { ContactAvatar } from "./ContactAvatar";
export {
  AVATAR_PALETTE_SIZE,
  contactSeed,
  conversationAvatar,
  initials,
  paletteIndex,
  seedKey,
  type AvatarIdentity,
  type AvatarKind,
} from "./avatar";
export {
  formatDateTime,
  formatElapsedShort,
  formatRelativeTime,
  isoDateTime,
  toDate,
  type DateTimeFormatOptions,
  type DateTimeInput,
} from "./datetime";
export {
  Archive,
  ArchiveRestore,
  Bell,
  Check,
  CheckCheck,
  CheckCircle,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Clock,
  Copy,
  Download,
  ExternalLink,
  FileText,
  FlaskConical,
  Image,
  Info,
  ImageDown,
  ImageOff,
  Link,
  ListFilter,
  Loader2,
  Lock,
  LockKeyhole,
  LockOpen,
  Palette,
  MessageSquare,
  MessageSquarePlus,
  Minus,
  Plus,
  Radio,
  RotateCw,
  ShieldAlert,
  ShieldCheck,
  Smartphone,
  SquarePen,
  Trash2,
  TriangleAlert,
  User,
  UserPlus,
  Users,
  WifiOff,
  X,
} from "lucide-react";

export const tokens = {
  sidebarWidth: 280,
  rowHeight: 56,
  avatarSize: 36,
  messageFont: "15px",
  space: { xs: 4, sm: 8, md: 12, lg: 16, xl: 24 },
} as const;
/** Deprecated inline-token bridge retained for downstream compatibility. */
export const themeTokens = {
  light: { "--surface-0": "#fff" },
  dark: { "--surface-0": "#1f1f1f" },
} as const;
export type Conversation = {
  id: string;
  name: string;
  /** Display-only identity, seeded from raw addresses. */
  avatar?: AvatarIdentity;
  preview: string;
  unread: number;
  status?: string;
  recipientlessDraft?: boolean;
  draftOnly?: boolean;
  pinned?: boolean;
  folder?: ConversationFolder;
  folderReason?: "marked" | "blocked";
  unblock?: "pending" | "failed";
  canUnblock?: boolean;
};
export type Attachment = {
  id: string;
  name: string;
  state: "pending" | "ready" | "uploading" | "failed";
  error?: string;
  previewUrl?: string;
};

export type StatusTone = "neutral" | "ok" | "warning" | "error";
export type PanelStatus = { tone: StatusTone; summary: string; details: ReactNode };
export type NavigationView = "conversations" | "notifications" | "settings" | "contacts";
export type NavigationPosition = "side-rail" | "title-bar";
export type PopoutMode = "bubble" | "window";

export function isNavigationPosition(value: unknown): value is NavigationPosition {
  return value === "side-rail" || value === "title-bar";
}

export function isPopoutMode(value: unknown): value is PopoutMode {
  return value === "bubble" || value === "window";
}

export function StatusPopover({ tone, summary, children }: { tone: StatusTone; summary: string; children?: ReactNode }): ReactElement {
  const panelId = `status-panel-${useId()}`;
  const wrapperRef = useRef<HTMLDivElement>(null);
  const hoverTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const suppressReopenRef = useRef(false);
  const announcedSummaryRef = useRef(summary);
  const [open, setOpen] = useState(false);
  const [pinned, setPinned] = useState(false);
  const [announcedSummary, setAnnouncedSummary] = useState(summary);
  const clearHoverTimer = () => {
    if (hoverTimerRef.current !== null) clearTimeout(hoverTimerRef.current);
    hoverTimerRef.current = null;
  };
  const dismiss = () => {
    clearHoverTimer();
    suppressReopenRef.current = true;
    setPinned(false);
    setOpen(false);
  };
  useEffect(() => () => clearHoverTimer(), []);
  useEffect(() => {
    if (!open && summary !== announcedSummaryRef.current) {
      announcedSummaryRef.current = summary;
      setAnnouncedSummary(summary);
    }
  }, [open, summary]);
  useEffect(() => {
    if (!open) return;
    const onDocumentPointerDown = (event: globalThis.PointerEvent) => {
      if (event.target instanceof Node && !wrapperRef.current?.contains(event.target)) dismiss();
    };
    const onDocumentKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") dismiss();
    };
    document.addEventListener("pointerdown", onDocumentPointerDown);
    document.addEventListener("keydown", onDocumentKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onDocumentPointerDown);
      document.removeEventListener("keydown", onDocumentKeyDown);
    };
  }, [open]);
  const openAfterHoverDelay = () => {
    clearHoverTimer();
    suppressReopenRef.current = false;
    hoverTimerRef.current = setTimeout(() => {
      hoverTimerRef.current = null;
      if (!suppressReopenRef.current) setOpen(true);
    }, 120);
  };
  const closeAfterHoverDelay = () => {
    if (pinned) return;
    clearHoverTimer();
    hoverTimerRef.current = setTimeout(() => {
      hoverTimerRef.current = null;
      if (!pinned) setOpen(false);
    }, 200);
  };
  return <div
    ref={wrapperRef}
    data-status-popover
    onPointerEnter={openAfterHoverDelay}
    onPointerLeave={closeAfterHoverDelay}
    onFocusCapture={(event) => {
      if (event.relatedTarget instanceof Node && wrapperRef.current?.contains(event.relatedTarget)) return;
      clearHoverTimer();
      suppressReopenRef.current = false;
      setOpen(true);
    }}
    onBlurCapture={(event) => {
      if (pinned || (event.relatedTarget instanceof Node && wrapperRef.current?.contains(event.relatedTarget))) return;
      dismiss();
    }}
  >
    <button type="button" data-status-trigger data-status-tone={tone} aria-label={`Status: ${summary}`} aria-controls={panelId} aria-expanded={open} onClick={() => {
      clearHoverTimer();
      if (pinned) dismiss();
      else {
        suppressReopenRef.current = false;
        setPinned(true);
        setOpen(true);
      }
    }}>
      <span className="connection-dot" aria-hidden />
    </button>
    <div id={panelId} data-status-panel role="region" aria-label="Status details" hidden={!open}>{children}</div>
    <span data-status-announcement className="sr-only" role="status">{announcedSummary}</span>
  </div>;
}

export function ConnectionDot({ state, label, serverUnreachable = false }: { state: string; label: string; serverUnreachable?: boolean }): ReactElement {
  return <span role="status" aria-label={label} title={label} data-connection-state={state} data-server-unreachable={serverUnreachable || undefined}>
    <span className="connection-dot" aria-hidden />
  </span>;
}

export type RecipientChip = { id: string; label: string; avatarUrl?: string; avatarSeed?: string; avatarName?: string };
/** A contact phone offered while typing; `id` is the phone address that becomes the recipient. */
export type RecipientSuggestion = { id: string; rowId?: string; label: string; detail?: string; avatarUrl?: string; avatarSeed?: string; avatarName?: string };
export type RecipientPosition = { x: number; y: number };

type RecipientSuggestionRow = { suggestion: RecipientSuggestion; rowId: string };

function recipientSuggestionRows(suggestions: RecipientSuggestion[]): RecipientSuggestionRow[] {
  const occurrences = new Map<string, number>();
  return suggestions.map((suggestion) => {
    const content = suggestion.rowId ?? JSON.stringify([
      suggestion.id,
      suggestion.label,
      suggestion.detail ?? "",
      suggestion.avatarUrl ?? "",
      suggestion.avatarSeed ?? "",
      suggestion.avatarName ?? "",
    ]);
    const occurrence = occurrences.get(content) ?? 0;
    occurrences.set(content, occurrence + 1);
    return { suggestion, rowId: occurrence ? `${content}:${occurrence}` : content };
  });
}

function recipientSuggestionOptionId(listId: string, rowId: string): string {
  return `${listId}-recipient-${encodeURIComponent(rowId)}`;
}

export function isRecipientPosition(value: unknown): value is RecipientPosition {
  return typeof value === "object" && value !== null
    && Number.isFinite((value as RecipientPosition).x) && (value as RecipientPosition).x >= 0
    && Number.isFinite((value as RecipientPosition).y) && (value as RecipientPosition).y >= 0;
}

function recipientLabel(label: string): string {
  return normalizePhoneNumber(label) ? formatPhoneNumber(label) : label;
}

function selectionForDigitCount(value: string, digits: number): number {
  if (!digits) return 0;
  let seen = 0;
  for (let index = 0; index < value.length; index += 1) {
    if (/\d/.test(value[index])) seen += 1;
    if (seen === digits) return index + 1;
  }
  return value.length;
}

export function RecipientPanel({
  recipients,
  onCommit,
  position,
  onPositionChange,
  hint,
  bottomOffset = 8,
  onPendingChange,
  searchContacts,
  onSuggestionChosen,
}: {
  recipients: RecipientChip[];
  onCommit(ids: string[]): void;
  position: RecipientPosition | null;
  onPositionChange(position: RecipientPosition): void;
  hint?: string;
  bottomOffset?: number;
  onPendingChange?(pending: boolean): void;
  /** Contact discovery: phone numbers (not contact IDs) matching a name or digits. */
  searchContacts?(query: string): Promise<RecipientSuggestion[]>;
  onSuggestionChosen?(suggestion: RecipientSuggestion): void;
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  const panelRef = useRef<HTMLElement>(null);
  const boundaryRef = useRef<HTMLElement>(null);
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [dimensions, setDimensions] = useState({ boundaryWidth: 0, boundaryHeight: 0, panelWidth: 0, panelHeight: 0 });
  const [livePosition, setLivePosition] = useState<RecipientPosition | null>(null);
  const [dragging, setDragging] = useState(false);
  const latestCommittedPositionRef = useRef<RecipientPosition | null>(null);
  const controlledPositionRef = useRef<RecipientPosition | null>(null);
  const dragRef = useRef<{
    pointerId: number;
    startX: number;
    startY: number;
    startPosition: RecipientPosition;
    clientX: number;
    clientY: number;
  } | null>(null);
  const pendingSelectionRef = useRef<number | null>(null);
  const skipBlurCommitRef = useRef(false);
  const [suggestions, setSuggestions] = useState<RecipientSuggestion[]>([]);
  const [activeSuggestion, setActiveSuggestion] = useState(0);
  const searchSeqRef = useRef(0);
  const suggestionListId = useId();
  const suggestionRows = recipientSuggestionRows(suggestions);
  const recipientIds = recipients.map((recipient) => recipient.id).join("\n");
  const chosenRecipientIds = new Set(recipients.map((recipient) => recipient.id));
  // Latest callback without re-running the search on every parent render.
  const searchRef = useRef(searchContacts);
  searchRef.current = searchContacts;
  const canSearch = Boolean(searchContacts);
  useEffect(() => {
    const query = value.trim();
    const request = ++searchSeqRef.current;
    const search = searchRef.current;
    if (!canSearch || !search || !query) {
      setSuggestions([]);
      return;
    }
    const timer = setTimeout(() => {
      void search(query)
        .then((found) => {
          if (request !== searchSeqRef.current) return;
          const chosen = new Set(recipientIds.split("\n"));
          setSuggestions(found.filter((item) => !chosen.has(item.id)).slice(0, 20));
          setActiveSuggestion(0);
        })
        .catch(() => {
          if (request === searchSeqRef.current) setSuggestions([]);
        });
    }, 120);
    return () => clearTimeout(timer);
  }, [canSearch, value, recipientIds]);
  const chooseSuggestion = (suggestion: RecipientSuggestion) => {
    searchSeqRef.current += 1;
    setSuggestions([]);
    onSuggestionChosen?.(suggestion);
    if (!chosenRecipientIds.has(suggestion.id)) {
      onCommit([...recipients.map((recipient) => recipient.id), suggestion.id]);
    }
    setError(null);
    setValue("");
    inputRef.current?.focus();
  };

  useEffect(() => {
    onPendingChange?.(Boolean(value));
  }, [onPendingChange, value]);
  useLayoutEffect(() => {
    const selection = pendingSelectionRef.current;
    if (selection === null || !inputRef.current) return;
    inputRef.current.setSelectionRange(selection, selection);
    pendingSelectionRef.current = null;
  }, [value]);

  const boundaryForPanel = (panel = panelRef.current) => panel?.closest<HTMLElement>("#recipient-rail-layer")
    ?? panel?.closest<HTMLElement>("#conversation-stage");
  const measure = () => {
    const panel = panelRef.current;
    const boundary = boundaryForPanel(panel);
    if (!panel || !boundary) return;
    boundaryRef.current = boundary;
    const boundaryRect = boundary.getBoundingClientRect();
    const panelRect = panel.getBoundingClientRect();
    const next = {
      boundaryWidth: boundaryRect.width,
      boundaryHeight: boundaryRect.height,
      panelWidth: panelRect.width,
      panelHeight: panelRect.height,
    };
    if (Object.values(next).every(Number.isFinite)) {
      setDimensions((current) => Object.keys(next).every((key) => current[key as keyof typeof current] === next[key as keyof typeof next]) ? current : next);
    }
  };
  useLayoutEffect(() => {
    measure();
    const boundary = boundaryRef.current;
    const panel = panelRef.current;
    if (!boundary || !panel || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(boundary);
    observer.observe(panel);
    return () => observer.disconnect();
  }, [bottomOffset, recipients.length]);

  const hasBounds = (current = dimensions) => current.boundaryWidth > 0 && current.boundaryHeight > 0 && current.panelWidth > 0 && current.panelHeight > 0;
  const clamp = (candidate: RecipientPosition, current = dimensions): RecipientPosition => !hasBounds(current) ? candidate : {
    x: Math.min(Math.max(candidate.x, 0), Math.max(current.boundaryWidth - current.panelWidth, 0)),
    y: Math.min(Math.max(candidate.y, 0), Math.max(current.boundaryHeight - current.panelHeight, 0)),
  };
  const defaultPosition = hasBounds()
    ? clamp({ x: 12, y: dimensions.boundaryHeight - bottomOffset - dimensions.panelHeight })
    : { x: 12, y: 0 };
  if (position && (position.x !== controlledPositionRef.current?.x || position.y !== controlledPositionRef.current?.y)) {
    controlledPositionRef.current = { ...position };
    latestCommittedPositionRef.current = { ...position };
  }
  const displayedPosition = clamp(livePosition ?? position ?? defaultPosition);
  const currentBounds = () => {
    const panel = panelRef.current;
    const boundary = boundaryForPanel(panel);
    if (!panel || !boundary) return dimensions;
    boundaryRef.current = boundary;
    const boundaryRect = boundary.getBoundingClientRect();
    const panelRect = panel.getBoundingClientRect();
    return boundaryRect.width > 0 && boundaryRect.height > 0 && panelRect.width > 0 && panelRect.height > 0
      ? { boundaryWidth: boundaryRect.width, boundaryHeight: boundaryRect.height, panelWidth: panelRect.width, panelHeight: panelRect.height }
      : dimensions;
  };

  const commitTokens = (text: string, keepTrailing = false) => {
    const pieces = text.split(/[,;\n]/);
    const trailing = keepTrailing && !/[,;\n]$/.test(text) ? pieces.pop() ?? "" : "";
    if (trailing && /[^\d\s().+-]/.test(trailing)) {
      setError("Enter a complete phone number.");
      setValue(text);
      return;
    }
    const newIds = pieces.map((piece) => piece.trim()).filter(Boolean);
    const normalized = newIds.map(normalizePhoneNumber);
    if (normalized.some((id) => !id)) {
      setError("Enter a complete phone number.");
      setValue(text);
      return;
    }
    const existingIds = recipients.map((recipient) => recipient.id);
    const seen = new Set(existingIds.map((id) => normalizePhoneNumber(id) ?? id));
    const newIdsToAppend = (normalized as string[]).filter((id) => {
      if (seen.has(id)) return false;
      seen.add(id);
      return true;
    });
    const next = [...existingIds, ...newIdsToAppend];
    if (next.length !== recipients.length || next.some((id, index) => id !== recipients[index]?.id)) {
      onCommit(next);
    }
    setError(null);
    setValue(trailing.trim());
  };
  const updateValue = (nextValue: string, selectionStart: number | null, composing = false) => {
    if (composing) {
      setValue(nextValue);
      return;
    }
    const formatted = formatPhoneInput(nextValue);
    if (formatted !== nextValue && selectionStart !== null) {
      pendingSelectionRef.current = selectionForDigitCount(formatted, (nextValue.slice(0, selectionStart).match(/\d/g) ?? []).length);
    }
    setError(null);
    setValue(formatted);
  };
  const onGripPointerDown = (event: PointerEvent<HTMLButtonElement>) => {
    if (event.button && event.button !== 0) return;
    const panel = panelRef.current;
    const boundary = boundaryForPanel(panel);
    if (!panel || !boundary) return;
    boundaryRef.current = boundary;
    event.currentTarget.setPointerCapture?.(event.pointerId);
    dragRef.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      startPosition: displayedPosition,
      clientX: event.clientX,
      clientY: event.clientY,
    };
    setDragging(true);
  };
  const onGripPointerMove = (event: PointerEvent<HTMLButtonElement>) => {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    drag.clientX = event.clientX;
    drag.clientY = event.clientY;
    setLivePosition(clamp({ x: drag.startPosition.x + event.clientX - drag.startX, y: drag.startPosition.y + event.clientY - drag.startY }, currentBounds()));
  };
  const finishDrag = (event: PointerEvent<HTMLButtonElement>, cancelled = false) => {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    drag.clientX = event.clientX;
    drag.clientY = event.clientY;
    const next = clamp({ x: drag.startPosition.x + drag.clientX - drag.startX, y: drag.startPosition.y + drag.clientY - drag.startY }, currentBounds());
    event.currentTarget.releasePointerCapture?.(event.pointerId);
    dragRef.current = null;
    setDragging(false);
    setLivePosition(null);
    if (!cancelled && (next.x !== drag.startPosition.x || next.y !== drag.startPosition.y)) {
      latestCommittedPositionRef.current = next;
      onPositionChange(next);
    }
  };
  const gripKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
    const amount = event.shiftKey ? 32 : 8;
    const delta = event.key === "ArrowUp" ? { x: 0, y: -amount }
      : event.key === "ArrowDown" ? { x: 0, y: amount }
      : event.key === "ArrowLeft" ? { x: -amount, y: 0 }
      : event.key === "ArrowRight" ? { x: amount, y: 0 }
      : null;
    if (!delta) return;
    event.preventDefault();
    const base = latestCommittedPositionRef.current ?? displayedPosition;
    const next = clamp({ x: base.x + delta.x, y: base.y + delta.y }, currentBounds());
    latestCommittedPositionRef.current = next;
    onPositionChange(next);
  };

  return <section
      id="draft-recipients"
      ref={panelRef}
      aria-label="Message recipients"
      data-dragging={dragging ? "true" : undefined}
      style={{ left: displayedPosition.x, top: displayedPosition.y }}
    >
      <button
        id="recipient-panel-grip"
        type="button"
        aria-label="Move recipients panel"
        title="Move recipients panel"
        onPointerDown={onGripPointerDown}
        onPointerMove={onGripPointerMove}
        onPointerUp={finishDrag}
        onPointerCancel={(event) => finishDrag(event, true)}
        onKeyDown={gripKeyDown}
      >
        <GripVertical size={14} aria-hidden />
        <span className="sr-only">Use arrow keys to move recipients; hold Shift to move further.</span>
      </button>
      <span className="recipient-label" aria-hidden>To</span>
      <ul id="recipient-chips" aria-label="Recipient list">
        {recipients.map((recipient) => {
          return <li key={recipient.id} data-recipient-id={recipient.id} className="recipient-chip">
            <ContactAvatar seed={recipient.avatarSeed ?? seedKey(recipient.id)} photoDataUrl={recipient.avatarUrl} name={recipient.avatarName} size={18} className="recipient-avatar" />
            <span className="recipient-chip-label">{recipientLabel(recipient.label)}</span>
            <button
              type="button"
              data-recipient-remove
              aria-label={`Remove ${recipientLabel(recipient.label)}`}
              title={`Remove ${recipientLabel(recipient.label)}`}
              onPointerDown={() => { skipBlurCommitRef.current = true; }}
              onClick={() => {
                onCommit(recipients.filter((item) => item.id !== recipient.id).map((item) => item.id));
                inputRef.current?.focus();
                skipBlurCommitRef.current = false;
              }}
            >
              <X size={12} aria-hidden />
            </button>
          </li>;
        })}
      </ul>
      <input
        ref={inputRef}
        aria-label="Recipients"
        aria-controls={suggestionRows.length ? suggestionListId : undefined}
        aria-activedescendant={suggestionRows.length ? recipientSuggestionOptionId(suggestionListId, suggestionRows[Math.min(activeSuggestion, suggestionRows.length - 1)].rowId) : undefined}
        aria-autocomplete={searchContacts ? "list" : undefined}
        aria-describedby={`draft-recipients-hint${error ? " draft-recipients-error" : ""}`}
        aria-invalid={error ? "true" : undefined}
        autoComplete="tel"
        inputMode="tel"
        placeholder={recipients.length ? "Add" : "Add phone number"}
        size={Math.min(Math.max(value.length, recipients.length ? 3 : 16), 24)}
        value={value}
        onChange={(event) => updateValue(event.target.value, event.target.selectionStart, (event.nativeEvent as InputEvent).isComposing)}
        onKeyDown={(event) => {
          if (event.nativeEvent.isComposing) return;
          if (suggestions.length && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
            event.preventDefault();
            setActiveSuggestion((current) => event.key === "ArrowDown"
              ? Math.min(current + 1, suggestions.length - 1)
              : Math.max(current - 1, 0));
          } else if (suggestions.length && event.key === "Escape") {
            event.preventDefault();
            searchSeqRef.current += 1;
            setSuggestions([]);
          } else if (event.key === "Enter" && suggestions.length && !normalizePhoneNumber(value)) {
            // A typed complete number still commits itself; names pick the highlighted phone.
            event.preventDefault();
            chooseSuggestion(suggestions[Math.min(activeSuggestion, suggestions.length - 1)]);
          } else if (event.key === "Enter") {
            event.preventDefault();
            commitTokens(value);
          } else if (event.key === "," || event.key === ";") {
            event.preventDefault();
            commitTokens(`${value}${event.key}`);
          } else if (event.key === "Backspace" && !value && recipients.length) {
            onCommit(recipients.slice(0, -1).map((recipient) => recipient.id));
          } else if (event.key === "Backspace" && event.currentTarget.selectionStart === event.currentTarget.selectionEnd && event.currentTarget.selectionStart && !/\d/.test(value[event.currentTarget.selectionStart - 1])) {
            event.preventDefault();
            let digitIndex = event.currentTarget.selectionStart - 1;
            while (digitIndex >= 0 && !/\d/.test(value[digitIndex])) digitIndex -= 1;
            if (digitIndex < 0) return;
            updateValue(`${value.slice(0, digitIndex)}${value.slice(digitIndex + 1)}`, digitIndex);
          }
        }}
        onBlur={(event) => {
          const nextTarget = event.relatedTarget as HTMLElement | null;
          if (suggestions.length && !normalizePhoneNumber(value) && /[^\d\s().+-]/.test(value)) {
            // A partial name is search text, not a number to commit.
            return;
          }
          if (skipBlurCommitRef.current || nextTarget?.matches("[data-recipient-remove]")) {
            skipBlurCommitRef.current = false;
            return;
          }
          commitTokens(value);
        }}
        onPaste={(event) => {
          const pasted = event.clipboardData.getData("text");
          if (!/[,;\n]/.test(pasted)) return;
          event.preventDefault();
          commitTokens(`${value}${pasted}`, true);
        }}
      />
      {suggestionRows.length > 0 && (
        <ul id={suggestionListId} role="listbox" aria-label="Contact suggestions" className="recipient-suggestions" data-recipient-suggestions>
          {suggestionRows.map(({ suggestion, rowId }, index) => (
            <li
              key={rowId}
              id={recipientSuggestionOptionId(suggestionListId, rowId)}
              role="option"
              aria-selected={index === activeSuggestion}
              data-suggestion-address={suggestion.id}
              onMouseDown={(event) => {
                event.preventDefault();
                chooseSuggestion(suggestion);
              }}
            >
              <ContactAvatar seed={suggestion.avatarSeed ?? seedKey(suggestion.id)} photoDataUrl={suggestion.avatarUrl} name={suggestion.avatarName} size={18} className="recipient-avatar" />
              <span className="recipient-suggestion-copy">
                <b>{suggestion.label}</b>
                {suggestion.detail && <small>{suggestion.detail}</small>}
              </span>
            </li>
          ))}
        </ul>
      )}
      {recipients.length > 1 && <span className="recipient-group-tag" data-recipient-group>Group · MMS</span>}
      <span id="draft-recipients-hint" className="sr-only">{hint}</span>
      {error && <span id="draft-recipients-error" className="recipient-error" role="alert">{error}</span>}
    </section>;
}

function smsCounter(text: string): string | null {
  if (!text) return null;
  const gsmBasic = new Set(
    "@£$¥èéùìòÇ\nØø\rÅåΔ_ΦΓΛΩΠΨΣΘΞ\u001bÆæßÉ " +
      "!\"#¤%&'()*+,-./0123456789:;<=>?¡" +
      "ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÑÜ§¿" +
      "abcdefghijklmnopqrstuvwxyzäöñüà",
  );
  const gsmExtension = new Set("^{}\\[~]|€\f");
  const gsm7 = Array.from(text).every(
    (character) => gsmBasic.has(character) || gsmExtension.has(character),
  );
  const single = gsm7 ? 160 : 70;
  const multi = gsm7 ? 153 : 67;
  const units = gsm7
    ? Array.from(text).reduce(
        (total, character) => total + (gsmExtension.has(character) ? 2 : 1),
        0,
      )
    : text.length;
  if (units < Math.floor(single * 0.85)) return null;
  if (units <= single) return `${units}/${single}`;
  const segments = Math.ceil(units / multi);
  const remaining = segments * multi - units;
  return `${segments} SMS · ${remaining} left`;
}

export function detectPlatform(): "macos" | "windows" | "linux" {
  if (typeof navigator === "undefined") return "linux";
  const source = navigator.platform ?? navigator.userAgent;
  return /Mac/.test(source) ? "macos" : /Win/.test(source) ? "windows" : "linux";
}

const logoUrl = new URL("./assets/peppy-logo-coral.svg", import.meta.url).href;

export function AppTitlebar({
  onMinimize,
  onMaximize,
  onClose,
  platform,
  nativeWindowControls = false,
  isComposer = false,
  title,
  status,
  onToggleSidebar,
  sidebarExpanded,
  sidebarControls,
  onNewMessage,
  newMessageDisabled = false,
  navigation,
  search,
  viewSwitch,
  actions,
}: {
  onMinimize?(): void;
  onMaximize?(): void;
  onClose?(): void;
  simulated?: boolean;
  platform?: "macos" | "windows" | "linux";
  nativeWindowControls?: boolean;
  isComposer?: boolean;
  title?: string;
  status?: ReactNode;
  onToggleSidebar?(): void;
  sidebarExpanded?: boolean;
  sidebarControls?: string;
  onNewMessage?(): void;
  newMessageDisabled?: boolean;
  navigation?: ReactNode;
  search?: ReactNode;
  viewSwitch?: ReactNode;
  actions?: ReactNode;
}) {
  const titlebarRef = useRef<HTMLElement>(null);
  const macos = (platform ?? detectPlatform()) === "macos";
  const hasSearch = Boolean(search);
  const renderedNavigation = !isComposer && navigation;
  useLayoutEffect(() => {
    const titlebar = titlebarRef.current;
    if (isComposer || !hasSearch || !titlebar || typeof ResizeObserver === "undefined") return;
    const previousHeight = titlebar.style.getPropertyValue("--peppy-active-titlebar-height");
    const updateHeight = () => {
      const height = `${titlebar.getBoundingClientRect().height}px`;
      if (titlebar.style.getPropertyValue("--peppy-active-titlebar-height") !== height) {
        titlebar.style.setProperty("--peppy-active-titlebar-height", height);
      }
    };
    const observer = new ResizeObserver(updateHeight);
    observer.observe(titlebar);
    updateHeight();
    return () => {
      observer.disconnect();
      if (previousHeight) titlebar.style.setProperty("--peppy-active-titlebar-height", previousHeight);
      else titlebar.style.removeProperty("--peppy-active-titlebar-height");
    };
  }, [hasSearch, isComposer]);
  return (
    <header
      id="desktop-titlebar"
      ref={titlebarRef}
      className="titlebar"
      data-tauri-drag-region
      role="banner"
      aria-label="Peppy title bar"
      data-composer={isComposer || undefined}
      data-navigation-placement={renderedNavigation ? (macos ? "trailing" : "leading") : undefined}
    >
      {!macos && renderedNavigation}
      {macos && nativeWindowControls ? <div
        id="window-controls"
        className="window-controls window-controls-macos"
        data-native-window-controls
        aria-hidden="true"
        data-tauri-drag-region
      /> : macos && (onClose || onMinimize || onMaximize) && <div
        id="window-controls"
        className="window-controls window-controls-macos"
        role="toolbar"
        aria-label="Window controls"
      >
        {onClose && <button
          className="traffic-light traffic-light-close"
          aria-label={isComposer ? "Close composer" : "Close window"}
          title={isComposer ? "Close composer" : "Close window"}
          onClick={onClose}
        >
          <X size={10} strokeWidth={1.75} aria-hidden />
        </button>}
        {onMinimize && <button
          className="traffic-light traffic-light-minimize"
          aria-label="Minimize window"
          title="Minimize window"
          onClick={onMinimize}
          disabled={isComposer}
          aria-hidden={isComposer || undefined}
          tabIndex={isComposer ? -1 : undefined}
        ><Minus size={10} strokeWidth={1.75} aria-hidden /></button>}
        {onMaximize && <button
          className="traffic-light traffic-light-maximize"
          aria-label="Maximize window"
          title="Maximize window"
          onClick={onMaximize}
          disabled={isComposer}
          aria-hidden={isComposer || undefined}
          tabIndex={isComposer ? -1 : undefined}
        ><Maximize2 size={10} strokeWidth={2} aria-hidden /></button>}
      </div>}
      {!isComposer && (onToggleSidebar || onNewMessage || actions) && <div id="titlebar-actions" role="toolbar" aria-label="Conversation controls">
        {onToggleSidebar && <button
          type="button"
          data-action="toggle-sidebar"
          aria-label="Toggle conversation list"
          title="Toggle conversation list"
          aria-expanded={sidebarExpanded}
          aria-controls={sidebarControls}
          onClick={onToggleSidebar}
        ><PanelLeft size={16} strokeWidth={1.75} aria-hidden /></button>}
        {onNewMessage && <button
          type="button"
          data-action="new-message"
          aria-label="New conversation"
          title="New conversation"
          disabled={newMessageDisabled}
          onClick={onNewMessage}
        ><MessageSquarePlus size={16} strokeWidth={1.75} aria-hidden /></button>}
        {actions}
      </div>}
      <span className="titlebar-product" aria-hidden>
        <img src={logoUrl} alt="" draggable={false} />
      </span>
      {!isComposer && search}
      {(!search || isComposer) && <strong className="titlebar-wordmark" data-tauri-drag-region>
        {isComposer ? (
          <b className="titlebar-conversation-title">
            {title ?? "Compose message"}
          </b>
        ) : "Peppy"}
      </strong>}
      <div className="titlebar-spacer" data-tauri-drag-region />
      {isComposer && viewSwitch && <div id="titlebar-view-switch" className="titlebar-view-switch">{viewSwitch}</div>}
      {status && <div id="titlebar-status" className="titlebar-status">{status}</div>}
      {!macos && (onClose || (!isComposer && (onMinimize || onMaximize))) && <div
        id="window-controls"
        className="window-controls window-controls-windows"
        role="toolbar"
        aria-label="Window controls"
      >
        {!isComposer && (
          <>
            {onMinimize && <button
              aria-label="Minimize window"
              title="Minimize window"
              onClick={onMinimize}
            >
              <Minus size={10} strokeWidth={1.75} aria-hidden />
            </button>}
            {onMaximize && <button
              id="window-maximize"
              aria-label="Maximize window"
              title="Maximize window"
              onClick={onMaximize}
            >
              <Square size={10} strokeWidth={1.75} aria-hidden />
            </button>}
          </>
        )}
        {onClose && <button
          aria-label={isComposer ? "Close composer" : "Close window"}
          title={isComposer ? "Close composer" : "Close window"}
          onClick={onClose}
          className="close-button"
        >
          <X size={10} strokeWidth={1.75} aria-hidden />
        </button>}
      </div>}
      {macos && renderedNavigation}
    </header>
  );
}

const FOLDER_CONFIRMATION_ID = "conversation-folder-confirmation";
const SENDER_UNBLOCK_CONFIRMATION_ID = "sender-unblock-confirmation";
type FolderActionKind = "archive" | "restore" | "spam" | "notSpam";
const FOLDER_ACTIONS: Record<FolderActionKind, { to: ConversationFolder; label: string; title(name: string): string; Icon: typeof Archive; hook: string; confirms: boolean }> = {
  archive: { to: "archived", label: "Archive conversation", title: (name) => `Archive ${name}`, Icon: Archive, hook: "data-archive-conversation-id", confirms: true },
  restore: { to: "inbox", label: "Move conversation to Inbox", title: (name) => `Move ${name} to Inbox`, Icon: ArchiveRestore, hook: "data-folder-conversation-id", confirms: false },
  spam: { to: "spam", label: "Mark conversation as spam", title: (name) => `Mark ${name} as spam`, Icon: ShieldAlert, hook: "data-spam-conversation-id", confirms: true },
  notSpam: { to: "inbox", label: "Not spam", title: (name) => `Move ${name} out of Spam`, Icon: ShieldCheck, hook: "data-folder-conversation-id", confirms: false },
};
function folderActionsFor(folder: ConversationFolder): FolderActionKind[] {
  if (folder === "spam") return ["notSpam"];
  return folder === "archived" ? ["restore", "spam"] : ["archive", "spam"];
}

export function ConversationList({
  conversations,
  selectedId,
  onSelect,
  onPopout,
  onDelete,
  onTogglePin,
  onCancelDelete,
  deleteConfirmationId,
  deletingId,
  deleteWarningId,
  pendingPinId,
  onRequestFolderChange,
  pendingFolderId,
  pendingFolder,
  confirmingFolderId,
  confirmingFolder,
  emptyLabel = "No conversations yet",
  loading = false,
  popoutMode = "bubble",
}: {
  conversations: Conversation[];
  selectedId: string;
  onSelect(id: string): void;
  onPopout?(id: string): void;
  onDelete?(id: string): void;
  /** Requests the desired durable pin state for an established conversation. */
  onTogglePin?(id: string, pinned: boolean): void;
  onCancelDelete?(): void;
  deleteConfirmationId?: string;
  deletingId?: string;
  deleteWarningId?: string;
  pendingPinId?: string;
  /** Requests a folder change; the caller confirms archive and spam before writing. */
  onRequestFolderChange?(id: string, folder: ConversationFolder, trigger: HTMLButtonElement): void;
  pendingFolderId?: string;
  /** Target folder of the pending change; without it every folder action on the pending row shows as pending. */
  pendingFolder?: ConversationFolder;
  confirmingFolderId?: string;
  /** Target of the open confirmation on `confirmingFolderId`; without it both archive and spam read as expanded. */
  confirmingFolder?: "archived" | "spam";
  /** Shown when the list has no rows. */
  emptyLabel?: string;
  loading?: boolean;
  popoutMode?: PopoutMode;
}) {
  return (
    <nav id="conversation-list" aria-label="Conversations" aria-busy={loading}>
      <ul role="list">
        {loading ? (
          <li className="sidebar-empty">Loading conversations…</li>
        ) : conversations.length ? (
          conversations.map((c) => {
            const isDeleteConfirmation = deleteConfirmationId === c.id;
            const isPinPending = pendingPinId === c.id;
            const isFolderPending = pendingFolderId === c.id;
            const isOtherActionPending = pendingPinId !== undefined || pendingFolderId !== undefined;
            const folder = c.folder ?? "inbox";
            const canPin = !c.draftOnly && folder !== "spam";
            const canUnpin = Boolean(onTogglePin && canPin && c.pinned);
            const hasTrailingPinAction = Boolean(onTogglePin && canPin && !c.pinned);
            return <li key={c.id}>
              <div className={`conversation-row ${selectedId === c.id ? "selected" : ""}`} data-conversation-row={c.id}>
                {canUnpin && <button
                  type="button"
                  className={`conversation-pin conversation-pin-leading ${isPinPending ? "pending" : ""}`}
                  data-pin-conversation-id={c.id}
                  aria-label="Unpin conversation"
                  title={`Unpin ${c.name}`}
                  disabled={isOtherActionPending}
                  onClick={() => onTogglePin?.(c.id, false)}
                >
                  {isPinPending ? <Loader className="busy-indicator-icon" size={12} aria-hidden /> : <><Pin className="conversation-pin-icon" size={12} aria-hidden /><PinOff className="conversation-unpin-icon" size={12} aria-hidden /></>}
                </button>}
                <button data-conversation-id={c.id} className="conversation-select" aria-current={selectedId === c.id ? "location" : undefined} onClick={() => onSelect(c.id)}>
                  <ContactAvatar {...(c.avatar ?? conversationAvatar({ id: c.id, name: c.name }))} size={36} className="avatar" />
                  <span className="conversation-copy">
                    <b title={c.name} className={isDeleteConfirmation ? "sr-only" : undefined}>{canUnpin ? <span className="conversation-pin-marker-placeholder" aria-hidden /> : c.pinned && !c.draftOnly && <span className="conversation-pin-marker" role="img" aria-label="Pinned" title="Pinned"><Pin size={12} aria-hidden /></span>}{c.name}</b>
                    {isDeleteConfirmation ? (
                      <small id={deleteWarningId ?? `delete-draft-warning-${c.id}`} className="conversation-delete-warning" role="status">Deleting this draft permanently removes its text and attachments.</small>
                    ) : <small>{c.preview}</small>}
                  </span>
                  {c.unread > 0 && (
                    <span aria-label={`${c.unread} unread messages`} className="unread">
                      <span aria-hidden>{c.unread}</span>
                    </span>
                  )}
                </button>
                {((c.recipientlessDraft && c.draftOnly && onDelete) || onPopout || hasTrailingPinAction || (onRequestFolderChange && !c.draftOnly)) && <div data-conversation-actions data-pending-pin={isPinPending || undefined}>
                  {hasTrailingPinAction && <button
                    type="button"
                    className={`conversation-pin ${isPinPending ? "pending" : ""}`}
                    data-pin-conversation-id={c.id}
                    aria-label="Pin conversation"
                    title={`Pin ${c.name}`}
                    disabled={isOtherActionPending}
                    onClick={(event) => {
                      event.stopPropagation();
                      onTogglePin?.(c.id, true);
                    }}
                  >
                    {isPinPending ? <Loader className="busy-indicator-icon" size={15} aria-hidden /> : <Pin size={15} aria-hidden />}
                  </button>}
                  {onRequestFolderChange && !c.draftOnly && folderActionsFor(folder).map((kind) => {
                    const action = FOLDER_ACTIONS[kind];
                    const isActive = isFolderPending && (pendingFolder === undefined || pendingFolder === action.to);
                    const blockedNotSpam = kind === "notSpam" && c.folderReason === "blocked";
                    const confirms = action.confirms || blockedNotSpam;
                    const isConfirming = confirms && confirmingFolderId === c.id && (confirmingFolder === undefined || confirmingFolder === action.to);
                    return <button
                      key={kind}
                      type="button"
                      className={`conversation-folder-action ${isActive ? "pending" : ""}`}
                      {...{ [action.hook]: c.id }}
                      aria-label={action.label}
                      title={blockedNotSpam ? `Not spam: unblock ${c.name} on your phone` : action.title(c.name)}
                      aria-haspopup={confirms ? "dialog" : undefined}
                      aria-expanded={confirms ? isConfirming : undefined}
                      aria-controls={isConfirming ? blockedNotSpam ? SENDER_UNBLOCK_CONFIRMATION_ID : FOLDER_CONFIRMATION_ID : undefined}
                      disabled={isOtherActionPending}
                      onClick={(event) => {
                        event.stopPropagation();
                        onRequestFolderChange(c.id, action.to, event.currentTarget);
                      }}
                    >
                      {isActive ? <Loader className="busy-indicator-icon" size={15} aria-hidden /> : <action.Icon size={15} aria-hidden />}
                    </button>;
                  })}
                  {c.recipientlessDraft && c.draftOnly && onDelete && <button
                    type="button"
                    className={`conversation-delete ${isDeleteConfirmation ? "confirming" : ""}`}
                    data-delete-conversation-id={c.id}
                    aria-describedby={isDeleteConfirmation ? deleteWarningId ?? `delete-draft-warning-${c.id}` : undefined}
                    aria-label={`${isDeleteConfirmation ? "Confirm delete draft" : "Delete draft"} for ${c.name}`}
                    title={isDeleteConfirmation ? "Confirm delete draft" : "Delete draft"}
                    disabled={deletingId === c.id}
                    onBlur={() => isDeleteConfirmation && onCancelDelete?.()}
                    onKeyDown={(event) => {
                      if (event.key !== "Escape" || !isDeleteConfirmation) return;
                      event.preventDefault();
                      event.stopPropagation();
                      onCancelDelete?.();
                    }}
                    onClick={(event) => {
                      event.currentTarget.focus();
                      onDelete(c.id);
                    }}
                  >
                    {deletingId === c.id ? <Loader size={15} aria-hidden /> : <Trash2 size={16} aria-hidden />}
                  </button>}
                  {onPopout && <button type="button" className="conversation-popout" data-popout-conversation-id={c.id} aria-label={popoutMode === "bubble" ? "Open as chat bubble" : "Open in new window"} title={`Open ${c.name} ${popoutMode === "bubble" ? "as chat bubble" : "in new window"}`} onClick={() => onPopout(c.id)}><ExternalLink size={15} aria-hidden /></button>}
                </div>}
              </div>
            </li>;
          })
        ) : (
          <li className="sidebar-empty">{emptyLabel}</li>
        )}
      </ul>
    </nav>
  );
}

type PickerOption =
  | { kind: "existing"; id: string; name: string }
  | { kind: "contact"; id: string; name: string; detail?: string; avatarUrl?: string; avatarSeed?: string; avatarName?: string; value: string }
  | { kind: "new"; id: string; name: string; value: string };
const NEW_OPTION_ID = "new-recipient";
export function RecipientPicker({
  recipients,
  onChange,
  onNewRecipient,
  searchContacts,
}: {
  recipients: Conversation[];
  onChange(ids: string[]): void;
  onNewRecipient?(value: string): void;
  /** Contacts without a conversation: each phone number is its own option (its address). */
  searchContacts?(query: string): Promise<RecipientSuggestion[]>;
}) {
  const [query, setQuery] = useState("");
  const [contactMatches, setContactMatches] = useState<RecipientSuggestion[]>([]);
  const contactSeq = useRef(0);
  const searchRef = useRef(searchContacts);
  searchRef.current = searchContacts;
  const canSearch = Boolean(searchContacts && onNewRecipient);
  useEffect(() => {
    const text = query.trim();
    const request = ++contactSeq.current;
    const search = searchRef.current;
    if (!canSearch || !search || !text) {
      setContactMatches([]);
      return;
    }
    const timer = setTimeout(() => {
      void search(text)
        .then((found) => {
          if (request === contactSeq.current) setContactMatches(found.slice(0, 20));
        })
        .catch(() => {
          if (request === contactSeq.current) setContactMatches([]);
        });
    }, 120);
    return () => clearTimeout(timer);
  }, [canSearch, query]);
  const [chosen, setChosen] = useState<string[]>([]);
  const [active, setActive] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const listId = useId();
  const errorId = useId();
  const contactRows = recipientSuggestionRows(contactMatches);
  const typed = query.trim();
  const normalizedQuery = normalizePhoneNumber(typed);
  const options: PickerOption[] = [
    ...recipients
      .filter(
        (r) =>
          (r.name.toLowerCase().includes(query.toLowerCase()) ||
            (normalizedQuery !== null && normalizePhoneNumber(r.name) === normalizedQuery)) &&
          !chosen.includes(r.id),
      )
      .map((r) => ({ kind: "existing" as const, id: r.id, name: r.name })),
    ...contactRows.map(({ suggestion: match, rowId }) => ({
      kind: "contact" as const,
      id: `contact-${encodeURIComponent(rowId)}`,
      name: match.label,
      detail: match.detail,
      avatarUrl: match.avatarUrl,
      avatarSeed: match.avatarSeed,
      avatarName: match.avatarName,
      value: match.id,
    })),
    ...(onNewRecipient && normalizedQuery && !contactMatches.some((match) => match.id === normalizedQuery)
      ? [
          {
            kind: "new" as const,
            id: NEW_OPTION_ID,
            name: `Message ${formatPhoneNumber(normalizedQuery)}`,
            value: normalizedQuery,
          },
        ]
      : []),
  ];
  const reset = () => {
    setQuery("");
    setActive(0);
  };
  const commit = (option: PickerOption) => {
    if (option.kind === "new" || option.kind === "contact") {
      onNewRecipient?.(option.value);
      setError(null);
      reset();
      return;
    }
    const next = [...chosen, option.id];
    setChosen(next);
    onChange(next);
    reset();
  };
  const keydown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.nativeEvent.isComposing) return;
    if (event.key === "ArrowDown") {
      if (!options.length) return;
      event.preventDefault();
      setActive((x) => Math.min(x + 1, options.length - 1));
    } else if (event.key === "ArrowUp") {
      if (!options.length) return;
      event.preventDefault();
      setActive((x) => Math.max(x - 1, 0));
    } else if (event.key === "Enter") {
      event.preventDefault();
      if (options.length) commit(options[Math.max(0, Math.min(active, options.length - 1))]);
      else if (typed) setError("Enter a complete phone number.");
    } else if (event.key === "Escape") setQuery("");
  };
  return (
    <section
      id="recipient-picker"
      className="recipient-picker"
      aria-label="New message recipients"
    >
      <div className="chips">
        {chosen.map((id) => {
          const r = recipients.find((x) => x.id === id);
          return (
            <button
              key={id}
              className="chip"
              data-recipient-id={id}
              onClick={() => {
                const next = chosen.filter((x) => x !== id);
                setChosen(next);
                onChange(next);
              }}
              aria-label={`Remove ${r?.name ?? id}`}
            >
              {r?.name ?? id} ×
            </button>
          );
        })}
      </div>
      <input
        id="recipient-search"
        role="combobox"
        aria-autocomplete="list"
        aria-expanded={Boolean(query) && options.length > 0}
        aria-haspopup="listbox"
        aria-controls={listId}
        aria-activedescendant={
          query && options[active]
            ? `${listId}-${options[active].id}`
            : undefined
        }
        aria-label="Search recipients"
        aria-invalid={error ? "true" : undefined}
        aria-describedby={error ? errorId : undefined}
        value={query}
        onKeyDown={keydown}
        onChange={(e) => {
          setQuery(e.target.value);
          setActive(0);
          setError(null);
        }}
        placeholder="Search or start new"
      />
      {error && <span id={errorId} className="recipient-picker-error" role="alert">{error}</span>}
      {query && (
        <ul id={listId} role="listbox">
          {options.map((option, index) => (
            <li
              id={`${listId}-${option.id}`}
              role="option"
              aria-selected={index === active}
              key={option.id}
              data-recipient-id={
                option.kind === "existing" ? option.id : undefined
              }
              data-new-recipient={option.kind === "new" ? "true" : undefined}
              data-contact-address={option.kind === "contact" ? option.value : undefined}
              onMouseDown={(e) => {
                e.preventDefault();
                commit(option);
              }}
            >
              {option.kind === "contact" ? (
                <>
                  <ContactAvatar seed={option.avatarSeed ?? seedKey(option.value)} photoDataUrl={option.avatarUrl} name={option.avatarName ?? option.name} size={18} className="recipient-avatar" />
                  <span className="recipient-suggestion-copy">
                    <b>{option.name}</b>
                    {option.detail && <small>{option.detail}</small>}
                  </span>
                </>
              ) : option.name}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

export function Composer({
  draft,
  attachments,
  sendSupported,
  readOnly = false,
  onDraftChange,
  onSend,
  status,
  onAddAttachment,
  onRemoveAttachment,
  unavailableReason,
  gatewaySlot,
  bannerSlot,
  statusSlot,
  statusActive = false,
  composerName,
  placeholder,
  composerUserSized = false,
  maxAutoGrowHeight = 176,
  hideSmsCounter = false,
  typingSlot,
  typingSendState = "idle",
  platform,
}: {
  draft: string;
  attachments: Attachment[];
  sendSupported: boolean;
  /** Blocks editing and sending while the textarea stays focusable. */
  readOnly?: boolean;
  onDraftChange(value: string): void;
  onSend(): void;
  status?: string;
  onAddAttachment?(): void;
  onRemoveAttachment?(id: string): void;
  unavailableReason?: string;
  gatewaySlot?: ReactNode;
  bannerSlot?: ReactNode;
  statusSlot?: ReactNode;
  statusActive?: boolean;
  composerName?: string;
  placeholder?: string;
  composerUserSized?: boolean;
  /** CSS max-height: 176px remains the hard ceiling (8 lines); this can only lower auto-grow. */
  maxAutoGrowHeight?: number;
  hideSmsCounter?: boolean;
  typingSlot?: ReactNode;
  typingSendState?: "idle" | "active" | "suppressed";
  platform?: "macos" | "windows" | "linux";
}) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const hasContent = Boolean(draft.trim()) || attachments.length > 0;
  const canSend = sendSupported && !readOnly && hasContent;
  const counter = !attachments.length ? smsCounter(draft) : null;
  const fallbackShown = !sendSupported && !gatewaySlot && !statusSlot;
  useEffect(() => {
    const textarea = textareaRef.current;
    if (!textarea || composerUserSized) return;
    textarea.style.height = "0";
    textarea.style.height = `${Math.min(textarea.scrollHeight, maxAutoGrowHeight)}px`;
  }, [composerUserSized, draft, maxAutoGrowHeight]);
  const keydown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (
      event.key === "Enter" &&
      !event.shiftKey &&
      !event.nativeEvent.isComposing
    ) {
      event.preventDefault();
      if (canSend) onSend();
    }
  };
  const icon = (state: Attachment["state"]) =>
    state === "ready" ? (
      <Check size={12} />
    ) : state === "failed" ? (
      <X size={12} />
    ) : state === "uploading" ? (
      <Loader size={12} />
    ) : (
      <Clock size={12} />
    );
  return (
    <section
      id="shared-composer"
      className="composer"
      aria-label="Message composer"
      data-typing-send={typingSendState}
    >
      <div id="composer-field">
        <div id="composer-banner-row" hidden={!bannerSlot}>
          {bannerSlot}
        </div>
        {attachments.length > 0 && (
        <ul id="attachment-tray" aria-label="Attachments">
          {attachments.map((a) => (
            <li
              key={a.id}
              data-attachment-id={a.id}
              className={`attachment-chip ${a.state}`}
            >
              {a.previewUrl && (
                <img
                  src={a.previewUrl}
                  alt={a.name}
                  onError={(e) => {
                    e.currentTarget.hidden = true;
                  }}
                />
              )}
              <span>{a.name}</span>
              <span className="attachment-state" aria-label={a.state}>
                {icon(a.state)}
              </span>
               {a.error && <span role="alert">{a.error}</span>}
               {onRemoveAttachment && (
                 <button
                   type="button"
                   className="attachment-remove"
                   aria-label={`Remove ${a.name}`}
                   title={`Remove ${a.name}`}
                   onClick={() => onRemoveAttachment(a.id)}
                 >
                   <X size={12} aria-hidden />
                 </button>
               )}
            </li>
          ))}
        </ul>
      )}
        <div id="composer-input-row">
        <label htmlFor="composer-textarea" className="sr-only">
          Message
        </label>
        <textarea
          ref={textareaRef}
          id="composer-textarea"
          aria-label="Message"
          value={draft}
          readOnly={readOnly}
          onChange={(e) => {
            if (!readOnly) onDraftChange(e.target.value);
          }}
          onKeyDown={keydown}
          placeholder={
            placeholder ??
            (composerName ? `Message ${composerName}` : "Type a message")
          }
          rows={1}
        />
      </div>
        <div
          id="composer-status-row"
          hidden={!(statusActive || status || fallbackShown)}
        >
          {statusSlot}
          {status && (
            <p className="composer-status" role="status">
              {status}
            </p>
          )}
           {fallbackShown && <span id="unavailable-hint">Sending unavailable: {unavailableReason ?? "phone is offline"}</span>}
        </div>
        <div id="composer-toolbar">
        <button
          type="button"
          aria-label="Add attachment"
          title="Add attachment"
          onClick={onAddAttachment}
          disabled={!onAddAttachment}
        >
          <Paperclip size={16} aria-hidden />
        </button>
        {gatewaySlot}
        <div id="composer-typing-slot" className="composer-toolbar-spacer">{typingSlot}</div>
        {!hideSmsCounter && counter && (
          <span id="sms-counter" aria-live="polite" aria-label="SMS character count">
            {counter}
          </span>
        )}
        <span id="shift-enter-hint" className="composer-hint" aria-hidden>
          {platform === "macos" ? "⇧↵ new line" : "Shift+Enter new line"}
        </span>
        <button
          className="send-button"
          aria-label="Send"
          aria-describedby={!sendSupported ? "unavailable-hint" : undefined}
          title={unavailableReason}
          onClick={onSend}
          disabled={!canSend}
        >
          <SendHorizontal size={18} aria-hidden />
        </button>
        </div>
      </div>
    </section>
  );
}

export function NavButtons({
  orientation,
  activeView,
  onView,
  onToggleList,
  listCollapsed,
  threadListId,
  notificationUnread = 0,
  contactsPending = 0,
}: {
  orientation: "rail" | "titlebar";
  activeView: NavigationView;
  onView(view: NavigationView): void;
  onToggleList?(): void;
  listCollapsed?: boolean;
  threadListId?: string;
  notificationUnread?: number;
  contactsPending?: number;
}): ReactElement {
  const iconSize = orientation === "rail" ? 20 : 16;
  return <nav id={orientation === "titlebar" ? "titlebar-navigation" : undefined} aria-label="Main navigation" data-orientation={orientation}>
    <button
      data-rail-item="conversations"
      aria-label="Conversations"
      title="Conversations"
      aria-current={activeView === "conversations" ? "page" : undefined}
      aria-expanded={activeView === "conversations" ? !listCollapsed : undefined}
      aria-controls={activeView === "conversations" ? threadListId : undefined}
      onClick={() => activeView === "conversations" ? onToggleList?.() : onView("conversations")}
    >
      <MessageCircle size={iconSize} strokeWidth={1.75} aria-hidden />
    </button>
    <button data-rail-item="contacts" aria-label={contactsPending ? `Contacts, ${contactsPending} pending` : "Contacts"} title="Contacts" aria-current={activeView === "contacts" ? "page" : undefined} onClick={() => onView("contacts")}>
      <Users size={iconSize} strokeWidth={1.75} aria-hidden />
      {contactsPending > 0 && <span className="rail-notif-badge" aria-hidden>{contactsPending > 99 ? "99+" : contactsPending}</span>}
    </button>
    <button
      data-rail-item="notifications"
      aria-label={notificationUnread ? `Notifications, ${notificationUnread} unread` : "Notifications"}
      title="Notifications"
      aria-current={activeView === "notifications" ? "page" : undefined}
      onClick={() => onView("notifications")}
    >
      <Bell size={iconSize} strokeWidth={1.75} aria-hidden />
      {notificationUnread > 0 && <span className="rail-notif-badge" aria-hidden>{notificationUnread > 99 ? "99+" : notificationUnread}</span>}
    </button>
    <button
      data-rail-item="settings"
      aria-label="Settings"
      title="Settings"
      aria-current={activeView === "settings" ? "page" : undefined}
      onClick={() => onView("settings")}
    >
      <Settings size={iconSize} strokeWidth={1.75} aria-hidden />
    </button>
  </nav>;
}

export function Panel({
  children,
  activeView,
  onView,
  status,
  onToggleList,
  listCollapsed,
  threadListId,
  notificationUnread = 0,
  contactsPending = 0,
}: {
  children?: ReactNode;
  activeView: NavigationView;
  onView(view: NavigationView): void;
  status?: PanelStatus;
  onToggleList?(): void;
  listCollapsed?: boolean;
  threadListId?: string;
  notificationUnread?: number;
  contactsPending?: number;
}) {
  return (
    <aside id="desktop-rail" aria-label="Navigation rail">
      <NavButtons
        orientation="rail"
        activeView={activeView}
        onView={onView}
        onToggleList={onToggleList}
        listCollapsed={listCollapsed}
        threadListId={threadListId}
        notificationUnread={notificationUnread}
        contactsPending={contactsPending}
      />
      <div className="rail-spacer" />
      {status && <StatusPopover tone={status.tone} summary={status.summary}>{status.details}</StatusPopover>}
      {children}
    </aside>
  );
}
