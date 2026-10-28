import type { AttachmentView, MessageView } from "@peppy/desktop-ui/bridge-contracts";

export type GalleryAttachment = AttachmentView & {
  originalUrl?: string;
  durationMs?: number;
};

export type GalleryMessage = Omit<MessageView, "attachments"> & {
  attachments: GalleryAttachment[];
  senderAddress?: string;
};

export type MessageGroup = {
  id: string;
  messages: GalleryMessage[];
  attachments: Array<{ attachment: GalleryAttachment; message: GalleryMessage }>;
};

const GROUP_WINDOW_MS = 3 * 60 * 1000;

export function parseTimestampMs(timestamp: string): number | undefined {
  const trimmedTimestamp = timestamp.trim();
  if (!trimmedTimestamp) return undefined;
  const numericValue = Number(trimmedTimestamp);
  const value = Number.isFinite(numericValue) ? numericValue : Date.parse(trimmedTimestamp);
  return Number.isFinite(value) ? value : undefined;
}

function timestampMilliseconds(message: GalleryMessage): number | undefined {
  return parseTimestampMs(message.timestamp);
}

function isImageOnly(message: GalleryMessage): boolean {
  return message.attachments.length > 0 && message.attachments.every((attachment) => attachment.mediaType.startsWith("image/"));
}

function senderIdentity(message: GalleryMessage): string | undefined {
  if (message.sender === "self") return "self";
  return message.senderAddress ? `other:${message.senderAddress}` : undefined;
}

function canJoin(group: GalleryMessage[], candidate: GalleryMessage): boolean {
  const previous = group.at(-1);
  if (!previous || !isImageOnly(candidate) || !isImageOnly(previous)) return false;
  if (previous.body || previous.subject) return false;
  const previousIdentity = senderIdentity(previous);
  const candidateIdentity = senderIdentity(candidate);
  if (!previousIdentity || previousIdentity !== candidateIdentity) return false;
  const previousTime = timestampMilliseconds(previous);
  const candidateTime = timestampMilliseconds(candidate);
  return previousTime !== undefined && candidateTime !== undefined && candidateTime >= previousTime && candidateTime - previousTime <= GROUP_WINDOW_MS;
}

function toGroup(messages: GalleryMessage[]): MessageGroup {
  return {
    id: messages[0].id,
    messages,
    attachments: messages.flatMap((message) => message.attachments.map((attachment) => ({ attachment, message }))),
  };
}

/** Groups only chronological, same-sender image messages; all other messages stay solo. */
export function groupMessages(messages: GalleryMessage[]): MessageGroup[] {
  const groups: MessageGroup[] = [];
  let candidate: GalleryMessage[] = [];
  for (const message of messages) {
    if (!isImageOnly(message) || !senderIdentity(message) || timestampMilliseconds(message) === undefined) {
      if (candidate.length) groups.push(toGroup(candidate));
      candidate = [];
      groups.push(toGroup([message]));
      continue;
    }
    if (canJoin(candidate, message)) candidate.push(message);
    else {
      if (candidate.length) groups.push(toGroup(candidate));
      candidate = [message];
    }
  }
  if (candidate.length) groups.push(toGroup(candidate));
  return groups;
}
