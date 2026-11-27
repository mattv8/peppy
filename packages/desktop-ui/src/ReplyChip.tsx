import type { CSSProperties } from "react";
import { User, X } from "lucide-react";

export type ReplyChipProps = {
  /** The visible name: a sender's name, or "You" for your own message. */
  name: string;
  /** The accessible subject: the name, or "your message" for your own message. Defaults to `name`. */
  subject?: string;
  excerpt?: string;
  /** Conversation tone for the avatar; the CSS surface token is the default. */
  avatarStyle?: CSSProperties;
  onCancel(): void;
};

/** The avatar's letter: the name's first letter or digit, or nothing when it has neither. */
function avatarInitial(name: string): string {
  const character = Array.from(name.trim()).find((candidate) => /[\p{L}\p{N}]/u.test(candidate));
  return character?.toLocaleUpperCase() ?? "";
}

/** The composer's reply target: a sender avatar, the quoted excerpt, and a cancel button. */
export function ReplyChip({ name, subject = name, excerpt, avatarStyle, onCancel }: ReplyChipProps) {
  return <div
    id="composer-reply-chip"
    role="group"
    aria-label={`Replying to ${subject}${excerpt ? `: ${excerpt}` : ""}`}
  >
    <span data-reply-chip-avatar aria-hidden="true" style={avatarStyle}>{avatarInitial(name) || <User size={12} aria-hidden="true" />}</span>
    <div data-reply-chip-quote>
      <strong>{name}</strong>
      {excerpt && <span data-reply-chip-excerpt>{excerpt}</span>}
    </div>
    <button type="button" data-reply-chip-cancel aria-label={`Cancel reply to ${subject}`} onClick={onCancel}>
      <X size={14} aria-hidden="true" />
    </button>
  </div>;
}
