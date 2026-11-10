import { describe, expect, it } from "vitest";
import type { AttachmentView, MessageView } from "@peppy/desktop-ui/bridge-contracts";
import { fixtureBridge } from "./bridge";
import { fixtureRcsMediaMessages } from "./rcsMediaFixtures";

const attachments = (): AttachmentView[] => fixtureRcsMediaMessages.flatMap((message) => message.attachments);
const message = (id: string): MessageView => fixtureRcsMediaMessages.find((candidate) => candidate.id === id)!;

describe("RCS media fixtures", () => {
  it("covers every incoming state, including stored", () => {
    const states = new Set(attachments().map((item) => item.rcsState));
    for (const state of ["downloading", "stored", "too_large", "quota_full", "failed", "expired"]) expect(states).toContain(state);
  });

  it("covers every outgoing send status", () => {
    const statuses = new Set(attachments().map((item) => item.rcsSendStatus));
    for (const status of ["sending", "sent", "not_sent", "outcome_unknown", "members_changed"]) expect(statuses).toContain(status);
  });

  it("includes a ten-attachment incoming message with mixed states, where every index is its position", () => {
    const mixed = message("message-rcs-in-mixed-ten");
    expect(mixed.attachments).toHaveLength(10);
    expect(new Set(mixed.attachments.map((item) => item.rcsState)).size).toBeGreaterThanOrEqual(5);
    expect(mixed.attachments.map((item) => item.index)).toEqual([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    expect(mixed.attachments.some((item) => item.mediaType.startsWith("audio/"))).toBe(true);
    expect(mixed.attachments.some((item) => item.mediaType === "text/vcard")).toBe(true);
  });

  it("includes a partial-failure outgoing message with a sent, a refused and an unknown attachment", () => {
    const partial = message("message-rcs-out-partial");
    expect(partial.attachments.map((item) => item.rcsSendStatus)).toEqual(["sent", "not_sent", "outcome_unknown"]);
    expect(partial.attachments[1].error).toBe("not_allowed_mime");
  });

  it("includes a ten-attachment outgoing message with mixed send statuses", () => {
    const mixed = message("message-rcs-out-mixed-ten");
    expect(mixed.attachments).toHaveLength(10);
    expect(new Set(mixed.attachments.map((item) => item.rcsSendStatus)).size).toBeGreaterThanOrEqual(4);
  });

  it("answers the retry command in the fixture bridge", async () => {
    await expect(fixtureBridge.retryRcsMediaDownload("message-rcs-in-quota-full", 0)).resolves.toBeUndefined();
  });
});
