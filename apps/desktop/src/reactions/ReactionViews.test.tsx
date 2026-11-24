import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MessageView } from "@peppy/desktop-ui/bridge-contracts";
import { ReactionBadges, ReactionNotice } from "./ReactionViews";
import { linkReactions, type ReactionBadgeInfo } from "./link";
import { mergeReactions, type MergedReactionBadge } from "./merge";

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

  it("preserves the full label as the group description, a title and counts", () => {
    const countedBadges = linkReactions([target, reaction, secondReaction]).badges.get(target.id)!;
    render(<ReactionBadges messageId={target.id} badges={countedBadges} actorName={actorName} />);
    const group = screen.getByRole("group", { name: "Reactions" });
    expect(group).toHaveAttribute("title", "Reactions: 😆 from Ada, Grace");
    expect(group).toHaveAccessibleDescription("Reactions: 😆 from Ada, Grace");
    expect(group).toHaveAttribute("data-reaction-badges", target.id);
    expect(screen.getByRole("img", { name: "😆 from Ada, Grace" })).toHaveTextContent("😆2");
  });

  it("renders no group for an empty badge list", () => {
    const { container } = render(<ReactionBadges messageId={target.id} badges={[]} actorName={actorName} />);
    expect(container).toBeEmptyDOMElement();
  });
});

describe("own reaction chips", () => {
  const mine = (overrides: Partial<MergedReactionBadge["own"] & object> & { emoji?: string } = {}) => {
    const { emoji = "❤️", ...own } = overrides;
    return { emoji, actors: [{ key: "self", sender: "self" as const }], reactionIds: [], source: own.source ?? "native", own: { state: "sent" as const, source: own.source ?? "native", ...own } } as MergedReactionBadge;
  };
  const renderOwn = (badges: MergedReactionBadge[], props: Partial<Parameters<typeof ReactionBadges>[0]> = {}) => render(<ReactionBadges messageId="m1" badges={badges} actorName={actorName} {...props} />);
  const ownChip = (container: HTMLElement) => container.querySelector<HTMLElement>("[data-reaction-own]")!;

  it("labels a confirmed native chip and makes it a button outside the tab order", () => {
    const onOwnChip = vi.fn();
    const { container } = renderOwn([mine()], { onOwnChip });
    const chip = screen.getByRole("button", { name: "❤️ from you" });
    expect(chip).toHaveAttribute("data-reaction-state", "sent");
    expect(chip).toHaveAttribute("data-reaction-source", "native");
    expect(chip).toHaveAttribute("tabindex", "-1");
    expect(ownChip(container)).toBe(chip);
    fireEvent.click(chip);
    expect(onOwnChip).toHaveBeenCalledWith(expect.objectContaining({ emoji: "❤️", own: expect.objectContaining({ state: "sent" }) }));
  });

  it("shows a pending chip with the sending label and no action", () => {
    const { container } = renderOwn([mine({ state: "sending", requestId: "r1", intent: "add" })]);
    const chip = screen.getByRole("button", { name: "Your ❤️ reaction is sending." });
    expect(chip).toHaveAttribute("data-reaction-state", "pending");
    expect(chip).toHaveAttribute("tabindex", "-1");
    expect(chip.querySelector("svg")).not.toBeNull();
    expect(container.querySelector('[data-reaction-action]')).toBeNull();
  });

  it("dims a removing chip and marks it busy", () => {
    renderOwn([mine({ emoji: "😂", state: "removing", requestId: "r2", intent: "remove", previousEmoji: "😂" })]);
    const chip = screen.getByRole("button", { name: "Removing your 😂 reaction." });
    expect(chip).toHaveAttribute("data-reaction-state", "removing");
    expect(chip).toHaveAttribute("aria-busy", "true");
  });

  it("makes a failed chip focusable and labels the failed switch with the earlier emoji", () => {
    renderOwn([mine({ state: "not_sent", requestId: "r3", intent: "switch", previousEmoji: "👍" })]);
    const chip = screen.getByRole("button", { name: "Couldn't change your reaction to ❤️. Your earlier 👍 is still in place." });
    expect(chip).toHaveAttribute("data-reaction-state", "failed");
    expect(chip).toHaveAttribute("tabindex", "0");
  });

  it("labels an unknown chip and keeps it focusable", () => {
    renderOwn([mine({ emoji: "😂", state: "unknown", requestId: "r4", intent: "add" })]);
    const chip = screen.getByRole("button", { name: "Not sure your 😂 reaction was sent. It won't be sent again on its own." });
    expect(chip).toHaveAttribute("data-reaction-state", "unknown");
    expect(chip).toHaveAttribute("tabindex", "0");
  });

  it("renders text-sourced own chips as read-only images", () => {
    const { container } = renderOwn([mine({ source: "text" })]);
    const chip = screen.getByRole("img", { name: "❤️ from you" });
    expect(chip).toHaveAttribute("data-reaction-source", "text");
    expect(chip).not.toHaveAttribute("tabindex");
    expect(container.querySelector("button")).toBeNull();
  });

  it("marks the group placement for media overlays", () => {
    const { container } = renderOwn([mine()], { placement: "overlay-bottom-start" });
    expect(container.querySelector('[data-reaction-placement="overlay-bottom-start"]')).toHaveClass("reaction-badges-overlay");
  });

  it("merges native and text badges into chips with their own states", () => {
    const parsed: ReactionBadgeInfo[] = [{ emoji: "😂", actors: [{ key: "other:+15550002", sender: "other", senderAddress: "+15550002" }], reactionIds: ["m9"] }];
    renderOwn(mergeReactions({ parsed, native: [{ emoji: "👍", actors: [{ self: false, address: "+15550001" }] }], ownRequest: { requestId: "r5", emoji: "❤️", state: "sending", previousEmoji: undefined } }));
    expect(screen.getByRole("img", { name: "👍 from +15550001" })).toHaveAttribute("data-reaction-source", "native");
    expect(screen.getByRole("img", { name: "😂 from +15550002" })).toHaveAttribute("data-reaction-source", "text");
    expect(screen.getByRole("button", { name: "Your ❤️ reaction is sending." })).toBeInTheDocument();
  });
});

describe("failed and unknown recovery", () => {
  const failed: MergedReactionBadge = { emoji: "❤️", actors: [{ key: "self", sender: "self" }], reactionIds: [], source: "native", own: { state: "not_sent", source: "native", requestId: "req-add", intent: "add" } };
  const unknown: MergedReactionBadge = { emoji: "😂", actors: [{ key: "self", sender: "self" }], reactionIds: [], source: "native", own: { state: "unknown", source: "native", requestId: "req-unknown", intent: "add" } };
  const renderIn = (badges: MergedReactionBadge[], props: Partial<Parameters<typeof ReactionBadges>[0]> = {}) => render(<article tabIndex={-1}>
    <ReactionBadges messageId="m1" badges={badges} actorName={actorName} {...props} />
  </article>);

  it("opens the failed popover focused on Try again and returns focus to the chip on Escape", () => {
    renderIn([failed]);
    const chip = screen.getByRole("button", { name: "Couldn't send your ❤️ reaction. Nothing was sent." });
    chip.focus();
    fireEvent.click(chip);
    const dialog = document.getElementById("reaction-issue-m1")!;
    expect(dialog).toHaveAttribute("data-reaction-issue", "failed");
    expect(screen.getByRole("dialog", { name: "Couldn't send your ❤️ reaction" })).toBe(dialog);
    expect(document.activeElement).toBe(dialog.querySelector('[data-reaction-action="retry"]'));
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    expect(document.getElementById("reaction-issue-m1")).toBeNull();
    expect(document.activeElement).toBe(chip);
  });

  it("retries a failed add with its emoji and removes the popover", () => {
    const onRetry = vi.fn();
    renderIn([failed], { onRetry });
    fireEvent.click(screen.getByRole("button", { name: "Couldn't send your ❤️ reaction. Nothing was sent." }));
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(onRetry).toHaveBeenCalledWith("req-add", "❤️");
    expect(document.getElementById("reaction-issue-m1")).toBeNull();
  });

  it("blocks retry but keeps local discard available while paused", () => {
    const onRetry = vi.fn();
    const onDismiss = vi.fn();
    renderIn([failed], { onRetry, onDismiss, writesPaused: true });
    fireEvent.click(screen.getByRole("button", { name: "Couldn't send your ❤️ reaction. Nothing was sent." }));
    const retry = screen.getByRole("button", { name: "Try again" });
    expect(retry).toHaveAttribute("aria-disabled", "true");
    fireEvent.click(retry);
    expect(onRetry).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    expect(onDismiss).toHaveBeenCalledWith("req-add");
  });

  it("discards a failed add and hands focus back to the message when the chip is gone", () => {
    const onDismiss = vi.fn();
    const { rerender } = renderIn([failed], { onDismiss });
    fireEvent.click(screen.getByRole("button", { name: "Couldn't send your ❤️ reaction. Nothing was sent." }));
    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    expect(onDismiss).toHaveBeenCalledWith("req-add");
    rerender(<article tabIndex={-1}><ReactionBadges messageId="m1" badges={[]} actorName={actorName} onDismiss={onDismiss} /></article>);
    expect(document.activeElement).toBe(document.querySelector("article"));
  });

  it("offers only Dismiss for an unknown attempt and resolves the popover when the chip goes", () => {
    const onDismiss = vi.fn();
    const { rerender } = renderIn([unknown], { onDismiss });
    fireEvent.click(screen.getByRole("button", { name: "Not sure your 😂 reaction was sent. It won't be sent again on its own." }));
    const dialog = screen.getByRole("dialog", { name: "Not sure your 😂 reaction was sent" });
    expect(dialog).toHaveAttribute("data-reaction-issue", "unknown");
    expect(within(dialog).getAllByRole("button").map((button) => button.textContent)).toEqual(["Dismiss"]);
    expect(dialog).toHaveTextContent("Your phone may have sent it. Check your phone.");
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(onDismiss).toHaveBeenCalledWith("req-unknown");
    rerender(<article><ReactionBadges messageId="m1" badges={[]} actorName={actorName} /></article>);
    expect(document.getElementById("reaction-issue-m1")).toBeNull();
  });

  it("closes the popover without moving focus when the request resolves by itself", () => {
    const { rerender } = renderIn([unknown]);
    fireEvent.click(screen.getByRole("button", { name: "Not sure your 😂 reaction was sent. It won't be sent again on its own." }));
    expect(document.getElementById("reaction-issue-m1")).not.toBeNull();
    rerender(<article><ReactionBadges messageId="m1" badges={[{ emoji: "😂", actors: [{ key: "self", sender: "self" }], reactionIds: [], source: "native", own: { state: "sent", source: "native" } }]} actorName={actorName} /></article>);
    expect(document.getElementById("reaction-issue-m1")).toBeNull();
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
