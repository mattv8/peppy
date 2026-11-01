import { describe, expect, it, vi } from "vitest";
import {
  SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION,
  attachmentRetentionNoticeCopySupported,
  readAccountAccess,
  type AccountAccessResult,
} from "./bridge-contracts";

describe("attachment retention bridge contracts", () => {
  it("acknowledges only the policy version with reviewed client copy", () => {
    expect(
      attachmentRetentionNoticeCopySupported(
        SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION,
      ),
    ).toBe(true);
    expect(
      attachmentRetentionNoticeCopySupported("hosted_attachment_retention_v2"),
    ).toBe(false);
    expect(attachmentRetentionNoticeCopySupported(null)).toBe(false);
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
