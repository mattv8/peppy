import "@testing-library/jest-dom/vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { bridge, type ContactBookView } from "../bridge/bridge";
import { ContactsView } from "./Contacts";
import { BookRail } from "./BookRail";
import { RemovedBookStrip } from "./RemovedBookStrip";
import { deferred, mockContactBook, resetContactBridgeMocks, restoreContactDomGlobals } from "./contacts-test-utils";

vi.mock("../bridge/bridge", { spy: true });

const removedBook: ContactBookView = { ...mockContactBook, id: "book-iphone", deviceName: "iPhone", state: "retired", sourceRevoked: true, contactCount: 803 };
const UNLOCK_REASON = "Unlock sync to delete this book for all devices.";
const OFFLINE_REASON = "Connect to the server to delete this book for all devices.";
const SYNCING_REASON = "Finish syncing to delete this book for all devices.";

const deleteDialog = () => document.getElementById("contacts-book-delete-dialog");
const ackBox = () => document.getElementById("contacts-book-delete-ack") as HTMLInputElement;
const dialogDeleteButton = () => within(deleteDialog()!).getByRole("button", { name: /Delete for all devices|Deleting/ });
const menuDeleteItem = () => screen.getByRole("menuitem", { name: "Delete for all devices…" });
const selectedRow = () => document.querySelector<HTMLButtonElement>("#contacts-books .contacts-book-row[aria-current]");

async function openDeleteDialogFromStrip(books: ContactBookView[] = [removedBook, mockContactBook]) {
  vi.mocked(bridge.list_contact_books).mockResolvedValue(books);
  render(<ContactsView />);
  fireEvent.click(await screen.findByRole("button", { name: "Delete for all devices…" }));
}

describe("removed book delete actions", () => {
  beforeEach(() => {
    resetContactBridgeMocks();
    vi.mocked(bridge.delete_contact_book).mockResolvedValue({ bookId: removedBook.id, contacts: removedBook.contactCount });
  });
  afterEach(() => restoreContactDomGlobals());

  it("keeps both strip actions in one group, with the delete action last", () => {
    render(<RemovedBookStrip book={removedBook} onRemoveRequest={vi.fn()} onDeleteRequest={vi.fn()} />);
    const group = document.querySelector<HTMLElement>("#contacts-book-removed .contacts-strip-actions")!;
    const buttons = within(group).getAllByRole("button");
    expect(buttons.map(button => button.textContent)).toEqual(["Remove from this computer", "Delete for all devices…"]);
    expect(buttons[1]).toHaveClass("destructive-button");
    expect(buttons[1]).toHaveAttribute("data-action", "delete-book-everywhere");
  });

  it("passes the book and its strip button to onDeleteRequest", () => {
    const onDeleteRequest = vi.fn();
    render(<RemovedBookStrip book={removedBook} onRemoveRequest={vi.fn()} onDeleteRequest={onDeleteRequest} />);
    fireEvent.click(screen.getByRole("button", { name: "Delete for all devices…" }));
    expect(onDeleteRequest).toHaveBeenCalledWith(removedBook, expect.objectContaining({ current: expect.any(HTMLButtonElement) }));
  });

  it("lists remove and delete in the row menu, separated, with delete in the error colour", () => {
    render(<BookRail books={[mockContactBook, removedBook]} bookId={mockContactBook.id} width={220} panelRef={{ current: null }} onSelect={vi.fn()} onBooksChanged={vi.fn()} onRemoveRequest={vi.fn()} onDeleteRequest={vi.fn()} onNotice={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "More actions for iPhone" }));
    const menu = document.getElementById("contacts-book-menu")!;
    const separator = within(menu).getByRole("separator");
    expect(separator.previousElementSibling).toHaveTextContent("Remove from this computer");
    expect(separator.nextElementSibling).toHaveAttribute("data-action", "delete-book-everywhere");
    expect(within(menu).getAllByRole("menuitem").map(item => item.textContent)).toEqual(["Remove from this computer", "Delete for all devices…"]);
    expect(menuDeleteItem()).toHaveClass("contacts-book-menu-item-destructive");
  });

  it("opens the delete dialog from the row menu", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([mockContactBook, removedBook]);
    render(<ContactsView />);
    fireEvent.click(await screen.findByRole("button", { name: "More actions for iPhone" }));
    fireEvent.click(menuDeleteItem());
    expect(deleteDialog()).toBeInTheDocument();
    expect(bridge.delete_contact_book).not.toHaveBeenCalled();
  });

  it("disables both actions with the unlock reason while sync is locked", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    render(<ContactsView toneDisabled />);
    const button = await screen.findByRole("button", { name: "Delete for all devices…" });
    expect(button).toHaveAttribute("aria-disabled", "true");
    expect(button).toHaveAttribute("title", UNLOCK_REASON);
    expect(button).toHaveAttribute("aria-describedby", "contacts-book-delete-reason");
    expect(document.getElementById("contacts-book-delete-reason")).toHaveTextContent(UNLOCK_REASON);
    fireEvent.click(button);
    expect(deleteDialog()).not.toBeInTheDocument();
  });

  it("shows no reason line and no description while sync is unlocked", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    render(<ContactsView />);
    const button = await screen.findByRole("button", { name: "Delete for all devices…" });
    expect(button).not.toHaveAttribute("aria-describedby");
    expect(document.getElementById("contacts-book-delete-reason")).not.toBeInTheDocument();
  });

  it("shows the unlock reason on the locked menu item and does not open the dialog", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([mockContactBook, removedBook]);
    render(<ContactsView toneDisabled />);
    fireEvent.click(await screen.findByRole("button", { name: "More actions for iPhone" }));
    expect(menuDeleteItem()).toHaveAttribute("aria-disabled", "true");
    expect(menuDeleteItem()).toHaveAttribute("title", UNLOCK_REASON);
    fireEvent.click(menuDeleteItem());
    expect(deleteDialog()).not.toBeInTheDocument();
  });

  it("disables both actions with the offline reason when the server is not connected", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    render(<ContactsView serverConnected={false} />);
    const button = await screen.findByRole("button", { name: "Delete for all devices…" });
    expect(button).toHaveAttribute("aria-disabled", "true");
    expect(button).toHaveAttribute("title", OFFLINE_REASON);
    expect(button).toHaveAttribute("aria-describedby", "contacts-book-delete-reason");
    expect(document.getElementById("contacts-book-delete-reason")).toHaveTextContent(OFFLINE_REASON);
    fireEvent.click(button);
    expect(deleteDialog()).not.toBeInTheDocument();
  });

  it("disables the menu item with the offline reason when the server is not connected", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([mockContactBook, removedBook]);
    render(<ContactsView serverConnected={false} />);
    fireEvent.click(await screen.findByRole("button", { name: "More actions for iPhone" }));
    expect(menuDeleteItem()).toHaveAttribute("aria-disabled", "true");
    expect(menuDeleteItem()).toHaveAttribute("title", OFFLINE_REASON);
    fireEvent.click(menuDeleteItem());
    expect(deleteDialog()).not.toBeInTheDocument();
  });

  it("disables both actions with the syncing reason while contacts are rebuilding or need repair", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    const { rerender } = render(<ContactsView sync={{ repairRequired: false, projection: { state: "rebuilding" } }} />);
    const button = await screen.findByRole("button", { name: "Delete for all devices…" });
    expect(button).toHaveAttribute("aria-disabled", "true");
    expect(document.getElementById("contacts-book-delete-reason")).toHaveTextContent(SYNCING_REASON);

    rerender(<ContactsView sync={{ repairRequired: true }} />);
    expect(document.getElementById("contacts-book-delete-reason")).toHaveTextContent(SYNCING_REASON);
    rerender(<ContactsView />);
    expect(document.getElementById("contacts-book-delete-reason")).not.toBeInTheDocument();
  });

  it("gives the locked reason priority over offline and syncing", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    render(<ContactsView toneDisabled serverConnected={false} sync={{ repairRequired: true }} />);
    await screen.findByRole("button", { name: "Delete for all devices…" });
    expect(document.getElementById("contacts-book-delete-reason")).toHaveTextContent(UNLOCK_REASON);
  });

  it("gives the offline reason priority over syncing", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    render(<ContactsView serverConnected={false} sync={{ repairRequired: true }} />);
    await screen.findByRole("button", { name: "Delete for all devices…" });
    expect(document.getElementById("contacts-book-delete-reason")).toHaveTextContent(OFFLINE_REASON);
  });
});

describe("delete for all devices dialog", () => {
  beforeEach(() => {
    resetContactBridgeMocks();
    vi.mocked(bridge.delete_contact_book).mockResolvedValue({ bookId: removedBook.id, contacts: removedBook.contactCount });
  });
  afterEach(() => restoreContactDomGlobals());

  it("states what is deleted, what is not, and how it spreads", async () => {
    await openDeleteDialogFromStrip();
    expect(within(deleteDialog()!).getByRole("heading")).toHaveTextContent("Delete iPhone's contacts for all devices?");
    const items = Array.from(deleteDialog()!.querySelectorAll("ul > li")).map(item => item.textContent);
    expect(items).toEqual([
      "Deletes the saved copy of 803 contacts from every device in your vault.",
      "Devices that are offline remove it when they next connect.",
      "Names from this book stop showing in conversations on all your devices.",
      "This can't be undone. Contacts on the phone itself aren't touched.",
    ]);
    expect(within(deleteDialog()!).getByLabelText("I understand this can't be undone.")).toBe(ackBox());
  });

  it("moves focus to the acknowledgement when the blocked action is activated", async () => {
    await openDeleteDialogFromStrip();
    expect(dialogDeleteButton()).toHaveAttribute("aria-describedby", "contacts-book-delete-ack");
    fireEvent.click(dialogDeleteButton());
    expect(ackBox()).toHaveFocus();
    expect(bridge.delete_contact_book).not.toHaveBeenCalled();

    fireEvent.click(ackBox());
    expect(dialogDeleteButton()).not.toHaveAttribute("aria-describedby");
  });

  it("emphasises the irreversible sentence and wraps the acknowledgement label", async () => {
    await openDeleteDialogFromStrip();
    expect(ackBox().closest("label")).toHaveClass("contacts-book-delete-ack");
    expect(within(deleteDialog()!).getByText("This can't be undone.").tagName).toBe("STRONG");
  });

  it("keeps the destructive action inert until the acknowledgement is checked", async () => {
    await openDeleteDialogFromStrip();
    expect(dialogDeleteButton()).toHaveAttribute("aria-disabled", "true");
    fireEvent.click(dialogDeleteButton());
    expect(bridge.delete_contact_book).not.toHaveBeenCalled();

    fireEvent.click(ackBox());
    expect(dialogDeleteButton()).not.toHaveAttribute("aria-disabled");
    fireEvent.click(dialogDeleteButton());
    await waitFor(() => expect(bridge.delete_contact_book).toHaveBeenCalledTimes(1));
    expect(bridge.delete_contact_book).toHaveBeenCalledWith("book-iphone");
  });

  it("closes on success, drops the book, focuses the selected book, and announces the result", async () => {
    let books = [removedBook, mockContactBook];
    vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
    vi.mocked(bridge.delete_contact_book).mockImplementation(async id => {
      books = books.filter(item => item.id !== id);
      return { bookId: id, contacts: 803 };
    });
    render(<ContactsView />);
    fireEvent.click(await screen.findByRole("button", { name: "Delete for all devices…" }));
    fireEvent.click(ackBox());
    fireEvent.click(dialogDeleteButton());

    await waitFor(() => expect(deleteDialog()).not.toBeInTheDocument());
    expect(bridge.delete_contact_book).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(document.querySelector('button[data-book-id="book-iphone"]')).not.toBeInTheDocument());
    await waitFor(() => expect(selectedRow()).toHaveFocus());
    expect(selectedRow()).toHaveAttribute("data-book-id", "book-pixel8");
    expect(document.getElementById("contacts-book-removed")).not.toBeInTheDocument();
    expect(document.querySelector("#contacts-center-pane")).toHaveTextContent("Deleted iPhone's contacts for all devices. Other devices remove it as they sync.");
    expect(document.querySelector("#contacts-center-pane")).not.toHaveTextContent("deleted from another device");
  });

  it.each([
    ["contact-book-not-deletable", "This contact book can't be deleted for all devices."],
    ["locked", UNLOCK_REASON],
    ["offline", OFFLINE_REASON],
    ["sync-not-current", SYNCING_REASON],
    [undefined, "Couldn't delete this book for all devices. Try again."],
  ])("keeps the dialog open and explains a %s failure", async (code, message) => {
    vi.mocked(bridge.delete_contact_book).mockRejectedValue(code ? { code, message: "native" } : new Error("offline"));
    await openDeleteDialogFromStrip();
    fireEvent.click(ackBox());
    fireEvent.click(dialogDeleteButton());

    await waitFor(() => expect(document.getElementById("contacts-book-delete-error")).toHaveTextContent(message));
    expect(document.getElementById("contacts-book-delete-error")).toHaveAttribute("role", "alert");
    expect(deleteDialog()).toBeInTheDocument();
    expect(dialogDeleteButton()).not.toHaveAttribute("aria-busy");
  });

  it("explains an already-deleted book and refreshes the books", async () => {
    vi.mocked(bridge.delete_contact_book).mockRejectedValue({ code: "contact-book-deleted", message: "gone" });
    await openDeleteDialogFromStrip();
    const loadsBefore = vi.mocked(bridge.list_contact_books).mock.calls.length;
    fireEvent.click(ackBox());
    fireEvent.click(dialogDeleteButton());

    await waitFor(() => expect(document.getElementById("contacts-book-delete-error")).toHaveTextContent("This book was already deleted for all devices."));
    await waitFor(() => expect(vi.mocked(bridge.list_contact_books).mock.calls.length).toBe(loadsBefore + 1));
  });

  it("keeps the destructive action inert with its reason when the connection drops while open", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    const { rerender } = render(<ContactsView />);
    fireEvent.click(await screen.findByRole("button", { name: "Delete for all devices…" }));
    fireEvent.click(ackBox());
    expect(dialogDeleteButton()).not.toHaveAttribute("aria-disabled");

    rerender(<ContactsView serverConnected={false} />);
    expect(dialogDeleteButton()).toHaveAttribute("aria-disabled", "true");
    expect(dialogDeleteButton()).toHaveAttribute("aria-describedby", "contacts-book-delete-dialog-reason");
    expect(document.getElementById("contacts-book-delete-dialog-reason")).toHaveTextContent(OFFLINE_REASON);
    fireEvent.click(dialogDeleteButton());
    expect(bridge.delete_contact_book).not.toHaveBeenCalled();
  });

  it("blocks Escape and disables Cancel while deleting, then closes when it finishes", async () => {
    const pending = deferred<{ bookId: string; contacts: number }>();
    vi.mocked(bridge.delete_contact_book).mockReturnValue(pending.promise);
    await openDeleteDialogFromStrip();
    fireEvent.click(ackBox());
    fireEvent.click(dialogDeleteButton());

    await waitFor(() => expect(dialogDeleteButton()).toHaveTextContent("Deleting…"));
    expect(dialogDeleteButton()).toHaveAttribute("aria-busy", "true");
    expect(within(deleteDialog()!).getByRole("button", { name: "Cancel" })).toBeDisabled();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(deleteDialog()).toBeInTheDocument();

    pending.resolve({ bookId: removedBook.id, contacts: removedBook.contactCount });
    await waitFor(() => expect(deleteDialog()).not.toBeInTheDocument());
  });
});

describe("book deleted from another device", () => {
  beforeEach(() => resetContactBridgeMocks());
  afterEach(() => restoreContactDomGlobals());

  it("tells the user when the selected removed book disappears without this computer deleting it", async () => {
    let hint: (() => void) | undefined;
    vi.mocked(bridge.subscribe).mockImplementation(listener => { hint = listener; return () => undefined; });
    let books = [removedBook, mockContactBook];
    vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
    render(<ContactsView />);
    await screen.findByRole("button", { name: "Remove from this computer" });

    books = [mockContactBook];
    act(() => hint?.());

    await waitFor(() => expect(document.querySelector("#contacts-center-pane")).toHaveTextContent("iPhone's contacts were deleted from another device."));
    expect(bridge.delete_contact_book).not.toHaveBeenCalled();
  });
});
