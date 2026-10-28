import { useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEventHandler, type Ref } from "react";
import { Download, ImageDown, ImageOff, Link, Loader2, RotateCw, WifiOff } from "@peppy/desktop-ui";
import type { GalleryAttachment, GalleryMessage } from "./grouping";
import { canOpen, demandTarget, holdNoteText, originalPending, originalView, previewUrl, tileLabel, tileState } from "./mediaState";
import { useMediaActions } from "./MediaActionsContext";
import { TileContextMenu } from "./TileContextMenu";
import { useLazyDemand, usePreviewHydration } from "./useLazyDemand";
import { withHydratedPreview } from "./previewHydration";
import "./media-tile.css";

export type MediaTileProps = {
  attachment: GalleryAttachment;
  message: GalleryMessage;
  variant: "slide" | "single" | "cell";
  onOpen(): void;
  label?: string;
  prefetchIds?: readonly (string | undefined)[];
  bodyRef?: Ref<HTMLButtonElement>;
  onBodyKeyDown?: KeyboardEventHandler<HTMLButtonElement>;
  bodyAttributes?: Record<`data-${string}`, string | boolean | undefined>;
  suppressHeldControl?: boolean;
};

export type SyncState = "idle" | "requesting" | "queued" | "error";

/** Local-only cooldown after an accepted request; the button offers a retry when it elapses. Core dedupes requests for the same window. */
export const MEDIA_SYNC_RETRY_MS = 300000;

export type MediaSyncButtonProps = { attachmentIds: string[]; label: string; ariaLabel?: string; retryAriaLabel?: string; describedBy: string; initialState?: "queued"; resetKey?: string; compact?: boolean; className?: string; dataAttributes?: Record<`data-${string}`, string>; onFocus?(): void; onBlur?(): void; onStateChange?(state: SyncState): void };

/**
 * Returns a counter that increases only when an attachment's durable hold goes from requested back to held.
 * Renders are not counted; the first observed phases are the baseline.
 */
export function useReturnedToHoldGeneration(phases: ReadonlyArray<{ id: string; phase: string | undefined }>): number {
  const [generation, setGeneration] = useState(0);
  const requested = useRef<ReadonlySet<string>>(new Set());
  const phaseKey = phases.map(({ id, phase }) => `${id}\u0000${phase ?? ""}`).join("\u0001");
  useEffect(() => {
    const returned = phases.some(({ id, phase }) => phase === "held" && requested.current.has(id));
    requested.current = new Set(phases.filter(({ phase }) => phase === "requested").map(({ id }) => id));
    if (returned) setGeneration((value) => value + 1);
  }, [phaseKey]);
  return generation;
}

export function MediaSyncButton({ attachmentIds, label, ariaLabel, retryAriaLabel, describedBy, initialState, resetKey = "", compact = false, className = "tile-sync", dataAttributes, onFocus, onBlur, onStateChange }: MediaSyncButtonProps) {
  const actions = useMediaActions();
  // Local state is tagged with the reset key it was set under, so a changed key shows idle without an effect.
  const [sync, setSync] = useState<{ key: string; state: SyncState }>({ key: resetKey, state: "idle" });
  // Each accepted request starts a new queued episode; the cooldown timer is keyed to it.
  const [episode, setEpisode] = useState(0);
  const [expiredEpisode, setExpiredEpisode] = useState<string>();
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const local = sync.key === resetKey ? sync.state : "idle";
  // A real request in progress or a failure overrides the durable queued state from the snapshot.
  const displayState: SyncState = local === "requesting" || local === "error" ? local : initialState ?? local;
  const queued = displayState === "queued";
  const episodeKey = `${resetKey}#${episode}`;
  const retryable = queued && expiredEpisode === episodeKey;
  useEffect(() => {
    if (!queued) return;
    const timer = setTimeout(() => setExpiredEpisode(episodeKey), MEDIA_SYNC_RETRY_MS);
    return () => clearTimeout(timer);
  }, [queued, episodeKey]);
  if (!actions?.canRequestMediaSync || !actions.requestMediaSync) return null;
  const requestMediaSync = actions.requestMediaSync;
  const update = (state: SyncState) => {
    setSync({ key: resetKey, state });
    onStateChange?.(state);
  };
  const requestSync = () => {
    if (displayState === "requesting" || (queued && !retryable)) return;
    update("requesting");
    void requestMediaSync(attachmentIds).then(() => {
      if (!mounted.current) return;
      setEpisode((value) => value + 1);
      update("queued");
    }, () => {
      if (!mounted.current) return;
      update("error");
    });
  };
  const requesting = displayState === "requesting";
  const retry = displayState === "error";
  const text = requesting ? (compact ? "Sending…" : "Requesting…") : retryable ? (compact ? "Retry" : "Request again") : queued ? (compact ? "Queued" : "Request queued") : retry ? "Try again" : label;
  const icon = requesting ? <Loader2 className="tile-action-spinner" size={14} aria-hidden="true" /> : retry || retryable ? <RotateCw size={14} aria-hidden="true" /> : <ImageDown size={14} aria-hidden="true" />;
  return <button type="button" className={className} aria-label={retryable && retryAriaLabel ? retryAriaLabel : ariaLabel} aria-describedby={describedBy} aria-disabled={requesting || (queued && !retryable) || undefined} title="Uses mobile data on your phone" data-sync-state={displayState} data-retryable={retryable || undefined} onFocus={onFocus} onBlur={onBlur} onClick={requestSync} {...dataAttributes}>
    {icon}
    {text}
  </button>;
}

const FULLY_VISIBLE_RATIO = 0.98;

/** The keys that open a tile's action menu: the ContextMenu key and Shift+F10. */
function isMenuKey(event: { key: string; shiftKey: boolean }): boolean {
  return event.key === "ContextMenu" || (event.key === "F10" && event.shiftKey);
}

export function MediaTile({ attachment, message, variant, onOpen, label, prefetchIds = [], bodyRef, onBodyKeyDown, bodyAttributes, suppressHeldControl }: MediaTileProps) {
  const rootRef = useRef<HTMLDivElement>(null);
  const bodyElementRef = useRef<HTMLButtonElement>(null);
  const heldBodyRef = useRef<HTMLDivElement>(null);
  const actions = useMediaActions();
  const hydration = usePreviewHydration(rootRef, message.id, attachment);
  const shown = withHydratedPreview(attachment, message.id, hydration.preview);
  const state = tileState(shown, hydration.exhausted);
  const original = originalView(attachment);
  const pendingOriginal = originalPending(attachment);
  const imageUrl = previewUrl(shown);
  const [menuAnchor, setMenuAnchor] = useState<{ left: number; top: number }>();
  const [heldSyncState, setHeldSyncState] = useState<SyncState>("idle");
  const [slideFullyVisible, setSlideFullyVisible] = useState(true);
  const previousState = useRef(state);
  const previousPendingOriginal = useRef(pendingOriginal);
  const heldControlFocused = useRef(false);
  const canAct = canOpen(shown, hydration.exhausted);
  // A held original with a visible preview can still be copied natively; the menu then offers Copy only.
  const nativeHeldCopy = pendingOriginal && Boolean(actions?.nativeCopy) && previewUrl(shown) !== undefined && attachment.mediaType.startsWith("image/");
  const hasActions = Boolean(actions && canAct);
  const accessibleLabel = state === "ready" && label ? label : tileLabel(attachment, state);
  const isSaving = actions?.isSaving(attachment.id) ?? false;
  useLazyDemand(rootRef, [demandTarget(attachment), ...prefetchIds]);

  const mergedBodyRef = useCallback((element: HTMLButtonElement | null) => {
    bodyElementRef.current = element;
    if (typeof bodyRef === "function") bodyRef(element);
    else if (bodyRef) bodyRef.current = element;
  }, [bodyRef]);

  const openMenu = (left: number, top: number) => setMenuAnchor({ left, top });
  const handleKeyDown: KeyboardEventHandler<HTMLButtonElement> = (event) => {
    onBodyKeyDown?.(event);
    if (event.defaultPrevented || !actions || !canAct) return;
    if (event.key === "ContextMenu" || (event.key === "F10" && event.shiftKey)) {
      event.preventDefault();
      const bounds = event.currentTarget.getBoundingClientRect();
      openMenu(bounds.left + 8, bounds.top + 8);
    }
  };
  const bodyProps = { ...bodyAttributes, "data-attachment-id": attachment.id, "data-message-id": message.id };
  const noteId = `media-hold-note-${attachment.id}`;
  useLayoutEffect(() => {
    if (previousPendingOriginal.current && !pendingOriginal && heldControlFocused.current) (bodyElementRef.current ?? rootRef.current)?.focus();
    previousState.current = state;
    previousPendingOriginal.current = pendingOriginal;
    heldControlFocused.current = false;
  }, [pendingOriginal, state]);

  const resetGeneration = useReturnedToHoldGeneration([{ id: attachment.id, phase: original.phase }]);
  useEffect(() => { if (resetGeneration) setHeldSyncState("idle"); }, [resetGeneration]);
  // A partly visible slide clips its control and note, so they stay hidden until the slide is fully visible.
  const observeSlideVisibility = variant === "slide" && pendingOriginal && !suppressHeldControl;
  useEffect(() => {
    const tile = rootRef.current;
    const track = tile?.closest<HTMLElement>(".carousel-track");
    if (!observeSlideVisibility || !tile || !track) return;
    const measureVisibility = () => {
      const bounds = tile.getBoundingClientRect();
      const viewport = track.getBoundingClientRect();
      if (bounds.width === 0 || viewport.width === 0) return;
      const visibleWidth = Math.max(0, Math.min(bounds.right, viewport.right) - Math.max(bounds.left, viewport.left));
      setSlideFullyVisible(visibleWidth >= bounds.width * FULLY_VISIBLE_RATIO);
    };
    measureVisibility();
    track.addEventListener("scroll", measureVisibility, { passive: true });
    window.addEventListener("resize", measureVisibility);
    const resize = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(measureVisibility);
    resize?.observe(track);
    const observer = typeof IntersectionObserver === "undefined" ? undefined : new IntersectionObserver((entries) => {
      const entry = entries.at(-1);
      if (tile.getBoundingClientRect().width > 0) measureVisibility();
      else if (entry) setSlideFullyVisible(entry.intersectionRatio >= FULLY_VISIBLE_RATIO);
    }, { root: track, threshold: [FULLY_VISIBLE_RATIO] });
    observer?.observe(tile);
    return () => {
      observer?.disconnect();
      resize?.disconnect();
      track.removeEventListener("scroll", measureVisibility);
      window.removeEventListener("resize", measureVisibility);
    };
  }, [observeSlideVisibility]);
  const effectiveSyncState = original.phase === "requested" ? "queued" : heldSyncState;
  const heldNote = holdNoteText(effectiveSyncState, Boolean(actions?.canRequestMediaSync), attachment.error);
  const syncAccessibleLabel = original.phase === "requested" ? `Requested ${label?.toLowerCase() || attachment.name || "image"}` : `Sync ${label?.toLowerCase() || attachment.name || "image"} using mobile data if needed`;
  const syncLabel = attachment.mediaType.startsWith("image/") ? "Sync image" : attachment.mediaType.startsWith("video/") ? "Sync video" : "Sync file";
  return <div ref={rootRef} tabIndex={-1} data-media-tile data-variant={variant} data-state={state} data-original={pendingOriginal ? original.phase : undefined} data-partly-visible={observeSlideVisibility && !slideFullyVisible ? true : undefined} data-sync={pendingOriginal ? effectiveSyncState : undefined} data-has-actions={hasActions || undefined} data-menu-open={menuAnchor ? true : undefined} aria-busy={state === "loading" || heldSyncState === "requesting" || undefined}>
    {pendingOriginal ? <>
      <div ref={heldBodyRef} className="tile-body tile-held" role="img" aria-label={accessibleLabel} {...bodyProps} tabIndex={nativeHeldCopy ? 0 : undefined} aria-haspopup={nativeHeldCopy ? "menu" : undefined} onContextMenu={(event) => { if (!nativeHeldCopy) return; event.preventDefault(); openMenu(event.clientX, event.clientY); }} onKeyDown={(event) => { if (!nativeHeldCopy || !isMenuKey(event)) return; event.preventDefault(); const bounds = event.currentTarget.getBoundingClientRect(); openMenu(bounds.left + 8, bounds.top + 8); }}>
        {imageUrl ? <img className="tile-held-preview" loading="lazy" decoding="async" src={imageUrl} alt="" aria-hidden="true" /> : <WifiOff className="tile-held-icon" size={20} aria-hidden="true" />}
      </div>
      {!suppressHeldControl && <MediaSyncButton attachmentIds={[attachment.id]} label={variant === "cell" ? "Sync" : syncLabel} compact={variant === "cell"} resetKey={`${attachment.id}#${resetGeneration}`} ariaLabel={syncAccessibleLabel} retryAriaLabel={`${variant === "cell" ? "Retry" : "Request again"} ${label?.toLowerCase() || attachment.name || "image"} using mobile data if needed`} describedBy={noteId} initialState={original.phase === "requested" ? "queued" : undefined} dataAttributes={{ "data-sync-button": "true", "data-original-sync-button": "true", "data-attachment-id": attachment.id }} onFocus={() => { heldControlFocused.current = true; }} onBlur={() => { heldControlFocused.current = false; }} onStateChange={setHeldSyncState} />}
      {!suppressHeldControl && <span id={noteId} data-original-sync-note className="tile-held-note" aria-live="off">{heldNote}</span>}
    </> : state === "failed" ? <><div className="tile-body tile-failed" role="img" aria-label={accessibleLabel} {...bodyProps}><ImageOff size={20} aria-hidden="true" /></div>{actions && <button type="button" className="tile-retry" onClick={() => actions.retry(attachment)}><RotateCw size={14} aria-hidden="true" />Retry</button>}</> : <button ref={mergedBodyRef} type="button" className="tile-body" aria-label={accessibleLabel} onClick={onOpen} onKeyDown={handleKeyDown} onContextMenu={(event) => { if (!actions || !canAct) return; event.preventDefault(); openMenu(event.clientX, event.clientY); }} {...bodyProps}>
      {imageUrl && <img loading="lazy" decoding="async" src={imageUrl} alt="" aria-hidden="true" />}
      {state === "loading" && <Loader2 className="tile-loading-icon" size={20} aria-hidden="true" />}
      {state === "unavailable" && <span className="tile-unavailable"><ImageOff size={20} aria-hidden="true" />Preview unavailable</span>}
    </button>}
    {actions && canAct && <div data-tile-actions><button type="button" aria-label="Save image" title="Save image" aria-busy={isSaving || undefined} aria-disabled={isSaving || undefined} onClick={() => { if (!isSaving) actions.save(attachment); }}>{isSaving ? <Loader2 className="tile-action-spinner" size={16} aria-hidden="true" /> : <Download size={16} aria-hidden="true" />}</button><button type="button" aria-label={actions.publicCopy(attachment.id) ? "Copy public link" : "Create public link"} title={actions.publicCopy(attachment.id) ? "Copy public link" : "Create public link"} onClick={() => actions.shareLink(attachment)}><Link size={16} aria-hidden="true" /></button></div>}
    {menuAnchor && actions && <TileContextMenu attachment={attachment} actions={actions} anchor={menuAnchor} onRetryPreview={hydration.exhausted ? hydration.retry : undefined} onClose={() => setMenuAnchor(undefined)} returnFocus={() => (bodyElementRef.current ?? heldBodyRef.current)?.focus()} />}
  </div>;
}
