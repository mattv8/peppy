import { UPDATE_PAUSED_DESCRIPTION } from "@peppy/desktop-ui";
import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import { createPortal } from "react-dom";
import { TriangleAlert, X } from "@peppy/desktop-ui";
import type { MergedReactionBadge } from "./merge";

/** Where focus goes when the popover closes: back to the chip, to the message article, or nowhere. */
export type IssueFocus = "chip" | "article" | "none";
export type IssueAction = "retry" | "discard" | "keep" | "dismiss";
export type IssueCopy = {
  kind: "failed" | "unknown";
  title: string;
  body: string;
  actions: Array<{ action: IssueAction; label: string; primary?: boolean }>;
};

const POPOVER_WIDTH = 232;
const VIEWPORT_MARGIN = 8;
const POPOVER_GAP = 4;
/** The smallest the popover may be squeezed to when it has to scroll; enough for the title and one action. */
const MIN_POPOVER_HEIGHT = 96;
const UNKNOWN_BODY_TAIL = "Peppy won't send it again on its own. If your phone confirms it, this updates by itself.";

function unknownBody(verb: "sent" | "removed" | "changed"): string {
  return `Your phone may have ${verb} it. Check your phone. ${UNKNOWN_BODY_TAIL}`;
}

function failedCopy(emoji: string, intent: NonNullable<MergedReactionBadge["own"]>["intent"], previous: string | null | undefined): IssueCopy {
  if (intent === "remove") return {
    kind: "failed", title: `Couldn't remove your ${emoji} reaction`, body: "Your reaction is still there.",
    actions: [{ action: "retry", label: "Try again", primary: true }, { action: "keep", label: "Keep it" }],
  };
  if (intent === "switch") return {
    kind: "failed", title: `Couldn't change your reaction to ${emoji}`, body: previous ? `Your earlier ${previous} is still in place.` : "Your earlier reaction is still in place.",
    actions: [{ action: "retry", label: "Try again", primary: true }, { action: "discard", label: "Discard" }],
  };
  return {
    kind: "failed", title: `Couldn't send your ${emoji} reaction`, body: "Your phone didn't get it, so nothing was sent.",
    actions: [{ action: "retry", label: "Try again", primary: true }, { action: "discard", label: "Discard" }],
  };
}

function unknownCopy(emoji: string, intent: NonNullable<MergedReactionBadge["own"]>["intent"]): IssueCopy {
  const dismiss = [{ action: "dismiss" as const, label: "Dismiss" }];
  if (intent === "remove") return { kind: "unknown", title: `Not sure your ${emoji} reaction was removed`, body: unknownBody("removed"), actions: dismiss };
  if (intent === "switch") return { kind: "unknown", title: `Not sure your reaction changed to ${emoji}`, body: unknownBody("changed"), actions: dismiss };
  return { kind: "unknown", title: `Not sure your ${emoji} reaction was sent`, body: unknownBody("sent"), actions: dismiss };
}

/** Copy for a failed or unknown native attempt; undefined for every other chip. */
export function issueCopy(badge: MergedReactionBadge): IssueCopy | undefined {
  const own = badge.own;
  if (own?.state === "not_sent") return failedCopy(badge.emoji, own.intent ?? "add", own.previousEmoji);
  if (own?.state === "unknown") return unknownCopy(badge.emoji, own.intent ?? "add");
  return undefined;
}

export function ReactionIssuePopover({ messageId, badge, anchor, writesPaused = false, onRetry, onDismiss, onClose }: {
  messageId: string;
  badge: MergedReactionBadge;
  anchor: HTMLElement;
  writesPaused?: boolean;
  onRetry?(requestId: string, emoji: string | null): void;
  onDismiss?(requestId: string): void;
  onClose(focus: IssueFocus): void;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<CSSProperties>();
  const copy = issueCopy(badge);
  const requestId = badge.own?.requestId;
  const titleId = `reaction-issue-title-${messageId}`;
  const bodyId = `reaction-issue-body-${messageId}`;

  useLayoutEffect(() => {
    const place = () => {
      const rect = anchor.getBoundingClientRect();
      const maxLeft = window.innerWidth - POPOVER_WIDTH - VIEWPORT_MARGIN;
      // Below the chip when it fits above the composer; otherwise above the chip, with its height capped to the room there is.
      const height = rootRef.current?.offsetHeight ?? 0;
      const composerTop = document.getElementById("composer-area")?.getBoundingClientRect().top ?? window.innerHeight;
      const below = rect.bottom + POPOVER_GAP;
      const above = rect.top - POPOVER_GAP - height;
      const flip = below + height > composerTop - VIEWPORT_MARGIN && above >= VIEWPORT_MARGIN;
      const top = flip ? above : below;
      const room = flip ? rect.top - POPOVER_GAP - VIEWPORT_MARGIN : composerTop - VIEWPORT_MARGIN - below;
      setPosition({
        top: `${top}px`,
        left: `${Math.min(Math.max(VIEWPORT_MARGIN, rect.left), maxLeft)}px`,
        maxHeight: `${Math.max(room, MIN_POPOVER_HEIGHT)}px`,
        overflowY: "auto",
      });
    };
    place();
    window.addEventListener("resize", place);
    return () => window.removeEventListener("resize", place);
  }, [anchor]);

  // The root is visibility:hidden until it is positioned, and hidden elements cannot take focus, so focus waits for position.
  const focusedFirstAction = useRef(false);
  useEffect(() => {
    if (!position || focusedFirstAction.current) return;
    focusedFirstAction.current = true;
    rootRef.current?.querySelector<HTMLButtonElement>("[data-reaction-action]")?.focus();
  }, [position]);

  useEffect(() => {
    const outside = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!rootRef.current?.contains(target) && !anchor.contains(target)) onClose("none");
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [anchor, onClose]);

  if (!copy || typeof document === "undefined") return null;
  const run = (action: IssueAction) => {
    if (action === "retry" && writesPaused) return;
    if (requestId && action === "retry") onRetry?.(requestId, badge.own?.intent === "remove" ? null : badge.emoji);
    else if (requestId) onDismiss?.(requestId);
    onClose("article");
  };
  const Icon = copy.kind === "failed" ? X : TriangleAlert;

  return createPortal(<div ref={rootRef} id={`reaction-issue-${messageId}`} className="reaction-issue" role="dialog" aria-labelledby={titleId} aria-describedby={bodyId} data-reaction-issue={copy.kind} style={position ?? { visibility: "hidden" }}
    onClick={(event) => event.stopPropagation()}
    onKeyDown={(event) => { if (event.key === "Escape") { event.stopPropagation(); onClose("chip"); } }}>
    <p id={titleId} className="reaction-issue-title"><Icon size={12} strokeWidth={2} aria-hidden /><span>{copy.title}</span></p>
    <p id={bodyId} className="reaction-issue-body">{copy.body}</p>
    <div className="reaction-issue-actions">
      {copy.actions.map(({ action, label, primary }) => <button key={action} type="button" className={primary ? "reaction-issue-action reaction-issue-action-primary" : "reaction-issue-action"} data-reaction-action={action} aria-disabled={action === "retry" && writesPaused || undefined} aria-describedby={action === "retry" && writesPaused ? "update-paused-description" : undefined} title={action === "retry" && writesPaused ? UPDATE_PAUSED_DESCRIPTION : undefined} onClick={() => run(action)}>{label}</button>)}
    </div>
  </div>, document.body);
}
