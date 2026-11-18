import { describe, expect, it } from "vitest";
import type { AttachmentView, MessageView } from "@peppy/desktop-ui/bridge-contracts";
import { MAX_DRAFT_ATTACHMENTS } from "@peppy/desktop-ui/bridge-contracts";
import { blankContent, type DraftSlot } from "./DraftStore";
import { type CancelledSendRestore, restorableContent, settleCancelledSendRestore } from "./cancelledSendRestore";

const attachment = (overrides: Partial<AttachmentView> & { id: string }): AttachmentView => ({
  name: "photo.jpg",
  mediaType: "image/jpeg",
  byteSize: 1,
  state: "ready",
  ...overrides,
});

const message = (overrides: Partial<MessageView> = {}): MessageView => ({
  id: "message-one",
  revision: "1",
  sender: "self",
  body: "Hello",
  timestamp: "0",
  attachments: [],
  ...overrides,
});

const record = (overrides: Partial<CancelledSendRestore> = {}): CancelledSendRestore => ({
  messageId: "message-one",
  draftKey: "conversation-one",
  sequence: 1,
  generation: 4,
  restored: { text: "Hello", attachmentIds: ["file-one", "file-two"] },
  previous: { text: "", attachmentIds: [] },
  ...overrides,
});

const slot = (overrides: { generation?: number; text?: string; attachmentIds?: string[] } = {}): Pick<DraftSlot, "generation" | "content"> => ({
  generation: overrides.generation ?? 4,
  content: { ...blankContent(), text: overrides.text ?? "Hello", attachmentIds: overrides.attachmentIds ?? ["file-one", "file-two"] },
});

describe("restorableContent", () => {
  it("restores the body and attachments in message order", () => {
    const restored = restorableContent(message({
      attachments: [attachment({ id: "file-one" }), attachment({ id: "file-two", state: "failed" })],
    }));

    expect(restored).toEqual({ text: "Hello", attachmentIds: ["file-one", "file-two"], skippedAttachments: 0 });
  });

  it("prefers localAvailable over the upload state", () => {
    const restored = restorableContent(message({
      attachments: [
        attachment({ id: "pending-local", state: "pending", localAvailable: true }),
        attachment({ id: "ready-remote", state: "ready", localAvailable: false }),
      ],
    }));

    expect(restored).toEqual({ text: "Hello", attachmentIds: ["pending-local"], skippedAttachments: 1 });
  });

  it("falls back to the upload state when localAvailable is absent", () => {
    const restored = restorableContent(message({
      attachments: [attachment({ id: "pending", state: "pending" }), attachment({ id: "uploading", state: "uploading" })],
    }));

    expect(restored).toEqual({ text: "Hello", attachmentIds: ["uploading"], skippedAttachments: 1 });
  });

  it("caps restored attachments and counts the overflow as skipped", () => {
    const attachments = Array.from({ length: MAX_DRAFT_ATTACHMENTS + 2 }, (_, index) => attachment({ id: `file-${index}` }));

    const restored = restorableContent(message({ attachments }));

    expect(restored.attachmentIds).toEqual(attachments.slice(0, MAX_DRAFT_ATTACHMENTS).map(item => item.id));
    expect(restored.skippedAttachments).toBe(2);
  });

  it("counts non-local and over-cap attachments together as skipped", () => {
    const attachments = [
      ...Array.from({ length: MAX_DRAFT_ATTACHMENTS }, (_, index) => attachment({ id: `file-${index}` })),
      attachment({ id: "pending", state: "pending" }),
      attachment({ id: "over-cap" }),
    ];

    const restored = restorableContent(message({ attachments }));

    expect(restored.attachmentIds).toHaveLength(MAX_DRAFT_ATTACHMENTS);
    expect(restored.skippedAttachments).toBe(2);
  });
});

describe("settleCancelledSendRestore", () => {
  it("rolls back only a too-late cancel whose draft is still exactly the restored content", () => {
    expect(settleCancelledSendRestore(record(), "too-late", slot())).toBe("roll-back");
  });

  it("clears a too-late cancel when the draft was edited after restore", () => {
    expect(settleCancelledSendRestore(record(), "too-late", slot({ generation: 5 }))).toBe("clear");
    expect(settleCancelledSendRestore(record(), "too-late", slot({ text: "Hello!" }))).toBe("clear");
    expect(settleCancelledSendRestore(record(), "too-late", slot({ attachmentIds: ["file-two", "file-one"] }))).toBe("clear");
    expect(settleCancelledSendRestore(record(), "too-late", slot({ attachmentIds: ["file-one"] }))).toBe("clear");
  });

  it.each(["confirmed", "unknown"] as const)("clears a %s cancel without touching the draft", state => {
    expect(settleCancelledSendRestore(record(), state, slot())).toBe("clear");
  });

  it.each(["none", "requested", undefined] as const)("keeps waiting while the state is %s", state => {
    expect(settleCancelledSendRestore(record(), state, slot())).toBe("wait");
  });

  it.each(["too-late", "confirmed", "unknown"] as const)("clears a %s cancel when the draft slot is missing", state => {
    expect(settleCancelledSendRestore(record(), state, undefined)).toBe("clear");
  });

  it.each(["none", "requested", undefined] as const)("keeps waiting for %s when the draft slot is missing", state => {
    expect(settleCancelledSendRestore(record(), state, undefined)).toBe("wait");
  });
});
