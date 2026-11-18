import { useState, type KeyboardEvent } from "react";
import type { ReactionOrigin } from "./useReactionTarget";

/**
 * Keyboard path through the message list. The list has one tab stop: the last focused reactable article,
 * or the newest one by default. Arrow keys move between reactable articles; Enter or R opens the bar.
 */
export function useRovingMessages(reactableIds: readonly string[], reveal: (article: HTMLElement, origin: ReactionOrigin) => void) {
  const [chosen, setChosen] = useState<string>();
  const stopId = chosen && reactableIds.includes(chosen) ? chosen : reactableIds.at(-1);

  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    const target = event.target;
    if (!(target instanceof HTMLElement)) return;
    const list = event.currentTarget.querySelectorAll<HTMLElement>("article[data-reaction-ok]");
    const article = target.closest<HTMLElement>("[data-reaction-ok]");
    if (!article) return;
    const isArticle = target === article && article.tagName === "ARTICLE";
    if (isArticle && (event.key === "ArrowUp" || event.key === "ArrowDown" || event.key === "Home" || event.key === "End")) {
      const index = Array.from(list).indexOf(article);
      const next = event.key === "Home" ? 0
        : event.key === "End" ? list.length - 1
          : index + (event.key === "ArrowDown" ? 1 : -1);
      const destination = list[Math.max(0, Math.min(next, list.length - 1))];
      event.preventDefault();
      if (destination && destination !== article) {
        setChosen(destination.dataset.messageId);
        destination.focus();
      }
      return;
    }
    const opensBar = event.key === "r" || event.key === "R" || (event.key === "Enter" && isArticle);
    if (opensBar) {
      event.preventDefault();
      reveal(article, "keyboard");
    }
  };

  const onArticleFocus = (messageId: string | undefined) => setChosen(messageId);

  return { stopId, onKeyDown, onArticleFocus };
}
