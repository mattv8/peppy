import "@testing-library/jest-dom/vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { bridge, type ContactBookView } from "../bridge/bridge";
import { ContactsView } from "./Contacts";
import { deferred, mockContactBook, resetContactBridgeMocks, restoreContactDomGlobals } from "./contacts-test-utils";
import { stubMatchMedia } from "../test-support/browserStubs";

vi.mock("../bridge/bridge", { spy: true });

beforeEach(() => resetContactBridgeMocks());
afterEach(() => { restoreContactDomGlobals(); vi.useRealTimers(); });

const noticeText = () => document.querySelector("#contacts-action-notice .contacts-action-notice-text")?.textContent;
const shortNotice = "Android phone's earlier contact book was replaced by its current one.";
const longNotice = `${shortNotice} No contact book is used for names or recipient search.`;

it("selects the book identifying the vanished selection and announces its replacement", async () => {
  let hint: (() => void) | undefined;
  const oldBook = { ...mockContactBook, id: "old", deviceName: "Android phone" };
  const other = { ...mockContactBook, id: "other", deviceName: "Other" };
  const replacement = { ...oldBook, id: "new", replacedBookIds: ["old"] };
  let books: ContactBookView[] = [oldBook, other, replacement];
  vi.mocked(bridge.subscribe).mockImplementation(listener => { hint = listener; return () => undefined; });
  vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
  render(<ContactsView />);
  await waitFor(() => expect(document.querySelector('button[data-book-id="old"]')).toBeInTheDocument());
  books = [other, replacement];
  act(() => hint?.());
  await waitFor(() => expect(document.querySelector("#contacts-books .contacts-book-row[aria-current]")).toHaveAttribute("data-book-id", "new"));
  expect(noticeText()).toBe(shortNotice);
});

async function setup(oldOverrides: Partial<ContactBookView> = {}, replacementOverrides: Partial<ContactBookView> = {}) {
  let hint: (() => void) | undefined;
  const oldBook = { ...mockContactBook, id: "old", deviceName: "Android phone", ...oldOverrides };
  const replacement = { ...oldBook, id: "new", replacedBookIds: ["old"], ...replacementOverrides };
  let books: ContactBookView[] = [oldBook, replacement];
  vi.mocked(bridge.subscribe).mockImplementation(listener => { hint = listener; return () => undefined; });
  vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
  render(<ContactsView />);
  await waitFor(() => expect(document.querySelector('button[data-book-id="old"]')).toBeInTheDocument());
  return {
    oldBook, replacement,
    trigger: (next: ContactBookView[]) => { books = next; hint?.(); },
    refresh: async (next: ContactBookView[]) => {
      books = next;
      act(() => hint?.());
      await waitFor(() => expect(document.querySelector('button[data-book-id="old"]')).not.toBeInTheDocument());
    },
  };
}

it("keeps excluded lookup unchanged, displays the extended notice and removes duplicate IDs", async () => {
  const { replacement, refresh } = await setup({}, { lookupExcluded: true });
  expect(document.querySelectorAll(".contacts-book-id")).toHaveLength(2);
  await refresh([replacement]);
  expect(noticeText()).toBe(longNotice);
  expect(screen.getByText("Hidden from names")).toBeInTheDocument();
  expect(screen.getByRole("switch")).not.toBeChecked();
  expect(bridge.set_contact_book_lookup).not.toHaveBeenCalled();
  expect(document.querySelector(".contacts-book-id")).toBeNull();
  expect(document.getElementById("contacts-list-title")).toHaveTextContent("Android phone");
});

it("does not announce an unselected disappearance", async () => {
  const { replacement, refresh } = await setup();
  fireEvent.click(document.querySelector('button[data-book-id="new"]')!);
  await refresh([replacement]);
  expect(document.getElementById("contacts-action-notice")).toBeEmptyDOMElement();
});

it("announces names stopping when an unselected replaced book disappears without moving focus", async () => {
  const { replacement, refresh } = await setup({}, { lookupExcluded: true });
  const row = document.querySelector<HTMLButtonElement>('button[data-book-id="new"]')!;
  fireEvent.click(row);
  row.focus();
  await refresh([replacement]);
  expect(row).toHaveFocus();
  expect(row).toHaveAttribute("aria-current");
  expect(noticeText()).toBe(longNotice);
});

it.each([
  [false, 4000, shortNotice],
  [true, 8000, longNotice],
])("holds the replacement notice when excluded is %s for %i ms", async (excluded, holdMs, text) => {
  const { replacement, trigger } = await setup({}, { lookupExcluded: excluded });
  vi.useFakeTimers();
  await act(async () => { trigger([replacement]); vi.advanceTimersByTime(150); });
  expect(noticeText()).toBe(text);
  act(() => vi.advanceTimersByTime(holdMs - 1));
  expect(noticeText()).toBe(text);
  act(() => vi.advanceTimersByTime(2));
  expect(noticeText()).toBeUndefined();
});

it("preserves the removed-phone notice over replacement", async () => {
  const { replacement, refresh } = await setup({ sourceRevoked: true });
  await refresh([replacement]);
  expect(document.getElementById("contacts-action-notice")).toHaveTextContent("Android phone's contacts were deleted from another device.");
});

it("keeps local removal behavior when a remaining book identifies the removed book", async () => {
  const oldBook: ContactBookView = { ...mockContactBook, id: "old", deviceName: "Android phone", state: "retired", sourceRevoked: true };
  const otherBook: ContactBookView = { ...mockContactBook, id: "other", deviceName: "Other" };
  const replacement: ContactBookView = { ...mockContactBook, id: "new", replacedBookIds: ["old"] };
  let books = [oldBook, otherBook, replacement];
  vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
  vi.mocked(bridge.forget_contact_book).mockImplementation(async id => {
    books = books.filter(book => book.id !== id);
  });
  render(<ContactsView />);
  fireEvent.click(await screen.findByRole("button", { name: "Remove from this computer" }));
  fireEvent.click(screen.getByRole("dialog").querySelector<HTMLButtonElement>(".destructive-button")!);
  await waitFor(() => expect(bridge.forget_contact_book).toHaveBeenCalledWith("old"));
  await waitFor(() => expect(document.querySelector('button[data-book-id="old"]')).not.toBeInTheDocument());
  const selectedRow = document.querySelector<HTMLButtonElement>("#contacts-books .contacts-book-row[aria-current]")!;
  expect(selectedRow).toHaveAttribute("data-book-id", "other");
  await waitFor(() => expect(selectedRow).toHaveFocus());
  expect(document.getElementById("contacts-action-notice")).toHaveTextContent("Removed Android phone from this computer.");
  expect(document.getElementById("contacts-action-notice")).not.toHaveTextContent("earlier contact book was replaced");
});

it("closes Recently deleted and returns focus to its trigger", async () => {
  const { replacement, refresh } = await setup();
  const trigger = screen.getByRole("button", { name: "Recently deleted contacts" });
  fireEvent.click(trigger);
  await waitFor(() => expect(document.getElementById("contacts-restore-panel")).toBeInTheDocument());
  await refresh([replacement]);
  expect(document.getElementById("contacts-restore-panel")).not.toBeInTheDocument();
  await waitFor(() => expect(trigger).toHaveFocus());
  expect(noticeText()).toBe(shortNotice);
});

it("moves focus from a vanished row's lookup switch to the replacement", async () => {
  const { replacement, refresh } = await setup();
  document.getElementById("contacts-book-use-switch-old")!.focus();
  await refresh([replacement]);
  expect(document.querySelector('button[data-book-id="new"]')).toHaveFocus();
});

it("does not move surviving focus after a replacement", async () => {
  const { replacement, refresh } = await setup();
  const trigger = screen.getByRole("button", { name: "Recently deleted contacts" });
  trigger.focus();
  await refresh([replacement]);
  expect(trigger).toHaveFocus();
});

it("moves focus from the vanished row to the replacement", async () => {
  const { replacement, refresh } = await setup();
  document.querySelector<HTMLButtonElement>('button[data-book-id="old"]')!.focus();
  await refresh([replacement]);
  await waitFor(() => expect(document.querySelector('button[data-book-id="new"]')).toHaveFocus());
});

it("uses the compact focus fallback when the rail is collapsed", async () => {
  stubMatchMedia(["(max-width: 700px)"]);
  let hint: (() => void) | undefined;
  const oldBook = { ...mockContactBook, id: "old" };
  const replacement = { ...oldBook, id: "new", replacedBookIds: ["old"] };
  let books = [oldBook, replacement];
  vi.mocked(bridge.subscribe).mockImplementation(listener => { hint = listener; return () => undefined; });
  vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
  render(<ContactsView />);
  await waitFor(() => expect(document.getElementById("contacts-book-selector")).toBeInTheDocument());
  books = [replacement];
  act(() => hint?.());
  await waitFor(() => expect(document.getElementById("contacts-list-title")).toHaveFocus());
});

it("retains an ordinary retired book without a notice", async () => {
  const { oldBook } = await setup({ state: "retired" });
  expect(document.querySelector('button[data-book-id="old"]')).toHaveAttribute("aria-current");
  expect(oldBook.state).toBe("retired");
  expect(document.getElementById("contacts-action-notice")).toBeEmptyDOMElement();
});

it("falls back silently when no survivor identifies the vanished book", async () => {
  const { replacement, refresh } = await setup({}, { replacedBookIds: undefined });
  await refresh([replacement]);
  expect(document.getElementById("contacts-action-notice")).toBeEmptyDOMElement();
});

it("isolates a bidirectional device name and allows dismissal", async () => {
  const { replacement, refresh } = await setup({ deviceName: "Phone \u202eABC" });
  await refresh([replacement]);
  const notice = document.getElementById("contacts-action-notice")!;
  expect(notice).toHaveAttribute("role", "status");
  expect(notice.querySelector("bdi")).toHaveTextContent("Phone");
  expect(screen.queryByRole("button", { name: "Try again" })).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Dismiss notification" }));
  expect(notice).toBeEmptyDOMElement();
});

it("clears old scan state while the replacement status is pending", async () => {
  const pending = deferred<Awaited<ReturnType<typeof bridge.get_contact_sync_request>>>();
  vi.mocked(bridge.get_contact_sync_request).mockImplementation(id => id === "new" ? pending.promise : Promise.resolve({ requestId: "scan", bookId: "old", state: "blocked", reason: "incomplete_scan" }));
  const { replacement, refresh } = await setup({ capabilities: { ...mockContactBook.capabilities, canSync: true } });
  await waitFor(() => expect(document.getElementById("contacts-status")).toHaveAttribute("data-status-kind", "scan"));
  await refresh([replacement]);
  expect(document.getElementById("contacts-status")).not.toBeInTheDocument();
  await act(async () => pending.resolve({ requestId: "new-scan", bookId: "new", state: "waiting" }));
  await waitFor(() => expect(document.getElementById("contacts-status")).toHaveAttribute("data-status-kind", "scan"));
});

it("clears old scan state during a silent fallback", async () => {
  const pending = deferred<Awaited<ReturnType<typeof bridge.get_contact_sync_request>>>();
  vi.mocked(bridge.get_contact_sync_request).mockImplementation(id => id === "new" ? pending.promise : Promise.resolve({ requestId: "scan", bookId: "old", state: "blocked" }));
  const { replacement, refresh } = await setup({ capabilities: { ...mockContactBook.capabilities, canSync: true } }, { replacedBookIds: undefined });
  await waitFor(() => expect(document.getElementById("contacts-status")).toHaveAttribute("data-status-kind", "scan"));
  await refresh([replacement]);
  expect(document.getElementById("contacts-status")).not.toBeInTheDocument();
  expect(noticeText()).toBeUndefined();
});
