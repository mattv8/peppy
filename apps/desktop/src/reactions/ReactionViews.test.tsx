import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MessageView } from "@peppy/desktop-ui/bridge-contracts";
import { ReactionBadges, ReactionNotice } from "./ReactionViews";
import { linkReactions } from "./link";

afterEach(cleanup);

const message = (id: string, body: string, senderAddress = "Ada"): MessageView => ({
  id, body, revision: "1", sender: "other", senderAddress, timestamp: "1788435180000", attachments: [],
});
const target = message("target", "plan?");
const reaction = message("reaction", "Reacted 😆 to “plan?”");
const secondReaction = message("second", "Reacted 😆 to “plan?”", "Grace");
const linked = linkReactions([target, reaction]);
const badges = linked.badges.get(target.id)!;
const notice = linked.notices.get(reaction.id)!;
const actorName = (actor: { senderAddress?: string }) => actor.senderAddress ?? "You";

describe("ReactionBadges", () => {
  it("registers the actual chip under its target and emoji key without ref churn on count changes", () => {
    const chipRef = vi.fn();
    const props = { messageId: target.id, badges, actorName, chipRef };
    const { container, rerender, unmount } = render(<ReactionBadges {...props} />);
    const chip = container.querySelector('[data-reaction-emoji="😆"]');
    expect(chipRef.mock.calls).toEqual([['["target","😆"]', chip]]);
    const countedBadges = linkReactions([target, reaction, secondReaction]).badges.get(target.id)!;
    rerender(<ReactionBadges {...props} badges={countedBadges} />);
    expect(chip).toHaveTextContent("😆2");
    expect(chipRef).toHaveBeenCalledTimes(1);
    unmount();
    expect(chipRef.mock.calls).toEqual([['["target","😆"]', chip], ['["target","😆"]', null]]);
  });

  it("detaches the previous registration when the target or callback changes", () => {
    const chipRef = vi.fn();
    const replacementRef = vi.fn();
    const props = { messageId: target.id, badges, actorName, chipRef };
    const { container, rerender } = render(<ReactionBadges {...props} />);
    const chip = container.querySelector('[data-reaction-emoji="😆"]');
    rerender(<ReactionBadges {...props} messageId="new-target" />);
    expect(chipRef.mock.calls).toEqual([
      ['["target","😆"]', chip], ['["target","😆"]', null], ['["new-target","😆"]', chip],
    ]);
    rerender(<ReactionBadges {...props} messageId="new-target" chipRef={replacementRef} />);
    expect(chipRef).toHaveBeenLastCalledWith('["new-target","😆"]', null);
    expect(replacementRef).toHaveBeenLastCalledWith('["new-target","😆"]', chip);
    rerender(<ReactionBadges {...props} messageId="new-target" chipRef={replacementRef} badges={[]} />);
    expect(replacementRef).toHaveBeenLastCalledWith('["new-target","😆"]', null);
  });

  it("preserves label, title and count semantics without the optional callback", () => {
    const countedBadges = linkReactions([target, reaction, secondReaction]).badges.get(target.id)!;
    render(<ReactionBadges messageId={target.id} badges={countedBadges} actorName={actorName} />);
    const group = screen.getByRole("img", { name: "Reactions: 😆 from Ada, Grace" });
    expect(group).toHaveAttribute("title", "Reactions: 😆 from Ada, Grace");
    expect(group).toHaveTextContent("😆2");
  });
});

describe("ReactionNotice", () => {
  it("replaces a suppressed notice with only a hidden registry marker and restores notice state", () => {
    const rowRef = vi.fn();
    const onOpen = vi.fn();
    const props = { reaction, notice, actorName, rowRef, onOpen, meta: <time>12:00</time> };
    const { container, rerender, unmount } = render(<ReactionNotice {...props} />);
    const originalButton = screen.getByRole("button");
    fireEvent.click(originalButton);
    expect(onOpen).toHaveBeenCalledWith(reaction, notice);
    const suppressedProps = { ...props, suppressedHint: true };
    rerender(<ReactionNotice {...suppressedProps} />);
    const marker = container.querySelector('[data-reaction-marker="reaction"]');
    expect(marker).toHaveAttribute("data-message-id", "reaction");
    expect(marker).toHaveAttribute("hidden");
    expect(marker).not.toBeVisible();
    expect(marker).toBe(container.firstElementChild);
    expect(container.children).toHaveLength(1);
    expect(marker?.children).toHaveLength(0);
    expect(container.querySelector("button, .message-row, .message-bubble, time")).toBeNull();
    expect(rowRef.mock.calls.map(([node]) => node)).toEqual([originalButton, null, marker]);
    rerender(<ReactionNotice {...props} busy feedback="Finding original" />);
    const restoredButton = screen.getByRole("button");
    expect(container.querySelector("[data-reaction-marker]")).toBeNull();
    expect(restoredButton).toHaveAttribute("aria-busy", "true");
    expect(restoredButton).toHaveAttribute("aria-disabled", "true");
    expect(screen.getByRole("status")).toHaveTextContent("Finding original");
    expect(container.querySelector("time")).toHaveTextContent("12:00");
    fireEvent.click(restoredButton);
    expect(onOpen).toHaveBeenCalledTimes(1);
    expect(rowRef.mock.calls.slice(-2).map(([node]) => node)).toEqual([null, restoredButton]);
    unmount();
    expect(rowRef.mock.calls.at(-1)?.[0]).toBeNull();
  });
});
