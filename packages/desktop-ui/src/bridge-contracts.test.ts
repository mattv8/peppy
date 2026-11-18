import { describe, expect, it, vi } from "vitest";
import { notificationPersistence, readAccountAccess, type AccountAccessResult, type DesktopBridge, type MessageStatus, type MessageView, type SendCancellationView, type SendRecovery, type SendRecoveryKind } from "./bridge-contracts";

describe("message bridge contract for RCS", () => {
  it("accepts the read status and rcs transport and keeps rcsReported optional", () => {
    const status: MessageStatus = "read";
    const transport: MessageView["transport"] = "rcs";
    const message: MessageView = { id: "m", revision: "1", sender: "self", body: "", timestamp: "0", attachments: [], status, transport };
    expect(message.status).toBe("read");
    expect(message.transport).toBe("rcs");
    expect(message.rcsReported).toBeUndefined();
    expect(transport).toBe("rcs");
  });
});

describe("send cancellation bridge contract", () => {
  it("keeps cancellation projection optional for hosts that do not support it", () => {
    const cancellation: SendCancellationView = { state: "requested", canCancel: false, requiresGatewayUpdate: false };
    const message: MessageView = { id: "m", revision: "1", sender: "self", body: "", timestamp: "0", attachments: [], sendCancellation: cancellation };

    expect(message.sendCancellation).toEqual(cancellation);
  });
});

describe("send recovery bridge contract", () => {
  it("accepts the three recovery kinds, with canSendAgain required and sentAgain optional", () => {
    const kinds: SendRecoveryKind[] = ["not-sent", "unconfirmed-removed", "unconfirmed-removed-cancel"];
    for (const kind of kinds) {
      const recovery: SendRecovery = { kind, canSendAgain: kind !== "unconfirmed-removed-cancel" };
      const message: MessageView = { id: "m", revision: "1", sender: "self", body: "", timestamp: "0", attachments: [], sendRecovery: recovery };
      expect(message.sendRecovery).toEqual(recovery);
      expect(message.sendRecovery?.sentAgain).toBeUndefined();
    }
    const sent: SendRecovery = { kind: "not-sent", canSendAgain: false, sentAgain: { messageId: "m-again" } };
    expect(sent.sentAgain?.messageId).toBe("m-again");
  });

  it("declares send_again with the gateway and SIM route and a { messageId } result", () => {
    const send: DesktopBridge["send_again"] = async (input) => ({ messageId: `${input.messageId}-again` });
    expect(send).toBeTypeOf("function");
  });
});

describe("account access bridge contract", () => {
  const notice: AccountAccessResult = {
    kind: "notice",
    notice: {
      kind: "read_only_past_due",
      severity: "error",
      deadline: null,
      manageUrl: "https://peppy.pro/account",
      dismissible: false,
      snoozeKey: "read_only_past_due:none",
    },
  };

  it("treats a bridge without the method as unsupported", async () => {
    await expect(readAccountAccess({})).resolves.toEqual({ kind: "unsupported" });
  });

  it("passes through each typed host result", async () => {
    for (const result of [notice, { kind: "none" }, { kind: "unsupported" }, { kind: "temporarily_unavailable" }] as const) {
      const account_access = vi.fn().mockResolvedValue(result);
      await expect(readAccountAccess({ account_access })).resolves.toEqual(result);
    }
  });

  it("never turns a thrown host error into a notice", async () => {
    const account_access = vi.fn().mockRejectedValue(new Error("offline"));
    await expect(readAccountAccess({ account_access })).resolves.toEqual({ kind: "temporarily_unavailable" });
  });
});

describe("notification persistence bridge contract", () => {
  const cases: Array<{ input: unknown; expected?: { seconds: number | null } }> = [
    { input: { seconds: null }, expected: { seconds: null } },
    { input: { seconds: 1 }, expected: { seconds: 1 } },
    { input: { seconds: 5.0 }, expected: { seconds: 5 } },
    { input: { seconds: 5.5 } },
    { input: { seconds: 3600 }, expected: { seconds: 3600 } },
    { input: { seconds: 0 } },
    { input: { seconds: 3601 } },
    { input: { seconds: -1 } },
    { input: { seconds: -0 } },
    { input: { seconds: Number.NaN } },
    { input: { seconds: Number.POSITIVE_INFINITY } },
    { input: { seconds: Number.NEGATIVE_INFINITY } },
    { input: { seconds: "5" } },
    { input: { seconds: true } },
    { input: { seconds: undefined } },
    { input: {} },
    { input: { seconds: 5, extra: 1 } },
    { input: { seconds: null, extra: 1 } },
    { input: { extra: 1 } },
    { input: [] },
    { input: null },
    { input: undefined },
    { input: 5 },
  ];

  it.each(cases)("validates $input at the persistence boundary", ({ input, expected }) => {
    const result = notificationPersistence(input);
    expect(result).toEqual(expected);
    if (expected) expect(result).not.toBe(input);
  });
});
