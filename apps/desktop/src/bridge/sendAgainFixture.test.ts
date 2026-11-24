import { beforeEach, describe, expect, it, vi } from "vitest";
import type { MessageView, SendRecovery } from "@peppy/desktop-ui/bridge-contracts";

let fixtureBridge: typeof import("./bridge").fixtureBridge;
let addFixtureMessageForTests: typeof import("./bridge").addFixtureMessageForTests;

beforeEach(async () => {
  vi.resetModules();
  ({ fixtureBridge, addFixtureMessageForTests } = await import("./bridge"));
});

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

  it("sends again the fixture not-sent message and sets the top-level sentAgain on the original", async () => {
    await expect(fixtureBridge.send_again({ messageId: "fixture-resend-not-sent", gatewayId: "pixel", simId: "sim-1" })).resolves.toEqual({ messageId: "fixture-resend-not-sent-sent-again" });
    const messages = await conversationMessages("conv-resend");
    expect(messages.find((message) => message.id === "fixture-resend-not-sent")).toMatchObject({ sentAgain: { messageId: "fixture-resend-not-sent-sent-again" }, sendRecovery: { kind: "not-sent", canSendAgain: false, sentAgain: { messageId: "fixture-resend-not-sent-sent-again" } } });
  });
});
