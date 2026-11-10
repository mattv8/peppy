import type { AttachmentView, MessageView } from "@peppy/desktop-ui/bridge-contracts";

/** Fixture-only RCS media: one message per incoming state and per outgoing send status, plus the two composite cases. */
const preview = "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==";

type AttachmentInput = Pick<AttachmentView, "id" | "name" | "mediaType"> & Partial<AttachmentView>;

function attachment({ id, name, mediaType, ...rest }: AttachmentInput): AttachmentView {
  const image = mediaType.startsWith("image/");
  return { id, name, mediaType, byteSize: image ? 67 : 182_000, state: "ready", previewUrl: image ? preview : undefined, ...rest };
}

const incoming = (id: string, timestamp: string, attachments: AttachmentView[], body = ""): MessageView =>
  ({ id, revision: "1", sender: "other", senderAddress: "+15555550142", body, timestamp, transport: "rcs", attachments });

const outgoing = (id: string, timestamp: string, status: MessageView["status"], attachments: AttachmentView[], body = ""): MessageView =>
  ({ id, revision: "1", sender: "self", body, timestamp, transport: "rcs", status, attachments });

/** Ten attachments in one incoming message: stored, every placeholder state, audio, and an unknown type. */
const mixedIncoming: AttachmentView[] = [
  attachment({ id: "rcs-in-mixed-1", name: "IMG_0101.jpg", mediaType: "image/jpeg", rcsState: "stored", index: 0 }),
  attachment({ id: "rcs-in-mixed-2", name: "IMG_0102.jpg", mediaType: "image/jpeg", state: "pending", rcsState: "downloading", index: 1 }),
  attachment({ id: "rcs-in-mixed-3", name: "RAW_0103.heic", mediaType: "image/heic", state: "pending", rcsState: "too_large", index: 2 }),
  attachment({ id: "rcs-in-mixed-4", name: "clip.mp4", mediaType: "video/mp4", state: "pending", rcsState: "quota_full", byteSize: 48_000_000, index: 3 }),
  attachment({ id: "rcs-in-mixed-5", name: "report.pdf", mediaType: "application/pdf", state: "pending", rcsState: "failed", index: 4 }),
  attachment({ id: "rcs-in-mixed-6", name: "old-scan.pdf", mediaType: "application/pdf", state: "pending", rcsState: "expired", index: 5 }),
  attachment({ id: "rcs-in-mixed-7", name: "voice-note.ogg", mediaType: "audio/ogg", rcsState: "stored", index: 6, mediaMeta: { durationMs: "12000" } }),
  attachment({ id: "rcs-in-mixed-8", name: "contact.vcf", mediaType: "text/vcard", rcsState: "stored", index: 7 }),
  attachment({ id: "rcs-in-mixed-9", name: "IMG_0109.jpg", mediaType: "image/jpeg", state: "pending", rcsState: "quota_full", index: 8 }),
  attachment({ id: "rcs-in-mixed-10", name: "IMG_0110.jpg", mediaType: "image/jpeg", rcsState: "stored", index: 9 }),
];

/** Outgoing partial failure: one sent, one refused by type, one with an unknown outcome. */
const partialOutgoing: AttachmentView[] = [
  attachment({ id: "rcs-out-partial-1", name: "photo.jpg", mediaType: "image/jpeg", rcsSendStatus: "sent", index: 0 }),
  attachment({ id: "rcs-out-partial-2", name: "notes.pdf", mediaType: "application/pdf", rcsSendStatus: "not_sent", error: "not_allowed_mime", index: 1 }),
  attachment({ id: "rcs-out-partial-3", name: "receipt.jpg", mediaType: "image/jpeg", rcsSendStatus: "outcome_unknown", index: 2 }),
];

const outgoingMixed: AttachmentView[] = Array.from({ length: 10 }, (_, index) => {
  const statuses = ["sent", "sent", "sending", "sent", "not_sent", "outcome_unknown", "sent", "members_changed", "sent", "sent"] as const;
  const status = statuses[index];
  return attachment({
    id: `rcs-out-mixed-${index + 1}`,
    name: index % 3 === 0 ? `clip-${index + 1}.mp4` : `IMG_02${index + 1}.jpg`,
    mediaType: index % 3 === 0 ? "video/mp4" : "image/jpeg",
    rcsSendStatus: status,
    error: status === "not_sent" ? "rejected" : undefined,
    index,
  });
});

export const fixtureRcsMediaMessages: MessageView[] = [
  incoming("message-rcs-in-stored", "1788434800000", [attachment({ id: "rcs-in-stored-image", name: "sunset.jpg", mediaType: "image/jpeg", rcsState: "stored", index: 0 })], "Stored photo"),
  incoming("message-rcs-in-downloading", "1788434810000", [attachment({ id: "rcs-in-downloading", name: "IMG_0201.jpg", mediaType: "image/jpeg", state: "pending", rcsState: "downloading", index: 0 })]),
  incoming("message-rcs-in-too-large", "1788434820000", [attachment({ id: "rcs-in-too-large", name: "panorama.jpg", mediaType: "image/jpeg", state: "pending", rcsState: "too_large", index: 0 })], "Caption survives a too-large photo"),
  incoming("message-rcs-in-quota-full", "1788434830000", [attachment({ id: "rcs-in-quota-full", name: "holiday-video.mp4", mediaType: "video/mp4", state: "pending", rcsState: "quota_full", byteSize: 48_000_000, index: 0 })], "Holiday clip"),
  incoming("message-rcs-in-failed", "1788434840000", [attachment({ id: "rcs-in-failed", name: "estimate.pdf", mediaType: "application/pdf", state: "pending", rcsState: "failed", index: 0 })]),
  incoming("message-rcs-in-expired", "1788434850000", [attachment({ id: "rcs-in-expired", name: "invoice.pdf", mediaType: "application/pdf", state: "pending", rcsState: "expired", index: 0 })]),
  incoming("message-rcs-in-voice", "1788434860000", [attachment({ id: "rcs-in-voice", name: "voice-note.ogg", mediaType: "audio/ogg", rcsState: "stored", index: 0, mediaMeta: { durationMs: "12000" } })]),
  incoming("message-rcs-in-unknown-type", "1788434870000", [attachment({ id: "rcs-in-unknown", name: "contact.vcf", mediaType: "text/vcard", rcsState: "stored", index: 0 })]),
  incoming("message-rcs-in-mixed-ten", "1788434880000", mixedIncoming, "Ten files from the trip"),
  outgoing("message-rcs-out-sending", "1788434900000", "gateway-persisted", [attachment({ id: "rcs-out-sending", name: "IMG_0301.jpg", mediaType: "image/jpeg", rcsSendStatus: "sending", index: 0 })], "Sending now"),
  outgoing("message-rcs-out-sent", "1788434910000", "delivery-confirmed", [attachment({ id: "rcs-out-sent", name: "IMG_0302.jpg", mediaType: "image/jpeg", rcsSendStatus: "sent", index: 0 })], "All sent"),
  outgoing("message-rcs-out-not-sent", "1788434920000", "failed-confirmed", [attachment({ id: "rcs-out-not-sent", name: "notes.pdf", mediaType: "application/pdf", rcsSendStatus: "not_sent", error: "not_allowed_mime", index: 0 })], "Notes attached"),
  outgoing("message-rcs-out-unknown", "1788434930000", "unknown", [attachment({ id: "rcs-out-unknown", name: "receipt.jpg", mediaType: "image/jpeg", rcsSendStatus: "outcome_unknown", index: 0 })]),
  outgoing("message-rcs-out-members-changed", "1788434940000", "failed-confirmed", [attachment({ id: "rcs-out-members", name: "IMG_0303.jpg", mediaType: "image/jpeg", rcsSendStatus: "members_changed", index: 0 })]),
  outgoing("message-rcs-out-partial", "1788434950000", "unknown", partialOutgoing, "Three files"),
  outgoing("message-rcs-out-mixed-ten", "1788434960000", "delivery-confirmed", outgoingMixed, "Ten attachments"),
];
