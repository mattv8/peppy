import { useCallback, useEffect, useRef, useState, type RefObject } from "react";

export type ReactionOrigin = "pointer" | "focus" | "touch" | "keyboard";
export type ReactionTarget = { messageId: string; article: HTMLElement; origin: ReactionOrigin };

/** Matches StatusPopover (open) and InlineStatus (close). */
export const REACTION_OPEN_DELAY_MS = 120;
export const REACTION_CLOSE_DELAY_MS = 150;
export const REACTION_LONG_PRESS_MS = 500;

/** Elements inside a bubble that keep their own click and must not reveal the bar on touch. */
const TEXT_CONTROL = "a, button, input, textarea, select, summary, label, [data-message-cancel], [data-one-time-code]";

export type ReactionTargetControls = {
  target: ReactionTarget | undefined;
  /** True while a pointer drag selects text; the bar hides until pointer-up. */
  selecting: boolean;
  reveal(article: HTMLElement, origin: ReactionOrigin): void;
  close(): void;
  keepOpen(): void;
  scheduleClose(): void;
  /** Moves focus back after a close without the focus-in path re-opening the bar. */
  restoreFocus(node: HTMLElement): void;
};

/** Reactable element for a DOM node: a message article, or a media slide. */
export function reactableOf(node: EventTarget | null): HTMLElement | null {
  return node instanceof Element ? node.closest<HTMLElement>("[data-reaction-ok]") : null;
}

export function reactionMessageId(article: HTMLElement): string | undefined {
  return article.dataset.reactionMessage ?? article.dataset.messageId;
}

/**
 * One delegated set of listeners on the message content: hover and focus reveal, touch tap and long-press,
 * and selection-drag hiding. The bar itself is rendered elsewhere and reports its hover through keepOpen and scheduleClose.
 */
export function useReactionTarget(
  /** The message content element itself, not a ref: listeners must follow the node, which can be replaced. */
  content: HTMLElement | null,
  barRef: RefObject<HTMLElement | null>,
  enabled: boolean,
): ReactionTargetControls {
  const [target, setTargetState] = useState<ReactionTarget>();
  const [selecting, setSelecting] = useState(false);
  const current = useRef<ReactionTarget>(undefined);
  const timers = useRef(new Map<string, number>());

  const setTarget = useCallback((next: ReactionTarget | undefined) => {
    current.current = next;
    setTargetState(next);
  }, []);

  const clearTimer = useCallback((name: string) => {
    window.clearTimeout(timers.current.get(name));
    timers.current.delete(name);
  }, []);

  const later = useCallback((name: string, ms: number, run: () => void) => {
    clearTimer(name);
    timers.current.set(name, window.setTimeout(() => {
      timers.current.delete(name);
      run();
    }, ms));
  }, [clearTimer]);

  const close = useCallback(() => {
    ["open", "close", "press"].forEach(clearTimer);
    setTarget(undefined);
  }, [clearTimer, setTarget]);

  const reveal = useCallback((article: HTMLElement, origin: ReactionOrigin) => {
    const messageId = reactionMessageId(article);
    if (!messageId) return;
    clearTimer("close");
    const previous = current.current;
    if (previous && previous.article === article && previous.origin === origin) return;
    setTarget({ messageId, article, origin });
  }, [clearTimer, setTarget]);

  const restoring = useRef(false);
  const restoreFocus = useCallback((node: HTMLElement) => {
    restoring.current = true;
    try {
      node.focus();
    } finally {
      restoring.current = false;
    }
  }, []);

  const keepOpen = useCallback(() => clearTimer("close"), [clearTimer]);
  const scheduleClose = useCallback(() => {
    later("close", REACTION_CLOSE_DELAY_MS, () => {
      if (current.current?.origin === "pointer" || current.current?.origin === "touch") setTarget(undefined);
    });
  }, [later, setTarget]);

  useEffect(() => {
    if (!enabled || !content) return undefined;
    const isInside = (node: EventTarget | null) => node instanceof Node && (content.contains(node) || Boolean(barRef.current?.contains(node)));
    let pointerType = "mouse";
    let modality: "pointer" | "keyboard" | undefined;
    let swallowClick = false;
    let pressedArticle: HTMLElement | null = null;

    const onDocumentPointerDown = () => { modality = "pointer"; };
    const onDocumentKeyDown = (event: KeyboardEvent) => {
      if (!["Meta", "Control", "Alt", "Shift", "CapsLock", "Fn"].includes(event.key)) modality = "keyboard";
    };
    const onPointerOver = (event: PointerEvent) => {
      pointerType = event.pointerType;
      if (event.pointerType === "touch") return;
      const article = reactableOf(event.target);
      if (!article || !content.contains(article)) return;
      const previous = current.current;
      if (previous?.article === article) { clearTimer("close"); return; }
      later("open", REACTION_OPEN_DELAY_MS, () => reveal(article, "pointer"));
    };
    const onPointerOut = (event: PointerEvent) => {
      if (event.pointerType === "touch") return;
      const article = reactableOf(event.target);
      if (!article) return;
      const next = event.relatedTarget;
      if (next instanceof Node && article.contains(next)) return;
      clearTimer("open");
      if (current.current?.article === article) scheduleClose();
    };
    const onPointerDown = (event: PointerEvent) => {
      pointerType = event.pointerType;
      const article = reactableOf(event.target);
      if (event.pointerType === "touch") {
        pressedArticle = article;
        if (article?.dataset.reactionMessage) {
          later("press", REACTION_LONG_PRESS_MS, () => {
            swallowClick = true;
            reveal(article, "touch");
          });
        }
        return;
      }
      if (article && !(event.target as Element).closest(TEXT_CONTROL)) {
        setSelecting(true);
        const release = () => {
          setSelecting(false);
          document.removeEventListener("pointerup", release);
        };
        document.addEventListener("pointerup", release);
      }
      if (current.current && !isInside(event.target)) close();
    };
    const onPointerMove = (event: PointerEvent) => {
      if (event.pointerType === "touch") clearTimer("press");
    };
    const onPointerUp = (event: PointerEvent) => {
      if (event.pointerType === "touch") clearTimer("press");
    };
    const onClick = (event: MouseEvent) => {
      if (swallowClick) {
        swallowClick = false;
        event.preventDefault();
        event.stopPropagation();
        return;
      }
      const article = reactableOf(event.target);
      if (!article || !content.contains(article)) return;
      // A screen-reader activation on plain text opens the bar and moves focus into it.
      if (event.detail === 0) {
        if ((event.target as Element).closest(TEXT_CONTROL)) return;
        reveal(article, "keyboard");
        return;
      }
      if (pointerType === "touch" && pressedArticle === article && article.classList.contains("message-bubble") && !(event.target as Element).closest(TEXT_CONTROL)) {
        reveal(article, "touch");
      }
    };
    const onFocusIn = (event: FocusEvent) => {
      if (restoring.current || modality === "pointer") return;
      const article = reactableOf(event.target);
      if (!article || !content.contains(article)) return;
      const control = event.target instanceof Element ? event.target.closest(TEXT_CONTROL) : null;
      if (control && control !== article && article.contains(control)) return;
      if (current.current?.origin === "keyboard" && current.current.article === article) return;
      reveal(article, "focus");
    };
    const onFocusOut = (event: FocusEvent) => {
      // No related element means window blur or the OS panel took focus: never a dismissal.
      if (!(event.relatedTarget instanceof Node) || isInside(event.relatedTarget)) return;
      if (current.current?.origin === "focus" || current.current?.origin === "keyboard") close();
    };
    const onScroll = () => {
      if (current.current?.origin === "touch") close();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && current.current) {
        event.preventDefault();
        const article = current.current.article;
        close();
        restoreFocus(article);
      }
    };

    document.addEventListener("pointerdown", onDocumentPointerDown, true);
    document.addEventListener("keydown", onDocumentKeyDown, true);
    content.addEventListener("pointerover", onPointerOver);
    content.addEventListener("pointerout", onPointerOut);
    content.addEventListener("pointerdown", onPointerDown);
    content.addEventListener("pointermove", onPointerMove);
    content.addEventListener("pointerup", onPointerUp);
    content.addEventListener("click", onClick, true);
    content.addEventListener("focusin", onFocusIn);
    content.addEventListener("focusout", onFocusOut);
    content.addEventListener("keydown", onKeyDown);
    content.addEventListener("scroll", onScroll);
    return () => {
      document.removeEventListener("pointerdown", onDocumentPointerDown, true);
      document.removeEventListener("keydown", onDocumentKeyDown, true);
      content.removeEventListener("pointerover", onPointerOver);
      content.removeEventListener("pointerout", onPointerOut);
      content.removeEventListener("pointerdown", onPointerDown);
      content.removeEventListener("pointermove", onPointerMove);
      content.removeEventListener("pointerup", onPointerUp);
      content.removeEventListener("click", onClick, true);
      content.removeEventListener("focusin", onFocusIn);
      content.removeEventListener("focusout", onFocusOut);
      content.removeEventListener("keydown", onKeyDown);
      content.removeEventListener("scroll", onScroll);
      ["open", "close", "press"].forEach(clearTimer);
    };
  }, [barRef, clearTimer, close, content, enabled, later, reveal, restoreFocus, scheduleClose, setTarget]);

  useEffect(() => {
    if (!enabled) setTarget(undefined);
  }, [enabled, setTarget]);

  return { target, selecting, reveal, close, keepOpen, scheduleClose, restoreFocus };
}
