import { describe, expect, it } from "vitest";
import type { AttachmentView, MessageView, RcsAttachmentState, RcsSendStatus } from "@peppy/desktop-ui/bridge-contracts";
import { peppyCopy } from "../generated/peppyCopy";
import { canRetryRcsDownload, rcsAttachmentIndex, rcsDirection, rcsOutcomeStatus, rcsPlaceholderCopy, rcsPlaceholderState, rcsRetryBlockCopy, rcsRetryKey, rcsSendNeedsAttention, rcsSendStatusLabel } from "./rcsMediaState";

const attachment = (overrides: Partial<AttachmentView> = {}): AttachmentView => ({ id: "rcs-1", name: "photo.jpg", mediaType: "image/jpeg", byteSize: 10, state: "ready", ...overrides });
const message = (attachments: AttachmentView[], status?: MessageView["status"]): MessageView => ({ id: "message-1", revision: "1", sender: "self", body: "", timestamp: "1788434800000", transport: "rcs", status, attachments });

describe("rcsPlaceholderState", () => {
  it("maps every incoming state except stored to a placeholder", () => {
    const placeholders: RcsAttachmentState[] = ["downloading", "too_large", "quota_full", "failed", "expired"];
    for (const state of placeholders) expect(rcsPlaceholderState(attachment({ rcsState: state }))).toBe(state);
    expect(rcsPlaceholderState(attachment({ rcsState: "stored" }))).toBeUndefined();
    expect(rcsPlaceholderState(attachment())).toBeUndefined();
  });
});

describe("rcsDirection", () => {
  it("is incoming for receive state, outgoing for send status, and absent otherwise", () => {
    expect(rcsDirection(attachment({ rcsState: "stored" }))).toBe("incoming");
    expect(rcsDirection(attachment({ rcsSendStatus: "sent" }))).toBe("outgoing");
    expect(rcsDirection(attachment())).toBeUndefined();
  });
});

describe("canRetryRcsDownload", () => {
  it("offers a retry only for storage full and failed downloads", () => {
    expect(canRetryRcsDownload("quota_full")).toBe(true);
    expect(canRetryRcsDownload("failed")).toBe(true);
    for (const state of ["downloading", "too_large", "expired", "stored"] as const) expect(canRetryRcsDownload(state)).toBe(false);
    expect(canRetryRcsDownload(undefined)).toBe(false);
  });
});

describe("rcsPlaceholderCopy", () => {
  it("gives every placeholder a label, and detail only where the user can act", () => {
    expect(rcsPlaceholderCopy("downloading")).toEqual({ label: peppyCopy.rcs_media_state_downloading });
    expect(rcsPlaceholderCopy("too_large")).toEqual({ label: peppyCopy.rcs_media_state_too_large, detail: peppyCopy.rcs_media_detail_too_large.replace("{limit}", "32 MiB") });
    expect(rcsPlaceholderCopy("quota_full")).toEqual({ label: peppyCopy.rcs_media_state_quota_full, detail: peppyCopy.rcs_media_detail_quota_full });
    expect(rcsPlaceholderCopy("failed")).toEqual({ label: peppyCopy.rcs_media_state_failed, detail: peppyCopy.rcs_media_detail_failed });
    expect(rcsPlaceholderCopy("expired")).toEqual({ label: peppyCopy.rcs_media_state_expired });
  });
});

describe("rcsSendStatusLabel and rcsSendNeedsAttention", () => {
  it("labels every outgoing status and flags only the outcomes that need the user", () => {
    const labels: Record<RcsSendStatus, string> = {
      sending: peppyCopy.rcs_media_state_sending,
      sent: peppyCopy.rcs_media_state_sent,
      not_sent: peppyCopy.rcs_media_state_not_sent,
      send_failed: peppyCopy.rcs_media_state_send_failed,
      outcome_unknown: peppyCopy.rcs_media_state_unknown,
      members_changed: peppyCopy.rcs_media_state_not_sent,
    };
    for (const [status, label] of Object.entries(labels) as [RcsSendStatus, string][]) expect(rcsSendStatusLabel(status)).toBe(label);
    expect(rcsSendNeedsAttention("not_sent")).toBe(true);
    expect(rcsSendNeedsAttention("outcome_unknown")).toBe(true);
    expect(rcsSendNeedsAttention("members_changed")).toBe(true);
    expect(rcsSendNeedsAttention("send_failed" as RcsSendStatus)).toBe(true);
    expect(rcsSendNeedsAttention("sending")).toBe(false);
    expect(rcsSendNeedsAttention("sent")).toBe(false);
    expect(rcsSendNeedsAttention(undefined)).toBe(false);
  });

  it("treats definitive attempted failures as confirmed failures without replacing unresolved outcomes", () => {
    const sendFailed = attachment({ rcsSendStatus: "send_failed" as RcsSendStatus });
    const unknown = attachment({ id: "unknown", rcsSendStatus: "outcome_unknown" });

    expect(rcsSendStatusLabel(sendFailed.rcsSendStatus!)).toBe("Send failed");
    expect(rcsOutcomeStatus(message([sendFailed], "read")).status).toBe("failed-confirmed");
    expect(rcsOutcomeStatus(message([sendFailed, unknown], "read")).status).toBe("unknown");
  });
});

describe("rcsAttachmentIndex", () => {
  it("prefers the DTO index, including zero, and falls back to the position in the message", () => {
    const first = attachment({ id: "a", index: 0 });
    const second = attachment({ id: "b" });
    const value = message([first, second]);
    expect(rcsAttachmentIndex(value, first)).toBe(0);
    expect(rcsAttachmentIndex(value, second)).toBe(1);
    expect(rcsAttachmentIndex(value, attachment({ id: "b", index: 4 }))).toBe(4);
  });
});

describe("rcsOutcomeStatus", () => {
  it("shows the worst attachment outcome: unknown beats refused", () => {
    const sent = attachment({ id: "a", rcsSendStatus: "sent" });
    const refused = attachment({ id: "b", rcsSendStatus: "not_sent" });
    const unknown = attachment({ id: "c", rcsSendStatus: "outcome_unknown" });
    expect(rcsOutcomeStatus(message([sent, refused], "delivery-confirmed")).status).toBe("failed-confirmed");
    expect(rcsOutcomeStatus(message([sent, refused, unknown], "delivery-confirmed")).status).toBe("unknown");
  });

  it("reports members changed as a failure before submission", () => {
    expect(rcsOutcomeStatus(message([attachment({ rcsSendStatus: "members_changed" })], "sent")).status)
      .toBe("failed-before-submit");
  });

  it.each([
    ["sending", [attachment({ rcsSendStatus: "sending" })]],
    ["an unresolved attachment beside a sent attachment", [attachment({ id: "sent", rcsSendStatus: "sent" }), attachment({ id: "unresolved" })]],
  ])("does not report a successful message status while %s", (_label, attachments) => {
    for (const status of ["sent", "delivery-confirmed", "read"] as const) {
      expect(rcsOutcomeStatus(message(attachments, status)).status).toBe("submitted");
    }
  });

  it("preserves established delivery evidence only when every RCS attachment is sent", () => {
    const sent = attachment({ rcsSendStatus: "sent" });
    const delivered = message([sent], "delivery-confirmed");
    const read = message([sent], "read");
    const noMessageStatus = message([sent]);
    expect(rcsOutcomeStatus(delivered)).toBe(delivered);
    expect(rcsOutcomeStatus(read)).toBe(read);
    expect(rcsOutcomeStatus(noMessageStatus)).toBe(noMessageStatus);
  });

  it("leaves incoming and messages without RCS send states unchanged", () => {
    const incoming = message([attachment({ rcsState: "stored" })], "sent");
    const ordinary = message([attachment()], "delivery-confirmed");
    expect(rcsOutcomeStatus(incoming)).toBe(incoming);
    expect(rcsOutcomeStatus(ordinary)).toBe(ordinary);
  });
});

describe("rcsRetryBlockCopy and rcsRetryKey", () => {
  it("explains offline and link blocks and stays silent when the retry is allowed", () => {
    expect(rcsRetryBlockCopy({ kind: "offline" })).toBe(peppyCopy.rcs_media_retry_offline);
    expect(rcsRetryBlockCopy({ kind: "link" })).toBe(peppyCopy.rcs_media_retry_link);
    expect(rcsRetryBlockCopy({ kind: "none" })).toBeUndefined();
  });

  it("keys requests by message and attachment index", () => {
    expect(rcsRetryKey("message-a", 1)).not.toBe(rcsRetryKey("message-a", 2));
    expect(rcsRetryKey("message-a", 1)).not.toBe(rcsRetryKey("message-b", 1));
  });
});
