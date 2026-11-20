import "@testing-library/jest-dom/vitest";
import { useState } from "react";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { bridge, type ContactBookView } from "../bridge/bridge";
import { ContactsView } from "./Contacts";
import { BookRail } from "./BookRail";
import type { ContactActionNotice } from "./contact-action-notice";
import { deferred, mockContactBook, resetContactBridgeMocks, restoreContactDomGlobals } from "./contacts-test-utils";
import { RemovedBookStrip } from "./RemovedBookStrip";

vi.mock("../bridge/bridge", { spy: true });

const otherBook: ContactBookView = { ...mockContactBook, id: "book-other", deviceName: "Other" };
const removedBook: ContactBookView = { ...mockContactBook, id: "book-iphone", deviceName: "iPhone", state: "retired", sourceRevoked: true, contactCount: 803 };

function renderRail(books: ContactBookView[]) {
  const onSelect = vi.fn();
  const onBooksChanged = vi.fn();
  const onRemoveRequest = vi.fn();
  const onDeleteRequest = vi.fn();
  const onNotice = vi.fn();
  render(<BookRail books={books} bookId={books[0].id} width={220} panelRef={{ current: null }} onSelect={onSelect} onBooksChanged={onBooksChanged} onRemoveRequest={onRemoveRequest} onDeleteRequest={onDeleteRequest} onNotice={onNotice} />);
  return { onSelect, onBooksChanged, onRemoveRequest, onNotice };
}

/** Renders the last notice's message so tests can assert its copy; the toast lane owns the visible surface. */
function lastNotice(onNotice: Mock) {
  const notice = onNotice.mock.lastCall?.[0] as ContactActionNotice;
  return { notice, text: render(<>{notice.message}</>).container.textContent };
}

const rowSwitch = (bookId: string) => document.getElementById(`contacts-book-use-switch-${bookId}`) as HTMLInputElement;

describe("removed contact book", () => {
  beforeEach(() => resetContactBridgeMocks());
  afterEach(() => restoreContactDomGlobals());

  const removalDialog = () => document.getElementById("contacts-book-remove-dialog");

  it("passes the book and its strip button to onRemoveRequest", () => {
    const onRemoveRequest = vi.fn();
    render(<RemovedBookStrip book={mockContactBook} onRemoveRequest={onRemoveRequest} onDeleteRequest={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Remove from this computer" }));
    expect(onRemoveRequest).toHaveBeenCalledWith(mockContactBook, expect.objectContaining({ current: expect.any(HTMLButtonElement) }));
  });

  it("confirms local removal before forgetting the book", async () => {
    let books = [removedBook, mockContactBook];
    vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
    vi.mocked(bridge.forget_contact_book).mockImplementation(async id => {
      books = books.filter(item => item.id !== id);
    });
    render(<ContactsView />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove from this computer" }));
    expect(bridge.forget_contact_book).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("dialog").querySelector<HTMLButtonElement>(".destructive-button")!);
    await waitFor(() => expect(bridge.forget_contact_book).toHaveBeenCalledWith("book-iphone"));
    await waitFor(() => expect(removalDialog()).not.toBeInTheDocument());
    await waitFor(() => expect(document.querySelector("#contacts-center-pane")).toHaveTextContent("Removed iPhone from this computer."));
  });

  it("keeps the dialog open on failure", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    vi.mocked(bridge.forget_contact_book).mockRejectedValue(new Error("failed"));
    render(<ContactsView />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove from this computer" }));
    fireEvent.click(screen.getByRole("dialog").querySelector(".destructive-button")!);
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't remove this book");
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  it("blocks dismissal while removal is in flight", async () => {
    const pending = deferred<void>();
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    vi.mocked(bridge.forget_contact_book).mockReturnValue(pending.promise);
    render(<ContactsView />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove from this computer" }));
    fireEvent.click(screen.getByRole("dialog").querySelector(".destructive-button")!);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled();
    pending.resolve();
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("stays open when an error strip replaces the removed strip in the status slot", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    vi.mocked(bridge.list_restorable_contacts).mockRejectedValue(new Error("offline"));
    render(<ContactsView />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove from this computer" }));
    expect(removalDialog()).toBeInTheDocument();

    fireEvent.click(document.querySelector<HTMLButtonElement>('button[aria-label="Recently deleted contacts"]')!);
    await waitFor(() => expect(document.getElementById("contacts-status")).toHaveAttribute("data-status-kind", "error"));
    expect(document.getElementById("contacts-book-removed")).not.toBeInTheDocument();
    expect(removalDialog()).toBeInTheDocument();
    expect(document.querySelectorAll("#contacts-book-remove-dialog")).toHaveLength(1);
  });

  it("returns focus to the strip button when cancelled", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    render(<ContactsView />);
    const stripButton = await screen.findByRole("button", { name: "Remove from this computer" });
    fireEvent.click(stripButton);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(removalDialog()).not.toBeInTheDocument();
    await waitFor(() => expect(stripButton).toHaveFocus());
  });
});

describe("contact book rail", () => {
  beforeEach(() => {
    resetContactBridgeMocks();
    vi.mocked(bridge.set_contact_book_lookup).mockResolvedValue(undefined);
  });
  afterEach(() => restoreContactDomGlobals());

  it("shows a lookup switch on each row only with two or more books", () => {
    renderRail([mockContactBook, otherBook]);
    expect(screen.getAllByRole("switch")).toHaveLength(2);
    expect(rowSwitch("book-pixel8")).toHaveAccessibleName("Use Pixel 8 for names and recipient search");
    expect(rowSwitch("book-pixel8")).toHaveAccessibleDescription("This computer only. Contacts stay here to browse.");
  });

  it("hides the switch for one included book and keeps it for one excluded book", () => {
    const { unmount } = render(<BookRail books={[mockContactBook]} bookId={mockContactBook.id} width={220} panelRef={{ current: null }} onSelect={vi.fn()} onBooksChanged={vi.fn()} onRemoveRequest={vi.fn()} onDeleteRequest={vi.fn()} onNotice={vi.fn()} />);
    expect(screen.queryByRole("switch")).not.toBeInTheDocument();
    unmount();
    renderRail([{ ...mockContactBook, lookupExcluded: true }]);
    expect(screen.getByRole("switch")).not.toBeChecked();
  });

  it("saves a row switch and reports the result through onNotice", async () => {
    const { onNotice, onBooksChanged } = renderRail([mockContactBook, otherBook]);
    fireEvent.click(rowSwitch("book-pixel8"));
    await waitFor(() => expect(bridge.set_contact_book_lookup).toHaveBeenCalledWith("book-pixel8", false));
    await waitFor(() => expect(onNotice).toHaveBeenCalledTimes(1));
    expect(onNotice.mock.lastCall?.[0]).toMatchObject({ kind: "success" });
    expect(lastNotice(onNotice).text).toBe("Names and recipient search no longer use Pixel 8. Its contacts are unchanged.");
    expect(onBooksChanged).toHaveBeenCalledTimes(1);
  });

  it("reports a re-enabled book with its name", async () => {
    const { onNotice } = renderRail([{ ...mockContactBook, lookupExcluded: true }, otherBook]);
    fireEvent.click(rowSwitch("book-pixel8"));
    await waitFor(() => expect(onNotice).toHaveBeenCalledTimes(1));
    expect(lastNotice(onNotice).text).toBe("Pixel 8 is used again for names and recipient search.");
  });

  it("reports a failed save as an error whose retry repeats the same change", async () => {
    vi.mocked(bridge.set_contact_book_lookup).mockRejectedValueOnce(new Error("failed"));
    const { onNotice } = renderRail([mockContactBook, otherBook]);
    fireEvent.click(rowSwitch("book-pixel8"));
    await waitFor(() => expect(onNotice).toHaveBeenCalledTimes(1));
    const { notice } = lastNotice(onNotice);
    expect(notice).toMatchObject({ kind: "error" });
    expect(lastNotice(onNotice).text).toBe("Couldn't save this change. Try again.");
    expect(rowSwitch("book-pixel8")).toBeChecked();
    await act(async () => notice.retry?.());
    expect(bridge.set_contact_book_lookup).toHaveBeenCalledTimes(2);
    expect(bridge.set_contact_book_lookup).toHaveBeenLastCalledWith("book-pixel8", false);
    await waitFor(() => expect(onNotice.mock.lastCall?.[0]).toMatchObject({ kind: "success" }));
  });

  it("asks before turning off the last included book; Escape cancels and refocuses the switch", async () => {
    renderRail([mockContactBook, { ...otherBook, lookupExcluded: true }]);
    rowSwitch("book-pixel8").focus();
    fireEvent.click(rowSwitch("book-pixel8"));
    expect(bridge.set_contact_book_lookup).not.toHaveBeenCalled();
    expect(screen.getByRole("group", { name: "Confirm turning off the last contact book" })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus());
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(rowSwitch("book-pixel8")).toHaveFocus());
    expect(document.getElementById("contacts-book-use-confirm")).not.toBeInTheDocument();
    expect(bridge.set_contact_book_lookup).not.toHaveBeenCalled();
    fireEvent.click(rowSwitch("book-pixel8"));
    fireEvent.click(screen.getByRole("button", { name: "Turn off" }));
    await waitFor(() => expect(bridge.set_contact_book_lookup).toHaveBeenCalledWith("book-pixel8", false));
    await waitFor(() => expect(rowSwitch("book-pixel8")).toHaveFocus());
  });

  it("distinguishes books with duplicate device names", () => {
    renderRail([mockContactBook, { ...mockContactBook, id: "book-1234" }]);
    expect(document.querySelector('button[data-book-id="book-pixel8"]')).toHaveTextContent("ID xel8");
    expect(document.querySelector('button[data-book-id="book-1234"]')).toHaveTextContent("ID 1234");
  });

  it("marks a removed book row with the phone-removed label and icon", () => {
    renderRail([mockContactBook, removedBook]);
    const row = document.querySelector<HTMLButtonElement>('button[data-book-id="book-iphone"]')!;
    expect(row).toHaveTextContent("Phone removed");
    expect(row).toHaveTextContent("803 contacts");
    expect(row.textContent).not.toContain("·");
    expect(row.querySelector("svg.lucide-phone-off")).toBeInTheDocument();
    expect(row.querySelector(".contacts-book-removed-text")).toHaveTextContent("Phone removed");
  });

  it("puts state and count on one line and the account on its own line", () => {
    renderRail([mockContactBook, otherBook]);
    const row = document.querySelector<HTMLButtonElement>('button[data-book-id="book-pixel8"]')!;
    expect(row.querySelector("small[data-book-state]")).toHaveTextContent(/^Active, 2 contacts$/);
    expect(row.querySelector(".contacts-book-account")).toHaveTextContent(/^Google$/);
    expect(row.textContent).not.toContain("·");
  });

  it("covers the whole selected row with the highlight, not only the select button", () => {
    renderRail([mockContactBook, otherBook]);
    const selectedItem = document.querySelector<HTMLElement>('li[data-book-row][data-book-id="book-pixel8"]')!;
    expect(selectedItem).toHaveAttribute("data-selected", "true");
    expect(selectedItem.querySelector('.contacts-book-row[aria-current="location"]')).not.toBeNull();
    expect(document.querySelector('li[data-book-row][data-book-id="book-other"]')).not.toHaveAttribute("data-selected");
  });
});

describe("ContactsView book lookup", () => {
  beforeEach(() => {
    resetContactBridgeMocks();
    vi.mocked(bridge.set_contact_book_lookup).mockResolvedValue(undefined);
  });
  afterEach(() => restoreContactDomGlobals());

  it("hides the book-use strip for one included book", async () => {
    render(<ContactsView />);
    await screen.findByRole("button", { name: /Pixel 8/ });
    expect(document.getElementById("contacts-book-use")).not.toBeInTheDocument();
  });

  it("hides a book from lookup, reloads the books, and offers no undo", async () => {
    let books = [mockContactBook, otherBook];
    vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
    vi.mocked(bridge.set_contact_book_lookup).mockImplementation(async (id, enabled) => {
      books = books.map(item => item.id === id ? { ...item, lookupExcluded: !enabled } : item);
    });
    render(<ContactsView />);
    fireEvent.click(await screen.findByRole("switch", { name: "Use Pixel 8 for names and recipient search" }));
    await waitFor(() => expect(bridge.set_contact_book_lookup).toHaveBeenCalledWith("book-pixel8", false));
    await waitFor(() => expect(document.querySelector('button[data-book-id="book-pixel8"]')).toHaveAttribute("data-book-use", "excluded"));
    expect(screen.getByText("Hidden from names")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Undo" })).not.toBeInTheDocument();
  });

  it("shows the compact selector and lookup strip instead of the rail at 700px", async () => {
    vi.stubGlobal("matchMedia", vi.fn((query: string) => ({
      matches: query === "(max-width: 700px)", media: query,
      addEventListener: vi.fn(), removeEventListener: vi.fn(),
    })));
    vi.mocked(bridge.list_contact_books).mockResolvedValue([mockContactBook, { ...otherBook, id: "book-1234", deviceName: "Pixel 8", lookupExcluded: true }]);
    render(<ContactsView />);
    expect(await screen.findByRole("option", { name: "Pixel 8, ID 1234, Active, hidden from names" })).toBeInTheDocument();
    expect(document.getElementById("contacts-book-use")).toBeInTheDocument();
    expect(document.getElementById("contacts-book-use-title")).toBeNull();
    expect(screen.getByRole("switch", { name: "Use for names and recipient search" })).toBeInTheDocument();
    expect(document.getElementById("contacts-book-list")).not.toBeInTheDocument();
  });

  it("shows the removal strip for a selected removed book", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    render(<ContactsView />);
    await waitFor(() => expect(document.getElementById("contacts-book-removed")).toBeInTheDocument());
    expect(screen.getByText("This phone was removed.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove from this computer" })).toBeInTheDocument();
    expect(document.getElementById("contacts-sync-unavailable")).not.toBeInTheDocument();
  });

  it("removes the selected book, selects and focuses the first remaining book", async () => {
    let books = [removedBook, mockContactBook];
    vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
    vi.mocked(bridge.forget_contact_book).mockImplementation(async id => {
      books = books.filter(item => item.id !== id);
    });
    render(<ContactsView />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove from this computer" }));
    fireEvent.click(screen.getByRole("dialog").querySelector<HTMLButtonElement>(".destructive-button")!);
    expect(bridge.forget_contact_book).toHaveBeenCalledWith("book-iphone");
    await waitFor(() => expect(document.querySelector('button[data-book-id="book-iphone"]')).not.toBeInTheDocument());
    expect(document.querySelector("#contacts-books")).not.toHaveTextContent("iPhone");
    const selectedRow = document.querySelector<HTMLButtonElement>("#contacts-books .contacts-book-row[aria-current]")!;
    expect(selectedRow).toHaveAttribute("data-book-id", "book-pixel8");
    await waitFor(() => expect(selectedRow).toHaveFocus());
    expect(document.getElementById("contacts-book-removed")).not.toBeInTheDocument();
  });
});

const railHandle = () => screen.getByRole("separator", { name: "Resize contact books" });
const storedLayout = (): Record<string, unknown> => JSON.parse(localStorage.getItem("peppy.layout.v1") ?? "{}");

describe("contact book rail collapse and resize", () => {
  beforeEach(() => resetContactBridgeMocks());
  afterEach(() => restoreContactDomGlobals());

  it("Enter on the handle collapses the rail and restores it through the controlled props", async () => {
    const onBookCollapsedChange = vi.fn();
    const { rerender } = render(<ContactsView bookCollapsed={false} onBookCollapsedChange={onBookCollapsedChange} />);
    await screen.findByRole("button", { name: /Pixel 8/ });
    fireEvent.keyDown(railHandle(), { key: "Enter" });
    expect(onBookCollapsedChange).toHaveBeenLastCalledWith(true);
    rerender(<ContactsView bookCollapsed onBookCollapsedChange={onBookCollapsedChange} />);
    expect(document.getElementById("contacts-book-list")).not.toBeInTheDocument();
    expect(document.getElementById("contacts-view")).toHaveAttribute("data-collapsed-reason", "user");
    expect(railHandle()).toHaveAttribute("aria-valuetext", "Collapsed");
    fireEvent.keyDown(railHandle(), { key: "Enter" });
    expect(onBookCollapsedChange).toHaveBeenLastCalledWith(false);
    rerender(<ContactsView bookCollapsed={false} onBookCollapsedChange={onBookCollapsedChange} />);
    expect(document.getElementById("contacts-book-list")).toBeInTheDocument();
  });

  it("double-click on a collapsed handle leaves the rail restored; double-click on an expanded rail collapses it", async () => {
    const pointer = { setPointerCapture: vi.fn(), releasePointerCapture: vi.fn(), hasPointerCapture: vi.fn(() => false) };
    Object.assign(HTMLElement.prototype, pointer);
    const now = vi.spyOn(Date, "now").mockReturnValue(1_000);
    try {
      render(<ContactsView />);
      await screen.findByRole("button", { name: /Pixel 8/ });
      fireEvent.keyDown(railHandle(), { key: "Enter" });
      expect(document.getElementById("contacts-book-list")).not.toBeInTheDocument();

      // A real double-click: each click's press and release, then the dblclick.
      for (let click = 0; click < 2; click += 1) {
        fireEvent.pointerDown(railHandle(), { pointerId: 1 });
        fireEvent.pointerUp(railHandle(), { pointerId: 1 });
        fireEvent.click(railHandle());
      }
      fireEvent.doubleClick(railHandle());
      expect(document.getElementById("contacts-book-list")).toBeInTheDocument();
      expect(railHandle()).toHaveAttribute("aria-valuetext", expect.stringMatching(/pixels$/));

      now.mockReturnValue(5_000);
      fireEvent.doubleClick(railHandle());
      expect(document.getElementById("contacts-book-list")).not.toBeInTheDocument();
    } finally {
      now.mockRestore();
      for (const key of Object.keys(pointer)) delete (HTMLElement.prototype as unknown as Record<string, unknown>)[key];
    }
  });

  it("starts collapsed when the controlled prop says so", async () => {
    render(<ContactsView bookCollapsed onBookCollapsedChange={vi.fn()} />);
    expect(document.getElementById("contacts-book-list")).not.toBeInTheDocument();
    await waitFor(() => expect(bridge.list_contact_books).toHaveBeenCalled());
    expect(railHandle()).toHaveAttribute("data-collapsed-side", "before");
  });

  it("collapses and restores locally when uncontrolled, keeping the stored width at least 180", async () => {
    render(<ContactsView />);
    await screen.findByRole("button", { name: /Pixel 8/ });
    fireEvent.keyDown(railHandle(), { key: "Enter" });
    expect(document.getElementById("contacts-book-list")).not.toBeInTheDocument();
    expect(storedLayout().contactsBookWidth).toBeGreaterThanOrEqual(180);
    fireEvent.keyDown(railHandle(), { key: "Enter" });
    expect(document.getElementById("contacts-book-list")).toBeInTheDocument();
  });

  it("persists the width after keyboard resizing", async () => {
    render(<ContactsView />);
    await screen.findByRole("button", { name: /Pixel 8/ });
    fireEvent.keyDown(railHandle(), { key: "ArrowRight" });
    expect(railHandle()).toHaveAttribute("aria-valuenow", "228");
    expect(document.getElementById("contacts-book-list")!.style.getPropertyValue("--contacts-book-width")).toBe("228px");
    fireEvent.keyDown(railHandle(), { key: "End" });
    expect(storedLayout().contactsBookWidth).toBe(320);
    fireEvent.keyDown(railHandle(), { key: "Home" });
    expect(storedLayout().contactsBookWidth).toBe(180);
  });

  it("clamps a stored rail width into range on load", async () => {
    localStorage.setItem("peppy.layout.v1", JSON.stringify({ contactsBookWidth: 999 }));
    render(<ContactsView />);
    await screen.findByRole("button", { name: /Pixel 8/ });
    expect(railHandle()).toHaveAttribute("aria-valuenow", "320");
  });

  it("omits the resize handle at 700px and below", async () => {
    vi.stubGlobal("matchMedia", vi.fn((query: string) => ({
      matches: query === "(max-width: 700px)", media: query,
      addEventListener: vi.fn(), removeEventListener: vi.fn(),
    })));
    render(<ContactsView />);
    await waitFor(() => expect(bridge.list_contact_books).toHaveBeenCalled());
    expect(screen.queryByRole("separator", { name: "Resize contact books" })).not.toBeInTheDocument();
  });

  it("collapses for lack of room without persisting, so the stored preference is untouched", async () => {
    const originalWidth = Object.getOwnPropertyDescriptor(window, "innerWidth");
    try {
      Object.defineProperty(window, "innerWidth", { configurable: true, value: 400 });
      render(<ContactsView />);
      await waitFor(() => expect(bridge.list_contact_books).toHaveBeenCalled());
      expect(document.getElementById("contacts-book-list")).not.toBeInTheDocument();
      expect(document.getElementById("contacts-view")).toHaveAttribute("data-collapsed-reason", "space");
      expect(storedLayout()).not.toHaveProperty("contactsBookCollapsed");
      expect(storedLayout()).not.toHaveProperty("contactsBookWidth");
    } finally {
      if (originalWidth) Object.defineProperty(window, "innerWidth", originalWidth);
      else delete (window as { innerWidth?: number }).innerWidth;
    }
  });
});

describe("book rail placement, hidden-name line, icon, and sync reason", () => {
  beforeEach(() => resetContactBridgeMocks());
  afterEach(() => restoreContactDomGlobals());

  it("anchors the menu under its more button, never left of the rail", () => {
    const panelRef = { current: null as HTMLElement | null };
    render(<BookRail books={[mockContactBook, removedBook]} bookId={mockContactBook.id} width={220} panelRef={panelRef} onSelect={vi.fn()} onBooksChanged={vi.fn()} onRemoveRequest={vi.fn()} onDeleteRequest={vi.fn()} onNotice={vi.fn()} />);
    vi.spyOn(panelRef.current!, "getBoundingClientRect").mockReturnValue({ left: 60 } as DOMRect);
    const button = screen.getByRole("button", { name: "More actions for iPhone" });
    vi.spyOn(button, "getBoundingClientRect").mockReturnValue({ left: 80, right: 100, top: 26, bottom: 50 } as DOMRect);
    fireEvent.click(button);
    expect(document.getElementById("contacts-book-menu")).toHaveStyle({ left: "64px", top: "54px" });
  });

  it("keeps a right-click menu's pointer position but clamps it to the rail", () => {
    const panelRef = { current: null as HTMLElement | null };
    render(<BookRail books={[mockContactBook, removedBook]} bookId={mockContactBook.id} width={220} panelRef={panelRef} onSelect={vi.fn()} onBooksChanged={vi.fn()} onRemoveRequest={vi.fn()} onDeleteRequest={vi.fn()} onNotice={vi.fn()} />);
    vi.spyOn(panelRef.current!, "getBoundingClientRect").mockReturnValue({ left: 60 } as DOMRect);
    fireEvent.contextMenu(document.querySelector<HTMLElement>('li[data-book-id="book-iphone"]')!, { clientX: 20, clientY: 90 });
    expect(document.getElementById("contacts-book-menu")).toHaveStyle({ left: "64px", top: "90px" });
  });

  it("shows the hidden-name line only on an excluded book", () => {
    renderRail([mockContactBook, { ...otherBook, lookupExcluded: true }]);
    expect(document.querySelector('li[data-book-id="book-pixel8"] .contacts-book-hidden')).toBeNull();
    expect(document.querySelector('li[data-book-id="book-other"] .contacts-book-hidden')).toHaveTextContent("Hidden from names");
  });

  it("marks the removed-book icon with its warning-colour class", () => {
    renderRail([mockContactBook, removedBook]);
    expect(document.querySelector('li[data-book-id="book-iphone"] svg')).toHaveClass("contacts-book-removed-icon");
    expect(document.querySelector('li[data-book-id="book-pixel8"] svg')).not.toHaveClass("contacts-book-removed-icon");
  });

  it("gives Sync now a reason for a removed phone", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    render(<ContactsView />);
    await waitFor(() => expect(screen.getByRole("button", { name: "Sync now" })).toHaveAttribute("title", "Can't sync a removed phone"));
    expect(screen.getByRole("button", { name: "Sync now" })).toBeDisabled();
  });
});

describe("removed book row menu", () => {
  beforeEach(() => resetContactBridgeMocks());
  afterEach(() => restoreContactDomGlobals());

  const moreButton = () => screen.queryByRole("button", { name: "More actions for iPhone" });
  const menu = () => document.getElementById("contacts-book-menu");
  const removedRow = () => document.querySelector<HTMLElement>('li[data-book-id="book-iphone"]')!;

  it("shows the more-actions button only on removed rows", () => {
    renderRail([mockContactBook, removedBook]);
    expect(screen.getAllByRole("button", { name: /More actions for/ })).toHaveLength(1);
    expect(moreButton()).toHaveAttribute("aria-haspopup", "menu");
    expect(moreButton()).toHaveAttribute("aria-expanded", "false");
    expect(document.querySelector('li[data-book-id="book-pixel8"] [data-action="book-menu"]')).toBeNull();
    expect(fireEvent.contextMenu(document.querySelector<HTMLElement>('li[data-book-id="book-pixel8"]')!)).toBe(true);
    expect(menu()).not.toBeInTheDocument();
  });

  it("opens from the button, focuses the item, and Escape returns focus to the button", async () => {
    renderRail([mockContactBook, removedBook]);
    fireEvent.click(moreButton()!);
    expect(menu()).toHaveAttribute("role", "menu");
    expect(moreButton()).toHaveAttribute("aria-expanded", "true");
    const item = screen.getByRole("menuitem", { name: "Remove from this computer" });
    expect(item).toHaveAttribute("data-action", "remove-book-local");
    await waitFor(() => expect(item).toHaveFocus());
    fireEvent.keyDown(item, { key: "Escape" });
    expect(menu()).not.toBeInTheDocument();
    expect(moreButton()).toHaveFocus();
  });

  it("toggles closed on a second click of the button", () => {
    renderRail([mockContactBook, removedBook]);
    fireEvent.click(moreButton()!);
    fireEvent.click(moreButton()!);
    expect(menu()).not.toBeInTheDocument();
  });

  it("opens the same menu from a right-click on the removed row", () => {
    renderRail([mockContactBook, removedBook]);
    expect(fireEvent.contextMenu(removedRow(), { clientX: 120, clientY: 90 })).toBe(false);
    expect(menu()).toHaveAttribute("aria-label", "Actions for iPhone");
  });

  it("closes on an outside press, and on Tab with focus returning to the button", () => {
    renderRail([mockContactBook, removedBook]);
    fireEvent.click(moreButton()!);
    fireEvent.mouseDown(document.body);
    expect(menu()).not.toBeInTheDocument();
    fireEvent.click(moreButton()!);
    fireEvent.keyDown(screen.getAllByRole("menuitem")[0], { key: "Tab" });
    expect(menu()).not.toBeInTheDocument();
    expect(moreButton()).toHaveFocus();
  });

  it("Remove from this computer hands the book to the one view-level dialog, which forgets it", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([mockContactBook, removedBook]);
    vi.mocked(bridge.forget_contact_book).mockResolvedValue(undefined);
    render(<ContactsView />);
    fireEvent.click(await screen.findByRole("button", { name: "More actions for iPhone" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Remove from this computer" }));
    expect(menu()).not.toBeInTheDocument();
    expect(document.querySelectorAll("#contacts-book-remove-dialog")).toHaveLength(1);
    expect(bridge.forget_contact_book).not.toHaveBeenCalled();
    fireEvent.click(document.querySelector<HTMLButtonElement>("#contacts-book-remove-dialog .destructive-button")!);
    await waitFor(() => expect(bridge.forget_contact_book).toHaveBeenCalledWith("book-iphone"));
  });

  it("returns focus to the row's more-actions button when the dialog from the menu is cancelled", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([mockContactBook, removedBook]);
    render(<ContactsView />);
    const more = await screen.findByRole("button", { name: "More actions for iPhone" });
    fireEvent.click(more);
    fireEvent.click(screen.getByRole("menuitem", { name: "Remove from this computer" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(document.getElementById("contacts-book-remove-dialog")).not.toBeInTheDocument();
    await waitFor(() => expect(more).toHaveFocus());
  });
});

describe("contact book drag collapse and focus after removal", () => {
  beforeEach(() => resetContactBridgeMocks());
  afterEach(() => restoreContactDomGlobals());

  it("reports the collapse once while the sash keeps moving below the collapse threshold", async () => {
    const onChange = vi.fn();
    function Controlled() {
      const [collapsed, setCollapsed] = useState(false);
      return <ContactsView bookCollapsed={collapsed} onBookCollapsedChange={value => { onChange(value); setCollapsed(value); }} />;
    }
    // A real drag: pointer moves queue one animation frame each, and the test runs those frames by hand.
    const pointer = { setPointerCapture: vi.fn(), releasePointerCapture: vi.fn(), hasPointerCapture: vi.fn(() => true) };
    Object.assign(HTMLElement.prototype, pointer);
    const frames: FrameRequestCallback[] = [];
    const frameSpy = vi.spyOn(window, "requestAnimationFrame").mockImplementation(callback => { frames.push(callback); return frames.length; });
    try {
      render(<Controlled />);
      await screen.findByRole("button", { name: /Pixel 8/ });

      fireEvent.pointerDown(railHandle(), { pointerId: 1, clientX: 220 });
      for (let step = 1; step <= 20; step += 1) {
        fireEvent.pointerMove(railHandle(), { pointerId: 1, clientX: 220 - step * 8 });
        act(() => frames.splice(0).forEach(callback => callback(0)));
      }
      fireEvent.pointerUp(railHandle(), { pointerId: 1, clientX: 60 });

      expect(onChange).toHaveBeenCalledTimes(1);
      expect(onChange).toHaveBeenCalledWith(true);
      expect(document.getElementById("contacts-book-list")).not.toBeInTheDocument();
    } finally {
      frameSpy.mockRestore();
      for (const key of Object.keys(pointer)) delete (HTMLElement.prototype as unknown as Record<string, unknown>)[key];
    }
  });

  it("keeps the focus request until the removed book is gone, past a books refresh that started earlier", async () => {
    let hint: (() => void) | undefined;
    vi.mocked(bridge.subscribe).mockImplementation(listener => { hint = listener; return () => undefined; });
    const responses: Array<ReturnType<typeof deferred<ContactBookView[]>>> = [];
    vi.mocked(bridge.list_contact_books).mockImplementation(() => {
      const response = deferred<ContactBookView[]>();
      responses.push(response);
      return response.promise;
    });
    render(<ContactsView />);
    await waitFor(() => expect(responses).toHaveLength(1));
    await act(async () => responses[0].resolve([removedBook, mockContactBook]));
    fireEvent.click(await screen.findByRole("button", { name: "Remove from this computer" }));

    act(() => hint?.());
    await waitFor(() => expect(responses).toHaveLength(2));
    fireEvent.click(screen.getByRole("dialog").querySelector<HTMLButtonElement>(".destructive-button")!);
    await waitFor(() => expect(responses).toHaveLength(3));

    // The earlier refresh still lists the removed book; it must not consume the focus request.
    await act(async () => responses[1].resolve([removedBook, mockContactBook]));
    expect(document.querySelector('#contacts-books button[data-book-id="book-pixel8"]')).not.toHaveFocus();

    await act(async () => responses[2].resolve([mockContactBook]));
    await waitFor(() => expect(document.querySelector("#contacts-books .contacts-book-row[aria-current]")).toHaveFocus());
  });
});

describe("book lookup save that finishes after its strip unmounted", () => {
  beforeEach(() => resetContactBridgeMocks());
  afterEach(() => restoreContactDomGlobals());

  it("still reports the result and refreshes the books when the rail swaps out mid-save", async () => {
    const save = deferred<void>();
    vi.mocked(bridge.set_contact_book_lookup).mockReturnValue(save.promise);
    const onNotice = vi.fn();
    const onBooksChanged = vi.fn();
    const rail = render(<BookRail books={[mockContactBook, otherBook]} bookId={mockContactBook.id} width={220} panelRef={{ current: null }} onSelect={vi.fn()} onBooksChanged={onBooksChanged} onRemoveRequest={vi.fn()} onDeleteRequest={vi.fn()} onNotice={onNotice} />);
    fireEvent.click(rowSwitch("book-pixel8"));
    await waitFor(() => expect(bridge.set_contact_book_lookup).toHaveBeenCalledWith("book-pixel8", false));

    rail.unmount();
    await act(async () => save.resolve());

    expect(onBooksChanged).toHaveBeenCalledTimes(1);
    expect(onNotice).toHaveBeenCalledTimes(1);
    expect(onNotice.mock.lastCall?.[0]).toMatchObject({ kind: "success" });
  });
});
