import { useLayoutEffect, useRef, type FocusEventHandler, type ReactNode, type RefObject } from "react";
import type { RcsAttachmentState } from "@peppy/desktop-ui/bridge-contracts";

/** Keep focus owned by the original slot while retry controls disappear during download. */
export function useRcsReplacementFocus(root: RefObject<HTMLDivElement | null>, phase: RcsAttachmentState | undefined) {
  const ownsFocus = useRef(false);
  const previousPhase = useRef(phase);
  const onFocusCapture: FocusEventHandler<HTMLDivElement> = () => { ownsFocus.current = true; };
  const onBlurCapture: FocusEventHandler<HTMLDivElement> = (event) => {
    if (!event.currentTarget.contains(event.relatedTarget)) ownsFocus.current = false;
  };
  useLayoutEffect(() => {
    const element = root.current;
    if (element && ownsFocus.current && previousPhase.current !== phase) {
      // A removed focused control leaves focus on body. Park it on the persistent slot,
      // then hand it to the stored control; a real focus/pointer departure cancels ownership.
      (element.querySelector<HTMLElement>("button, [tabindex='0']") ?? element).focus();
    }
    previousPhase.current = phase;
  }, [phase, root]);
  return { onFocusCapture, onBlurCapture };
}

export function RcsAttachmentSlot({ slotId, phase, children }: { slotId: string; phase?: RcsAttachmentState; children: ReactNode }) {
  const root = useRef<HTMLDivElement>(null);
  const focus = useRcsReplacementFocus(root, phase);
  return <div ref={root} tabIndex={-1} {...focus} data-rcs-attachment-slot={slotId}>{children}</div>;
}
