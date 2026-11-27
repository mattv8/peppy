import "@testing-library/jest-dom/vitest";
import { createRef } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MessageView, ReactionRoute } from "@peppy/desktop-ui/bridge-contracts";
import { MessageReactionLayer } from "./ReactionLayer";
import type { ReactionTarget, ReactionTargetControls } from "./useReactionTarget";

// jsdom lays out nothing, so every rect is zero and the real placement would hide the bar.
vi.mock("./placement", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./placement")>()),
  placeReactionBar: () => ({ left: 16, top: 16, placement: "above" as const }),
}));

let list: HTMLElement;

beforeEach(() => {
  list = document.createElement("div");
  list.id = "message-list";
  const article = document.createElement("article");
  article.className = "message-bubble";
  article.dataset.messageId = "m1";
  list.append(article);
  document.body.append(list);
});

afterEach(() => {
  cleanup();
  list.remove();
});

function messageView(overrides: Partial<MessageView>): MessageView {
  return { id: "m1", sender: "other", transport: "rcs", reactable: "native", ...overrides } as MessageView;
}

function controls(target: ReactionTarget): ReactionTargetControls {
  return { target, selecting: false, reveal: vi.fn(), close: vi.fn(), keepOpen: vi.fn(), scheduleClose: vi.fn(), restoreFocus: vi.fn() };
}

type BarOptions = {
  route?: ReactionRoute;
  sender?: "self" | "other";
  writesPaused?: boolean;
  origin?: ReactionTarget["origin"];
  selected?: string;
  listWidth?: number;
  canReply?: boolean;
};

function renderBar({ route = "native", sender = "other", writesPaused = false, origin = "pointer", selected, listWidth = 771, canReply = true }: BarOptions = {}) {
  // The bar sizes itself from the list's width, so the list reports one.
  list.getBoundingClientRect = () => ({ top: 0, bottom: 800, left: 0, right: listWidth, width: listWidth, height: 800, x: 0, y: 0, toJSON() {} });
  const message = messageView({
    reactable: route,
    sender,
    reactions: selected ? [{ emoji: selected, actors: [{ self: true }] }] : undefined,
  } as Partial<MessageView>);
  const article = list.querySelector<HTMLElement>("article")!;
  const reaction = controls({ messageId: "m1", article, origin });
  const onReply = vi.fn();
  const onSubmit = vi.fn(async () => undefined);
  render(<MessageReactionLayer
    reaction={reaction}
    barRef={createRef<HTMLDivElement>()}
    messages={new Map([["m1", message]])}
    routeOffline={false}
    writesPaused={writesPaused}
    nameOf={() => "Alex"}
    announce={vi.fn()}
    isInFlight={() => false}
    onSubmit={onSubmit}
    onReply={onReply}
    canReply={canReply}
  />);
  return { reaction, onReply, onSubmit, bar: document.getElementById("message-reaction-bar")! };
}

describe("reaction bar Reply", () => {
  it("puts Reply first, then the divider and the quick emoji, on the native route", () => {
    const { bar } = renderBar();
    const [first, second, third] = Array.from(bar.children);
    expect(first).toHaveAttribute("data-reaction-reply");
    expect(first).toHaveAccessibleName("Reply to Alex");
    expect(first).toHaveTextContent("Reply");
    expect(second).toHaveAttribute("data-reaction-divider");
    expect(third).toHaveAttribute("data-reaction-quick", "thumbs-up");
    expect(bar.querySelector("[data-reaction-route-label]")).toBeNull();
  });

  it("puts Reply first on the text route, ahead of the passive Text tag", () => {
    const { bar } = renderBar({ route: "text" });
    const [first, second, third] = Array.from(bar.children);
    expect(first).toHaveAttribute("data-reaction-reply");
    expect(second).toHaveAttribute("data-reaction-divider");
    expect(third).toHaveAttribute("data-reaction-route-label", "text");
    expect(third.tagName).toBe("SPAN");
    expect(third).toHaveTextContent("Text");
  });

  it("names Reply for your own message", () => {
    renderBar({ sender: "self" });
    expect(screen.getByRole("button", { name: "Reply to your message" })).toHaveAttribute("data-reaction-reply");
  });

  it("labels the toolbar as reply or react, for each route and sender", () => {
    renderBar();
    expect(screen.getByRole("toolbar")).toHaveAttribute("aria-label", "Reply or react to message from Alex");
  });

  it("labels the toolbar for your own message and for the text route", () => {
    const { bar } = renderBar({ sender: "self" });
    expect(bar).toHaveAttribute("aria-label", "Reply or react to your message");
    bar.remove();
    cleanup();
    renderBar({ route: "text" });
    expect(screen.getByRole("toolbar")).toHaveAttribute("aria-label", "Reply or react with a text message");
  });

  it("labels the toolbar as react only when the conversation cannot take a reply", () => {
    renderBar({ canReply: false });
    expect(screen.getByRole("toolbar")).toHaveAttribute("aria-label", "React to message from Alex");
    cleanup();
    renderBar({ canReply: false, sender: "self" });
    expect(screen.getByRole("toolbar")).toHaveAttribute("aria-label", "React to your message");
    cleanup();
    renderBar({ canReply: false, route: "text" });
    expect(screen.getByRole("toolbar")).toHaveAttribute("aria-label", "React with a text message");
  });

  it("labels the toolbar as react only while the emoji capture field is open", () => {
    renderBar();
    fireEvent.click(screen.getByRole("button", { name: "More emoji" }));
    expect(screen.getByRole("toolbar")).toHaveAttribute("aria-label", "React to message from Alex");
  });

  it("keeps the Reply label visible when the list has room", () => {
    const { bar } = renderBar({ listWidth: 771 });
    expect(bar).not.toHaveAttribute("data-reply-compact");
    expect(screen.getByRole("button", { name: "Reply to Alex" })).toHaveTextContent("Reply");
  });

  it("shows Reply as an icon with its accessible name when the list is too narrow for the label", () => {
    const { bar } = renderBar({ route: "text", listWidth: 240 });
    const reply = screen.getByRole("button", { name: "Reply to Alex" });
    expect(bar).toHaveAttribute("data-reply-compact");
    expect(reply).not.toHaveTextContent("Reply");
    expect(reply).toHaveAttribute("title", "Reply");
  });

  it("leaves Reply and its divider out when the conversation is read-only", () => {
    const { bar } = renderBar({ canReply: false });
    const [first, second] = Array.from(bar.children);
    expect(bar.querySelector("[data-reaction-reply]")).toBeNull();
    expect(first).toHaveAttribute("data-reaction-quick", "thumbs-up");
    expect(second).not.toHaveAttribute("data-reaction-divider");
    expect(first).toHaveAttribute("tabindex", "0");
  });

  it("keyboard open focuses the first emoji when Reply is left out", () => {
    renderBar({ origin: "keyboard", canReply: false });
    expect(document.querySelector('[data-reaction-quick="thumbs-up"]')).toHaveFocus();
  });

  it("starts the tab stop on Reply and moves through it with the arrow keys and Home and End", () => {
    renderBar();
    const reply = screen.getByRole("button", { name: "Reply to Alex" });
    const thumbs = document.querySelector<HTMLElement>('[data-reaction-quick="thumbs-up"]')!;
    expect(reply).toHaveAttribute("tabindex", "0");
    expect(thumbs).toHaveAttribute("tabindex", "-1");

    reply.focus();
    fireEvent.keyDown(reply, { key: "ArrowRight" });
    expect(thumbs).toHaveFocus();
    expect(thumbs).toHaveAttribute("tabindex", "0");

    fireEvent.keyDown(thumbs, { key: "Home" });
    expect(reply).toHaveFocus();

    fireEvent.keyDown(reply, { key: "End" });
    expect(document.querySelector("[data-reaction-more]")).toHaveFocus();
  });

  it("keyboard open focuses Reply when no emoji is selected", () => {
    renderBar({ origin: "keyboard" });
    expect(screen.getByRole("button", { name: "Reply to Alex" })).toHaveFocus();
  });

  it("keyboard open focuses the selected emoji instead of Reply", () => {
    renderBar({ origin: "keyboard", selected: "😂" });
    expect(document.querySelector('[data-reaction-quick="laughing"]')).toHaveFocus();
    expect(screen.getByRole("button", { name: "Reply to Alex" })).toHaveAttribute("tabindex", "-1");
  });

  it.each(["native", "text"] as const)("calls onReply, then closes the bar, on the %s route", (route) => {
    const { reaction, onReply, onSubmit } = renderBar({ route });
    fireEvent.click(screen.getByRole("button", { name: "Reply to Alex" }));
    expect(onReply).toHaveBeenCalledTimes(1);
    expect(onReply).toHaveBeenCalledWith(expect.objectContaining({ id: "m1" }));
    expect(reaction.close).toHaveBeenCalled();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("blocks Reply while writes are paused", () => {
    const { reaction, onReply } = renderBar({ writesPaused: true });
    const reply = screen.getByRole("button", { name: "Reply to Alex" });
    expect(reply).toHaveAttribute("aria-disabled", "true");
    fireEvent.click(reply);
    expect(onReply).not.toHaveBeenCalled();
    expect(reaction.close).not.toHaveBeenCalled();
  });
});
