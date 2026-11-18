import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { REACTION_OPEN_DELAY_MS, useReactionTarget } from "./useReactionTarget";

/** A pointer event as a browser dispatches it: bubbling, with pointerType set. */
function pointer(type: string, target: Element, pointerType = "mouse") {
  target.dispatchEvent(Object.assign(new Event(type, { bubbles: true }), { pointerType }));
}

function contentWith(id: string, messageId: string): HTMLDivElement {
  const content = document.createElement("div");
  content.id = id;
  content.innerHTML = `<article data-message-id="${messageId}" data-reaction-ok="">Hello</article>`;
  document.body.appendChild(content);
  return content;
}

describe("useReactionTarget hover", () => {
  const barRef = { current: null };
  let previous: HTMLDivElement | undefined;

  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    previous?.remove();
    previous = undefined;
    vi.useRealTimers();
  });

  it("follows the content node when the list replaces it, so hover still opens the bar", () => {
    const first = contentWith("message-list-content", "message-a");
    previous = first;
    const { result, rerender } = renderHook(({ content }) => useReactionTarget(content, barRef, true), { initialProps: { content: first as HTMLElement | null } });

    // The list re-renders its content node while reactions stay enabled.
    first.remove();
    const second = contentWith("message-list-content", "message-b");
    previous = second;
    rerender({ content: second });

    act(() => pointer("pointerover", second.querySelector("article")!));
    act(() => { vi.advanceTimersByTime(REACTION_OPEN_DELAY_MS); });
    expect(result.current.target?.messageId).toBe("message-b");
  });

  it("does not open for touch hover, which reveals on tap instead", () => {
    const content = contentWith("message-list-content", "message-c");
    previous = content;
    const { result } = renderHook(() => useReactionTarget(content, barRef, true));
    act(() => pointer("pointerover", content.querySelector("article")!, "touch"));
    act(() => { vi.advanceTimersByTime(REACTION_OPEN_DELAY_MS); });
    expect(result.current.target).toBeUndefined();
  });
});
