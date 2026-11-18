import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { issueCopy, ReactionIssuePopover } from "./ReactionIssuePopover";
import type { MergedReactionBadge } from "./merge";

afterEach(cleanup);

const badge = (own: NonNullable<MergedReactionBadge["own"]>, emoji = "❤️"): MergedReactionBadge => ({ emoji, actors: [{ key: "self", sender: "self" }], reactionIds: [], source: "native", own });

describe("issueCopy", () => {
  it("uses the exact failed copy for an add, with Try again and Discard", () => {
    expect(issueCopy(badge({ state: "not_sent", source: "native", requestId: "r", intent: "add" }, "😂"))).toEqual({
      kind: "failed",
      title: "Couldn't send your 😂 reaction",
      body: "Your phone didn't get it, so nothing was sent.",
      actions: [{ action: "retry", label: "Try again", primary: true }, { action: "discard", label: "Discard" }],
    });
  });

  it("offers Try again and Keep it for a failed removal", () => {
    expect(issueCopy(badge({ state: "not_sent", source: "native", requestId: "r", intent: "remove" }, "😂"))).toMatchObject({
      title: "Couldn't remove your 😂 reaction",
      body: "Your reaction is still there.",
      actions: [{ action: "retry" }, { action: "keep", label: "Keep it" }],
    });
  });

  it("names the earlier emoji for a failed switch", () => {
    expect(issueCopy(badge({ state: "not_sent", source: "native", requestId: "r", intent: "switch", previousEmoji: "👍" }))).toMatchObject({
      title: "Couldn't change your reaction to ❤️",
      body: "Your earlier 👍 is still in place.",
    });
  });

  it("offers only Dismiss for every unknown attempt, with the matching body", () => {
    const add = issueCopy(badge({ state: "unknown", source: "native", requestId: "r", intent: "add" }, "😂"));
    expect(add).toMatchObject({ kind: "unknown", title: "Not sure your 😂 reaction was sent", actions: [{ action: "dismiss", label: "Dismiss" }] });
    expect(add?.body).toBe("Your phone may have sent it. Check your phone. Peppy won't send it again on its own. If your phone confirms it, this updates by itself.");
    expect(issueCopy(badge({ state: "unknown", source: "native", requestId: "r", intent: "remove" }, "😂"))?.body).toContain("Your phone may have removed it.");
    expect(issueCopy(badge({ state: "unknown", source: "native", requestId: "r", intent: "switch" }))?.title).toBe("Not sure your reaction changed to ❤️");
  });

  it("has no popover for chips that are not recoverable", () => {
    expect(issueCopy(badge({ state: "sent", source: "native" }))).toBeUndefined();
    expect(issueCopy(badge({ state: "sending", source: "native", requestId: "r" }))).toBeUndefined();
  });
});

describe("ReactionIssuePopover", () => {
  const anchor = document.createElement("button");
  const failed = badge({ state: "not_sent", source: "native", requestId: "req-1", intent: "add" });
  const setup = (overrides: Partial<Parameters<typeof ReactionIssuePopover>[0]> = {}) => {
    const onClose = vi.fn();
    const onRetry = vi.fn();
    const onDismiss = vi.fn();
    document.body.appendChild(anchor);
    render(<ReactionIssuePopover messageId="m1" badge={failed} anchor={anchor} onClose={onClose} onRetry={onRetry} onDismiss={onDismiss} {...overrides} />);
    return { onClose, onRetry, onDismiss };
  };

  it("portals to the body, focuses the first action and carries the hooks", () => {
    setup();
    const dialog = document.getElementById("reaction-issue-m1")!;
    expect(dialog.parentElement).toBe(document.body);
    expect(dialog).toHaveAttribute("data-reaction-issue", "failed");
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Try again" }));
  });

  it("reports Escape as a return to the chip and stops it reaching the message", () => {
    const { onClose } = setup();
    const parentKeys = vi.fn();
    document.addEventListener("keydown", parentKeys);
    fireEvent.keyDown(screen.getByRole("button", { name: "Try again" }), { key: "Escape" });
    expect(onClose).toHaveBeenCalledWith("chip");
    expect(parentKeys).not.toHaveBeenCalled();
    document.removeEventListener("keydown", parentKeys);
  });

  it("closes without focus when the pointer goes outside the popover and chip", () => {
    const { onClose } = setup();
    fireEvent.pointerDown(document.body);
    expect(onClose).toHaveBeenCalledWith("none");
  });

  it("passes a null emoji on retry of a failed removal and the request id on dismiss", () => {
    const removal = badge({ state: "not_sent", source: "native", requestId: "req-2", intent: "remove", previousEmoji: "😂" }, "😂");
    const { onRetry, onDismiss, onClose } = setup({ badge: removal });
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(onRetry).toHaveBeenCalledWith("req-2", null);
    expect(onClose).toHaveBeenLastCalledWith("article");
    expect(onDismiss).not.toHaveBeenCalled();
  });

  it("renders nothing for a chip without a recovery state", () => {
    setup({ badge: badge({ state: "sent", source: "native" }) });
    expect(document.getElementById("reaction-issue-m1")).toBeNull();
  });
});
