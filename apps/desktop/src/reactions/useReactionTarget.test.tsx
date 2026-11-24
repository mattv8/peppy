import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { REACTION_CLOSE_DELAY_MS, REACTION_OPEN_DELAY_MS, useReactionTarget } from "./useReactionTarget";

/** A pointer event as a browser dispatches it: bubbling, with pointerType set. */
function pointer(type: string, target: Element, pointerType = "mouse") {
  target.dispatchEvent(Object.assign(new Event(type, { bubbles: true }), { pointerType }));
}

function contentWith(id: string, messageId: string): HTMLDivElement {
  const content = document.createElement("div");
  content.id = id;
  content.innerHTML = `<article data-message-id="${messageId}" data-reaction-ok="" tabindex="-1"><p>See <a href="https://example.com/">link</a></p></article>`;
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
    cleanup();
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

  it("keeps a hovered target pointer-origin after a mouse press and closes on leave", () => {
    const content = contentWith("message-list-content", "mouse");
    previous = content;
    const article = content.querySelector("article")!;
    const text = content.querySelector("p")!;
    const { result } = renderHook(() => useReactionTarget(content, barRef, true));
    act(() => pointer("pointerover", article));
    act(() => { vi.advanceTimersByTime(REACTION_OPEN_DELAY_MS); });
    act(() => {
      pointer("pointerdown", text);
      article.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
      pointer("pointerup", text);
      text.dispatchEvent(new MouseEvent("click", { bubbles: true, detail: 1 }));
    });
    expect(result.current.target?.origin).toBe("pointer");
    act(() => pointer("pointerout", article));
    act(() => { vi.advanceTimersByTime(REACTION_CLOSE_DELAY_MS); });
    expect(result.current.target).toBeUndefined();
  });

  it("does not reveal on a touch tap on a link", () => {
    const content = contentWith("message-list-content", "touch");
    previous = content;
    const article = content.querySelector("article")!;
    const link = content.querySelector("a")!;
    const { result } = renderHook(() => useReactionTarget(content, barRef, true));
    act(() => {
      pointer("pointerdown", link, "touch");
      article.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
      pointer("pointerup", link, "touch");
      link.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, detail: 1 }));
    });
    expect(result.current.target).toBeUndefined();
  });

  it("ignores keyboard activation of a link but reveals for plain text", () => {
    const content = contentWith("message-list-content", "activation");
    previous = content;
    const { result } = renderHook(() => useReactionTarget(content, barRef, true));
    act(() => {
      content.querySelector("a")!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, detail: 0 }));
    });
    expect(result.current.target).toBeUndefined();
    act(() => {
      content.querySelector("p")!.dispatchEvent(new MouseEvent("click", { bubbles: true, detail: 0 }));
    });
    expect(result.current.target?.origin).toBe("keyboard");
  });

  it("reveals on keyboard focus", () => {
    const content = contentWith("message-list-content", "keyboard");
    previous = content;
    const { result } = renderHook(() => useReactionTarget(content, barRef, true));
    act(() => {
      document.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, key: "Tab" }));
      content.querySelector("article")!.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    });
    expect(result.current.target?.origin).toBe("focus");
  });

  it("ignores keyboard focus on a nested link but reveals on the article", () => {
    const content = contentWith("message-list-content", "nested-focus");
    previous = content;
    const { result } = renderHook(() => useReactionTarget(content, barRef, true));
    act(() => {
      document.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, key: "Tab" }));
      content.querySelector("a")!.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    });
    expect(result.current.target).toBeUndefined();
    act(() => content.querySelector("article")!.dispatchEvent(new FocusEvent("focusin", { bubbles: true })));
    expect(result.current.target?.origin).toBe("focus");
  });

  it.each(["Meta", "Control", "Alt", "Shift", "CapsLock", "Fn"])("ignores %s after mouse focus", (key) => {
    const content = contentWith("message-list-content", "modifier");
    previous = content;
    const article = content.querySelector("article")!;
    const { result } = renderHook(() => useReactionTarget(content, barRef, true));
    act(() => {
      pointer("pointerdown", article);
      article.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
      document.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, key }));
      article.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    });
    expect(result.current.target).toBeUndefined();
  });

  it("restores keyboard focus modality after Tab following a mouse release outside content", () => {
    const content = contentWith("message-list-content", "outside");
    previous = content;
    const { result } = renderHook(() => useReactionTarget(content, barRef, true));
    act(() => {
      pointer("pointerdown", document.body);
      pointer("pointerup", document.body);
      document.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, key: "Tab" }));
      content.querySelector("article")!.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    });
    expect(result.current.target?.origin).toBe("focus");
  });
});
