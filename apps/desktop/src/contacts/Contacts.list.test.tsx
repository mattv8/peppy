import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ContactsView } from "./Contacts";
import { bridge, type ContactBookView } from "../bridge/bridge";
import { contactPage, mockContactBook, mockContactRiver, resetContactBridgeMocks, restoreContactDomGlobals } from "./contacts-test-utils";

vi.mock("../bridge/bridge", { spy: true });

const otherBook: ContactBookView = { ...mockContactBook, id: "book-other", deviceName: "Other", contactCount: 1 };

function stubMediaQueries(matching: string[]) {
  vi.stubGlobal("matchMedia", vi.fn((query: string) => ({
    matches: matching.includes(query), media: query,
    addEventListener: vi.fn(), removeEventListener: vi.fn(),
  })));
}

describe("ContactsView list header", () => {
  beforeEach(() => resetContactBridgeMocks());
  afterEach(() => restoreContactDomGlobals());

  it("starts with a visually hidden page heading and keeps the actions in the list header", async () => {
    const { container } = render(<ContactsView />);
    const heading = container.querySelector("#contacts-view > h1");
    expect(heading).toHaveTextContent("Contacts");
    expect(heading).toHaveClass("visually-hidden");
    expect(document.getElementById("contacts-header-slot")).toBeNull();
    expect(document.getElementById("contacts-center-toolbar")).toBeNull();

    const header = document.getElementById("contacts-list-header")!;
    expect(header.closest("#contacts-center-pane")).toBe(document.getElementById("contacts-center-pane"));
    expect(header.firstElementChild).toBe(document.getElementById("contacts-list-heading"));
    const toolbar = header.querySelector("#contacts-header-actions");
    expect(toolbar).toHaveAttribute("role", "toolbar");
    expect(toolbar).toHaveAttribute("aria-label", "Contact actions");
    expect(await screen.findByRole("button", { name: "New contact" })).toBeInTheDocument();
    expect(toolbar!.querySelectorAll("button")).toHaveLength(4);
  });

  it("titles the list with the selected book's device name", async () => {
    render(<ContactsView />);
    expect(await screen.findByRole("heading", { level: 2, name: "Pixel 8" })).toHaveAttribute("id", "contacts-list-title");
  });

  it("adds the duplicate-name suffix to the title when another book shares the device name", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([mockContactBook, { ...otherBook, deviceName: "Pixel 8", id: "book-1234" }]);
    render(<ContactsView />);
    // The selected book is the first one; its id tail (`book-pixel8` → `xel8`) becomes the suffix.
    expect(await screen.findByRole("heading", { level: 2, name: "Pixel 8, ID xel8" })).toBeInTheDocument();
  });

  it("shows a loading count until the book list arrives", async () => {
    vi.mocked(bridge.list_contact_books).mockReturnValue(new Promise(() => {}));
    render(<ContactsView />);
    const count = document.getElementById("contacts-count")!;
    expect(count).toHaveTextContent("Loading contacts…");
    expect(count).toHaveAttribute("data-count-state", "loading");
  });

  it.each([
    [3, "3 contacts"],
    [1, "1 contact"],
    [0, "No contacts"],
  ])("counts a book with %i contacts as %s", async (contactCount, text) => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([{ ...mockContactBook, contactCount }]);
    vi.mocked(bridge.list_contacts_page).mockResolvedValue({ contacts: [], total: contactCount });
    render(<ContactsView />);
    expect(await screen.findByText(text)).toHaveAttribute("id", "contacts-count");
    expect(document.getElementById("contacts-count")).toHaveAttribute("data-count-state", "known");
  });

  it("shows the book selector in place of the title when the rail is hidden, and switches books through it", async () => {
    stubMediaQueries(["(max-width: 700px)"]);
    vi.mocked(bridge.list_contact_books).mockResolvedValue([mockContactBook, otherBook]);
    vi.mocked(bridge.list_contacts_page).mockImplementation(async bookId => ({ contacts: [], total: bookId === "book-other" ? 1 : 2 }));
    render(<ContactsView />);
    const selector = await screen.findByRole("combobox", { name: "Contact book" });
    expect(selector).toBe(document.getElementById("contacts-book-selector"));
    expect(document.getElementById("contacts-list-title")).toBeNull();
    expect(document.getElementById("contacts-book-list")).toBeNull();
    expect(screen.getByRole("option", { name: "Other, Active" })).toBeInTheDocument();

    fireEvent.change(selector, { target: { value: "book-other" } });
    await waitFor(() => expect(bridge.list_contacts_page).toHaveBeenLastCalledWith("book-other", { cursor: undefined, sort: expect.anything() }));
    expect(await screen.findByText("1 contact")).toBeInTheDocument();
  });

  it("keeps the title when only one book exists even if the rail is hidden", async () => {
    stubMediaQueries(["(max-width: 700px)"]);
    render(<ContactsView />);
    expect(await screen.findByRole("heading", { level: 2, name: "Pixel 8" })).toBeInTheDocument();
    expect(document.getElementById("contacts-book-selector")).toBeNull();
  });

  describe("name format", () => {
    it("shows names as first then last by default", async () => {
      render(<ContactsView />);
      expect(await screen.findByRole("button", { name: /^Aurora Chen,/ })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: /^River Park,/ })).toBeInTheDocument();
    });

    it("switches rows to last, first from the View menu and persists the choice", async () => {
      render(<ContactsView />);
      fireEvent.click(await screen.findByRole("button", { name: /^View options/ }));
      fireEvent.click(await screen.findByRole("menuitemradio", { name: /^Last, First/ }));

      expect(localStorage.getItem("peppy.contacts.nameFormat.v1")).toBe("last-first");
      expect(screen.queryByRole("menu", { name: "Sort contacts" })).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: /^Chen, Aurora,/ })).toBeInTheDocument();
      expect(document.querySelector("[data-contact-id='contact-aurora'] b")).toHaveTextContent("Chen, Aurora");
    });

    it("applies the saved name order on mount", async () => {
      localStorage.setItem("peppy.contacts.nameFormat.v1", "last-first");
      render(<ContactsView />);
      expect(await screen.findByRole("button", { name: /^Park, River,/ })).toBeInTheDocument();
    });

    it("marks the chosen name order and reflects it in the View button label", async () => {
      render(<ContactsView />);
      fireEvent.click(await screen.findByRole("button", { name: /^View options/ }));
      expect(screen.getByRole("menuitemradio", { name: /^First Last/ })).toHaveAttribute("aria-checked", "true");
      expect(screen.getByRole("menuitemradio", { name: /^Last, First/ })).toHaveAttribute("aria-checked", "false");
      fireEvent.click(screen.getByRole("menuitemradio", { name: /^Last, First/ }));

      const view = await screen.findByRole("button", { name: /^View options/ });
      expect(view).toHaveAttribute("title", "View options");
      expect(view).toHaveAccessibleName("View options. Sorted by last name, ascending. Names shown as last name, first name.");
    });

    it("reorders a structured Ada Lovelace name between the two View menu orders", async () => {
      vi.mocked(bridge.list_contacts_page).mockResolvedValue(contactPage([{ ...mockContactRiver, id: "contact-ada", displayName: "Ada Lovelace", givenName: "Ada", familyName: "Lovelace" }]));
      render(<ContactsView />);
      expect(await screen.findByRole("button", { name: /^Ada Lovelace,/ })).toBeInTheDocument();

      fireEvent.click(screen.getByRole("button", { name: /^View options/ }));
      fireEvent.click(await screen.findByRole("menuitemradio", { name: /^Last, First/ }));
      expect(await screen.findByRole("button", { name: /^Lovelace, Ada,/ })).toBeInTheDocument();

      fireEvent.click(screen.getByRole("button", { name: /^View options/ }));
      fireEvent.click(await screen.findByRole("menuitemradio", { name: /^First Last/ }));
      expect(await screen.findByRole("button", { name: /^Ada Lovelace,/ })).toBeInTheDocument();
    });

    it("keeps the detail heading in the chosen order", async () => {
      localStorage.setItem("peppy.contacts.nameFormat.v1", "last-first");
      render(<ContactsView />);
      fireEvent.click(await screen.findByRole("button", { name: /^Chen, Aurora,/ }));
      expect(await screen.findByRole("heading", { level: 2, name: "Chen, Aurora" })).toBeInTheDocument();
    });

    it("keeps the display name for a contact with only one structured name part", async () => {
      vi.mocked(bridge.list_contacts_page).mockResolvedValue(contactPage([{ ...mockContactRiver, givenName: undefined, familyName: "Park" }]));
      localStorage.setItem("peppy.contacts.nameFormat.v1", "last-first");
      render(<ContactsView />);
      expect(await screen.findByRole("button", { name: /^River Park,/ })).toBeInTheDocument();
    });
  });
});

describe("ContactsView with no contact books", () => {
  beforeEach(() => resetContactBridgeMocks());
  afterEach(() => restoreContactDomGlobals());

  it("renders no book rail, resize handle or lookup strip once the books load empty", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([]);
    const { container } = render(<ContactsView />);
    expect(await screen.findByRole("heading", { level: 2, name: "Contacts" })).toHaveAttribute("id", "contacts-list-title");
    expect(container.querySelector("#contacts-book-list")).toBeNull();
    expect(container.querySelector("#contacts-book-resize")).toBeNull();
    expect(container.querySelector("#contacts-book-use")).toBeNull();
  });

  it("drops the count and shows the no-books empty state without actions", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([]);
    const { container } = render(<ContactsView />);
    const empty = await waitFor(() => {
      const element = container.querySelector("#contacts-no-books");
      expect(element).not.toBeNull();
      return element!;
    });
    expect(empty).toHaveAttribute("data-empty-reason", "no-books");
    expect(empty).toHaveTextContent("No contact books yet");
    expect(empty).toHaveTextContent("Contacts appear here after a phone shares its contacts with Peppy.");
    expect(empty.querySelector("button")).toBeNull();
    expect(container.querySelector("#contacts-count")).toBeNull();
    expect(container.querySelector("#contacts-empty-state")).toBeNull();
    expect(container.querySelector("#contacts-list")).toBeNull();
  });

  it("shows no scan or readiness strip while there is no book to select", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([]);
    const { container } = render(<ContactsView />);
    await screen.findByText("No contact books yet");
    expect(container.querySelector("#contacts-status")).toBeNull();
    expect(container.querySelector("#contacts-sync-unavailable")).toBeNull();
    expect(screen.queryByText("Select a contact book to request a scan.")).not.toBeInTheDocument();
  });

  it("disables the toolbar actions and explains why in their titles", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([]);
    const { container } = render(<ContactsView />);
    await screen.findByText("No contact books yet");
    const buttons = Array.from(container.querySelectorAll<HTMLButtonElement>("#contacts-header-actions button"));
    expect(buttons).toHaveLength(4);
    for (const button of buttons) {
      expect(button).toBeDisabled();
      expect(button).toHaveAttribute("title", "Connect a phone to see its contacts.");
    }
    expect(screen.getByRole("button", { name: "New contact" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Sync now" })).toBeDisabled();
  });

  it("keeps the no-books title over the paused reason when both apply", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([]);
    const { container } = render(<ContactsView writesPaused />);
    await screen.findByText("No contact books yet");
    const buttons = Array.from(container.querySelectorAll<HTMLButtonElement>("#contacts-header-actions button"));
    for (const button of buttons) {
      expect(button).toBeDisabled();
      expect(button).toHaveAttribute("title", "Connect a phone to see its contacts.");
      expect(button).not.toHaveAttribute("aria-describedby");
    }
  });

  it("announces only that there are no books, never a 0 of 0 count", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([]);
    const { container } = render(<ContactsView />);
    await waitFor(() => expect(container.querySelector("#contacts-announcer")).toHaveTextContent("No contact books."));
    expect(container.querySelector("#contacts-announcer")).not.toHaveTextContent("contacts loaded");
  });

  it("keeps the skeleton and loading count until the books arrive", () => {
    vi.mocked(bridge.list_contact_books).mockReturnValue(new Promise(() => {}));
    const { container } = render(<ContactsView />);
    expect(container.querySelector("#contacts-no-books")).toBeNull();
    expect(container.querySelector("#contacts-skeleton")).toBeInTheDocument();
    expect(container.querySelector("#contacts-count")).toHaveTextContent("Loading contacts…");
    expect(container.querySelector("#contacts-announcer")).toHaveTextContent("Loading contacts.");
  });
});
