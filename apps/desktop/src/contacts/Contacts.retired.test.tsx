import "@testing-library/jest-dom/vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { bridge, type ContactBookView } from "../bridge/bridge";
import { ContactsView } from "./Contacts";
import { mockContactBook, resetContactBridgeMocks, restoreContactDomGlobals } from "./contacts-test-utils";
import { stubMatchMedia } from "../test-support/browserStubs";

vi.mock("../bridge/bridge", { spy: true });

beforeEach(() => resetContactBridgeMocks());
afterEach(() => restoreContactDomGlobals());

const retiredBook: ContactBookView = { ...mockContactBook, id: "book-iphone-retired", deviceName: "Phone", state: "retired", contactCount: 1605 };

it("offers local removal of a retired book and focuses the remaining book", async () => {
  let books = [retiredBook, mockContactBook];
  vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
  vi.mocked(bridge.forget_contact_book).mockImplementation(async id => { books = books.filter(book => book.id !== id); });
  render(<ContactsView />);
  const strip = await waitFor(() => {
    const element = document.getElementById("contacts-book-retired");
    expect(element).toBeInTheDocument();
    return element!;
  });
  fireEvent.click(within(strip).getByRole("button", { name: "Remove from this computer" }));
  expect(screen.getByRole("dialog")).toHaveTextContent("Remove Phone's retired contact book from this computer?");
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Remove from this computer" }));
  await waitFor(() => expect(bridge.forget_contact_book).toHaveBeenCalledWith("book-iphone-retired"));
  await waitFor(() => expect(document.querySelector('button[data-book-id="book-pixel8"]')).toHaveFocus());
  expect(document.getElementById("contacts-action-notice")).toHaveTextContent("Removed Phone's retired contact book from this computer.");
});

it("offers a disambiguated context menu on the retired row", async () => {
  vi.mocked(bridge.list_contact_books).mockResolvedValue([retiredBook, { ...mockContactBook, deviceName: "Phone" }]);
  render(<ContactsView />);
  const more = await screen.findByRole("button", { name: "More actions for Phone, ID ired" });
  const row = more.closest("li")!;
  const state = row.querySelector('small[data-book-state="retired"]');
  expect(state).toHaveTextContent(/^Retired$/);
  expect(state).not.toHaveClass("contacts-book-removed-text");
  expect(state?.nextElementSibling).toHaveTextContent(/^1,605 contacts$/);
  expect(state?.nextElementSibling?.tagName).toBe("SMALL");
  expect(row.querySelector(".contacts-book-removed-icon, .contacts-book-removed-text")).toBeNull();
  expect(within(row).getByRole("switch", { name: "Use Phone, ID ired for names and recipient search" })).toBeInTheDocument();
  fireEvent.contextMenu(row);
  expect(screen.getByRole("menu", { name: "Actions for Phone, ID ired" })).toBeInTheDocument();
  expect(screen.getByRole("menuitem", { name: "Remove from this computer" })).toBeInTheDocument();
  expect(screen.getByRole("menuitem", { name: "Delete for all devices…" })).toBeInTheDocument();
  fireEvent.click(screen.getByRole("menuitem", { name: "Remove from this computer" }));
  expect(screen.getByRole("dialog")).toHaveTextContent("Remove Phone's retired contact book from this computer?");
});

it("deletes a retired book only after acknowledgement", async () => {
  let books = [retiredBook, mockContactBook];
  vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
  vi.mocked(bridge.delete_contact_book).mockImplementation(async id => {
    books = books.filter(book => book.id !== id);
    return { bookId: id, contacts: 1605 };
  });
  render(<ContactsView />);
  fireEvent.click(await screen.findByRole("button", { name: "Delete for all devices…" }));
  const dialog = screen.getByRole("dialog");
  expect(dialog).toHaveTextContent("Delete Phone's retired contact book for all devices?");
  expect(dialog).toHaveTextContent("from every device in your vault except the phone itself.");
  expect(dialog).toHaveTextContent("any newer book from this phone isn't affected.");
  const button = within(dialog).getByRole("button", { name: "Delete for all devices" });
  fireEvent.click(button);
  expect(bridge.delete_contact_book).not.toHaveBeenCalled();
  fireEvent.click(within(dialog).getByRole("checkbox"));
  fireEvent.click(button);
  await waitFor(() => expect(bridge.delete_contact_book).toHaveBeenCalledWith("book-iphone-retired"));
  await waitFor(() => expect(document.getElementById("contacts-action-notice")).toHaveTextContent("Deleted Phone's retired contact book for all devices. Other devices remove it as they sync."));
});

it("focuses the list title after removing the only retired book", async () => {
  let books = [retiredBook];
  vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
  vi.mocked(bridge.forget_contact_book).mockImplementation(async () => { books = []; });
  render(<ContactsView />);
  fireEvent.click(await screen.findByRole("button", { name: "Remove from this computer" }));
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Remove from this computer" }));
  await waitFor(() => expect(document.getElementById("contacts-list-title")).toHaveFocus());
});

it("announces a retired book deleted from another device", async () => {
  let hint: (() => void) | undefined;
  let books = [retiredBook, mockContactBook];
  vi.mocked(bridge.subscribe).mockImplementation(listener => { hint = listener; return () => undefined; });
  vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
  render(<ContactsView />);
  await screen.findByRole("button", { name: "Remove from this computer" });
  books = [mockContactBook];
  act(() => hint?.());
  await waitFor(() => expect(document.getElementById("contacts-action-notice")).toHaveTextContent("Phone's retired contact book was deleted from another device."));
});

it("preserves removed wording when the retired phone is also removed", async () => {
  vi.mocked(bridge.list_contact_books).mockResolvedValue([{ ...retiredBook, sourceRevoked: true }]);
  render(<ContactsView />);
  await waitFor(() => expect(document.getElementById("contacts-book-removed")).toHaveTextContent("This phone was removed."));
  expect(document.getElementById("contacts-book-retired")).not.toBeInTheDocument();
});

it("shows retired status and keeps sync unavailable without a dangling description", async () => {
  vi.mocked(bridge.list_contact_books).mockResolvedValue([retiredBook]);
  render(<ContactsView />);
  await waitFor(() => expect(document.getElementById("contacts-status")).toHaveAttribute("data-status-kind", "retired"));
  expect(document.getElementById("contacts-book-retired")).toHaveAttribute("role", "status");
  expect(document.getElementById("contacts-book-retired")).not.toHaveTextContent("removed");
  expect(document.getElementById("contacts-sync-unavailable")).not.toBeInTheDocument();
  const sync = screen.getByRole("button", { name: "Sync now" });
  expect(sync).toHaveAttribute("title", "This book is retired.");
  expect(sync).toBeDisabled();
  expect(sync).not.toHaveAttribute("aria-describedby");
});

it("opens the retired deletion dialog from the row menu", async () => {
  vi.mocked(bridge.list_contact_books).mockResolvedValue([retiredBook]);
  render(<ContactsView />);
  fireEvent.click(await screen.findByRole("button", { name: "More actions for Phone" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Delete for all devices…" }));
  expect(screen.getByRole("dialog")).toHaveTextContent("Delete Phone's retired contact book for all devices?");
});

it.each([
  ["locked", { toneDisabled: true }, "Unlock sync to delete this book for all devices."],
  ["offline", { serverConnected: false }, "Connect to the server to delete this book for all devices."],
  ["syncing", { sync: { repairRequired: true } }, "Finish syncing to delete this book for all devices."],
  ["paused", { writesPaused: true }, undefined],
])("keeps local removal available while deletion is %s", async (_state, props, reason) => {
  vi.mocked(bridge.list_contact_books).mockResolvedValue([retiredBook]);
  render(<ContactsView {...props} />);
  const remove = await screen.findByRole("button", { name: "Remove from this computer" });
  const strip = document.getElementById("contacts-book-retired")!;
  const deleteButton = within(strip).getByRole("button", { name: "Delete for all devices…" });
  expect(remove).toBeEnabled();
  expect(deleteButton).toHaveAttribute("aria-disabled", "true");
  expect(deleteButton).toHaveAttribute("aria-describedby", "contacts-book-delete-reason");
  expect(document.getElementById("contacts-book-delete-reason")).toHaveTextContent(reason ?? deleteButton.getAttribute("title")!);
  fireEvent.click(screen.getByRole("button", { name: "More actions for Phone" }));
  expect(screen.getByRole("menuitem", { name: "Delete for all devices…" })).toHaveAttribute("aria-disabled", "true");
});

it("shows a deletion refusal in the dialog", async () => {
  vi.mocked(bridge.list_contact_books).mockResolvedValue([retiredBook]);
  vi.mocked(bridge.delete_contact_book).mockRejectedValue({ code: "contact-book-not-deletable" });
  render(<ContactsView />);
  fireEvent.click(await screen.findByRole("button", { name: "Delete for all devices…" }));
  const dialog = screen.getByRole("dialog");
  fireEvent.click(within(dialog).getByRole("checkbox"));
  fireEvent.click(within(dialog).getByRole("button", { name: "Delete for all devices" }));
  await waitFor(() => expect(document.getElementById("contacts-book-delete-error")).toHaveTextContent("This contact book can't be deleted for all devices."));
});

it("keeps the retired strip and selector available in narrow layout", async () => {
  stubMatchMedia(["(max-width: 700px)"]);
  vi.mocked(bridge.list_contact_books).mockResolvedValue([retiredBook, mockContactBook]);
  render(<ContactsView />);
  await screen.findByRole("button", { name: "Remove from this computer" });
  expect(document.getElementById("contacts-book-list")).not.toBeInTheDocument();
  expect(document.getElementById("contacts-book-retired")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Delete for all devices…" })).toBeInTheDocument();
  expect(within(document.getElementById("contacts-book-selector")!).getByRole("option", { name: "Phone, Retired" })).toBeInTheDocument();
});
