import { describe, expect, it } from "vitest";
import {
  SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION,
  attachmentRetentionNoticeCopySupported,
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
