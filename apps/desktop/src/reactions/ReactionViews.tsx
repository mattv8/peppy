import type { ReactNode } from "react";
import type { MessageView } from "@peppy/desktop-ui/bridge-contracts";
import type { ReactionBadgeInfo, ReactionNoticeInfo } from "./link";
import "./reactions.css";

export type ActorName = (actor: { sender: MessageView["sender"]; senderAddress?: string }) => string;

function badgeLabel(badges: ReactionBadgeInfo[], actorName: ActorName): string {
  return `Reactions: ${badges.map((badge) => `${badge.emoji} from ${badge.actors.map(actorName).join(", ")}`).join("; ")}`;
}

export function ReactionBadges({ messageId, badges, actorName }: { messageId: string; badges: ReactionBadgeInfo[]; actorName: ActorName }) {
  const label = badgeLabel(badges, actorName);
  return <span className="reaction-badges" data-reaction-badges={messageId} role="img" aria-label={label} title={label}>
    {badges.map((badge) => <span key={badge.emoji} className="reaction-badge" data-reaction-emoji={badge.emoji} aria-hidden="true">
      <span className="reaction-emoji">{badge.emoji}</span>
      {badge.actors.length > 1 && <span className="reaction-count">{badge.actors.length}</span>}
    </span>)}
  </span>;
}

export function ReactionNotice({ reaction, notice, actorName, rowRef, onOpen, meta, busy = false, feedback }: {
  reaction: MessageView;
  notice: ReactionNoticeInfo;
  actorName: ActorName;
  rowRef: (row: HTMLElement | null) => void;
  onOpen(reaction: MessageView, notice: ReactionNoticeInfo): void;
  meta?: ReactNode;
  busy?: boolean;
  feedback?: string;
}) {
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
