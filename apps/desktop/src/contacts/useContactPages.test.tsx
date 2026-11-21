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

  it("has more rows only while the last page is full, even when the book count is stale", async () => {
    vi.mocked(bridge.list_contacts).mockResolvedValueOnce(contactsFrom("a", 200)).mockResolvedValueOnce(contactsFrom("b", 10));
    const { result } = renderPages({ bookId: "book-pixel8" });
    await waitFor(() => expect(result.current.contacts).toHaveLength(200));
    expect(result.current.hasMore).toBe(true);

    act(() => result.current.loadMore());
    await waitFor(() => expect(result.current.contacts).toHaveLength(210));
    expect(result.current.hasMore).toBe(false);
    expect(bridge.list_contacts).toHaveBeenLastCalledWith("book-pixel8", "", 200, SORT);
  });

  it("drops loaded rows and starts again when the book changes, discarding the old book's response", async () => {
    const oldBook = deferred<ContactView[]>();
    vi.mocked(bridge.list_contacts).mockReturnValueOnce(oldBook.promise).mockResolvedValueOnce(contactsFrom("new", 2));
    const { result, rerender } = renderPages({ bookId: "book-old" });
    expect(result.current.loading).toBe(true);

    rerender({ bookId: "book-pixel8" });

    await waitFor(() => expect(result.current.contacts.map(contact => contact.id)).toEqual(["new-0", "new-1"]));
    await act(async () => oldBook.resolve(contactsFrom("old", 5)));
    expect(result.current.contacts.map(contact => contact.id)).toEqual(["new-0", "new-1"]);
  });

  it("keeps the shown rows when a refresh fails and reports the error", async () => {
    let fail = false;
    vi.mocked(bridge.list_contacts).mockImplementation(async () => {
      if (fail) throw new Error("offline");
      return contactsFrom("a", 3);
    });
    const { result, onError } = renderPages({ bookId: "book-pixel8" });
    await waitFor(() => expect(result.current.contacts).toHaveLength(3));

    fail = true;
    act(() => result.current.refresh());

    await waitFor(() => expect(onError.mock.lastCall?.[0]).toBeTruthy());
    expect(result.current.contacts).toHaveLength(3);
  });
});
