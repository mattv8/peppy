import { Search } from "@peppy/desktop-ui";
import "./search-no-match.css";

/**
 * Empty state for a scoped search that matches nothing, shared by every tab that has a scope chip.
 * `id` keeps the tab's own hook (e.g. `#notifications-no-match`); `noun` names the rows in the heading.
 */
export function SearchNoMatch({ id, noun, query, hint, onClearQuery }: {
  id: string;
  noun: string;
  query: string;
  hint: string;
  onClearQuery(): void;
}) {
  return <div id={id} className="search-no-match" data-empty-state="no-match">
    <Search size={32} aria-hidden />
    <h2 tabIndex={-1}>No {noun} match “{query}”</h2>
    <p>{hint}</p>
    <button className="secondary-button" onClick={onClearQuery}>Clear search</button>
  </div>;
}
