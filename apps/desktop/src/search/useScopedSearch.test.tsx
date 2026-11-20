import "@testing-library/jest-dom/vitest";
import { act, renderHook } from "@testing-library/react";
import { expect, it } from "vitest";
import { useScopedSearch } from "./useScopedSearch";

const options = { active: true, locked: false, label: "Contacts", placeholder: "Search contacts" };

it("offers a scope with the tab's label and placeholder while active and unlocked", () => {
  const { result } = renderHook(() => useScopedSearch(options));

  expect(result.current.scope).toMatchObject({ label: "Contacts", placeholder: "Search contacts", query: "" });
});

it("keeps the query in state and passes edits through the scope", () => {
  const { result } = renderHook(() => useScopedSearch(options));

  act(() => {
    const scope = result.current.scope;
    if (scope && scope.mode !== "results") scope.onQueryChange("ada");
  });

  expect(result.current.query).toBe("ada");
  expect(result.current.scope).toMatchObject({ query: "ada" });
});

it("offers a results scope without a controlled query", () => {
  const { result } = renderHook(() => useScopedSearch({ ...options, mode: "results" }));
  expect(result.current.scope).toMatchObject({ mode: "results" });
  expect(result.current.scope).not.toHaveProperty("query");
  act(() => result.current.scope!.onExit());
  expect(result.current.scope).toBeUndefined();
  act(() => result.current.resetSearch());
  expect(result.current.scope).toMatchObject({ mode: "results" });
});

it("exits to global search and clears the query until the search is reset", () => {
  const { result } = renderHook(() => useScopedSearch(options));
  act(() => result.current.setQuery("ada"));

  act(() => result.current.scope!.onExit());

  expect(result.current.scope).toBeUndefined();
  expect(result.current.query).toBe("");

  act(() => result.current.resetSearch());

  expect(result.current.scope).toMatchObject({ label: "Contacts", query: "" });
});

it("offers no scope while inactive or locked", () => {
  const { result, rerender } = renderHook(props => useScopedSearch(props), { initialProps: { ...options, active: false } });
  expect(result.current.scope).toBeUndefined();

  rerender({ ...options, active: true, locked: true });
  expect(result.current.scope).toBeUndefined();

  rerender({ ...options, active: true, locked: false });
  expect(result.current.scope).toMatchObject({ label: "Contacts" });
});

it("re-arms the scope on reset when the chip was exited and the query was left", () => {
  const { result } = renderHook(() => useScopedSearch(options));
  act(() => result.current.scope!.onExit());

  act(() => result.current.resetSearch());

  expect(result.current.scope).toBeDefined();
});
