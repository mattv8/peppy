import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import type { GatewayView, MessageView } from "@peppy/desktop-ui/bridge-contracts";
import { bridge } from "../bridge/bridge";
import { errorCode } from "../bridge/bridgeErrors";
import { peppyCopy } from "../generated/peppyCopy";
import type { SendAgainStore } from "./SendAgain";
import { sendAgainRoutes, type SendAgainRoute } from "./SendAgainRouteList";

export const findOpener = (messageId: string): HTMLElement | null => document.getElementById(`message-send-again-${messageId}`);

const focusInsideOrLost = (dialog: HTMLElement | null): boolean => {
  const active = document.activeElement;
  return !active || active === document.body || Boolean(dialog?.contains(active));
};

export type UseSendAgainOptions = {
  message: MessageView;
  gateways: GatewayView[];
  serverOffline: boolean;
  store: SendAgainStore;
  onClose(): void;
};

/**
 * The Send again popover's state machine: which route is selected, what blocks confirm, the send call and
 * its focus handling. The call itself is guarded by the host store, so the guard holds across popover instances.
 */
export function useSendAgain({ message, gateways, serverOffline, store, onClose }: UseSendAgainOptions) {
  const id = message.id;
  const dialogRef = useRef<HTMLDivElement>(null);
  const titleRef = useRef<HTMLElement>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);
  const closedRef = useRef(false);
  const callActiveRef = useRef(false);
  const [rejected, setRejected] = useState<ReadonlySet<string>>(() => new Set());
  const [failures, setFailures] = useState(0);
  // Only a popover that OPENED with exactly one eligible route preselects it (ruling I1).
  const [preselected] = useState<string | undefined>(() => {
    const eligible = sendAgainRoutes(gateways, message, new Set()).filter((route) => route.reason === undefined);
    return eligible.length === 1 ? eligible[0].key : undefined;
  });
  const [chosen, setChosen] = useState<string | undefined>(preselected);
  // The preselection is in force until its selection is cleared; after that the route is an ordinary choice.
  const [preselectionKept, setPreselectionKept] = useState(preselected !== undefined);

  const routes: SendAgainRoute[] = sendAgainRoutes(gateways, message, rejected);
  const eligible = routes.filter((route) => route.reason === undefined);
  const selected = eligible.find((route) => route.key === chosen);
  // The read-only row only while the preselection is still the current selection; a route that comes back after being cleared is a radio.
  const onlyKey = preselectionKept && selected !== undefined && eligible.length === 1 && selected.key === preselected ? selected.key : undefined;
  const noRoute = eligible.length === 0;
  // Any eligible route that is not selected needs an explicit choice, including one that became eligible after opening.
  const needsChoice = !noRoute && !selected;
  const pending = store.isPending(id);

  const sentAgainId = message.sentAgain?.messageId ?? message.sendRecovery?.sentAgain?.messageId ?? store.sentAgainFor(id);
  const sendable = message.sendRecovery?.canSendAgain === true && sentAgainId === undefined;
  const storedError = store.errorFor(id);
  const missingAttachments = storedError === "attachments-unavailable" || message.attachments.some((attachment) => attachment.localAvailable === false);
  // Offline is the live server state (not a latched error): confirm is blocked while the server is unreachable.
  const blocked = missingAttachments || serverOffline || !sendable;
  const confirmAriaDisabled = noRoute || needsChoice || blocked;

  const alert = missingAttachments ? peppyCopy.message_send_again_attachments_missing
    : serverOffline ? peppyCopy.message_send_again_offline
      : storedError === "route-unavailable" ? peppyCopy.message_send_again_route_gone
        : storedError === "offline" ? peppyCopy.message_send_again_offline
          : storedError === "invalid-request" ? peppyCopy.message_send_again_failed
            : storedError !== undefined ? peppyCopy.message_send_again_unconfirmed_call
            : undefined;
  const confirmDescribedBy = needsChoice ? `send-again-hint-${id}` : alert !== undefined ? `send-again-error-${id}` : undefined;

  const latest = useRef({ sendable });
  latest.current = { sendable };

  // Clear a chosen route the moment it stops being eligible; never move the choice to another phone. A cleared
  // preselection stays cleared: the route can come back, but only as a radio the user explicitly chooses.
  useEffect(() => {
    if (chosen !== undefined && !selected) {
      setChosen(undefined);
      setPreselectionKept(false);
    }
  }, [chosen, selected]);

  const close = () => {
    if (closedRef.current) return;
    closedRef.current = true;
    onClose();
  };
  /** Success, or this device already holds its copy (already-sent-again): the done row takes focus only if focus was still here. */
  const finish = () => {
    if (closedRef.current) return;
    if (focusInsideOrLost(dialogRef.current)) store.requestDoneFocus(id);
    close();
  };
  /** Escape, Cancel, Tab out and outside presses. Nothing closes while a call is pending; focus returns to the opener found now. */
  const dismiss = ({ returnFocus }: { returnFocus: boolean }) => {
    if (callActiveRef.current) return;
    if (returnFocus && dialogRef.current?.contains(document.activeElement)) findOpener(id)?.focus();
    close();
  };
  const dismissRef = useRef(dismiss);
  dismissRef.current = dismiss;

  const send = async () => {
    if (!selected || confirmAriaDisabled || callActiveRef.current || !store.startCall(id)) return;
    callActiveRef.current = true;
    try {
      const result = await bridge.send_again({ messageId: id, gatewayId: selected.gateway.id, simId: selected.gateway.simId });
      callActiveRef.current = false;
      store.settleCall(id, { kind: "sent", messageId: result.messageId });
      finish();
    } catch (failure) {
      callActiveRef.current = false;
      const code = errorCode(failure) ?? "failed";
      if (code === "already-sent-again") {
        store.settleCall(id, { kind: "already" });
        finish();
        return;
      }
      store.settleCall(id, { kind: "failed", code });
      if (code === "route-unavailable") setRejected((current) => new Set(current).add(selected.key));
      setFailures((count) => count + 1);
      if (!latest.current.sendable) close();
    }
  };

  // Mount: the title takes focus so a screen reader reads the title and body first.
  useEffect(() => { titleRef.current?.focus(); }, []);
  // A failed call leaves the popover focused: the button is enabled again only after this render.
  useEffect(() => {
    if (failures === 0) return;
    if (confirmRef.current && !confirmRef.current.disabled) confirmRef.current.focus();
    else titleRef.current?.focus();
  }, [failures]);
  // This device now knows its copy (a settled call here, or the local snapshot carries sentAgain), or the message is no longer
  // sendable: close, unless a call is pending. sentAgain is device-local (R16), so other devices never reach this branch.
  useEffect(() => {
    if (callActiveRef.current) return;
    if (sentAgainId !== undefined) finish();
    else if (!sendable) close();
    // finish and close read only refs and the stable onClose wrapper, so the dependency list is the state that matters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sentAgainId, sendable]);
  // Outside presses close without taking focus back. Presses on the opener are left to the host (which toggles).
  useEffect(() => {
    const outside = (event: PointerEvent) => {
      const target = event.target as Node;
      if (dialogRef.current?.contains(target) || findOpener(id)?.contains(target)) return;
      dismissRef.current({ returnFocus: false });
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [id]);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      dismiss({ returnFocus: true });
      return;
    }
    if (event.key !== "Tab") return;
    if (callActiveRef.current) {
      event.preventDefault();
      return;
    }
    // Tab past the last control, or Shift+Tab from the first control or the title, leaves the popover: close and return to the opener.
    const stops = Array.from(dialogRef.current?.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled)") ?? []);
    const target = event.target as HTMLElement;
    const leavesBackward = event.shiftKey && (target === titleRef.current || target === stops[0]);
    const leavesForward = !event.shiftKey && target === stops.at(-1);
    if (leavesBackward || leavesForward) {
      event.preventDefault();
      dismiss({ returnFocus: true });
    }
  };

  return {
    dialogRef,
    titleRef,
    confirmRef,
    routes,
    selected,
    onlyKey,
    noRoute,
    needsChoice,
    pending,
    alert,
    blocked,
    confirmAriaDisabled,
    confirmDescribedBy,
    send,
    choose: (key: string) => setChosen(key),
    dismiss,
    onKeyDown,
  };
}
