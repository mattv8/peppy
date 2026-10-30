import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { Copy, Download, Link, RotateCw } from "@peppy/desktop-ui";
import type { GalleryAttachment } from "./grouping";
import type { MediaActions } from "./MediaActionsContext";
import { originalPending } from "./mediaState";

type MenuPosition = { left: number; top: number };

export type TileContextMenuProps = {
  attachment: GalleryAttachment;
  actions: MediaActions;
  anchor: MenuPosition;
  /** Set only when native hydration gave up on this tile; re-arms it explicitly. */
  onRetryPreview?(): void;
  returnFocus(): void;
  onClose(): void;
};

function clampPosition(position: MenuPosition, menu: HTMLDivElement): MenuPosition {
  const margin = 8;
  return {
    left: Math.max(margin, Math.min(position.left, window.innerWidth - menu.offsetWidth - margin)),
    top: Math.max(margin, Math.min(position.top, window.innerHeight - menu.offsetHeight - margin)),
  };
}

export function TileContextMenu({ attachment, actions, anchor, onRetryPreview, returnFocus, onClose }: TileContextMenuProps) {
  const menuRef = useRef<HTMLDivElement>(null);
  const itemRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const [position, setPosition] = useState(anchor);
  // A held original offers only Copy: Save and public links need the original on this phone-less device.
  const originalHeld = originalPending(attachment);
  // Native Copy demands the original itself, so a held original stays enabled there; the browser path cannot.
  const copyBlocked = originalHeld && !actions.nativeCopy;
  const copyItem = actions.canCopyImage ? { label: "Copy image", icon: Copy, disabled: copyBlocked, title: copyBlocked ? "Still downloading from your phone" : undefined, action: () => actions.copyImage(attachment) } : undefined;
  const otherItems = originalHeld ? [] : [
    onRetryPreview ? { label: "Retry preview", icon: RotateCw, disabled: false, title: undefined, action: onRetryPreview } : undefined,
    attachment.state === "failed" ? { label: "Retry download", icon: RotateCw, disabled: false, title: undefined, action: () => actions.retry(attachment) } : undefined,
    { label: "Save image", icon: Download, disabled: false, title: undefined, action: () => actions.save(attachment) },
    actions.canShareLink ? { label: actions.publicCopy(attachment.id) ? "Copy public link" : "Create public link", icon: Link, disabled: false, title: undefined, action: () => actions.shareLink(attachment) } : undefined,
  ];
  const items = [copyItem, ...otherItems].filter((item): item is NonNullable<typeof item> => Boolean(item));

  const close = (restoreFocus: boolean) => {
    onClose();
    if (restoreFocus) requestAnimationFrame(returnFocus);
  };
  const focusItem = (index: number) => itemRefs.current[index]?.focus();

  useLayoutEffect(() => {
    if (menuRef.current) setPosition(clampPosition(anchor, menuRef.current));
    const firstEnabled = items.findIndex((item) => !item.disabled);
    if (firstEnabled >= 0) focusItem(firstEnabled);
  }, []);

  useEffect(() => {
    const closeForExternalEvent = () => close(false);
    const closeForPointer = (event: PointerEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) close(false);
    };
    document.addEventListener("pointerdown", closeForPointer);
    window.addEventListener("scroll", closeForExternalEvent, true);
    window.addEventListener("resize", closeForExternalEvent);
    window.addEventListener("blur", closeForExternalEvent);
    return () => {
      document.removeEventListener("pointerdown", closeForPointer);
      window.removeEventListener("scroll", closeForExternalEvent, true);
      window.removeEventListener("resize", closeForExternalEvent);
      window.removeEventListener("blur", closeForExternalEvent);
    };
  }, []);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const currentIndex = itemRefs.current.findIndex((item) => item === document.activeElement);
    if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close(true); return; }
    if (event.key === "Tab") { event.stopPropagation(); close(false); return; }
    if (!items.length) return;
    if (event.key === "Home") { event.preventDefault(); event.stopPropagation(); focusItem(0); return; }
    if (event.key === "End") { event.preventDefault(); event.stopPropagation(); focusItem(items.length - 1); return; }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      event.stopPropagation();
      const activeIndex = Math.max(0, currentIndex);
      const offset = event.key === "ArrowDown" ? 1 : -1;
      focusItem((activeIndex + offset + items.length) % items.length);
    }
    if (event.key === "Enter" || event.key === " ") event.stopPropagation();
  };

  return createPortal(<div ref={menuRef} data-tile-context-menu role="menu" aria-label="Image actions" style={position} onClick={(event) => event.stopPropagation()} onKeyDown={onKeyDown}>
    {items.map((item, index) => {
      const Icon = item.icon;
      return <button key={item.label} ref={(element) => { itemRefs.current[index] = element; }} type="button" role="menuitem" tabIndex={-1} aria-disabled={item.disabled || undefined} title={item.title} onPointerMove={(event) => { event.stopPropagation(); event.currentTarget.focus(); }} onClick={(event) => { event.stopPropagation(); if (item.disabled) return; item.action(); close(true); }}><Icon size={16} aria-hidden="true" />{item.label}</button>;
    })}
  </div>, document.body);
}
