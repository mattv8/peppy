import { describe, expect, it, vi } from "vitest";
import { composerStatusItems } from "./composerStatusModel";

const input = () => ({
  groupLeft: false,
  rcsBlock: { kind: "none" as const },
  groupItems: [],
});

describe("composerStatusItems", () => {
  it("orders refusals before send blockers and gives the first blocker the unavailable hint id", () => {
    const items = composerStatusItems({
      ...input(),
      pendingRecipientReason: "finish adding the recipient before sending",
      groupItems: [{ key: "group-members-changed", tone: "error", message: "Not sent: group members changed.", actions: [{ id: "resend", label: "Resend", onAction: vi.fn() }] }],
      routeProblem: "choose a phone",
    });

    expect(items.map((item) => item.key)).toEqual(["group-members-changed", "pending-recipient", "route-problem"]);
    expect(items[1]).toMatchObject({ id: "unavailable-hint", message: "Finish adding the recipient before sending." });
  });

  it("uses pending recipient before the group-left send blocker", () => {
    const items = composerStatusItems({
      ...input(),
      pendingRecipientReason: "finish adding the recipient before sending",
      groupLeft: true,
      groupItems: [{ key: "group-left", tone: "warning", message: "You left this group." }],
    });

    expect(items.map((item) => item.key)).toEqual(["pending-recipient", "group-left"]);
    expect(items[0]).toMatchObject({ id: "unavailable-hint", message: "Finish adding the recipient before sending." });
  });

  it("formats main-window reasons after adding the suffix", () => {
    const [item] = composerStatusItems({ ...input(), routeProblem: "choose a phone and SIM", mainWindowRecovery: true });

    expect(item.message).toBe("Choose a phone and SIM in the main window.");
  });

  it("keeps attachment failures screen-reader only and deduplicates matching feedback", () => {
    const [attachment] = composerStatusItems({ ...input(), draftSendBlock: "a file in this draft can't be sent here", draftBlockKind: "attachment" });
    expect(attachment).toMatchObject({ key: "attachment-block", id: "unavailable-hint" });

    const [deduped] = composerStatusItems({ ...input(), routeProblem: "choose a phone", composerFeedback: { key: "draft", message: "Not sent: choose a phone." } });
    expect(deduped).toMatchObject({ srOnly: true, id: "unavailable-hint" });
  });

  it("uses the RCS status copy and data hook", () => {
    const [item] = composerStatusItems({ ...input(), rcsBlock: { kind: "link", help: "relink" } });

    expect(item).toMatchObject({ key: "rcs-relink", hooks: { "data-route-block": "link-relink" }, id: "unavailable-hint" });
  });

  it.each(["conflict", "failed-attachment"] as const)("keeps the %s send blocker available to the send button", (draftBlockKind) => {
    const [item] = composerStatusItems({ ...input(), draftSendBlock: "resolve the draft before sending", draftBlockKind });

    expect(item).toMatchObject({ key: draftBlockKind === "conflict" ? "draft-conflict" : "failed-attachment", id: "unavailable-hint", srOnly: true });
  });
});
