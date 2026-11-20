import { useMemo, useState } from "react";
import type { OmniSearchScope } from "@peppy/desktop-ui";

export type ScopedSearchOptions = {
  /** The tab that owns the search is showing. The scope exists only while it is. */
  active: boolean;
  /** Sync is locked; no scope is offered. */
  locked: boolean;
  /** Chip text, e.g. "Notifications". */
  label: string;
  placeholder: string;
};

/**
 * Query and removable title-bar scope of one tab. The chip starts armed; exiting it returns the search to
 * everything until `resetSearch` re-arms it (the caller calls it when the tab is left or re-entered).
 */
export function useScopedSearch({ active, locked, label, placeholder }: ScopedSearchOptions) {
  const [query, setQuery] = useState("");
  const [scoped, setScoped] = useState(true);
  const resetSearch = () => {
    setScoped(true);
    setQuery("");
  };
  const scope = useMemo<OmniSearchScope | undefined>(() => (
    active && scoped && !locked
      ? {
        label,
        placeholder,
        query,
        onQueryChange: setQuery,
        onExit: () => {
          setScoped(false);
          setQuery("");
        },
      }
      : undefined
  ), [active, label, locked, placeholder, query, scoped]);
  return { query, setQuery, scope, resetSearch };
}
