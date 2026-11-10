import { describe, expect, it, vi } from "vitest";
import type { DesktopSnapshot } from "@peppy/desktop-ui/bridge-contracts";
import { BrowserBridge } from "./browser-bridge";
import { normalizeGroupSnapshot, parseRcsAttachmentFields, parseSendResult } from "./group-payload";

class TestPort {
  public onmessage: ((event: MessageEvent) => void) | null = null;
  public onmessageerror: (() => void) | null = null;
  public readonly sent: unknown[] = [];
  public start = vi.fn();
  public close = vi.fn();
  public postMessage(message: unknown): void { this.sent.push(message); }
}

function bridgeWithPort(): { bridge: BrowserBridge; port: TestPort } {
  const port = new TestPort();
  return { bridge: new BrowserBridge(port as unknown as MessagePort), port };
}

const snapshotWith = (messages: unknown[]): DesktopSnapshot => ({ conversations: [{ id: "conv-1", name: "Ada", preview: "", unread: 0, messages }] } as unknown as DesktopSnapshot);

describe("retryRcsMediaDownload in the browser bridge", () => {
  it("sends the message and attachment index to the worker", () => {
    const { bridge, port } = bridgeWithPort();
    void bridge.retryRcsMediaDownload("message-rcs-1", 3);
    expect(JSON.stringify(port.sent)).toContain("retry_rcs_media_download");
    expect(JSON.stringify(port.sent)).toContain("message-rcs-1");
  });

  it.each([-1, 1.5, Number.NaN])("rejects the attachment index %s without calling the worker", async (index) => {
    const { bridge, port } = bridgeWithPort();
    await expect(bridge.retryRcsMediaDownload("message-rcs-1", index)).rejects.toThrow("Invalid attachment index.");
    expect(port.sent).toHaveLength(0);
  });

  it.each(["", undefined])("rejects a missing message ID (%s) without calling the worker", async (messageId) => {
    const { bridge, port } = bridgeWithPort();
    await expect(bridge.retryRcsMediaDownload(messageId as unknown as string, 0)).rejects.toThrow("Invalid message ID.");
    expect(port.sent).toHaveLength(0);
  });
});

describe("RCS attachment fields at the web boundary", () => {
  it("keeps every known rcsState, rcsSendStatus and a non-negative integer index", () => {
    expect(parseRcsAttachmentFields({ rcsState: "quota_full", rcsSendStatus: undefined, index: 4 })).toEqual({ rcsState: "quota_full", index: 4 });
    expect(parseRcsAttachmentFields({ rcsSendStatus: "outcome_unknown" })).toEqual({ rcsSendStatus: "outcome_unknown" });
  });

  it.each([
    ["rcsState", { rcsState: "pending" }],
    ["rcsSendStatus", { rcsSendStatus: "uploading" }],
    ["index", { index: -1 }],
    ["index", { index: 1.5 }],
    ["index", { index: "2" }],
  ])("drops a malformed %s field", (_field, value) => {
    expect(parseRcsAttachmentFields(value)).toEqual({});
  });

  it("normalizes every attachment in a snapshot and drops only the malformed field", () => {
    const attachment = { id: "a-1", name: "x.jpg", mediaType: "image/jpeg", byteSize: 1, state: "ready", rcsState: "bogus", rcsSendStatus: "sent", index: 0 };
    const normalized = normalizeGroupSnapshot(snapshotWith([{ id: "m-1", attachments: [attachment] }]));
    const [message] = normalized.conversations[0].messages;
    expect(message.attachments[0]).toEqual({ id: "a-1", name: "x.jpg", mediaType: "image/jpeg", byteSize: 1, state: "ready", rcsSendStatus: "sent", index: 0 });
  });
});

describe("refusedAttachmentIds in send results", () => {
  it("keeps string IDs and drops a malformed list without rejecting the result", () => {
    expect(parseSendResult({ accepted: false, status: "failed-before-submit", reason: "create_not_rcs", refusedAttachmentIds: ["a-1", "a-2"] }).refusedAttachmentIds).toEqual(["a-1", "a-2"]);
    const malformed = parseSendResult({ accepted: false, status: "failed-before-submit", refusedAttachmentIds: ["a-1", 2] });
    expect(malformed.accepted).toBe(false);
    expect(malformed).not.toHaveProperty("refusedAttachmentIds");
  });
});
