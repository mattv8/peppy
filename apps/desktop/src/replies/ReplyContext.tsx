import { Reply } from "@peppy/desktop-ui";
import type { ContactResolutionMap } from "../bridge/bridge";
import { senderLabel } from "../sender/attribution";
import type { MessageReplyView } from "@peppy/generated/message";

/**
 * The line above a reply's bubble that names the message it answers. Locates the original when it is available.
 * `excerpt` is the quote to show: the core's, or a media word when the original has only attachments.
 * `sourceMessageId` is the replying message's id, so the line can be told apart from other replies to the same original.
 */
export function ReplyContext({ reply, sourceMessageId, resolution, excerpt, onOpen }: {
  reply: MessageReplyView;
  sourceMessageId: string;
  resolution: ContactResolutionMap;
  excerpt?: string;
  onOpen(messageId: string): void;
}) {
  const name = reply.sender === "self" ? "you" : senderLabel(reply.senderAddress, resolution).text;
  const subject = reply.sender === "self" ? "Replying to you" : `Replying to ${name}`;
  // The full text, untruncated, for a tooltip and the button's name. The visible excerpt may be cut with an ellipsis.
  const fullText = reply.available ? `${subject}${excerpt ? ` “${excerpt}”` : ""}` : `${subject}: Original message not available`;
  const who = reply.sender === "self" ? <strong>Replying to you</strong> : <>Replying to <strong>{name}</strong></>;
  const detail = reply.available
    ? excerpt && <span className="message-reply-excerpt">“{excerpt}”</span>
    : <>: <span className="message-reply-unavailable">Original message not available</span></>;
  const content = <>
    <Reply size={12} aria-hidden />
    <span className="message-reply-who">{who}</span>
    {detail}
  </>;
  if (!reply.available) {
    return <div className="message-reply-context" data-message-reply-context={reply.messageId} data-reply-source={sourceMessageId} data-reply-available="false" title={fullText}>{content}</div>;
  }
  return (
    <button
      type="button"
      className="message-reply-context"
      data-message-reply-context={reply.messageId}
      data-reply-source={sourceMessageId}
      data-reply-available="true"
      title={fullText}
      aria-label={`${fullText}. Go to original message`}
      onClick={() => onOpen(reply.messageId)}
    >
      {content}
    </button>
  );
}
