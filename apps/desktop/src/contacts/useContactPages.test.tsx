import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { bridge, type ContactView } from "../bridge/bridge";
import { deferred, mockContactRiver, resetContactBridgeMocks, restoreContactDomGlobals } from "./contacts-test-utils";
import { useContactPages } from "./useContactPages";

vi.mock("../bridge/bridge", { spy: true });

const SORT = { key: "family", direction: "asc" } as const;

function contactsFrom(prefix: string, count: number): ContactView[] {
  return Array.from({ length: count }, (_, index) => ({ ...mockContactRiver, id: `${prefix}-${index}`, bookId: "book-pixel8" }));
}

function renderPages(initial: { bookId: string; sort?: typeof SORT }) {
  const onError = vi.fn();
  const view = renderHook((props: typeof initial) => useContactPages({ ...props, sort: props.sort ?? SORT, onError }), { initialProps: initial });
  return { ...view, onError };
}

describe("useContactPages", () => {
  beforeEach(() => {
    resetContactBridgeMocks();
  });

  afterEach(() => {
    restoreContactDomGlobals();
  });

  it("continues using the returned cursor and stops when none remains", async () => {
    vi.mocked(bridge.list_contacts_page).mockResolvedValueOnce({ contacts: contactsFrom("a", 200), total: 210, nextCursor: "next" }).mockResolvedValueOnce({ contacts: contactsFrom("b", 10), total: 210 });
    const { result } = renderPages({ bookId: "book-pixel8" });
    await waitFor(() => expect(result.current.contacts).toHaveLength(200));
    expect(result.current.hasMore).toBe(true);

    act(() => result.current.loadMore());
    await waitFor(() => expect(result.current.contacts).toHaveLength(210));
    expect(result.current.hasMore).toBe(false);
    expect(result.current.total).toBe(210);
    expect(bridge.list_contacts_page).toHaveBeenLastCalledWith("book-pixel8", { cursor: "next", sort: SORT });
  });

  it("drops loaded rows and starts again when the book changes, discarding the old book's response", async () => {
    const oldBook = deferred<{ contacts: ContactView[]; total: number }>();
    vi.mocked(bridge.list_contacts_page).mockReturnValueOnce(oldBook.promise).mockResolvedValueOnce({ contacts: contactsFrom("new", 2), total: 2 });
    const { result, rerender } = renderPages({ bookId: "book-old" });
    expect(result.current.loading).toBe(true);

    rerender({ bookId: "book-pixel8" });

    await waitFor(() => expect(result.current.contacts.map(contact => contact.id)).toEqual(["new-0", "new-1"]));
    await act(async () => oldBook.resolve({ contacts: contactsFrom("old", 5), total: 5 }));
    expect(result.current.contacts.map(contact => contact.id)).toEqual(["new-0", "new-1"]);
  });

  it("keeps the shown rows when a refresh fails and reports the error", async () => {
    let fail = false;
    vi.mocked(bridge.list_contacts_page).mockImplementation(async () => {
      if (fail) throw new Error("offline");
      return { contacts: contactsFrom("a", 3), total: 3 };
    });
    const { result, onError } = renderPages({ bookId: "book-pixel8" });
    await waitFor(() => expect(result.current.contacts).toHaveLength(3));

    fail = true;
    act(() => result.current.refresh());

    await waitFor(() => expect(onError.mock.lastCall?.[0]).toBeTruthy());
    expect(result.current.contacts).toHaveLength(3);
  });

  it("refreshes until the previously loaded count and swaps all pages at once", async () => {
    const second = deferred<{ contacts: ContactView[]; total: number }>();
    vi.mocked(bridge.list_contacts_page)
      .mockResolvedValueOnce({ contacts: contactsFrom("old", 3), total: 3 })
      .mockResolvedValueOnce({ contacts: contactsFrom("new", 1), total: 3, nextCursor: "next" })
      .mockReturnValueOnce(second.promise);
    const { result } = renderPages({ bookId: "book-pixel8" });
    await waitFor(() => expect(result.current.contacts).toHaveLength(3));
    act(() => result.current.refresh());
    await waitFor(() => expect(bridge.list_contacts_page).toHaveBeenLastCalledWith("book-pixel8", { cursor: "next", sort: SORT }));
    expect(result.current.contacts.map(contact => contact.id)).toEqual(["old-0", "old-1", "old-2"]);
    await act(async () => second.resolve({ contacts: contactsFrom("rest", 2), total: 3 }));
    expect(result.current.contacts.map(contact => contact.id)).toEqual(["new-0", "rest-0", "rest-1"]);
    expect(result.current.hasMore).toBe(false);
  });
});
