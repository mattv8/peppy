import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ContactsView } from "./Contacts";
import { bridge, type ContactView } from "../bridge/bridge";
import { deferred, mockContactAurora, mockContactBook, mockContactRiver, resetContactBridgeMocks, restoreContactDomGlobals } from "./contacts-test-utils";

vi.mock("../bridge/bridge", { spy: true });

const PAGE_SIZE = 200;
const SORT = { key: "family", direction: "asc" };

/** Contacts `start` .. `start + count - 1`, named so tests can find them by text. */
function contactsFrom(start: number, count: number): ContactView[] {
  return Array.from({ length: count }, (_, index) => ({
    ...mockContactRiver,
    id: `c-${start + index}`,
    displayName: `Person ${start + index}`,
    givenName: undefined,
    familyName: undefined,
  }));
}

function bookWithCount(contactCount: number) {
  return [{ ...mockContactBook, contactCount }];
}

/** Serves `total` contacts in pages of the requested offset and size, the way the core does. */
function servePages(total: number) {
  vi.mocked(bridge.list_contacts).mockImplementation(async (_book, _query, offset = 0) => contactsFrom(offset, Math.max(0, Math.min(PAGE_SIZE, total - offset))));
}

function rows(): HTMLButtonElement[] {
  return Array.from(document.querySelectorAll<HTMLButtonElement>("#contacts-list button[data-contact-id]"));
}

type ObserverRecord = { callback: IntersectionObserverCallback; options?: IntersectionObserverInit };
let liveObservers: ObserverRecord[] = [];

/** Replaces IntersectionObserver with a stub; `revealSentinel` plays the role of the browser scrolling the sentinel in. */
function stubIntersectionObserver() {
  liveObservers = [];
  class SentinelObserver {
    private readonly record: ObserverRecord;
    constructor(callback: IntersectionObserverCallback, options?: IntersectionObserverInit) {
      this.record = { callback, options };
    }
    observe() { liveObservers.push(this.record); }
    disconnect() { liveObservers = liveObservers.filter(record => record !== this.record); }
  }
  vi.stubGlobal("IntersectionObserver", SentinelObserver);
}

/** Flushes pending effects first, so the sentinel observer for the rows on screen is registered. */
async function revealSentinel() {
  await act(async () => undefined);
  const record = liveObservers[liveObservers.length - 1];
  act(() => record?.callback([{ isIntersecting: true } as IntersectionObserverEntry], {} as IntersectionObserver));
}

describe("ContactsView paging", () => {
  beforeEach(() => {
    resetContactBridgeMocks();
    stubIntersectionObserver();
  });

  afterEach(() => {
    restoreContactDomGlobals();
  });

  it("shows a skeleton until the first page lands, then the rows and the final count", async () => {
    const first = deferred<ContactView[]>();
    vi.mocked(bridge.list_contacts).mockReturnValue(first.promise);
    const { container } = render(<ContactsView />);

    await waitFor(() => expect(bridge.list_contacts).toHaveBeenCalled());
    expect(container.querySelectorAll("#contacts-skeleton > li[data-contact-skeleton]")).toHaveLength(10);
    expect(container.querySelector("#contacts-skeleton")).toHaveAttribute("aria-hidden", "true");
    expect(document.getElementById("contacts-list")).toHaveAttribute("aria-busy", "true");
    expect(container.querySelector("#contacts-announcer")).toHaveTextContent("Loading contacts.");

    await act(async () => first.resolve(contactsFrom(0, 3)));

    expect(await screen.findByText("Person 0")).toBeInTheDocument();
    expect(container.querySelector("#contacts-skeleton")).toBeNull();
    expect(rows()).toHaveLength(3);
    expect(document.getElementById("contacts-list")).toHaveAttribute("aria-busy", "false");
    expect(document.getElementById("contacts-list")).toHaveAttribute("aria-label", "Contacts in Pixel 8");
    expect(container.querySelector("#contacts-announcer")).toHaveTextContent("3 of 3 contacts loaded.");
  });

  it("never renders the empty message while the first page is still loading", async () => {
    vi.mocked(bridge.list_contacts).mockReturnValue(new Promise(() => undefined));
    const { container } = render(<ContactsView />);

    await waitFor(() => expect(bridge.list_contacts).toHaveBeenCalled());
    expect(container.querySelector("#contacts-empty-state")).toBeNull();
    expect(container).not.toHaveTextContent("No contacts in");
  });

  it("appends the next page when the sentinel is revealed without moving the scroll position", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue(bookWithCount(250));
    servePages(250);
    render(<ContactsView />);
    await screen.findByText("Person 0");
    const list = document.getElementById("contacts-list")!;
    Object.defineProperty(list, "scrollTop", { configurable: true, writable: true, value: 120 });
    const sentinelOptions = liveObservers[liveObservers.length - 1]?.options;
    expect(sentinelOptions?.root).toBe(list);
    expect(sentinelOptions?.rootMargin).toBe("0px 0px 600px 0px");

    await revealSentinel();

    expect(await screen.findByText("Person 249")).toBeInTheDocument();
    expect(rows()).toHaveLength(250);
    expect(list.scrollTop).toBe(120);
    expect(bridge.list_contacts).toHaveBeenLastCalledWith("book-pixel8", "", PAGE_SIZE, SORT);
  });

  it("shows a loading footer while the next page is in flight", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue(bookWithCount(250));
    const second = deferred<ContactView[]>();
    vi.mocked(bridge.list_contacts).mockImplementation((_book, _query, offset = 0) => offset === 0 ? Promise.resolve(contactsFrom(0, PAGE_SIZE)) : second.promise);
    const { container } = render(<ContactsView />);
    await screen.findByText("Person 0");

    await revealSentinel();

    expect(await screen.findByText("Loading more contacts…")).toBeInTheDocument();
    expect(container.querySelector("#contacts-list-more")).toHaveAttribute("data-more-state", "loading");
    await act(async () => second.resolve(contactsFrom(PAGE_SIZE, 50)));
    await waitFor(() => expect(container.querySelector("#contacts-list-more")).toBeNull());
  });

  it("shows a load error with a persistent alert and retries the page on Try again", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue(bookWithCount(250));
    let attempts = 0;
    vi.mocked(bridge.list_contacts).mockImplementation(async (_book, _query, offset = 0) => {
      if (offset === 0) return contactsFrom(0, PAGE_SIZE);
      attempts += 1;
      if (attempts === 1) throw new Error("boom");
      return contactsFrom(PAGE_SIZE, 50);
    });
    const { container } = render(<ContactsView />);
    await screen.findByText("Person 0");

    await revealSentinel();

    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load more contacts.");
    expect(container.querySelector("#contacts-list-more")).toHaveAttribute("data-more-state", "error");
    expect(container.querySelector("#contacts-error")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));

    expect(await screen.findByText("Person 249")).toBeInTheDocument();
    expect(container.querySelector("#contacts-list-more")).toBeNull();
  });

  it("shows count progress only after the load has lasted 500 ms and the loaded count is a valid ratio", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true, toFake: ["setTimeout", "clearTimeout", "Date"] });
    try {
      vi.mocked(bridge.list_contact_books).mockResolvedValue(bookWithCount(401));
      const second = deferred<ContactView[]>();
      vi.mocked(bridge.list_contacts).mockImplementation((_book, _query, offset = 0) => offset === 0 ? Promise.resolve(contactsFrom(0, PAGE_SIZE)) : second.promise);
      const { container } = render(<ContactsView />);
      await act(async () => { await vi.advanceTimersByTimeAsync(0); });
      await screen.findByText("Person 0");

      await revealSentinel();
      act(() => { vi.advanceTimersByTime(499); });
      expect(container.querySelector("#contacts-count")).toHaveAttribute("data-count-state", "known");
      act(() => { vi.advanceTimersByTime(1); });

      expect(container.querySelector("#contacts-count")).toHaveAttribute("data-count-state", "progress");
      expect(container.querySelector("#contacts-count")).toHaveTextContent("200 of 401 contacts");
      expect(container.querySelector("#contacts-count-bar")).toHaveAttribute("max", "401");
      expect(container.querySelector("#contacts-count-bar")).toHaveAttribute("value", "200");
      await act(async () => second.resolve(contactsFrom(PAGE_SIZE, 50)));
      expect(container.querySelector("#contacts-count-bar")).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("shows no count progress past the book total, even while it checks for another page", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true, toFake: ["setTimeout", "clearTimeout", "Date"] });
    try {
      vi.mocked(bridge.list_contact_books).mockResolvedValue(bookWithCount(PAGE_SIZE));
      servePages(PAGE_SIZE);
      const { container } = render(<ContactsView />);
      await act(async () => { await vi.advanceTimersByTimeAsync(0); });
      await screen.findByText("Person 0");

      await revealSentinel();
      act(() => { vi.advanceTimersByTime(1000); });

      expect(container.querySelector("#contacts-count-bar")).toBeNull();
      expect(container.querySelector("#contacts-count")).toHaveAttribute("data-count-state", "known");
      expect(bridge.list_contacts).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("starts again from the first page with its scroll position at the top when the sort changes", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue(bookWithCount(250));
    servePages(250);
    render(<ContactsView />);
    await screen.findByText("Person 0");
    await revealSentinel();
    await screen.findByText("Person 249");
    const list = document.getElementById("contacts-list")!;
    Object.defineProperty(list, "scrollTop", { configurable: true, writable: true, value: 500 });

    fireEvent.click(screen.getByRole("button", { name: /^View options/ }));
    fireEvent.click(document.getElementById("contacts-sort-direction-desc")!);

    await waitFor(() => expect(bridge.list_contacts).toHaveBeenLastCalledWith("book-pixel8", "", 0, { key: "family", direction: "desc" }));
    await waitFor(() => expect(rows()).toHaveLength(PAGE_SIZE));
    expect(list.scrollTop).toBe(0);
  });

  it("keeps the loaded rows while a background refresh re-fetches them", async () => {
    let hint: (() => void) | undefined;
    vi.mocked(bridge.subscribe).mockImplementation(listener => { hint = listener; return () => undefined; });
    vi.mocked(bridge.list_contact_books).mockResolvedValue(bookWithCount(250));
    servePages(250);
    render(<ContactsView />);
    await screen.findByText("Person 0");
    await revealSentinel();
    await screen.findByText("Person 249");

    act(() => hint?.());

    await waitFor(() => expect(bridge.list_contacts).toHaveBeenCalledWith("book-pixel8", "", 0, SORT));
    await waitFor(() => expect(vi.mocked(bridge.list_contacts).mock.calls.filter(([, , offset]) => offset === PAGE_SIZE)).toHaveLength(2));
    expect(rows()).toHaveLength(250);
  });

  it("moves focus with a roving tab stop, loads at the end on ArrowDown, and focuses the first new row", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue(bookWithCount(250));
    servePages(250);
    const { container } = render(<ContactsView />);
    await screen.findByText("Person 0");
    expect(rows()[0]).toHaveAttribute("tabindex", "0");
    expect(rows()[1]).toHaveAttribute("tabindex", "-1");

    rows()[0].focus();
    fireEvent.keyDown(rows()[0], { key: "ArrowDown" });
    expect(document.activeElement).toBe(rows()[1]);
    fireEvent.keyDown(rows()[1], { key: "PageDown" });
    expect(document.activeElement).toBe(rows()[11]);
    fireEvent.keyDown(rows()[11], { key: "Home" });
    expect(document.activeElement).toBe(rows()[0]);
    fireEvent.keyDown(rows()[0], { key: "End" });
    expect(document.activeElement).toBe(rows()[PAGE_SIZE - 1]);

    fireEvent.keyDown(rows()[PAGE_SIZE - 1], { key: "ArrowDown" });

    await waitFor(() => expect(document.activeElement).toBe(document.querySelector('#contacts-list button[data-contact-id="c-200"]')));
    expect(container.querySelector("#contacts-announcer")).toHaveTextContent("250 of 250 contacts loaded.");
  });

  it("finds a target beyond the first page, keeps every page through it, and focuses its row", async () => {
    const scrollIntoView = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: scrollIntoView });
    vi.mocked(bridge.list_contact_books).mockResolvedValue(bookWithCount(401));
    const target = { ...mockContactAurora, id: "contact-400", displayName: "Person 400", givenName: undefined, familyName: undefined };
    vi.mocked(bridge.list_contacts).mockImplementation(async (_book, _query, offset = 0) => offset < 400 ? contactsFrom(offset, PAGE_SIZE) : [target]);
    const consumed = vi.fn();

    render(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-400", requestId: 90 }} onTargetConsumed={consumed} />);

    const targetRow = await screen.findByRole("button", { name: /^Person 400/ });
    await waitFor(() => expect(document.activeElement).toBe(targetRow));
    expect(consumed).toHaveBeenCalledWith(90);
    expect(rows()).toHaveLength(401);
    expect(scrollIntoView).toHaveBeenCalledWith({ block: "center" });
  });
});
