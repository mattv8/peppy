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

  it("coalesces refreshes requested during a refresh into one more refresh after it", async () => {
    const pending = deferred<{ contacts: ContactView[]; total: number }>();
    vi.mocked(bridge.list_contacts_page)
      .mockResolvedValueOnce({ contacts: contactsFrom("old", 3), total: 3 })
      .mockReturnValueOnce(pending.promise)
      .mockResolvedValueOnce({ contacts: contactsFrom("new", 3), total: 3 });
    const { result } = renderPages({ bookId: "book-pixel8" });
    await waitFor(() => expect(result.current.contacts).toHaveLength(3));

    act(() => result.current.refresh());
    act(() => result.current.refresh());
    act(() => result.current.refresh());
    expect(bridge.list_contacts_page).toHaveBeenCalledTimes(2);

    await act(async () => pending.resolve({ contacts: contactsFrom("first", 3), total: 3 }));
    await waitFor(() => expect(result.current.contacts.map(contact => contact.id)).toEqual(["new-0", "new-1", "new-2"]));
    expect(bridge.list_contacts_page).toHaveBeenCalledTimes(3);
  });

  it("re-fetches only the capped pages and keeps the rows loaded beyond the cap after the refreshed ones", async () => {
    const { result } = renderPages({ bookId: "book-pixel8" });
    // The mount fetch is superseded by the restored range below, so its response is never shown.
    vi.mocked(bridge.list_contacts_page).mockImplementation(async (_book, { cursor }) => {
      const index = cursor === undefined ? 0 : Number(cursor.slice(1));
      return { contacts: contactsFrom(`p${index}`, 200), total: 3000, nextCursor: `c${index + 1}` };
    });
    act(() => result.current.resetTo({ contacts: contactsFrom("old", 2200), total: 3000, nextCursor: "c-old" }, { bookId: "book-pixel8", sort: SORT }));
    vi.mocked(bridge.list_contacts_page).mockClear();

    act(() => result.current.refresh());

    await waitFor(() => expect(result.current.contacts[0]?.id).toBe("p0-0"));
    expect(bridge.list_contacts_page).toHaveBeenCalledTimes(10);
    expect(result.current.contacts).toHaveLength(2200);
    expect(result.current.contacts[1999]?.id).toBe("p9-199");
    expect(result.current.contacts[2000]?.id).toBe("old-2000");
    expect(result.current.contacts[2199]?.id).toBe("old-2199");
    expect(result.current.hasMore).toBe(true);

    act(() => result.current.loadMore());
    await waitFor(() => expect(bridge.list_contacts_page).toHaveBeenLastCalledWith("book-pixel8", { cursor: "c10", sort: SORT }));
  });
});
