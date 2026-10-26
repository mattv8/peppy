import { useCallback, useRef, useState, type KeyboardEventHandler, type Ref } from "react";
import { Download, ImageOff, Link, Loader2, RotateCw } from "@peppy/desktop-ui";
import type { GalleryAttachment, GalleryMessage } from "./grouping";
import { demandTarget, previewUrl, tileLabel, tileState } from "./mediaState";
import { useMediaActions } from "./MediaActionsContext";
import { TileContextMenu } from "./TileContextMenu";
import { useLazyDemand } from "./useLazyDemand";
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
};

export function MediaTile({ attachment, message, variant, onOpen, label, prefetchIds = [], bodyRef, onBodyKeyDown, bodyAttributes }: MediaTileProps) {
  const rootRef = useRef<HTMLDivElement>(null);
  const bodyElementRef = useRef<HTMLButtonElement>(null);
  const actions = useMediaActions();
  const state = tileState(attachment);
  const imageUrl = previewUrl(attachment);
  const [menuAnchor, setMenuAnchor] = useState<{ left: number; top: number }>();
  const canAct = state === "ready" || state === "unavailable";
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

  return <div ref={rootRef} data-media-tile data-variant={variant} data-state={state} data-has-actions={hasActions || undefined} data-menu-open={menuAnchor ? true : undefined} aria-busy={state === "loading" || undefined}>
    {state === "failed" ? <><div className="tile-body tile-failed" role="img" aria-label={accessibleLabel} {...bodyProps}><ImageOff size={20} aria-hidden="true" /></div>{actions && <button type="button" className="tile-retry" onClick={() => actions.retry(attachment)}><RotateCw size={14} aria-hidden="true" />Retry</button>}</> : <button ref={mergedBodyRef} type="button" className="tile-body" aria-label={accessibleLabel} onClick={onOpen} onKeyDown={handleKeyDown} onContextMenu={(event) => { if (!actions || !canAct) return; event.preventDefault(); openMenu(event.clientX, event.clientY); }} {...bodyProps}>
      {imageUrl && <img loading="lazy" decoding="async" src={imageUrl} alt="" aria-hidden="true" />}
      {state === "loading" && <Loader2 className="tile-loading-icon" size={20} aria-hidden="true" />}
      {state === "unavailable" && <span className="tile-unavailable"><ImageOff size={20} aria-hidden="true" />Preview unavailable</span>}
    </button>}
    {actions && canAct && <div data-tile-actions><button type="button" aria-label="Save image" title="Save image" aria-busy={isSaving || undefined} aria-disabled={isSaving || undefined} onClick={() => { if (!isSaving) actions.save(attachment); }}>{isSaving ? <Loader2 className="tile-action-spinner" size={16} aria-hidden="true" /> : <Download size={16} aria-hidden="true" />}</button><button type="button" aria-label={actions.publicCopy(attachment.id) ? "Copy public link" : "Create public link"} title={actions.publicCopy(attachment.id) ? "Copy public link" : "Create public link"} onClick={() => actions.shareLink(attachment)}><Link size={16} aria-hidden="true" /></button></div>}
    {menuAnchor && actions && <TileContextMenu attachment={attachment} actions={actions} anchor={menuAnchor} onClose={() => setMenuAnchor(undefined)} returnFocus={() => bodyElementRef.current?.focus()} />}
  </div>;
}
