import { createPortal } from "react-dom";
import type { CSSProperties, KeyboardEvent, PointerEvent, ReactNode } from "react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Info, TriangleAlert, XCircle } from "lucide-react";

export type InlineStatusTone = "info" | "warning" | "error";
export type InlineStatusAction = { id: string; label: string; onAction(): void; disabled?: boolean; ariaLabel?: string; confirm?: { title: string; body: string; confirmLabel: string; cancelLabel?: string } };
export type InlineStatusItem = { key: string; tone: InlineStatusTone; message: string; detail?: string; detailItems?: string[]; role?: "status" | "alert"; id?: string; hooks?: Record<`data-${string}`, string | undefined>; actions?: InlineStatusAction[]; srOnly?: boolean };
export type InlineStatusLabels = { more?(count: number): string; dialog?: string };

const HOVER_OPEN_DELAY = 350;
const HOVER_CLOSE_DELAY = 150;
const VIEWPORT_MARGIN = 8;
const POPOVER_GAP = 6;

export function formatComposerReason(reason: string): string {
  const trimmed = reason.trim();
  if (!trimmed) return trimmed;
  const sentence = `${trimmed[0].toUpperCase()}${trimmed.slice(1)}`;
  return /[.!?…]$/.test(sentence) ? sentence : `${sentence}.`;
}

function focusIfConnected(element: HTMLElement | null | undefined): boolean {
  if (!element?.isConnected) return false;
  element.focus();
  return true;
}

function ToneIcon({ tone }: { tone: InlineStatusTone }) {
  const Icon = tone === "error" ? XCircle : tone === "warning" ? TriangleAlert : Info;
  return <Icon data-inline-status-icon size={12} strokeWidth={2} aria-hidden />;
}

function popoverNeeded(items: InlineStatusItem[], truncated: boolean): boolean {
  const primary = items[0];
  return items.length > 1 || truncated || Boolean(primary?.detail || primary?.detailItems?.length || primary?.actions?.length);
}

function toolbarNextFocusable(trigger: HTMLButtonElement | null): HTMLElement | null {
  const toolbar = trigger?.closest<HTMLElement>("#composer-toolbar");
  if (!toolbar || !trigger) return null;
  const controls = Array.from(toolbar.querySelectorAll<HTMLElement>("button:not([disabled]):not([aria-disabled='true']), [href], input, select, textarea"));
  return controls[controls.indexOf(trigger) + 1] ?? null;
}

export function InlineStatus({ items, idPrefix = "inline-status", labels }: { items: InlineStatusItem[]; idPrefix?: string; labels?: InlineStatusLabels }): ReactNode {
  const visibleItems = items.filter((item) => !item.srOnly);
  const primary = visibleItems[0];
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const hoverTimer = useRef<number | undefined>(undefined);
  const animationFrame = useRef<number | undefined>(undefined);
  const openerRef = useRef<HTMLElement | null>(null);
  const pinnedRef = useRef(false);
  const [truncated, setTruncated] = useState(false);
  const [open, setOpen] = useState(false);
  const [pinned, setPinned] = useState(false);
  const [confirming, setConfirming] = useState<{ itemKey: string; actionId: string } | null>(null);
  const [position, setPosition] = useState<CSSProperties>();
  const moreId = `${idPrefix}-more`;
  const popoverId = `${idPrefix}-popover`;
  const needsPopover = Boolean(primary) && popoverNeeded(visibleItems, truncated);
  const host = typeof document === "undefined" ? null : triggerRef.current?.closest<HTMLElement>(".theme-light, .theme-dark, .theme-system") ?? document.body;
  const moreLabel = labels?.more ?? ((count: number) => `and ${count} more`);
  const dialogLabel = labels?.dialog ?? "Composer status";
  const confirmationAction = confirming ? items.find((item) => item.key === confirming.itemKey)?.actions?.find((action) => action.id === confirming.actionId) : undefined;

  const focusTrigger = () => focusIfConnected(triggerRef.current) || focusIfConnected(document.getElementById("composer-textarea"));
  const close = (returnFocus = false) => {
    window.clearTimeout(hoverTimer.current);
    pinnedRef.current = false;
    setOpen(false);
    setPinned(false);
    if (returnFocus) focusTrigger();
  };
  const updatePosition = () => {
    const toolbarRect = triggerRef.current?.closest<HTMLElement>("#composer-toolbar")?.getBoundingClientRect();
    const trigger = triggerRef.current?.getBoundingClientRect();
    if (!trigger || !toolbarRect) return;
    const width = Math.min(360, window.innerWidth - VIEWPORT_MARGIN * 2);
    const next = { bottom: `${window.innerHeight - toolbarRect.top + POPOVER_GAP}px`, left: `${Math.min(Math.max(VIEWPORT_MARGIN, trigger.left), window.innerWidth - width - VIEWPORT_MARGIN)}px`, width: `${width}px`, maxHeight: `${Math.max(0, Math.min(320, toolbarRect.top - 16))}px` };
    setPosition((current) => current?.bottom === next.bottom && current.left === next.left && current.width === next.width && current.maxHeight === next.maxHeight ? current : next);
  };
  const scheduleClose = () => {
    if (!pinnedRef.current) hoverTimer.current = window.setTimeout(() => close(), HOVER_CLOSE_DELAY);
  };
  const scheduleOpen = (event: PointerEvent<HTMLElement>) => {
    if (event.pointerType === "touch" || pinnedRef.current) return;
    window.clearTimeout(hoverTimer.current);
    hoverTimer.current = window.setTimeout(() => setOpen(true), HOVER_OPEN_DELAY);
  };
  const focusPopover = () => {
    const firstAction = popoverRef.current?.querySelector<HTMLElement>("button:not([aria-disabled='true'])");
    if (!focusIfConnected(firstAction)) focusIfConnected(popoverRef.current);
  };
  const completeAction = () => close(true);
  const openConfirmation = (itemKey: string, action: InlineStatusAction, opener: HTMLButtonElement) => {
    openerRef.current = opener;
    close();
    setConfirming({ itemKey, actionId: action.id });
  };
  const actionButton = (itemKey: string, action: InlineStatusAction, id: string) => <button key={id} id={id} type="button" data-inline-status-action={action.id} aria-label={action.ariaLabel} aria-disabled={action.disabled || undefined} onClick={(event) => {
    if (action.disabled) return;
    if (action.confirm) openConfirmation(itemKey, action, event.currentTarget);
    else {
      action.onAction();
      completeAction();
    }
  }}>{action.label}</button>;

  useEffect(() => () => {
    window.clearTimeout(hoverTimer.current);
    if (animationFrame.current !== undefined) cancelAnimationFrame(animationFrame.current);
  }, []);
  useEffect(() => {
    if (!open || !pinned) return;
    const outside = (event: globalThis.PointerEvent) => {
      if (!triggerRef.current?.contains(event.target as Node) && !popoverRef.current?.contains(event.target as Node)) close();
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [open, pinned]);
  useLayoutEffect(() => {
    if (!open && !confirmationAction) return;
    updatePosition();
    window.addEventListener("resize", updatePosition);
    return () => window.removeEventListener("resize", updatePosition);
  }, [open, confirmationAction?.id, primary?.key, visibleItems.length]);
  useEffect(() => {
    const trigger = triggerRef.current;
    if (!trigger || !primary || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => setTruncated(trigger.scrollWidth > trigger.clientWidth));
    observer.observe(trigger);
    setTruncated(trigger.scrollWidth > trigger.clientWidth);
    return () => observer.disconnect();
  }, [primary?.key, primary?.message]);
  useEffect(() => {
    if (open && !needsPopover) close();
    if (confirming && !confirmationAction) setConfirming(null);
  }, [open, needsPopover, confirming, confirmationAction]);

  if (!primary) return items.length ? <div data-inline-status data-inline-status-count="0">{items.filter((item) => item.id || item.srOnly).map((item) => <span key={item.key} id={item.id} className="sr-only">{item.message}</span>)}</div> : null;
  const moreCount = visibleItems.length - 1;
  const liveProps = { id: primary.id, role: primary.role ?? "status", ...primary.hooks };
  const toggle = (keyboard: boolean) => {
    if (pinnedRef.current) close();
    else {
      pinnedRef.current = true;
      setPinned(true);
      setOpen(true);
      if (keyboard) animationFrame.current = requestAnimationFrame(focusPopover);
    }
  };
  const handlePopoverKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close(true);
      return;
    }
    const controls = Array.from(popoverRef.current?.querySelectorAll<HTMLElement>("button:not([aria-disabled='true'])") ?? []);
    const index = controls.indexOf(event.target as HTMLElement);
    if (event.key === "Tab" && index === 0 && event.shiftKey) {
      event.preventDefault();
      close(true);
    } else if (event.key === "Tab" && index === controls.length - 1 && !event.shiftKey) {
      event.preventDefault();
      close();
      if (!focusIfConnected(toolbarNextFocusable(triggerRef.current))) focusTrigger();
    }
  };
  const sections = visibleItems.map((item) => <section key={item.key} data-inline-status-key={item.key}>
    <div data-inline-status-popover-message data-inline-status-tone={item.tone}><ToneIcon tone={item.tone} /><strong>{item.message}</strong></div>
    {item.detail ? <p>{item.detail}</p> : null}
    {item.detailItems?.length ? <ul>{item.detailItems.map((detail, index) => <li key={`${index}-${detail}`}>{detail}</li>)}</ul> : null}
    {item.actions?.length ? <div data-inline-status-popover-actions>{item.actions.map((action) => actionButton(item.key, action, `${action.id}-popover`))}</div> : null}
  </section>);

  return <>
    <div data-inline-status data-inline-status-count={visibleItems.length}>
      <div data-inline-status-item data-inline-status-key={primary.key} data-inline-status-tone={primary.tone}>
        <div data-inline-status-live {...liveProps}>
          <ToneIcon tone={primary.tone} />
          {needsPopover ? <button ref={triggerRef} type="button" data-inline-status-trigger aria-expanded={open} aria-haspopup="dialog" aria-controls={popoverId} aria-describedby={moreCount ? moreId : undefined} onPointerEnter={scheduleOpen} onPointerLeave={scheduleClose} onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.preventDefault();
              event.stopPropagation();
              close(true);
            }
          }} onClick={(event) => toggle(event.detail === 0)}><span data-inline-status-message>{primary.message}</span>{moreCount > 0 ? <span data-inline-status-more data-count={moreCount} aria-hidden /> : null}</button> : <span data-inline-status-message>{primary.message}</span>}
        </div>
        {moreCount > 0 ? <span id={moreId} className="sr-only">{moreLabel(moreCount)}</span> : null}
        {primary.actions?.map((action) => actionButton(primary.key, action, action.id))}
      </div>
      {items.filter((item) => item !== primary && (item.id || item.srOnly)).map((item) => <span key={item.key} id={item.id} className="sr-only">{item.message}</span>)}
    </div>
    {open && host ? createPortal(<div ref={popoverRef} id={popoverId} data-inline-status-popover role="dialog" aria-label={dialogLabel} tabIndex={-1} style={position} onPointerEnter={() => window.clearTimeout(hoverTimer.current)} onPointerLeave={scheduleClose} onKeyDown={handlePopoverKeyDown}>{sections}</div>, host) : null}
    {confirmationAction?.confirm && host ? createPortal(<Confirmation action={confirmationAction} position={position} onCancel={() => {
      setConfirming(null);
      if (!focusIfConnected(openerRef.current)) focusTrigger();
    }} onConfirm={() => {
      confirmationAction.onAction();
      setConfirming(null);
      if (!focusIfConnected(openerRef.current)) focusTrigger();
    }} />, host) : null}
  </>;
}

function Confirmation({ action, position, onCancel, onConfirm }: { action: InlineStatusAction; position?: CSSProperties; onCancel(): void; onConfirm(): void }) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  const confirm = action.confirm!;
  useEffect(() => { focusIfConnected(cancelRef.current); }, []);
  return <div data-inline-status-confirmation role="alertdialog" aria-labelledby={`${action.id}-confirm-title`} aria-describedby={`${action.id}-confirm-body`} style={position} onKeyDown={(event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      onCancel();
    }
  }}><strong id={`${action.id}-confirm-title`}>{confirm.title}</strong><p id={`${action.id}-confirm-body`}>{confirm.body}</p><div data-inline-status-confirmation-actions><button ref={cancelRef} type="button" onClick={onCancel}>{confirm.cancelLabel ?? "Cancel"}</button><button type="button" data-inline-status-tone="warning" onClick={onConfirm}>{confirm.confirmLabel}</button></div></div>;
}
