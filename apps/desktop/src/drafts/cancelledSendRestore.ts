import { MAX_DRAFT_ATTACHMENTS, type AttachmentView, type MessageView } from "@peppy/desktop-ui/bridge-contracts";
import type { DraftContent, DraftSlot } from "./DraftStore";

export type RestoredContent = Pick<DraftContent, "text" | "attachmentIds">;

export type CancelledSendRestore = {
  messageId: string;
  draftKey: string;
  sequence: number;
  generation: number;
  restored: RestoredContent;
  previous: RestoredContent;
};

export const CANCELLED_ATTACHMENTS_SKIPPED_NOTICE = "Some attachments couldn't be returned to the draft because they aren't on this device.";

type CancellationState = NonNullable<MessageView["sendCancellation"]>["state"];

const isRestorableAttachment = (attachment: AttachmentView) => attachment.localAvailable ?? attachment.state !== "pending";

export function restorableContent(message: MessageView): RestoredContent & { skippedAttachments: number } {
  const restorable = message.attachments.filter(isRestorableAttachment).slice(0, MAX_DRAFT_ATTACHMENTS);
  return {
    text: message.body,
    attachmentIds: restorable.map(attachment => attachment.id),
    skippedAttachments: message.attachments.length - restorable.length,
  };
}

export type RestoreSettlement = "wait" | "roll-back" | "clear";

const isSettledState = (state: CancellationState | undefined) =>
  state === "too-late" || state === "confirmed" || state === "unknown";

const sameRestoredContent = (left: RestoredContent, right: RestoredContent) =>
  left.text === right.text && left.attachmentIds.join("\u0000") === right.attachmentIds.join("\u0000");

export function settleCancelledSendRestore(
  record: CancelledSendRestore,
  state: CancellationState | undefined,
  slot: Pick<DraftSlot, "generation" | "content"> | undefined,
): RestoreSettlement {
  if (!slot) return isSettledState(state) ? "clear" : "wait";
  if (state === "too-late" && slot.generation === record.generation && sameRestoredContent(slot.content, record.restored)) return "roll-back";
  return isSettledState(state) ? "clear" : "wait";
}
