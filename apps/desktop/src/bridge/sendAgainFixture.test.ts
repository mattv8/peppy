import { describe, expect, it } from "vitest";
import type { MessageView, SendRecovery } from "@peppy/desktop-ui/bridge-contracts";
import { addFixtureMessageForTests, fixtureBridge } from "./bridge";

const recoveryMessage = (id: string, sendRecovery: SendRecovery, overrides: Partial<MessageView> = {}): MessageView => ({
  id,
  revision: "1",
  sender: "self",
  body: "Meet at noon",
  timestamp: "1788434580000",
  status: "failed-before-submit",
  statusAt: "1788434640000",
  attachments: [],
  transport: "sms",
  participants: ["+12025550123", "+12025550199"],
  sendRecovery,
  ...overrides,
});

const conversationMessages = async (conversationId: string) => {
  const snapshot = await fixtureBridge.load_state(conversationId);
  return snapshot.conversations.find((conversation) => conversation.id === conversationId)?.messages ?? [];
};

describe("fixture send_again", () => {
  it("refuses an unknown message", async () => {
    await expect(fixtureBridge.send_again({ messageId: "missing", gatewayId: "pixel", simId: "sim-1" })).rejects.toMatchObject({ code: "not-found" });
  });

  it("refuses a message that is not in a recovery state", async () => {
    await expect(fixtureBridge.send_again({ messageId: "message-aurora-2", gatewayId: "pixel", simId: "sim-1" })).rejects.toMatchObject({ code: "invalid-request" });
  });

  it("refuses a recovery message that cannot be sent again, as the core does", async () => {
    addFixtureMessageForTests("conv-aurora", recoveryMessage("fixture-cancelled", { kind: "unconfirmed-removed-cancel", canSendAgain: false }));
    await expect(fixtureBridge.send_again({ messageId: "fixture-cancelled", gatewayId: "pixel", simId: "sim-1" })).rejects.toMatchObject({ code: "invalid-request" });
  });

  it("creates one new outgoing message with the same content, marks the original sent again, and refuses a second call", async () => {
    addFixtureMessageForTests("conv-aurora", recoveryMessage("fixture-not-sent", { kind: "not-sent", canSendAgain: true }));
    await expect(fixtureBridge.send_again({ messageId: "fixture-not-sent", gatewayId: "pixel", simId: "sim-1" })).resolves.toEqual({ messageId: "fixture-not-sent-sent-again" });
    const messages = await conversationMessages("conv-aurora");
    const original = messages.find((message) => message.id === "fixture-not-sent");
    const created = messages.find((message) => message.id === "fixture-not-sent-sent-again");
    expect(original?.sendRecovery).toEqual({ kind: "not-sent", canSendAgain: false, sentAgain: { messageId: "fixture-not-sent-sent-again" } });
    expect(original?.sentAgain).toEqual({ messageId: "fixture-not-sent-sent-again" });
    expect(created).toMatchObject({ sender: "self", body: "Meet at noon", status: "queued-local", transport: "sms", participants: ["+12025550123", "+12025550199"] });
    await expect(fixtureBridge.send_again({ messageId: "fixture-not-sent", gatewayId: "pixel", simId: "sim-1" })).rejects.toMatchObject({ code: "already-sent-again" });
  });
});

describe("send-again fixture states", () => {
  it("keeps the send-again link on the original as a top-level sentAgain for grouping", async () => {
    const messages = await conversationMessages("conv-resend");
    expect(messages.find((message) => message.id === "fixture-resend-pair-a")?.sentAgain).toEqual({ messageId: "fixture-resend-pair-b" });
    expect(messages.find((message) => message.id === "fixture-resend-triple-b")?.sentAgain).toEqual({ messageId: "fixture-resend-triple-c" });
    expect(messages.find((message) => message.id === "fixture-resend-unconfirmed")?.sentAgain).toBeUndefined();
  });

  it("has an ungrouped not-sent message that can be sent again", async () => {
    const messages = await conversationMessages("conv-resend");
    expect(messages.find((message) => message.id === "fixture-resend-not-sent")).toMatchObject({ sender: "self", status: "failed-before-submit", sendRecovery: { kind: "not-sent", canSendAgain: true } });
    expect(messages.find((message) => message.id === "fixture-resend-not-sent")?.sentAgain).toBeUndefined();
  });

  it("has an ungrouped unconfirmed-removed message that can be sent again", async () => {
    const messages = await conversationMessages("conv-resend");
    expect(messages.find((message) => message.id === "fixture-resend-unconfirmed")).toMatchObject({ sender: "self", status: "gateway-persisted", sendRecovery: { kind: "unconfirmed-removed", canSendAgain: true } });
  });

  it("has an adjacent two-attempt chain whose copy is server-accepted with the same body", async () => {
    const messages = await conversationMessages("conv-resend");
    const ids = messages.map((message) => message.id);
    const original = messages.find((message) => message.id === "fixture-resend-pair-a");
    const copy = messages.find((message) => message.id === "fixture-resend-pair-b");
    expect(ids.indexOf("fixture-resend-pair-b")).toBe(ids.indexOf("fixture-resend-pair-a") + 1);
    expect(original).toMatchObject({ sender: "self", status: "gateway-persisted", sendRecovery: { kind: "unconfirmed-removed", canSendAgain: false, sentAgain: { messageId: "fixture-resend-pair-b" } } });
    expect(copy).toMatchObject({ sender: "self", body: original?.body, status: "server-accepted" });
    expect(copy?.sendRecovery).toBeUndefined();
  });

  it("has an adjacent chain whose latest attempt is delivery-confirmed", async () => {
    const messages = await conversationMessages("conv-resend");
    const ids = messages.map((message) => message.id);
    expect(ids.indexOf("fixture-resend-delivered-b")).toBe(ids.indexOf("fixture-resend-delivered-a") + 1);
    expect(messages.find((message) => message.id === "fixture-resend-delivered-b")).toMatchObject({ status: "delivery-confirmed", statusAt: expect.any(String), body: "Table for four at 7?" });
  });

  it("has a three-attempt adjacent chain of not-sent attempts ending in a sendable latest", async () => {
    const messages = await conversationMessages("conv-resend");
    const ids = messages.map((message) => message.id);
    const [a, b, c] = ["fixture-resend-triple-a", "fixture-resend-triple-b", "fixture-resend-triple-c"];
    expect(ids.indexOf(b)).toBe(ids.indexOf(a) + 1);
    expect(ids.indexOf(c)).toBe(ids.indexOf(b) + 1);
    expect(messages.find((message) => message.id === a)).toMatchObject({ status: "gateway-persisted", sendRecovery: { kind: "unconfirmed-removed", canSendAgain: false }, sentAgain: { messageId: b } });
    expect(messages.find((message) => message.id === b)).toMatchObject({ status: "failed-before-submit", sendRecovery: { kind: "not-sent", canSendAgain: false }, sentAgain: { messageId: c } });
    expect(messages.find((message) => message.id === c)).toMatchObject({ status: "failed-before-submit", sendRecovery: { kind: "not-sent", canSendAgain: true } });
  });

  it("has a non-adjacent chain with an incoming reply between the original and its copy", async () => {
    const messages = await conversationMessages("conv-resend");
    const ids = messages.map((message) => message.id);
    expect(ids.indexOf("fixture-resend-gap-copy")).toBeGreaterThan(ids.indexOf("fixture-resend-gap-a") + 1);
    expect(messages.find((message) => message.id === "fixture-resend-gap-reply")).toMatchObject({ sender: "other" });
    expect(messages.find((message) => message.id === "fixture-resend-gap-a")?.sentAgain).toEqual({ messageId: "fixture-resend-gap-copy" });
    expect(messages.find((message) => message.id === "fixture-resend-gap-copy")).toMatchObject({ sender: "self", body: "Is the 3pm still on?", status: "server-accepted" });
  });

  it("sends again the fixture not-sent message and sets the top-level sentAgain on the original", async () => {
    await expect(fixtureBridge.send_again({ messageId: "fixture-resend-not-sent", gatewayId: "pixel", simId: "sim-1" })).resolves.toEqual({ messageId: "fixture-resend-not-sent-sent-again" });
    const messages = await conversationMessages("conv-resend");
    expect(messages.find((message) => message.id === "fixture-resend-not-sent")).toMatchObject({ sentAgain: { messageId: "fixture-resend-not-sent-sent-again" }, sendRecovery: { kind: "not-sent", canSendAgain: false, sentAgain: { messageId: "fixture-resend-not-sent-sent-again" } } });
  });
});
