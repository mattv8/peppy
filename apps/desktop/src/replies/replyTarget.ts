import type { MessageView } from "@peppy/desktop-ui/bridge-contracts";
import type { DraftIdentity } from "../drafts/DraftStore";
import { quoteExcerpt } from "./excerpt";

/** The message a conversation's composer replies to. Kept in memory with the name and quote captured when Reply was chosen. */
export type ReplyTarget = {
  messageId: string;
  sender: MessageView["sender"];
  senderAddress?: string;
  /** The visible name: the sender's name, or "You" for your own message. */
  name: string;
  /** The quoted body text. Only this is sent as the SMS/MMS prefix; a media-only target has none. */
  quote?: string;
  /** What the chip shows: the quote, or "Photo" or "Attachment" for a media-only target. */
  excerpt?: string;
};

/**
 * A reply target with the token of the selection that set it. Each Reply gets a new token, so a stale clear cannot remove a newer selection.
 * `draftIdentity` is the draft it was chosen for (DraftStore.ensureIdentity). `movedByMerge` marks a selection that a merge carried
 * onto the survivor's draft, so that a later takeover of that draft can say why the reply was removed.
 */
export type ReplySelection = ReplyTarget & { selection: number; draftIdentity: DraftIdentity; movedByMerge?: true };

/** True while the selection still belongs to `current`, the draft a Send would submit. The only draft it may be sent with. */
export function replyBelongsTo(selection: ReplySelection | undefined, current: DraftIdentity | undefined): boolean {
  if (!selection || !current) return false;
  const bound = selection.draftIdentity;
  return bound.draftId ? bound.draftId === current.draftId : bound.slot === current.slot;
}

/**
 * The reply targets of one search session, keyed by conversation id. `session` is the search session key they were chosen in
 * (App's `searchSessionKey`). A key that no longer matches the installed session makes every target absent.
 */
export type ReplyScope = { session: string; targets: Record<string, ReplySelection> };

export const NO_REPLY_SCOPE: ReplyScope = { session: "", targets: {} };

const NO_REPLY_TARGETS: Record<string, ReplySelection> = {};

/** The targets chosen in `session`. Targets from another session are never returned, so a replacement session cannot inherit them. */
export function replyTargetsIn(scope: ReplyScope, session: string): Record<string, ReplySelection> {
  return scope.session === session ? scope.targets : NO_REPLY_TARGETS;
}

/** The media word for a message with no text: "Photo" when every attachment is an image, else "Attachment". */
function mediaExcerpt(attachments: MessageView["attachments"]): string | undefined {
  if (!attachments.length) return undefined;
  return attachments.every((attachment) => attachment.mediaType.startsWith("image/")) ? "Photo" : "Attachment";
}

export function replyTargetFrom(message: MessageView, name: string): ReplyTarget {
  const quote = quoteExcerpt(message.body) || undefined;
  return {
    messageId: message.id,
    sender: message.sender,
    senderAddress: message.senderAddress,
    name,
    quote,
    excerpt: quote ?? mediaExcerpt(message.attachments),
  };
}

/** Who the reply is to, as the composer names it: "your message" for your own message, else the sender's name. */
export function replySubject(target: Pick<ReplyTarget, "sender" | "name">): string {
  return target.sender === "self" ? "your message" : target.name;
}

/** The quote a reply line shows: the core's excerpt, else the media word of the original when it is among the loaded messages. */
export function replyLineExcerpt(reply: { messageId: string; excerpt?: string }, messagesById: ReadonlyMap<string, MessageView>): string | undefined {
  if (reply.excerpt) return reply.excerpt;
  return mediaExcerpt(messagesById.get(reply.messageId)?.attachments ?? []);
}
