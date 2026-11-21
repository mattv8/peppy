import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ContactsView } from "./Contacts";
import { bridge } from "../bridge/bridge";
import { contactPage, mockContactAurora, mockContactBook, mockContactRiver, resetContactBridgeMocks, restoreContactDomGlobals } from "./contacts-test-utils";

vi.mock("../bridge/bridge", { spy: true });

/** Filters the two fixture contacts by display name, as the core search does, and counts the matches. */
function mockSearchableBook() {
  vi.mocked(bridge.list_contacts_page).mockImplementation(async (_bookId, { through, query }) => contactPage(
    [mockContactAurora, mockContactRiver].filter(contact => !query || contact.displayName.toLowerCase().includes(query.toLowerCase())),
    through,
  ));
}

const countText = () => document.getElementById("contacts-count")!;

describe("ContactsView search", () => {
  beforeEach(() => {
    resetContactBridgeMocks();
    mockSearchableBook();
  });

  afterEach(() => restoreContactDomGlobals());

  it("shows the match count in the list header and sends the query to the server", async () => {
    const { rerender } = render(<ContactsView books={[mockContactBook]} query="" onClearQuery={vi.fn()} />);
    await screen.findByText("River Park");
    expect(countText()).toHaveAttribute("data-count-state", "known");

    rerender(<ContactsView books={[mockContactBook]} query="river" onClearQuery={vi.fn()} />);

    await waitFor(() => expect(countText()).toHaveTextContent("1 contact matches “river”"));
    expect(countText()).toHaveAttribute("data-count-state", "search");
    expect(bridge.list_contacts_page).toHaveBeenLastCalledWith("book-pixel8", expect.objectContaining({ query: "river" }));
    expect(screen.getByRole("list", { name: "Contacts in Pixel 8" })).toHaveTextContent("River Park");
    expect(screen.queryByText("Aurora Chen")).not.toBeInTheDocument();
  });

  it("pluralizes the match count", async () => {
    const { rerender } = render(<ContactsView books={[mockContactBook]} query="" onClearQuery={vi.fn()} />);
    await screen.findByText("River Park");

    rerender(<ContactsView books={[mockContactBook]} query="e" onClearQuery={vi.fn()} />);

    await waitFor(() => expect(countText()).toHaveTextContent("2 contacts match “e”"));
  });

  it("shows the no-match state with Clear search when nothing matches", async () => {
    const onClearQuery = vi.fn();
    render(<ContactsView books={[mockContactBook]} query="zzz" onClearQuery={onClearQuery} />);

    const noMatch = await waitFor(() => {
      const element = document.getElementById("contacts-no-match");
      expect(element).not.toBeNull();
      return element!;
    });
    expect(noMatch).toHaveAttribute("data-empty-state", "no-match");
    expect(noMatch.querySelector("h2")).toHaveTextContent("No contacts match “zzz”");
    expect(noMatch).toHaveTextContent("Searching Pixel 8 only. Search covers names, phone numbers, email addresses and organizations.");
    expect(document.getElementById("contacts-empty-state")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Clear search" }));

    expect(onClearQuery).toHaveBeenCalledTimes(1);
  });

  it("clears the query when a contact deep link arrives, so the lookup sees the whole book", async () => {
    const onClearQuery = vi.fn();
    render(
      <ContactsView
        books={[mockContactBook]}
        query="zzz"
        onClearQuery={onClearQuery}
        target={{ bookId: "book-pixel8", contactId: "contact-river", requestId: 1 }}
      />,
    );

    await waitFor(() => expect(onClearQuery).toHaveBeenCalled());
  });
});
