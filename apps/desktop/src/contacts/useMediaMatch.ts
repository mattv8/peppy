import { useEffect, useState } from "react";

// Same cutoffs as contacts.css: the book rail hides at 700px and the detail overlays the list at 560px.
export const COMPACT_BOOK_LIST_QUERY = "(max-width: 700px)";
export const COMPACT_DETAIL_QUERY = "(max-width: 560px)";

export function useMediaMatch(query: string) {
  const [matches, setMatches] = useState(() => window.matchMedia?.(query)?.matches ?? false);
  useEffect(() => {
    const media = window.matchMedia?.(query);
    if (!media) return;
    const update = () => setMatches(media.matches);
    update();
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, [query]);
  return matches;
}
