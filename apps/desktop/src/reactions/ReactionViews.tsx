import { useCallback, useState, type ReactNode } from "react";
import { Clock, TriangleAlert, X } from "@peppy/desktop-ui";
import type { MessageView } from "@peppy/desktop-ui/bridge-contracts";
import type { ReactionBadgeInfo, ReactionNoticeInfo } from "./link";
import type { MergedReactionBadge, OwnReactionChip, OwnChipState } from "./merge";
import { ReactionIssuePopover, type IssueFocus } from "./ReactionIssuePopover";
import "./reactions.css";
import "./reaction-chips.css";

export type ActorName = (actor: { sender: MessageView["sender"]; senderAddress?: string }) => string;

type ChipRef = (key: string, node: HTMLElement | null) => void;
type IssueState = { requestId: string; anchor: HTMLElement; returnTarget: HTMLElement | null };
type ReactionPlacement = "row" | "overlay-bottom-start";

/** Hook values from ux-design: pending, sent, failed, unknown, removing. */
const STATE_HOOK: Record<OwnChipState, string> = { sending: "pending", sent: "sent", not_sent: "failed", unknown: "unknown", removing: "removing" };
type StateIcon = typeof Clock;
const STATE_ICON: Partial<Record<OwnChipState, StateIcon>> = { sending: Clock, removing: Clock, not_sent: X, unknown: TriangleAlert };

function asMerged(badge: MergedReactionBadge | ReactionBadgeInfo): MergedReactionBadge {
  return "source" in badge ? badge : { ...badge, source: "text" };
}

function ownChipLabel(emoji: string, own: OwnReactionChip): string {
  const intent = own.intent ?? "add";
  const previous = own.previousEmoji ? `Your earlier ${own.previousEmoji} is still in place.` : "Your earlier reaction is still in place.";
  if (own.state === "sent") return `${emoji} from you`;
  if (own.source === "text") {
    if (own.state === "sending") return `Your ${emoji} reaction is sending as a text message.`;
    if (own.state === "not_sent") return `Your ${emoji} reaction text wasn't sent.`;
    return `Not sure your ${emoji} reaction text was sent.`;
  }
  if (own.state === "sending") return `Your ${emoji} reaction is sending.`;
  if (own.state === "removing") return `Removing your ${emoji} reaction.`;
  if (own.state === "not_sent") {
    if (intent === "remove") return `Couldn't remove your ${emoji} reaction. Your reaction is still there.`;
    if (intent === "switch") return `Couldn't change your reaction to ${emoji}. ${previous}`;
    return `Couldn't send your ${emoji} reaction. Nothing was sent.`;
  }
  if (intent === "remove") return `Not sure your ${emoji} reaction was removed. It won't be sent again on its own.`;
  if (intent === "switch") return `Not sure your reaction changed to ${emoji}. It won't be sent again on its own.`;
  return `Not sure your ${emoji} reaction was sent. It won't be sent again on its own.`;
}

function chipLabel(badge: MergedReactionBadge, actorName: ActorName): string {
  return badge.own ? ownChipLabel(badge.emoji, badge.own) : `${badge.emoji} from ${badge.actors.map(actorName).join(", ")}`;
}

function chipKey(badge: MergedReactionBadge): string {
  return `${badge.own?.state ?? "other"}:${badge.own?.requestId ?? ""}:${badge.emoji}`;
}

function ChipContent({ badge }: { badge: MergedReactionBadge }) {
  const Icon = badge.own ? STATE_ICON[badge.own.state] : undefined;
  return <>
    <span className="reaction-emoji" aria-hidden="true">{badge.emoji}</span>
    {badge.actors.length > 1 && <span className="reaction-count" aria-hidden="true">{badge.actors.length}</span>}
    {Icon && <Icon className="reaction-state-icon" size={10} strokeWidth={2} aria-hidden />}
  </>;
}

function ReactionChip({ messageId, badge, label, chipRef, onActivate }: {
  messageId: string;
  badge: MergedReactionBadge;
  label: string;
  chipRef?: ChipRef;
  onActivate(badge: MergedReactionBadge, anchor: HTMLElement): void;
}) {
  const registryKey = JSON.stringify([messageId, badge.emoji]);
  const registerChip = useCallback((node: HTMLElement | null) => { chipRef?.(registryKey, node); }, [chipRef, registryKey]);
  const ref = chipRef ? registerChip : undefined;
  const own = badge.own;
  if (!own) return <span ref={ref} className="reaction-badge" data-reaction-emoji={badge.emoji} data-reaction-source={badge.source} role="img" aria-label={label}>
    <ChipContent badge={badge} />
  </span>;
  const hooks = { "data-reaction-emoji": badge.emoji, "data-reaction-own": "true", "data-reaction-state": STATE_HOOK[own.state], "data-reaction-source": own.source };
  if (own.source === "text") return <span ref={ref} className="reaction-badge reaction-badge-own reaction-badge-own-text" role="img" aria-label={label} {...hooks}>
    <ChipContent badge={badge} />
  </span>;
  const recoverable = own.state === "not_sent" || own.state === "unknown";
  return <button ref={ref} type="button" className="reaction-badge reaction-badge-own" tabIndex={recoverable ? 0 : -1} aria-label={label} aria-busy={own.state === "removing" || undefined} {...hooks}
    onClick={(event) => onActivate(badge, event.currentTarget)}>
    <ChipContent badge={badge} />
  </button>;
}

export function ReactionBadges({ messageId, badges, actorName, chipRef, placement = "row", onOwnChip, onRetry, onDismiss }: {
  messageId: string;
  badges: MergedReactionBadge[] | ReactionBadgeInfo[];
  actorName: ActorName;
  chipRef?: ChipRef;
  placement?: ReactionPlacement;
  onOwnChip?(badge: MergedReactionBadge): void;
  onRetry?(requestId: string, emoji: string | null): void;
  onDismiss?(requestId: string): void;
}) {
  const [issue, setIssue] = useState<IssueState | null>(null);
  const list: ReadonlyArray<MergedReactionBadge | ReactionBadgeInfo> = badges;
  const chips = list.map(asMerged);
  if (chips.length === 0) return null;
  const labels = chips.map((chip) => chipLabel(chip, actorName));
  const label = `Reactions: ${labels.map((text) => text.replace(/\.$/u, "")).join("; ")}`;
  const labelId = `reactions-label-${messageId}`;
  const openChip = issue ? chips.find((chip) => chip.own?.requestId === issue.requestId && (chip.own.state === "not_sent" || chip.own.state === "unknown")) : undefined;

  const closeIssue = (focus: IssueFocus) => {
    const target = focus === "none" ? null : focus === "chip" && issue?.anchor.isConnected ? issue.anchor : issue?.returnTarget;
    setIssue(null);
    target?.focus();
  };
  const activate = (badge: MergedReactionBadge, anchor: HTMLElement) => {
    const requestId = badge.own?.requestId;
    if (requestId && (badge.own?.state === "not_sent" || badge.own?.state === "unknown")) {
      setIssue({ requestId, anchor, returnTarget: anchor.closest<HTMLElement>("article") ?? document.getElementById("message-list") });
    } else if (badge.own?.state === "sent") onOwnChip?.(badge);
  };

  return <span id={`reactions-${messageId}`} className={placement === "overlay-bottom-start" ? "reaction-badges reaction-badges-overlay" : "reaction-badges"} data-reaction-badges={messageId} data-reaction-placement={placement}
    role="group" aria-label="Reactions" aria-describedby={labelId} title={label}>
    <span id={labelId} className="sr-only">{label}</span>
    {chips.map((chip, index) => <ReactionChip key={chipKey(chip)} messageId={messageId} badge={chip} label={labels[index]} chipRef={chipRef} onActivate={activate} />)}
    {openChip && issue && <ReactionIssuePopover messageId={messageId} badge={openChip} anchor={issue.anchor} onRetry={onRetry} onDismiss={onDismiss} onClose={closeIssue} />}
  </span>;
}

export function ReactionNotice({ reaction, notice, actorName, rowRef, onOpen, meta, busy = false, feedback, suppressedHint = false }: {
  reaction: MessageView;
  notice: ReactionNoticeInfo;
  actorName: ActorName;
  rowRef: (row: HTMLElement | null) => void;
  onOpen(reaction: MessageView, notice: ReactionNoticeInfo): void;
  meta?: ReactNode;
  busy?: boolean;
  feedback?: string;
  suppressedHint?: boolean;
}) {
  if (suppressedHint) return <span hidden data-reaction-marker={reaction.id} data-message-id={reaction.id} ref={rowRef} />;
  const verb = notice.action === "add" ? "reacted" : "removed";
  const preposition = notice.action === "add" ? "to" : "from";
  const quote = `${notice.quotedText}${notice.truncated ? "…" : ""}`;
  const state = busy ? "locating" : notice.targetId ? "linked" : "unresolved";
  return <div className="message-row reaction-notice-row" data-message-row={reaction.id} data-reaction-notice={reaction.id} data-reaction-state={state}>
    <button ref={rowRef} type="button" className="reaction-notice" data-message-id={reaction.id} title={notice.targetId ? "Show original message" : "Find original message"} aria-busy={busy || undefined} aria-disabled={busy || undefined} onClick={() => { if (!busy) onOpen(reaction, notice); }}>
      <span className="reaction-notice-text">
        <span className="reaction-notice-actor">{actorName(reaction)}</span>{" "}{verb}{" "}
        <span className="reaction-notice-emoji">{notice.emoji}</span>{" "}{preposition}{" "}
        <span className="reaction-notice-quote">“{quote}”</span>
      </span>
    </button>
    {feedback && <span className="reaction-notice-feedback" data-reaction-feedback={reaction.id} role="status" aria-live="polite">{feedback}</span>}
    {meta}
  </div>;
}
