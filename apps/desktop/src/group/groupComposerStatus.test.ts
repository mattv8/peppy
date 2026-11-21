import { expect, it, vi } from "vitest";
import { groupComposerStatusItems } from "./groupComposerStatus";

it("keeps group resend visible but disabled while updates pause writes", () => {
  const items = groupComposerStatusItems({
    creating: false,
    notice: { kind: "members-changed", memberRevision: "1" },
    groupLeft: false,
    displayName: address => address,
    canResend: true,
    checking: false,
    writesPaused: true,
    onResend: vi.fn(),
    onReview: vi.fn(),
    onCreateAgain: vi.fn(),
    onStartNewMms: vi.fn(),
  });
  expect(items[0].actions?.find(action => action.id === "group-send-updated"))
    .toMatchObject({ disabled: true, describedBy: "update-paused-description" });
});
