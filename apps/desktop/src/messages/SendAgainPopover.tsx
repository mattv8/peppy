import { UPDATE_PAUSED_DESCRIPTION } from "@peppy/desktop-ui";
import { createPortal } from "react-dom";
import { useLayoutEffect, useRef, useState, type CSSProperties, type RefObject } from "react";
import { TriangleAlert } from "@peppy/desktop-ui";
import type { GatewayView, MessageView } from "@peppy/desktop-ui/bridge-contracts";
import { formatCopy, peppyCopy } from "../generated/peppyCopy";
import type { SendAgainStore } from "./SendAgain";
import { RouteList, RouteReasons, type SendAgainRouteLabeler } from "./SendAgainRouteList";
import { findOpener, useSendAgain } from "./useSendAgain";

const PREVIEW_CHARACTERS = 120;
const VIEWPORT_MARGIN = 8;
const POPOVER_GAP = 6;
const POPOVER_MAX_WIDTH = 360;
const ABOVE_MIN_SPACE = 260;

/**
 * Keeps the popover beside its opener while the thread scrolls, the window resizes or the opener resizes.
 * Closes (without moving focus) when the opener scrolls out of the viewport.
 */
function useAnchoredPlacement(messageId: string, dialogRef: RefObject<HTMLDivElement | null>, onOffscreen: () => void): CSSProperties | undefined {
  const [position, setPosition] = useState<CSSProperties>();
  const offscreenRef = useRef(onOffscreen);
  offscreenRef.current = onOffscreen;
  useLayoutEffect(() => {
    const place = () => {
      const opener = findOpener(messageId);
      if (!opener) return;
      const rect = opener.getBoundingClientRect();
      if (rect.bottom < 0 || rect.top > window.innerHeight) {
        offscreenRef.current();
        return;
      }
      const width = Math.min(POPOVER_MAX_WIDTH, window.innerWidth - VIEWPORT_MARGIN * 2);
      const left = Math.min(Math.max(VIEWPORT_MARGIN, rect.left), window.innerWidth - width - VIEWPORT_MARGIN);
      setPosition(rect.top > ABOVE_MIN_SPACE
        ? { position: "fixed", bottom: window.innerHeight - rect.top + POPOVER_GAP, left, width, maxHeight: Math.max(0, rect.top - VIEWPORT_MARGIN - POPOVER_GAP) }
        : { position: "fixed", top: rect.bottom + POPOVER_GAP, left, width, maxHeight: Math.max(0, window.innerHeight - rect.bottom - VIEWPORT_MARGIN - POPOVER_GAP) });
    };
    place();
    const onScroll = (event: Event) => {
      if (!dialogRef.current?.contains(event.target as Node)) place();
    };
    window.addEventListener("resize", place);
    document.addEventListener("scroll", onScroll, true);
    const opener = findOpener(messageId);
    const observer = opener && typeof ResizeObserver !== "undefined" ? new ResizeObserver(place) : undefined;
    if (opener) observer?.observe(opener);
    return () => {
      window.removeEventListener("resize", place);
      document.removeEventListener("scroll", onScroll, true);
      observer?.disconnect();
    };
  }, [messageId, dialogRef]);
  return position;
}

export type SendAgainPopoverProps = {
  message: MessageView;
  gateways: GatewayView[];
  routeLabel: SendAgainRouteLabeler;
  serverOffline: boolean;
  /** The Send again button the popover was opened from; its portal host is the theme root above it. */
  anchor: HTMLElement;
  store: SendAgainStore;
  onClose(): void;
};

export function SendAgainPopover({ message, gateways, routeLabel, serverOffline, anchor, store, onClose }: SendAgainPopoverProps) {
  const id = message.id;
  const send = useSendAgain({ message, gateways, serverOffline, store, onClose });
  const position = useAnchoredPlacement(id, send.dialogRef, () => send.dismiss({ returnFocus: false }));
  const host = anchor.closest<HTMLElement>(".theme-light, .theme-dark, .theme-system") ?? document.body;
  const kind = message.sendRecovery?.kind ?? "not-sent";
  const removed = kind !== "not-sent";
  const characters = Array.from(message.body);
  const preview = characters.length > PREVIEW_CHARACTERS ? `${characters.slice(0, PREVIEW_CHARACTERS).join("")}…` : message.body;
  const attachmentCount = message.attachments.length;
  const recipients = message.participants?.length ?? 1;
  const noRouteId = `send-again-none-${id}`;

  return createPortal(<div ref={send.dialogRef} id={`send-again-popover-${id}`} data-send-again-popover role="dialog" aria-modal="false" aria-labelledby={`send-again-title-${id}`} aria-describedby={`send-again-body-${id}`} aria-busy={send.pending || undefined} style={position} onKeyDown={send.onKeyDown}>
    <strong id={`send-again-title-${id}`} ref={send.titleRef} tabIndex={-1}>{peppyCopy.message_send_again_title}</strong>
    <p id={`send-again-body-${id}`} data-send-again-body>{removed && <TriangleAlert size={12} aria-hidden />}{removed ? peppyCopy.message_send_again_body_unconfirmed : peppyCopy.message_send_again_body_not_sent}</p>
    <div data-send-again-summary>
      {message.body.length > 0 && <p data-send-again-preview><bdi title={message.body}>{preview}</bdi></p>}
      {attachmentCount > 0 && <p data-send-again-attachments>{attachmentCount === 1 ? peppyCopy.message_send_again_attachments_one : formatCopy(peppyCopy.message_send_again_attachments_many, String(attachmentCount))}</p>}
      {recipients > 1 && <p data-send-again-recipients>{peppyCopy.group_header_count.replace("{count}", String(recipients))}</p>}
    </div>
    {send.noRoute ? <div id={noRouteId} data-send-again-none>
      <p>{peppyCopy.message_send_again_none}</p>
      <RouteReasons routes={send.routes} routeLabel={routeLabel} />
      <p>{peppyCopy.message_send_again_none_hint}</p>
    </div> : <>
      <RouteList messageId={id} routes={send.routes} selectedKey={send.selected?.key} onlyKey={send.onlyKey} pending={send.pending} routeLabel={routeLabel} onChoose={send.choose} hintId={send.needsChoice ? `send-again-hint-${id}` : undefined} />
      {send.needsChoice && <p id={`send-again-hint-${id}`} data-send-again-hint>{peppyCopy.message_send_again_route_choose}</p>}
    </>}
    {send.alert !== undefined && <p id={`send-again-error-${id}`} role="alert" data-send-again-error>{send.alert}</p>}
    <div data-send-again-actions>
      <button id={`send-again-cancel-${id}`} type="button" className="secondary-button" disabled={send.pending} onClick={() => send.dismiss({ returnFocus: true })}>{peppyCopy.composer_status_cancel}</button>
      <button ref={send.confirmRef} id={`send-again-confirm-${id}`} type="button" className="primary-button" disabled={send.pending || store.writesPaused} title={store.writesPaused ? UPDATE_PAUSED_DESCRIPTION : undefined} aria-disabled={send.confirmAriaDisabled || undefined} aria-describedby={store.writesPaused ? "update-paused-description" : send.noRoute ? noRouteId : send.confirmDescribedBy} onClick={() => void send.send()}>{send.pending ? peppyCopy.message_send_again_pending : peppyCopy.message_send_again_action}</button>
    </div>
  </div>, host);
}
