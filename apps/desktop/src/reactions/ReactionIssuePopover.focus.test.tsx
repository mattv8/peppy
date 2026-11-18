import "@testing-library/jest-dom/vitest";
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ReactionIssuePopover } from "./ReactionIssuePopover";
import type { MergedReactionBadge } from "./merge";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const failedBadge: MergedReactionBadge = {
  emoji: "😂",
  actors: [{ key: "self", sender: "self" }],
  reactionIds: [],
  source: "native",
  own: { state: "not_sent", source: "native", requestId: "request-1", intent: "add" },
};

describe("ReactionIssuePopover placement", () => {
  it("flips above the chip when below it would cover the composer", () => {
    const anchor = document.createElement("span");
    document.body.appendChild(anchor);
    vi.spyOn(anchor, "getBoundingClientRect").mockReturnValue({ top: 80, bottom: 90, left: 40, right: 60, width: 20, height: 10, x: 40, y: 80, toJSON() {} });
    const composer = document.createElement("div");
    composer.id = "composer-area";
    document.body.appendChild(composer);
    vi.spyOn(composer, "getBoundingClientRect").mockReturnValue({ top: 100, bottom: 200, left: 0, right: 400, width: 400, height: 100, x: 0, y: 100, toJSON() {} });

    const { container } = render(<ReactionIssuePopover messageId="message-2" badge={failedBadge} anchor={anchor} onClose={() => undefined} />);

    // Below would start at 94 and end past the composer top (100 - 8); above is used instead, at 80 - 4 - height.
    expect(container.ownerDocument.querySelector<HTMLElement>(".reaction-issue")?.style.top).toBe("76px");
    anchor.remove();
    composer.remove();
  });

  it("focuses its first action only once the popover is visible, since hidden elements cannot take focus", () => {
    const anchor = document.createElement("span");
    document.body.appendChild(anchor);
    const visibilityAtFocus: string[] = [];
    const focus = HTMLElement.prototype.focus;
    vi.spyOn(HTMLElement.prototype, "focus").mockImplementation(function (this: HTMLElement, options?: FocusOptions) {
      if (this.hasAttribute("data-reaction-action")) visibilityAtFocus.push(this.closest<HTMLElement>(".reaction-issue")?.style.visibility ?? "");
      focus.call(this, options);
    });

    render(<ReactionIssuePopover messageId="message-1" badge={failedBadge} anchor={anchor} onClose={() => undefined} />);

    expect(visibilityAtFocus).toEqual([""]);
    expect(document.activeElement).toHaveAttribute("data-reaction-action", "retry");
    anchor.remove();
  });
});
