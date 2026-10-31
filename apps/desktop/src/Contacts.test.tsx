import "@testing-library/jest-dom/vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { StrictMode, useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ContactsView, type ContactTarget } from "./Contacts";
import {
  bridge,
  type ContactBookView,
  type ContactView,
  type ContactEditRequestInput,
} from "./bridge";

vi.mock("./bridge", { spy: true });

const mockContactBook: ContactBookView = {
  id: "book-pixel8",
  deviceName: "Pixel 8",
  state: "active",
  capabilities: {
    canWrite: true,
    canDelete: true,
    supportsNotes: true,
    supportsPhoto: true,
    supportsBirthday: true,
  },
  contactCount: 2,
  defaultAccountLabel: "Google",
  lastSyncAt: "Just now",
  pendingEditCount: 0,
};

const mockContactAurora: ContactView = {
  id: "contact-aurora",
  bookId: "book-pixel8",
  revision: "1",
  displayName: "Aurora Chen",
  givenName: "Aurora",
  familyName: "Chen",
  phones: [{ label: "mobile", number: "+12025550123" }],
  emails: [{ label: "work", address: "aurora@example.com" }],
  addresses: [],
  organization: "Northstar",
  title: "Director",
};

const mockContactRiver: ContactView = {
  id: "contact-river",
  bookId: "book-pixel8",
  revision: "1",
  displayName: "River Park",
  givenName: "River",
  familyName: "Park",
  phones: [{ label: "mobile", number: "+12025550124" }],
  emails: [],
  addresses: [],
};

const originalScrollIntoView = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollIntoView");
const storedPreferences = new Map<string, string>();
const preferenceStorage = {
  getItem: (key: string) => storedPreferences.get(key) ?? null,
  setItem: (key: string, value: string) => storedPreferences.set(key, value),
  removeItem: (key: string) => storedPreferences.delete(key),
};

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(resolvePromise => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
};

describe("ContactsView", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.stubGlobal("localStorage", preferenceStorage);
    storedPreferences.clear();
    localStorage.removeItem("peppy.contacts.sort.v1");
    localStorage.removeItem("peppy.layout.v1");
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
      configurable: true,
      value: vi.fn(),
    });
    vi.mocked(bridge.list_contact_books).mockResolvedValue([mockContactBook]);
    vi.mocked(bridge.list_contacts).mockResolvedValue([
      mockContactAurora,
      mockContactRiver,
    ]);
    vi.mocked(bridge.forget_contact_book).mockResolvedValue(undefined);
    vi.mocked(bridge.list_restorable_contacts).mockResolvedValue([]);
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    if (originalScrollIntoView) {
      Object.defineProperty(HTMLElement.prototype, "scrollIntoView", originalScrollIntoView);
    } else {
      delete (HTMLElement.prototype as { scrollIntoView?: unknown }).scrollIntoView;
    }
  });

  it("requests a fresh scan for the selected capable book", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([{ ...mockContactBook, capabilities: { ...mockContactBook.capabilities, canSync: true } }]);
    vi.mocked(bridge.request_contact_sync).mockResolvedValue({ requestId: "scan-1", bookId: "book-pixel8", state: "waiting" });
    render(<ContactsView />);
    fireEvent.click(await screen.findByRole("button", { name: "Sync now" }));
    await waitFor(() => expect(bridge.request_contact_sync).toHaveBeenCalledWith("book-pixel8"));
    expect(bridge.request_contact_repair).not.toHaveBeenCalled();
  });

  it("explains why limited and unready books cannot request a scan", async () => {
    let resolveBooks!: (books: ContactBookView[]) => void;
    vi.mocked(bridge.list_contact_books).mockImplementation(() => new Promise(resolve => { resolveBooks = resolve; }));
    render(<ContactsView sync={{ repairRequired: false, readiness: { state: "backfill_pending", contacts_ready: false, server_active: true, backfill_unreadable: 0 } }} />);
    const syncButton = await screen.findByRole("button", { name: "Sync now" });
    resolveBooks([{ ...mockContactBook, state: "limited", capabilities: { ...mockContactBook.capabilities, canSync: true } }]);
    expect(await screen.findByText("This phone has limited contact access and cannot complete an authoritative scan.")).toBeInTheDocument();
    expect(syncButton).toBeDisabled();
  });

  it.each([
    ["Add phone number", "Remove phone 1", "#contact-form-phones-list"],
    ["Add email address", "Remove email 1", "#contact-form-emails-list"],
    ["Add address", "Remove address 1", "#contact-form-addresses-list"],
  ])("keeps remaining field identity after removing an earlier row: %s", async (add, remove, list) => {
    const { container } = render(<ContactsView />);
    fireEvent.click(await screen.findByLabelText("New contact"));
    if (add === "Add address") fireEvent.click(container.querySelector('[data-form-disclosure="addresses"] > summary')!);
    fireEvent.click(screen.getByLabelText(add));
    fireEvent.click(screen.getByLabelText(add));
    const second = container.querySelectorAll(`${list} > li`)[1];
    expect(second).toBeTruthy();
    fireEvent.click(screen.getByLabelText(remove));
    expect(container.querySelector(`${list} > li`)).toBe(second);
  });

  it("shows retention paused even while contact production is ready", async () => {
    const readiness = { state: "ready" as const, contacts_ready: true, server_active: false, backfill_unreadable: 0 };
    const { container, rerender } = render(<ContactsView sync={{ repairRequired: false, readiness }} />);
    await screen.findByText("Pixel 8");
    expect(container.querySelector("#contacts-readiness-status")).toHaveAttribute("role", "status");
    rerender(<ContactsView sync={{ repairRequired: false, readiness: { ...readiness, server_active: true } }} />);
    expect(container.querySelector("#contacts-readiness-status")).toBeNull();
  });

  it("guards external navigation until a dirty contact is explicitly discarded", async () => {
    let guard: ((action: () => void) => void) | null = null;
    let navigated = false;
    render(<ContactsView onNavigationGuard={value => { guard = value; }} />);
    fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
    fireEvent.click(screen.getByLabelText("Edit contact"));
    fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Unsaved" } });
    await waitFor(() => expect(guard).not.toBeNull());
    fireEvent.click(screen.getByRole("button", { name: /River Park/ }));
    expect(screen.getByLabelText("Given name")).toHaveValue("Unsaved");
    fireEvent.click(screen.getByRole("button", { name: "Keep editing" }));
    // Exercise the host/rail navigation boundary, not a second contact-row click.
    act(() => guard?.(() => { navigated = true; }));
    expect(navigated).toBe(false);
    expect(screen.getByLabelText("Given name")).toHaveValue("Unsaved");
    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    expect(navigated).toBe(true);
  });

  it("shows the dirty Back confirmation inside the compact detail overlay", async () => {
    const originalMatchMedia = window.matchMedia;
    window.matchMedia = vi.fn((query: string) => ({
      matches: query === "(max-width: 560px)",
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })) as unknown as typeof window.matchMedia;
    try {
      const { container } = render(<ContactsView />);
      fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
      fireEvent.click(screen.getByLabelText("Edit contact"));
      fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Unsaved" } });
      fireEvent.click(screen.getByRole("button", { name: "Back" }));

      const panel = container.querySelector("#contacts-detail-panel");
      expect(panel?.querySelector("#contacts-unsaved-changes")).toBeInTheDocument();
      expect(container.querySelectorAll("#contacts-unsaved-changes")).toHaveLength(1);

      fireEvent.click(screen.getByRole("button", { name: "Keep editing" }));
      expect(container.querySelector("#contacts-unsaved-changes")).toBeNull();
      expect(screen.getByLabelText("Given name")).toHaveValue("Unsaved");

      fireEvent.click(screen.getByRole("button", { name: "Back" }));
      fireEvent.click(screen.getByRole("button", { name: "Discard" }));
      expect(container.querySelector("#contacts-detail-panel")).toBeNull();
      await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("button", { name: /Aurora Chen/ })));
    } finally {
      window.matchMedia = originalMatchMedia;
    }
  });

  it("moves compact detail focus into the overlay and returns it to the visible row", async () => {
    const originalMatchMedia = window.matchMedia;
    window.matchMedia = vi.fn((query: string) => ({
      matches: query === "(max-width: 560px)",
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })) as unknown as typeof window.matchMedia;
    try {
      const { container } = render(<ContactsView />);
      const row = await screen.findByRole("button", { name: /Aurora Chen/ });
      fireEvent.click(row);
      const back = await screen.findByRole("button", { name: "Back" });
      await waitFor(() => expect(document.activeElement).toBe(back));
      expect(container.querySelector("#contacts-center-pane")).toHaveAttribute("inert");

      fireEvent.click(back);
      await waitFor(() => expect(document.activeElement).toBe(row));
      expect(container.querySelector("#contacts-center-pane")).not.toHaveAttribute("inert");
    } finally {
      window.matchMedia = originalMatchMedia;
    }
  });

  it("keeps a target failure visible in the compact detail overlay", async () => {
    const originalMatchMedia = window.matchMedia;
    window.matchMedia = vi.fn((query: string) => ({
      matches: query === "(max-width: 560px)",
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })) as unknown as typeof window.matchMedia;
    try {
      const { container, rerender } = render(<ContactsView />);
      fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
      rerender(<ContactsView target={{ bookId: "missing-book", contactId: "missing", requestId: 70 }} />);

      const panel = container.querySelector("#contacts-detail-panel");
      await waitFor(() => expect(panel?.querySelector("#contacts-target-error")).toHaveTextContent("The requested contact book is no longer available."));
      expect(container.querySelector("#contacts-center-pane #contacts-target-error")).toBeNull();
    } finally {
      window.matchMedia = originalMatchMedia;
    }
  });

  it("returns compact create-form Back focus to New contact", async () => {
    const originalMatchMedia = window.matchMedia;
    window.matchMedia = vi.fn((query: string) => ({
      matches: query === "(max-width: 560px)",
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })) as unknown as typeof window.matchMedia;
    try {
      render(<ContactsView />);
      const newContact = await screen.findByRole("button", { name: "New contact" });
      fireEvent.click(newContact);
      fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Avery" } });
      fireEvent.click(screen.getByRole("button", { name: "Back" }));
      fireEvent.click(screen.getByRole("button", { name: "Discard" }));
      await waitFor(() => expect(document.activeElement).toBe(newContact));
    } finally {
      window.matchMedia = originalMatchMedia;
    }
  });

  describe("initial load", () => {
    it("loads and displays contact books", async () => {
      render(<ContactsView />);

      await waitFor(() => {
        expect(screen.getByText("Pixel 8")).toBeInTheDocument();
      });
    });

    it("loads and displays contacts from default book", async () => {
      render(<ContactsView />);

      await waitFor(() => {
        expect(screen.getByText("Aurora Chen")).toBeInTheDocument();
        expect(screen.getByText("River Park")).toBeInTheDocument();
      });
    });

    it("shows account label in book list", async () => {
      render(<ContactsView />);

      await waitFor(() => {
        expect(screen.getByText("Pixel 8")).toBeInTheDocument();
      });
    });
  });

  describe("selecting contacts", () => {
    it("shows contact detail when selected", async () => {
      render(<ContactsView />);

      await waitFor(() => {
        expect(screen.getByText("Aurora Chen")).toBeInTheDocument();
      });

      const auroraButton = screen.getByRole("button", { name: /Aurora Chen/ });
      fireEvent.click(auroraButton);

      await waitFor(() => {
        expect(screen.getByText("(202) 555-0123")).toBeInTheDocument();
      });
    });

    it("displays all contact fields in detail view", async () => {
      render(<ContactsView />);

      // findByRole waits for contacts to load before the click
      const auroraButton = await screen.findByRole("button", { name: /Aurora Chen/ }, { timeout: 3000 });
      fireEvent.click(auroraButton);

      // Email renders as "work · aurora@example.com" — use regex for substring
      await screen.findByText(/aurora@example\.com/, {}, { timeout: 3000 });
      // "Northstar · Director" is the full small-element text in the detail header
      await screen.findByText("Northstar · Director", {}, { timeout: 3000 });
    });
  });

  describe("creating contacts", () => {
    it("keeps empty optional contact fields collapsed until their disclosure is opened", async () => {
      const { container } = render(<ContactsView />);

      fireEvent.click(await screen.findByLabelText("New contact"));

      const organization = container.querySelector<HTMLDetailsElement>('[data-form-disclosure="organization"]');
      expect(organization).not.toHaveAttribute("open");

      fireEvent.click(container.querySelector('[data-form-disclosure="organization"] > summary')!);
      expect(organization).toHaveAttribute("open");
    });

    it("reveals and focuses birthday after the native host rejects it", async () => {
      vi.mocked(bridge.submit_contact_edit).mockResolvedValue({
        state: "rejected",
        reason: "The birthday is invalid.",
      });
      const { container } = render(<ContactsView />);

      fireEvent.click(await screen.findByLabelText("New contact"));
      fireEvent.click(container.querySelector('[data-form-disclosure="birthday"] > summary')!);
      fireEvent.change(screen.getByLabelText("Month"), { target: { value: "2" } });
      fireEvent.click(container.querySelector('[data-form-disclosure="birthday"] > summary')!);
      fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Avery" } });
      fireEvent.click(screen.getByText("Save"));

      await waitFor(() => expect(screen.getByRole("alert")).toBeInTheDocument());
      await waitFor(() => {
        expect(container.querySelector('[data-form-disclosure="birthday"]')).toHaveAttribute("open");
        expect(document.activeElement).toBe(screen.getByLabelText("Month"));
      });
      const givenName = screen.getByLabelText("Given name");
      givenName.focus();
      fireEvent.change(givenName, { target: { value: "Avery Lynn" } });
      expect(document.activeElement).toBe(givenName);
    });

    it("reveals populated disclosures for a thrown native validation error without stealing focus", async () => {
      vi.mocked(bridge.submit_contact_edit).mockRejectedValue({ message: "A contact value is too long." });
      const { container } = render(<ContactsView />);

      fireEvent.click(await screen.findByLabelText("New contact"));
      fireEvent.click(container.querySelector('[data-form-disclosure="organization"] > summary')!);
      fireEvent.change(screen.getByLabelText("Organization"), { target: { value: "Northstar" } });
      fireEvent.click(container.querySelector('[data-form-disclosure="organization"] > summary')!);
      fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Avery" } });
      fireEvent.click(screen.getByText("Save"));

      expect(await screen.findByRole("alert")).toHaveTextContent("A contact value is too long.");
      expect(container.querySelector('[data-form-disclosure="organization"]')).toHaveAttribute("open");
      expect(document.activeElement).not.toBe(screen.getByLabelText("Organization"));
    });

    it("opens create form on new contact button", async () => {
      render(<ContactsView />);

      await waitFor(() => {
        expect(screen.getByLabelText("New contact")).toBeInTheDocument();
      });

      fireEvent.click(screen.getByLabelText("New contact"));

      await waitFor(() => {
        expect(screen.getByRole("form")).toHaveAccessibleName("Create contact");
      });
      expect(screen.getByLabelText("Recently deleted contacts")).toBeDisabled();
    });

    it("calls submit_contact_edit with create kind", async () => {
      render(<ContactsView />);
      await screen.findByText("Pixel 8");

      fireEvent.click(screen.getByLabelText("New contact"));

      await waitFor(() => {
        expect(screen.getByRole("form")).toBeInTheDocument();
      });

      const givenNameInput = screen.getByLabelText("Given name");
      fireEvent.change(givenNameInput, { target: { value: "Alice" } });

      const saveButton = screen.getByText("Save");
      fireEvent.click(saveButton);

      await waitFor(() => {
        const calls = vi.mocked(bridge.submit_contact_edit).mock.calls;
        expect(calls.length).toBeGreaterThan(0);
        expect(calls[0][0].kind).toBe("create");
      });
    });

    it("includes changed fields in patches", async () => {
      render(<ContactsView />);
      await screen.findByText("Pixel 8");

      fireEvent.click(screen.getByLabelText("New contact"));

      await waitFor(() => {
        expect(screen.getByRole("form")).toBeInTheDocument();
      });

      const givenInput = screen.getByLabelText("Given name");
      fireEvent.change(givenInput, { target: { value: "Bob" } });

      const saveButton = screen.getByText("Save");
      fireEvent.click(saveButton);

      await waitFor(() => {
        const calls = vi.mocked(bridge.submit_contact_edit).mock.calls;
        const call = calls[calls.length - 1][0];
        expect(call.patches).toBeDefined();
      });
    });

    it("disables save when name is empty", async () => {
      render(<ContactsView />);
      await screen.findByText("Pixel 8");

      fireEvent.click(screen.getByLabelText("New contact"));

      await waitFor(() => {
        expect(screen.getByRole("form")).toBeInTheDocument();
      });

      const saveButton = screen.getByText("Save");
      expect(saveButton).toBeDisabled();
    });
  });

  describe("editing contacts", () => {
    it("opens edit form when edit button clicked", async () => {
      render(<ContactsView />);

      await waitFor(() => {
        expect(screen.getByText("Aurora Chen")).toBeInTheDocument();
      });

      const auroraButton = screen.getByRole("button", { name: /Aurora Chen/ });
      fireEvent.click(auroraButton);

      fireEvent.click(screen.getByLabelText("Edit contact"));

      await waitFor(() => {
        expect(screen.getByRole("form")).toHaveAccessibleName("Edit contact");
      });
    });

    it("populates form with existing contact data", async () => {
      render(<ContactsView />);

      await waitFor(() => {
        expect(screen.getByText("Aurora Chen")).toBeInTheDocument();
      });

      const auroraButton = screen.getByRole("button", { name: /Aurora Chen/ });
      fireEvent.click(auroraButton);

      fireEvent.click(screen.getByLabelText("Edit contact"));

      await waitFor(() => {
        expect(screen.getByDisplayValue("Aurora")).toBeDefined();
      });
    });

    it("opens populated optional fields in the editor", async () => {
      const { container } = render(<ContactsView />);

      fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
      fireEvent.click(screen.getByLabelText("Edit contact"));

      expect(container.querySelector('[data-form-disclosure="organization"]')).toHaveAttribute("open");
      expect(container.querySelector('[data-form-disclosure="title"]')).toHaveAttribute("open");
    });

    it("sends update kind with contact id and revision", async () => {
      render(<ContactsView />);

      await waitFor(() => {
        expect(screen.getByText("Aurora Chen")).toBeInTheDocument();
      });

      const auroraButton = screen.getByRole("button", { name: /Aurora Chen/ });
      fireEvent.click(auroraButton);

      fireEvent.click(screen.getByLabelText("Edit contact"));

      await waitFor(() => {
        expect(screen.getByRole("form")).toBeInTheDocument();
      });

      const saveButton = screen.getByText("Save");
      fireEvent.click(saveButton);

      await waitFor(() => {
        const calls = vi.mocked(bridge.submit_contact_edit).mock.calls;
        const call = calls[calls.length - 1][0];
        expect(call.kind).toBe("update");
        expect(call.contactId).toBe("contact-aurora");
      });
    });
  });

  describe("deleting contacts", () => {
    it("shows confirmation on delete button click", async () => {
      render(<ContactsView />);

      await waitFor(() => {
        const auroraButton = screen.getByRole("button", { name: /Aurora Chen/ });
        fireEvent.click(auroraButton);
      });

      await waitFor(() => {
        const deleteButton = screen.getByLabelText("Delete contact");
        expect(deleteButton).toHaveTextContent("Delete");
        fireEvent.click(deleteButton);
      });

      await waitFor(() => {
        expect(screen.getByLabelText("Confirm delete")).toHaveTextContent(
          "Confirm delete"
        );
      });
    });

    it("calls submit_contact_edit with delete kind", async () => {
      vi.mocked(bridge.submit_contact_edit).mockResolvedValue({
        state: "applied",
        newRevision: "2",
      });

      render(<ContactsView />);

      await waitFor(() => {
        expect(screen.getByText("Aurora Chen")).toBeInTheDocument();
      });

      const auroraButton = screen.getByRole("button", { name: /Aurora Chen/ });
      fireEvent.click(auroraButton);

      const deleteButton = screen.getByLabelText("Delete contact");
      fireEvent.click(deleteButton);

      await waitFor(() => {
        fireEvent.click(deleteButton);
      });

      await waitFor(() => {
        const calls = vi.mocked(bridge.submit_contact_edit).mock.calls;
        const call = calls[calls.length - 1][0];
        expect(call.kind).toBe("delete");
      });
    });

    it("clears contact selection after delete", async () => {
      vi.mocked(bridge.submit_contact_edit).mockResolvedValue({
        state: "applied",
        newRevision: "2",
      });

      render(<ContactsView />);

      await waitFor(() => {
        expect(screen.getByText("Aurora Chen")).toBeInTheDocument();
      });

      const auroraButton = screen.getByRole("button", { name: /Aurora Chen/ });
      fireEvent.click(auroraButton);

      const deleteButton = screen.getByLabelText("Delete contact");
      fireEvent.click(deleteButton);

      fireEvent.click(deleteButton);

      await waitFor(() => {
        expect(screen.queryByRole("form")).not.toBeInTheDocument();
      }, { timeout: 2000 });
    });
  });

  describe("phone management", () => {
    it("adds phone numbers in create form", async () => {
      render(<ContactsView />);
      await screen.findByText("Pixel 8");

      fireEvent.click(screen.getByLabelText("New contact"));

      await waitFor(() => {
        expect(screen.getByRole("form")).toBeInTheDocument();
      });

      const addPhoneButton = screen.getByLabelText("Add phone number");
      fireEvent.click(addPhoneButton);

      await waitFor(() => {
        // After adding one phone, the select defaults to "Mobile" label
        const phoneSelects = screen.getAllByDisplayValue("Mobile");
        expect(phoneSelects.length).toBeGreaterThanOrEqual(1);
      });
    });

    it("removes phone numbers", async () => {
      render(<ContactsView />);
      await screen.findByText("Pixel 8");

      fireEvent.click(screen.getByLabelText("New contact"));

      await waitFor(() => {
        expect(screen.getByRole("form")).toBeInTheDocument();
      });

      // Add a phone first so we have something to remove
      const addPhoneButton = screen.getByLabelText("Add phone number");
      fireEvent.click(addPhoneButton);

      await waitFor(() => {
        expect(screen.getByLabelText("Remove phone 1")).toBeInTheDocument();
      });

      const removeButton = screen.getByLabelText("Remove phone 1");
      fireEvent.click(removeButton);

      await waitFor(() => {
        const remainingRemoveButtons = screen.queryAllByLabelText(/Remove phone/);
        expect(remainingRemoveButtons.length).toBe(0);
      });
    });
  });

  describe("error handling", () => {
    it("displays conflict error from server", async () => {
      vi.mocked(bridge.submit_contact_edit).mockResolvedValue({
        state: "conflict",
        conflictSummary: "Contact was modified elsewhere",
      });

      render(<ContactsView />);

      // Wait for books to load so book is set before handleSave runs
      await screen.findByText("Pixel 8", {}, { timeout: 3000 });

      fireEvent.click(screen.getByLabelText("New contact"));

      await waitFor(() => expect(screen.getByRole("form")).toBeInTheDocument());

      const conflictInput = screen.getByLabelText("Given name");
      fireEvent.change(conflictInput, { target: { value: "Test" } });

      fireEvent.click(screen.getByText("Save"));

      await waitFor(() => {
        expect(screen.getByText(/Conflict/)).toBeInTheDocument();
      });
    });

    it("displays rejection error from server", async () => {
      vi.mocked(bridge.submit_contact_edit).mockResolvedValue({
        state: "rejected",
        reason: "A contact value is invalid.",
      });

      render(<ContactsView />);

      // Wait for books to load so book is set before handleSave runs
      await screen.findByText("Pixel 8", {}, { timeout: 3000 });

      fireEvent.click(screen.getByLabelText("New contact"));

      await waitFor(() => expect(screen.getByRole("form")).toBeInTheDocument());

      const rejectionInput = screen.getByLabelText("Given name");
      fireEvent.change(rejectionInput, { target: { value: "Test" } });

      fireEvent.click(screen.getByText("Save"));

      await waitFor(() => {
        expect(screen.getByText(/A contact value is invalid/)).toBeInTheDocument();
      });
    });
  });

  describe("state management", () => {
    it("closes editor on cancel", async () => {
      render(<ContactsView />);
      await screen.findByText("Pixel 8");

      fireEvent.click(screen.getByLabelText("New contact"));

      await waitFor(() => {
        expect(screen.getByRole("form")).toBeInTheDocument();
      });

      fireEvent.click(screen.getByText("Cancel"));

      await waitFor(() => {
        expect(screen.queryByRole("form")).not.toBeInTheDocument();
      });
    });

    it("reloads contacts after successful save", async () => {
      vi.mocked(bridge.list_contacts).mockClear();
      vi.mocked(bridge.list_contacts).mockResolvedValue([mockContactAurora]);

      render(<ContactsView />);
      await screen.findByText("Pixel 8");

      fireEvent.click(screen.getByLabelText("New contact"));

      const reloadInput = await screen.findByLabelText("Given name");
      fireEvent.change(reloadInput, { target: { value: "Test" } });

      fireEvent.click(screen.getByText("Save"));

      await waitFor(() => {
        expect(vi.mocked(bridge.list_contacts).mock.calls.length).toBeGreaterThan(0);
      });
    });
  });

  describe("accessibility behaviors", () => {
    it("supports menu radio keyboard navigation, selection, and dismissal", async () => {
      render(<ContactsView />);
      const trigger = await screen.findByRole("button", { name: /Sort:/ });
      fireEvent.keyDown(trigger, { key: "ArrowDown" });
      const menu = await screen.findByRole("menu", { name: "Sort contacts" });
      const lastName = screen.getByRole("menuitemradio", { name: "Last name" });
      await waitFor(() => expect(document.activeElement).toBe(lastName));
      fireEvent.keyDown(menu, { key: "End" });
      expect(document.activeElement).toBe(screen.getByRole("menuitemradio", { name: "Descending" }));
      fireEvent.keyDown(menu, { key: "Home" });
      expect(document.activeElement).toBe(lastName);
      fireEvent.keyDown(menu, { key: "ArrowDown" });
      fireEvent.click(document.activeElement as HTMLButtonElement);
      await waitFor(() => expect(bridge.list_contacts).toHaveBeenLastCalledWith("book-pixel8", "", 0, { key: "given", direction: "asc" }));
      expect(JSON.parse(localStorage.getItem("peppy.contacts.sort.v1")!)).toEqual({ key: "given", direction: "asc" });
      expect(screen.queryByRole("menu", { name: "Sort contacts" })).not.toBeInTheDocument();
      await waitFor(() => expect(document.activeElement).toBe(trigger));
      expect(trigger).toHaveAttribute("title", "Sort: First name, ascending");

      fireEvent.click(trigger);
      fireEvent.keyDown(screen.getByRole("menu", { name: "Sort contacts" }), { key: "Tab" });
      expect(screen.queryByRole("menu", { name: "Sort contacts" })).not.toBeInTheDocument();
      await waitFor(() => expect(document.activeElement).toBe(trigger));

      fireEvent.click(trigger);
      fireEvent.keyDown(screen.getByRole("menu", { name: "Sort contacts" }), { key: "Escape" });
      expect(screen.queryByRole("menu", { name: "Sort contacts" })).not.toBeInTheDocument();
      expect(document.activeElement).toBe(trigger);
    });

    it("renders header actions in the supplied host on the first Contacts mount", async () => {
      function ContactsWithHeaderHost() {
        const [host, setHost] = useState<HTMLDivElement | null>(null);
        return <><div id="contacts-header-slot" ref={setHost} /><ContactsView headerHost={host} /></>;
      }
      render(<ContactsWithHeaderHost />);
      const host = document.getElementById("contacts-header-slot")!;
      expect(await screen.findByRole("button", { name: "New contact" })).toBe(host.querySelector("button[data-action='create-contact']"));
    });

    it("portals the sort menu outside an overflow-clipped header", async () => {
      function ContactsWithClippedHeader() {
        const [host, setHost] = useState<HTMLDivElement | null>(null);
        return <div id="desktop-shell" className="theme-dark"><header id="thread-pane-header"><div id="contacts-header-slot" ref={setHost} /></header><ContactsView headerHost={host} /></div>;
      }
      render(<ContactsWithClippedHeader />);
      const header = document.getElementById("thread-pane-header")!;
      header.scrollTop = 0;
      fireEvent.click(await screen.findByRole("button", { name: /Sort:/ }));
      const menu = await screen.findByRole("menu", { name: "Sort contacts" });
      expect(menu.parentElement).toBe(document.getElementById("desktop-shell"));
      expect(header).not.toContainElement(menu);
      expect(header.scrollTop).toBe(0);
    });

    it("sanitizes stored sort preferences before forwarding them to the bridge", async () => {
      localStorage.setItem("peppy.contacts.sort.v1", JSON.stringify({ key: "given", direction: "desc", unexpected: true }));
      render(<ContactsView />);
      await waitFor(() => expect(bridge.list_contacts).toHaveBeenCalledWith("book-pixel8", "", 0, { key: "given", direction: "desc" }));
    });

    it("resizes, resets, and persists the detail width without replacing a dirty form", async () => {
      render(<ContactsView />);
      fireEvent.click(await screen.findByLabelText("New contact"));
      const input = await screen.findByLabelText("Given name");
      fireEvent.change(input, { target: { value: "Unsaved" } });
      const handle = screen.getByRole("separator", { name: "Resize contact detail" });
      fireEvent.keyDown(handle, { key: "ArrowLeft", shiftKey: true });
      expect(screen.getByLabelText("Given name")).toHaveValue("Unsaved");
      fireEvent.keyDown(handle, { key: "Home" });
      expect(JSON.parse(localStorage.getItem("peppy.layout.v1")!).contactsDetailWidth).toBe(280);
      fireEvent.doubleClick(handle);
      expect(JSON.parse(localStorage.getItem("peppy.layout.v1")!).contactsDetailWidth).toBe(320);
    });

    it("clamps corrupt saved detail widths and keyboard-resizes from the rendered width", async () => {
      const originalWidth = Object.getOwnPropertyDescriptor(window, "innerWidth");
      try {
        Object.defineProperty(window, "innerWidth", { configurable: true, value: 580 });
        localStorage.setItem("peppy.layout.v1", JSON.stringify({ contactsDetailWidth: 480 }));
        render(<ContactsView />);
        fireEvent.click(await screen.findByLabelText("New contact"));
        const handle = screen.getByRole("separator", { name: "Resize contact detail" });
        await waitFor(() => expect(handle).toHaveAttribute("aria-valuenow", "339"));
        fireEvent.keyDown(handle, { key: "ArrowRight" });
        expect(handle).toHaveAttribute("aria-valuenow", "331");
      } finally {
        if (originalWidth) Object.defineProperty(window, "innerWidth", originalWidth);
        else delete (window as { innerWidth?: number }).innerWidth;
      }
    });

    it("clamps an invalid persisted detail width to the minimum on a wide view", async () => {
      localStorage.setItem("peppy.layout.v1", JSON.stringify({ contactsDetailWidth: 0 }));
      render(<ContactsView />);
      fireEvent.click(await screen.findByLabelText("New contact"));
      expect(await screen.findByRole("separator", { name: "Resize contact detail" })).toHaveAttribute("aria-valuenow", "280");
    });

    it("contact row button has an aria-label that includes display name and formatted phone", async () => {
      render(<ContactsView />);

      await waitFor(() => {
        const btn = screen.getByRole("button", { name: /Aurora Chen/ });
        // aria-label must include national-formatted phone, not raw E.164
        expect(btn).toHaveAttribute("aria-label", expect.stringContaining("(202) 555-0123"));
        expect(btn).toHaveAttribute("aria-label", expect.stringContaining("Aurora Chen"));
      });
    });

    it("form inputs have labels that activate focus via htmlFor", async () => {
      render(<ContactsView />);
      await screen.findByText("Pixel 8");

      fireEvent.click(screen.getByLabelText("New contact"));

      await waitFor(() => {
        expect(screen.getByRole("form")).toBeInTheDocument();
      });

      // getByLabelText with selector="input" verifies htmlFor wiring
      const givenInput = screen.getByLabelText("Given name");
      expect(givenInput.tagName).toBe("INPUT");
      expect(givenInput).toHaveAttribute("id", "contact-form-given-name");

      const familyInput = screen.getByLabelText("Family name");
      expect(familyInput.tagName).toBe("INPUT");
      expect(familyInput).toHaveAttribute("id", "contact-form-family-name");
    });

    it("delete confirm state resets on Escape keydown", async () => {
      render(<ContactsView />);

      await waitFor(() => {
        expect(screen.getByText("Aurora Chen")).toBeInTheDocument();
      });

      const auroraButton = screen.getByRole("button", { name: /Aurora Chen/ });
      fireEvent.click(auroraButton);

      await waitFor(() => {
        expect(screen.getByLabelText("Delete contact")).toBeInTheDocument();
      });

      const deleteBtn = screen.getByLabelText("Delete contact");
      fireEvent.click(deleteBtn);

      await waitFor(() => {
        expect(screen.getByLabelText("Confirm delete")).toHaveTextContent("Confirm delete");
      });

      // Escape should reset the confirmation state
      fireEvent.keyDown(screen.getByLabelText("Confirm delete"), { key: "Escape" });

      await waitFor(() => {
        expect(screen.getByLabelText("Delete contact")).toHaveTextContent("Delete");
      });
    });

    it("pending state banner has role=status for screen reader announcement", async () => {
      const contactWithPending: typeof mockContactAurora = {
        ...mockContactAurora,
        pendingEditState: "pending",
        pendingEditSummary: "Phone number update",
      };
      vi.mocked(bridge.list_contacts).mockResolvedValue([contactWithPending, mockContactRiver]);

      render(<ContactsView />);

      await waitFor(() => {
        expect(screen.getByText("Aurora Chen")).toBeInTheDocument();
      });

      const auroraButton = screen.getByRole("button", { name: /Aurora Chen/ });
      fireEvent.click(auroraButton);

      await waitFor(() => {
        const banner = document.querySelector("[data-contact-banner]");
        expect(banner).toBeInTheDocument();
        expect(banner).toHaveAttribute("role", "status");
      });
    });

    it("form fields are readOnly when book canWrite is false", async () => {
      const readOnlyBook = {
        ...mockContactBook,
        capabilities: { canWrite: false, canDelete: false, supportsNotes: false, supportsPhoto: false },
      };
      vi.mocked(bridge.list_contact_books).mockResolvedValue([readOnlyBook]);
      vi.mocked(bridge.list_contacts).mockResolvedValue([mockContactAurora]);

      render(<ContactsView />);

      // Wait for books and contacts to load
      await screen.findByText("Aurora Chen", {}, { timeout: 3000 });

      // When canWrite=false, the Edit button is intentionally hidden.
      // Open the form via New Contact to verify fieldReadOnly applies to new forms too.
      // (The canWrite gate on Edit button is separate from per-field readOnly.)
      // Use New Contact to open form with the readOnly book active.
      fireEvent.click(screen.getByLabelText("New contact"));

      await waitFor(() => expect(screen.getByRole("form")).toBeInTheDocument());

      const givenInput = screen.getByLabelText("Given name");
      expect(givenInput).toHaveAttribute("readonly");
    });
  });

  describe("live updates, paging and honest status", () => {
    it("keeps requesting state independent from a concurrent status read", async () => {
      let resolveRequest!: (status: { requestId: string; bookId: string; state: "waiting" }) => void;
      vi.mocked(bridge.list_contact_books).mockResolvedValue([{ ...mockContactBook, capabilities: { ...mockContactBook.capabilities, canSync: true } }]);
      vi.mocked(bridge.get_contact_sync_request).mockResolvedValue({ requestId: "old", bookId: "book-pixel8", state: "waiting" });
      vi.mocked(bridge.request_contact_sync).mockImplementation(() => new Promise(resolve => { resolveRequest = resolve; }));
      render(<ContactsView />);
      const button = await screen.findByRole("button", { name: "Sync now" });
      fireEvent.click(button);
      expect(screen.getByRole("button", { name: "Requesting…" })).toBeDisabled();
      resolveRequest({ requestId: "new", bookId: "book-pixel8", state: "waiting" });
      expect(await screen.findByText("Waiting for the phone to scan contacts.")).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Sync now" })).toBeEnabled();
    });

    it("does not replace the selected book status with a stale status response", async () => {
      let resolveFirst!: (status: { requestId: string; bookId: string; state: "waiting" } | null) => void;
      const secondBook = { ...mockContactBook, id: "book-iphone", deviceName: "iPhone", capabilities: { ...mockContactBook.capabilities, canSync: true } };
      vi.mocked(bridge.list_contact_books).mockResolvedValue([{ ...mockContactBook, capabilities: { ...mockContactBook.capabilities, canSync: true } }, secondBook]);
      vi.mocked(bridge.get_contact_sync_request).mockImplementation(bookId => bookId === "book-pixel8"
        ? new Promise(resolve => { resolveFirst = resolve; })
        : Promise.resolve({ requestId: "iphone", bookId, state: "scanning" }));
      render(<ContactsView />);
      fireEvent.click(await screen.findByRole("button", { name: /iPhone/ }));
      expect(await screen.findByText("The phone is scanning contacts.")).toBeInTheDocument();
      await act(async () => { resolveFirst({ requestId: "pixel", bookId: "book-pixel8", state: "waiting" }); });
      expect(screen.queryByText("Waiting for the phone to scan contacts.")).not.toBeInTheDocument();
    });

    it("refreshes the page on native hints without discarding an open editor", async () => {
      let hint: (() => void) | undefined;
      vi.mocked(bridge.subscribe).mockImplementation(listener => {
        hint = listener;
        return () => undefined;
      });
      render(<ContactsView />);
      fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
      fireEvent.click(await screen.findByLabelText("Edit contact"));
      const given = await screen.findByLabelText("Given name");
      fireEvent.change(given, { target: { value: "Aurelia" } });

      vi.mocked(bridge.list_contacts).mockResolvedValue([
        { ...mockContactAurora, pendingEditId: "r1", pendingEditState: "awaiting-approval", pendingEditSummary: "Name" },
        mockContactRiver,
      ]);
      const before = vi.mocked(bridge.list_contacts).mock.calls.length;
      hint?.();
      await waitFor(() => expect(vi.mocked(bridge.list_contacts).mock.calls.length).toBeGreaterThan(before));
      expect(screen.getByLabelText("Given name")).toHaveValue("Aurelia");
      expect(screen.getByRole("form")).toHaveAccessibleName("Edit contact");
      expect(await screen.findByRole("button", { name: /Waiting for approval on the phone/ })).toBeInTheDocument();
    });

    it("refreshes a pending scan status from a native hint", async () => {
      let hint: (() => void) | undefined;
      vi.mocked(bridge.list_contact_books).mockResolvedValue([{ ...mockContactBook, capabilities: { ...mockContactBook.capabilities, canSync: true } }]);
      vi.mocked(bridge.subscribe).mockImplementation(listener => { hint = listener; return () => undefined; });
      vi.mocked(bridge.get_contact_sync_request).mockResolvedValue({ requestId: "scan", bookId: "book-pixel8", state: "waiting" });
      render(<ContactsView />);
      await screen.findByText("Pixel 8");
      await waitFor(() => expect(screen.getByText("Waiting for the phone to scan contacts.")).toBeInTheDocument());
      vi.mocked(bridge.get_contact_sync_request).mockResolvedValue({ requestId: "scan", bookId: "book-pixel8", state: "completed" });
      hint?.();
      expect(await screen.findByText("Scan completed.")).toBeInTheDocument();
    });

    it("pages beyond 200 contacts with bounded Previous/Next requests", async () => {
      const page = Array.from({ length: 200 }, (_, index) => ({ ...mockContactRiver, id: `c-${index}`, displayName: `Person ${index}` }));
      vi.mocked(bridge.list_contacts).mockImplementation(async (_book, _query, offset) =>
        offset ? [{ ...mockContactRiver, id: "c-200", displayName: "Person 200" }] : page,
      );
      render(<ContactsView />);
      await screen.findByText("Person 0");
      expect(document.querySelectorAll("#contacts-list > li")).toHaveLength(200);
      fireEvent.click(screen.getByRole("button", { name: "Next" }));
      await screen.findByText("Person 200");
      expect(vi.mocked(bridge.list_contacts)).toHaveBeenLastCalledWith("book-pixel8", "", 200, { key: "family", direction: "asc" });
      expect(screen.getByRole("button", { name: "Next" })).toBeDisabled();
      fireEvent.click(screen.getByRole("button", { name: "Previous" }));
      await screen.findByText("Person 0");
    });

    it("reports a sent edit as waiting for the phone, never as applied", async () => {
      vi.mocked(bridge.submit_contact_edit).mockResolvedValue({ state: "pending", requestId: "r9" });
      render(<ContactsView />);
      fireEvent.click(await screen.findByLabelText("New contact"));
      fireEvent.change(await screen.findByLabelText("Given name"), { target: { value: "Grace" } });
      fireEvent.click(screen.getByText("Save"));
      expect(await screen.findByText("Sent to Pixel 8. Waiting for the phone to apply it.")).toBeInTheDocument();
    });

    it("shows contact projection failures without relabeling repair as sync", async () => {
      render(<ContactsView sync={{ repairRequired: false, projection: { state: "failed", reason: "missing_record" } }} />);
      expect(await screen.findByText(/Contacts may be out of date \(rebuild failed: missing record\)/)).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Repair contact cache" })).not.toBeInTheDocument();
    });

    it.each([
      ["stale_epoch", "The phone's contact context changed before the scan. Use Sync now to request a fresh scan."],
      ["superseded_generation", "A newer contact book version replaced this scan. Use Sync now to request a fresh scan."],
      ["incomplete_scan", "The phone's scan did not finish. Use Sync now to try again."],
      ["cancelled", "The phone cancelled this scan. Use Sync now to try again."],
    ])("explains %s without asking for re-enrollment", async (reason, copy) => {
      vi.mocked(bridge.list_contact_books).mockResolvedValue([{ ...mockContactBook, capabilities: { ...mockContactBook.capabilities, canSync: true } }]);
      vi.mocked(bridge.get_contact_sync_request).mockResolvedValue({ requestId: "scan", bookId: "book-pixel8", state: "failed", reason });
      render(<ContactsView />);
      expect(await screen.findByText(`Scan failed. ${copy}`)).toBeInTheDocument();
      expect(screen.queryByText(/re-?enroll/i)).not.toBeInTheDocument();
    });

    it("does not claim completion while the local projection is rebuilding", async () => {
      vi.mocked(bridge.get_contact_sync_request).mockResolvedValue({ requestId: "scan", bookId: "book-pixel8", state: "completed" });
      render(<ContactsView sync={{ repairRequired: true, projection: { state: "rebuilding" } }} />);
      expect(await screen.findByText("The phone finished its scan. Contacts are still being repaired locally.")).toBeInTheDocument();
      expect(screen.queryByText("Scan completed.")).not.toBeInTheDocument();
    });

    it("finds an off-page target even when the book count is stale, then focuses its row", async () => {
      const pages = Array.from({ length: 2 }, (_, page) => Array.from({ length: 200 }, (_, index) => ({
        ...mockContactRiver,
        id: `contact-${page * 200 + index}`,
        displayName: `Person ${page * 200 + index}`,
      })));
      const targetContact = { ...mockContactAurora, id: "contact-400", displayName: "Person 400" };
      const scrollIntoView = vi.fn();
      Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: scrollIntoView });
      vi.mocked(bridge.list_contact_books).mockResolvedValue([{ ...mockContactBook, contactCount: 1 }]);
      vi.mocked(bridge.list_contacts).mockImplementation(async (_book, _query, offset = 0) => pages[offset / 200] ?? [targetContact]);
      const consumed = vi.fn();
      render(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-400", requestId: 10 }} onTargetConsumed={consumed} />);

      const selected = await screen.findByRole("button", { name: /Person 400/ });
      await waitFor(() => expect(consumed).toHaveBeenCalledWith(10));
      await waitFor(() => expect(document.activeElement).toBe(selected));
      expect(scrollIntoView).toHaveBeenCalled();
    });

    it("opens a create target with its raw phone number prefilled", async () => {
      const consumed = vi.fn();
      render(<ContactsView target={{ kind: "create", phone: "+18014320853", requestId: 70 }} onTargetConsumed={consumed} />);

      expect(await screen.findByRole("form", { name: "Create contact" })).toBeInTheDocument();
      expect(screen.getByLabelText("Phone 1 number")).toHaveValue("+18014320853");
      expect(consumed).toHaveBeenCalledWith(70);
    });

    it("consumes a create target when keeping a dirty edit", async () => {
      const consumed = vi.fn();
      const { rerender } = render(<ContactsView onTargetConsumed={consumed} />);
      fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
      fireEvent.click(screen.getByLabelText("Edit contact"));
      fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Unsaved" } });

      rerender(<ContactsView target={{ kind: "create", phone: "+18014320853", requestId: 71 }} onTargetConsumed={consumed} />);
      expect(await screen.findByText("You have unsaved changes.")).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Keep editing" }));

      expect(screen.getByLabelText("Given name")).toHaveValue("Unsaved");
      expect(consumed).toHaveBeenCalledWith(71);
    });

    it("reports and consumes a create target when no book is available", async () => {
      vi.mocked(bridge.list_contact_books).mockResolvedValue([]);
      const consumed = vi.fn();
      render(<ContactsView target={{ kind: "create", phone: "+18014320853", requestId: 72 }} onTargetConsumed={consumed} />);

      await waitFor(() => expect(document.querySelector("#contacts-target-error")).toHaveAttribute("data-target-state", "error"));
      expect(screen.queryByRole("form", { name: "Create contact" })).not.toBeInTheDocument();
      expect(consumed).toHaveBeenCalledWith(72);
    });

    it("opens an empty form from the manual new-contact action", async () => {
      render(<ContactsView />);
      fireEvent.click(await screen.findByLabelText("New contact"));

      expect(screen.getByRole("form", { name: "Create contact" })).toBeInTheDocument();
      expect(screen.queryByLabelText("Phone 1 number")).not.toBeInTheDocument();
    });

    it("cancels an active contact lookup before opening a create target", async () => {
      const lookup = deferred<ContactView[]>();
      vi.mocked(bridge.list_contacts).mockReturnValue(lookup.promise);
      const { rerender, container } = render(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-river", requestId: 73 }} />);
      await screen.findByText("Finding requested contact…");

      rerender(<ContactsView target={{ kind: "create", phone: "+18014320853", requestId: 74 }} />);

      expect(await screen.findByRole("form", { name: "Create contact" })).toBeInTheDocument();
      expect(container.querySelector("#contacts-target-status")).toBeNull();
    });

    it("resets a dirty prefilled form when a new request targets the same phone", async () => {
      const { rerender } = render(<ContactsView target={{ kind: "create", phone: "+18014320853", requestId: 75 }} />);
      await screen.findByRole("form", { name: "Create contact" });
      fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Unsaved" } });

      rerender(<ContactsView target={{ kind: "create", phone: "+18014320853", requestId: 76 }} />);
      expect(await screen.findByText("You have unsaved changes.")).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Discard" }));

      expect(screen.getByLabelText("Given name")).toHaveValue("");
      expect(screen.getByLabelText("Phone 1 number")).toHaveValue("+18014320853");
    });

    it("keeps the selected usable book when opening a create target", async () => {
      const secondBook = { ...mockContactBook, id: "book-iphone", deviceName: "iPhone" };
      vi.mocked(bridge.list_contact_books).mockResolvedValue([mockContactBook, secondBook]);
      const { rerender } = render(<ContactsView />);
      fireEvent.click(await screen.findByRole("button", { name: /iPhone/ }));

      rerender(<ContactsView target={{ kind: "create", phone: "+18014320853", requestId: 77 }} />);

      await screen.findByRole("form", { name: "Create contact" });
      expect(screen.getByRole("button", { name: /iPhone/ })).toHaveAttribute("aria-current", "location");
    });

    it("switches a create target from a read-only book to the first usable book", async () => {
      const readOnlyBook = { ...mockContactBook, capabilities: { ...mockContactBook.capabilities, canWrite: false } };
      const writableBook = { ...mockContactBook, id: "book-iphone", deviceName: "iPhone" };
      vi.mocked(bridge.list_contact_books).mockResolvedValue([readOnlyBook, writableBook]);
      const consumed = vi.fn();
      render(<ContactsView target={{ kind: "create", phone: "+18014320853", requestId: 81 }} onTargetConsumed={consumed} />);

      expect(await screen.findByRole("form", { name: "Create contact" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: /iPhone/ })).toHaveAttribute("aria-current", "location");
      expect(consumed).toHaveBeenCalledWith(81);
    });

    it("reports and consumes a create target when no usable book is available", async () => {
      const readOnlyBook = { ...mockContactBook, capabilities: { ...mockContactBook.capabilities, canWrite: false } };
      const revokedBook = { ...mockContactBook, id: "book-iphone", deviceName: "iPhone", sourceRevoked: true };
      vi.mocked(bridge.list_contact_books).mockResolvedValue([readOnlyBook, revokedBook]);
      const consumed = vi.fn();
      render(<ContactsView target={{ kind: "create", phone: "+18014320853", requestId: 82 }} onTargetConsumed={consumed} />);

      await waitFor(() => expect(document.querySelector("#contacts-target-error")).toHaveAttribute("data-target-state", "error"));
      expect(screen.queryByRole("form", { name: "Create contact" })).not.toBeInTheDocument();
      expect(consumed).toHaveBeenCalledWith(82);
    });

    it("waits for dirty draft confirmation before switching to a usable book for a create target", async () => {
      const readOnlyBook = { ...mockContactBook, capabilities: { ...mockContactBook.capabilities, canWrite: false } };
      const writableBook = { ...mockContactBook, id: "book-iphone", deviceName: "iPhone" };
      vi.mocked(bridge.list_contact_books).mockResolvedValue([readOnlyBook, writableBook]);
      const consumed = vi.fn();
      const { rerender } = render(<ContactsView />);
      fireEvent.click(await screen.findByLabelText("New contact"));
      fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Unsaved" } });

      rerender(<ContactsView target={{ kind: "create", phone: "+18014320853", requestId: 83 }} onTargetConsumed={consumed} />);

      expect(await screen.findByText("You have unsaved changes.")).toBeInTheDocument();
      expect(screen.getByRole("button", { name: /Pixel 8/ })).toHaveAttribute("aria-current", "location");
      expect(screen.getByRole("button", { name: /iPhone/ })).not.toHaveAttribute("aria-current", "location");
      expect(consumed).not.toHaveBeenCalled();

      fireEvent.click(screen.getByRole("button", { name: "Discard" }));

      expect(await screen.findByRole("form", { name: "Create contact" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: /iPhone/ })).toHaveAttribute("aria-current", "location");
      expect(consumed).toHaveBeenCalledWith(83);
    });

    it("sends the prefilled phone when saving a create target", async () => {
      vi.mocked(bridge.submit_contact_edit).mockResolvedValue({ state: "pending", requestId: "prefilled-create" });
      render(<ContactsView target={{ kind: "create", phone: "+18014320853", requestId: 78 }} />);
      fireEvent.change(await screen.findByLabelText("Given name"), { target: { value: "Grace" } });
      fireEvent.click(screen.getByRole("button", { name: "Save" }));

      await waitFor(() => expect(bridge.submit_contact_edit).toHaveBeenCalledWith(expect.objectContaining({
        kind: "create",
        patches: expect.arrayContaining([expect.objectContaining({ field: "phones", value: [{ label: "mobile", number: "+18014320853" }] })]),
      })));
    });

    it("consumes a create target during a save with an adding notice", async () => {
      const submit = deferred<Awaited<ReturnType<typeof bridge.submit_contact_edit>>>();
      vi.mocked(bridge.submit_contact_edit).mockReturnValue(submit.promise);
      const consumed = vi.fn();
      const { rerender } = render(<ContactsView onTargetConsumed={consumed} />);
      fireEvent.click(await screen.findByLabelText("New contact"));
      fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Grace" } });
      fireEvent.click(screen.getByRole("button", { name: "Save" }));
      await waitFor(() => expect(bridge.submit_contact_edit).toHaveBeenCalledTimes(1));

      rerender(<ContactsView target={{ kind: "create", phone: "+18014320853", requestId: 79 }} onTargetConsumed={consumed} />);

      expect(await screen.findByText("Finish saving the current contact before adding a new one.")).toBeInTheDocument();
      expect(consumed).toHaveBeenCalledWith(79);
      submit.resolve({ state: "pending", requestId: "save" });
    });

    it("consumes a create target when book loading fails", async () => {
      vi.mocked(bridge.list_contact_books).mockRejectedValue(new Error("offline"));
      const consumed = vi.fn();
      render(<ContactsView target={{ kind: "create", phone: "+18014320853", requestId: 80 }} onTargetConsumed={consumed} />);

      await waitFor(() => expect(consumed).toHaveBeenCalledWith(80));
    });

    it("consumes a cancelled dirty target so a newer request can ask again", async () => {
      const consumed = vi.fn();
      const { rerender } = render(<ContactsView onTargetConsumed={consumed} />);
      fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
      fireEvent.click(screen.getByLabelText("Edit contact"));
      fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Unsaved" } });

      rerender(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-river", requestId: 11 }} onTargetConsumed={consumed} />);
      expect(await screen.findByText("You have unsaved changes.")).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Keep editing" }));
      expect(consumed).toHaveBeenCalledWith(11);

      rerender(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-river", requestId: 12 }} onTargetConsumed={consumed} />);
      expect(await screen.findByText("You have unsaved changes.")).toBeInTheDocument();
    });

    it("keeps a dirty target prompt through sorting and opens it with the latest sort on discard", async () => {
      const consumed = vi.fn();
      const { rerender } = render(<ContactsView onTargetConsumed={consumed} />);
      fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
      fireEvent.click(screen.getByLabelText("Edit contact"));
      fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Unsaved" } });
      rerender(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-river", requestId: 60 }} onTargetConsumed={consumed} />);
      expect(await screen.findByText("You have unsaved changes.")).toBeInTheDocument();

      fireEvent.click(screen.getByRole("button", { name: /Sort:/ }));
      fireEvent.click(await screen.findByRole("menuitemradio", { name: "First name" }));
      expect(consumed).not.toHaveBeenCalled();
      fireEvent.click(screen.getByRole("button", { name: "Discard" }));

      expect(await screen.findByRole("heading", { name: "River Park" })).toBeInTheDocument();
      expect(consumed).toHaveBeenCalledTimes(1);
      expect(consumed).toHaveBeenCalledWith(60);
      expect(bridge.list_contacts).toHaveBeenCalledWith("book-pixel8", "", 0, { key: "given", direction: "asc" });
      expect(screen.queryByText(/Finding requested contact/)).not.toBeInTheDocument();
    });

    it("restarts an active target lookup under the selected sort and ignores its old page", async () => {
      const oldPage = deferred<ContactView[]>();
      const newPage = deferred<ContactView[]>();
      vi.mocked(bridge.list_contacts).mockImplementation((_book, _query, _offset, sort) => sort?.key === "given" ? newPage.promise : oldPage.promise);
      render(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-river", requestId: 61 }} />);
      await screen.findByText(/Finding requested contact/);
      fireEvent.click(screen.getByRole("button", { name: /Sort:/ }));
      fireEvent.click(await screen.findByRole("menuitemradio", { name: "First name" }));
      await waitFor(() => expect(bridge.list_contacts).toHaveBeenCalledWith("book-pixel8", "", 0, { key: "given", direction: "asc" }));
      await act(async () => { oldPage.resolve([mockContactRiver]); });
      expect(screen.queryByRole("heading", { name: "River Park" })).not.toBeInTheDocument();
      newPage.resolve([mockContactRiver]);
      expect(await screen.findByRole("heading", { name: "River Park" })).toBeInTheDocument();
    });

    it("clears list loading when a target page wins over an older ordinary page", async () => {
      const pages: Array<ReturnType<typeof deferred<ContactView[]>>> = [];
      vi.mocked(bridge.list_contacts).mockImplementation(() => {
        const page = deferred<ContactView[]>();
        pages.push(page);
        return page.promise;
      });
      render(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-river", requestId: 62 }} />);
      await waitFor(() => expect(pages).toHaveLength(2));
      pages[1].resolve([mockContactRiver]);
      expect(await screen.findByRole("heading", { name: "River Park" })).toBeInTheDocument();
      expect(document.getElementById("contacts-list")).toHaveAttribute("aria-busy", "false");
    });

    it("does not let a replaced target's deferred page apply", async () => {
      const resolvers: Array<(contacts: ContactView[]) => void> = [];
      vi.mocked(bridge.list_contacts).mockImplementation(() => new Promise(resolve => { resolvers.push(resolve); }));
      const consumed = vi.fn();
      const { rerender } = render(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-aurora", requestId: 13 }} onTargetConsumed={consumed} />);
      await screen.findByText("Pixel 8");
      await screen.findByText(/Finding requested contact/);
      rerender(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-river", requestId: 14 }} onTargetConsumed={consumed} />);
      await waitFor(() => expect(resolvers).toHaveLength(3));
      resolvers[0]([mockContactAurora]);
      resolvers[1]([mockContactAurora]);
      await act(async () => { resolvers[2]([mockContactRiver]); });
      expect(await screen.findByRole("heading", { name: "River Park" })).toBeInTheDocument();
      expect(consumed).toHaveBeenCalledWith(14);
      expect(consumed).not.toHaveBeenCalledWith(13);
    });

    it("consumes a pending target on unmount without applying a late page", async () => {
      let resolveLookup!: (contacts: ContactView[]) => void;
      vi.mocked(bridge.list_contacts).mockImplementation(() => new Promise(resolve => { resolveLookup = resolve; }));
      const consumed = vi.fn();
      const view = render(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-aurora", requestId: 15 }} onTargetConsumed={consumed} />);
      await screen.findByText("Pixel 8");
      await screen.findByText(/Finding requested contact/);
      view.unmount();
      resolveLookup([mockContactAurora]);
      await waitFor(() => expect(consumed).toHaveBeenCalledWith(15));
      expect(screen.queryByText("Aurora Chen")).not.toBeInTheDocument();
    });

    it("returns to the first page when a removed current book falls back", async () => {
      const secondBook = { ...mockContactBook, id: "book-iphone", deviceName: "iPhone" };
      let hint: (() => void) | undefined;
      let books = [mockContactBook, secondBook];
      vi.mocked(bridge.subscribe).mockImplementation(listener => { hint = listener; return () => undefined; });
      vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
      vi.mocked(bridge.list_contacts).mockImplementation(async (id, _query, offset = 0) => {
        if (id === "book-iphone" && offset < 400) {
          return Array.from({ length: 200 }, (_, index) => ({ ...mockContactRiver, id: `iphone-${offset + index}`, bookId: id, displayName: `iPhone ${offset + index}` }));
        }
        if (id === "book-iphone") return [{ ...mockContactRiver, id: "iphone-400", bookId: id, displayName: "iPhone 400" }];
        return [mockContactAurora];
      });
      render(<ContactsView />);
      fireEvent.click(await screen.findByRole("button", { name: /iPhone/ }));
      await screen.findByText("iPhone 0");
      fireEvent.click(screen.getByRole("button", { name: "Next" }));
      await screen.findByText("iPhone 200");
      fireEvent.click(screen.getByRole("button", { name: "Next" }));
      await screen.findByText("iPhone 400");

      books = [mockContactBook];
      act(() => hint?.());

      await screen.findByText("Aurora Chen");
      expect(bridge.list_contacts).toHaveBeenLastCalledWith("book-pixel8", "", 0, { key: "family", direction: "asc" });
    });

    it("keeps the real app target through a StrictMode fake unmount", async () => {
      const targetContact = { ...mockContactAurora, id: "contact-400", displayName: "Person 400" };
      const firstPage = Array.from({ length: 200 }, (_, index) => ({ ...mockContactRiver, id: `contact-${index}`, displayName: `Person ${index}` }));
      vi.mocked(bridge.list_contacts).mockImplementation(async (_book, _query, offset = 0) => offset === 0 ? firstPage : offset === 200 ? firstPage : [targetContact]);

      function StrictAppHarness() {
        const [target, setTarget] = useState<ContactTarget | null>({ bookId: "book-pixel8", contactId: "contact-400", requestId: 40 });
        return <ContactsView target={target} onTargetConsumed={requestId => {
          setTarget(current => current?.requestId === requestId ? null : current);
        }} />;
      }

      render(<StrictMode><StrictAppHarness /></StrictMode>);

      expect(await screen.findByRole("heading", { name: "Person 400" })).toBeInTheDocument();
    });

    it("does not restart a deferred target walk when a live hint refreshes books", async () => {
      let hint: (() => void) | undefined;
      const pageTwo = deferred<ContactView[]>();
      const firstPage = Array.from({ length: 200 }, (_, index) => ({ ...mockContactRiver, id: `contact-${index}`, displayName: `Person ${index}` }));
      const targetContact = { ...mockContactAurora, id: "contact-400", displayName: "Person 400" };
      vi.mocked(bridge.subscribe).mockImplementation(listener => { hint = listener; return () => undefined; });
      vi.mocked(bridge.list_contacts).mockImplementation((_book, query, offset = 0) => {
        if (query || offset === 0) return Promise.resolve(firstPage);
        if (offset === 200) return pageTwo.promise;
        return Promise.resolve([targetContact]);
      });
      render(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-400", requestId: 41 }} />);
      await waitFor(() => expect(vi.mocked(bridge.list_contacts).mock.calls.filter(([, query, offset]) => query === "" && offset === 200)).toHaveLength(1));
      const offsetZeroBefore = vi.mocked(bridge.list_contacts).mock.calls.filter(([, query, offset]) => query === "" && (offset ?? 0) === 0).length;

      vi.useFakeTimers({ shouldAdvanceTime: true });
      try {
        act(() => hint?.());
        await act(async () => { await vi.advanceTimersByTimeAsync(150); });
        expect(vi.mocked(bridge.list_contacts).mock.calls.filter(([, query, offset]) => query === "" && (offset ?? 0) === 0)).toHaveLength(offsetZeroBefore + 1);
        pageTwo.resolve(firstPage);

        expect(await screen.findByRole("heading", { name: "Person 400" })).toBeInTheDocument();
        expect(vi.mocked(bridge.list_contacts).mock.calls.filter(([, query, offset]) => query === "" && offset === 200)).toHaveLength(1);
      } finally {
        vi.useRealTimers();
      }
    });

    it("clears a failed target alert when a newer target succeeds", async () => {
      vi.mocked(bridge.list_contacts).mockImplementation(async (_book, _query, offset = 0) => offset === 0 ? [mockContactAurora, mockContactRiver] : []);
      const consumed = vi.fn();
      const { rerender, container } = render(<ContactsView target={{ bookId: "missing-book", contactId: "missing", requestId: 42 }} onTargetConsumed={consumed} />);
      await waitFor(() => expect(container.querySelector("#contacts-target-error")).toHaveAttribute("data-target-state", "error"));
      expect(consumed).toHaveBeenCalledWith(42);

      rerender(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-river", requestId: 43 }} onTargetConsumed={consumed} />);

      expect(await screen.findByRole("heading", { name: "River Park" })).toBeInTheDocument();
      expect(container.querySelector("#contacts-target-error")).toBeNull();
      expect(container.querySelectorAll("#contacts-target-status")).toHaveLength(0);
    });

    it("does not apply a late target page after editing starts", async () => {
      const latePage = deferred<ContactView[]>();
      const firstPage = Array.from({ length: 200 }, (_, index) => ({ ...mockContactRiver, id: `contact-${index}`, displayName: `Person ${index}` }));
      vi.mocked(bridge.list_contacts).mockImplementation((_book, _query, offset = 0) => offset === 200 ? latePage.promise : Promise.resolve(firstPage));
      render(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-400", requestId: 44 }} />);
      await waitFor(() => expect(vi.mocked(bridge.list_contacts)).toHaveBeenCalledWith("book-pixel8", "", 200, { key: "family", direction: "asc" }));
      fireEvent.click(screen.getByRole("button", { name: "New contact" }));
      expect(screen.getByRole("form")).toBeInTheDocument();
      await act(async () => { latePage.resolve([{ ...mockContactAurora, id: "contact-400", displayName: "Person 400" }]); });

      expect(screen.getByRole("form")).toBeInTheDocument();
      expect(screen.queryByRole("heading", { name: "Person 400" })).not.toBeInTheDocument();
    });

    it("handles A to B browsing then a new nonce for A", async () => {
      const consumed = vi.fn();
      const { rerender } = render(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-aurora", requestId: 45 }} onTargetConsumed={consumed} />);
      expect(await screen.findByRole("heading", { name: "Aurora Chen" })).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: /River Park/ }));
      expect(await screen.findByRole("heading", { name: "River Park" })).toBeInTheDocument();

      rerender(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-aurora", requestId: 46 }} onTargetConsumed={consumed} />);

      expect(await screen.findByRole("heading", { name: "Aurora Chen" })).toBeInTheDocument();
      const targetedRow = screen.getByRole("button", { name: /Aurora Chen/ });
      await waitFor(() => expect(document.activeElement).toBe(targetedRow));
      expect(consumed).toHaveBeenCalledWith(46);
    });

    it("keeps the old target absent when it resolves after its replacement", async () => {
      const oldLookup = deferred<ContactView[]>();
      const newLookup = deferred<ContactView[]>();
      let targetCall = 0;
      vi.mocked(bridge.list_contacts).mockImplementation((_book, _query, _offset) => {
        targetCall += 1;
        if (targetCall === 1) return Promise.resolve([]);
        return targetCall === 2 ? oldLookup.promise : newLookup.promise;
      });
      const { rerender } = render(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-aurora", requestId: 47 }} />);
      await screen.findByText(/Finding requested contact/);
      rerender(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-river", requestId: 48 }} />);
      await waitFor(() => expect(targetCall).toBe(3));

      await act(async () => { oldLookup.resolve([mockContactAurora]); });
      expect(screen.queryByRole("heading", { name: "Aurora Chen" })).not.toBeInTheDocument();
      newLookup.resolve([mockContactRiver]);
      expect(await screen.findByRole("heading", { name: "River Park" })).toBeInTheDocument();
    });

    it("consumes a target with a notice when loading books fails", async () => {
      vi.mocked(bridge.list_contact_books).mockRejectedValue(new Error("offline"));
      const consumed = vi.fn();
      const { container } = render(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-aurora", requestId: 49 }} onTargetConsumed={consumed} />);

      await waitFor(() => expect(consumed).toHaveBeenCalledWith(49));
      expect(container.querySelector("#contacts-target-error")).toHaveAttribute("data-target-state", "error");
      expect(consumed).toHaveBeenCalledTimes(1);
    });

    it("keeps a deferred target through one transient background book-list failure", async () => {
      let hint: (() => void) | undefined;
      const pageTwo = deferred<ContactView[]>();
      const firstPage = Array.from({ length: 200 }, (_, index) => ({ ...mockContactRiver, id: `contact-${index}`, displayName: `Person ${index}` }));
      const targetContact = { ...mockContactAurora, id: "contact-400", displayName: "Person 400" };
      const consumed = vi.fn();
      vi.mocked(bridge.subscribe).mockImplementation(listener => { hint = listener; return () => undefined; });
      vi.mocked(bridge.list_contacts).mockImplementation((_book, query, offset = 0) => {
        if (query || offset === 0) return Promise.resolve(firstPage);
        if (offset === 200) return pageTwo.promise;
        return Promise.resolve([targetContact]);
      });
      function StatefulParent() {
        const [target, setTarget] = useState<ContactTarget | null>({ bookId: "book-pixel8", contactId: "contact-400", requestId: 51 });
        return <ContactsView target={target} onTargetConsumed={requestId => {
          consumed(requestId);
          setTarget(current => current?.requestId === requestId ? null : current);
        }} />;
      }
      const { container } = render(<StatefulParent />);
      await waitFor(() => expect(vi.mocked(bridge.list_contacts).mock.calls.filter(([, query, offset]) => query === "" && offset === 200)).toHaveLength(1));

      vi.mocked(bridge.list_contact_books).mockRejectedValueOnce(new Error("transient"));
      act(() => hint?.());
      await waitFor(() => expect(vi.mocked(bridge.list_contact_books)).toHaveBeenCalledTimes(2));
      await act(async () => {});
      expect(consumed).not.toHaveBeenCalled();
      expect(screen.getByText(/Finding requested contact/)).toBeInTheDocument();

      pageTwo.resolve(firstPage);
      expect(await screen.findByRole("heading", { name: "Person 400" })).toBeInTheDocument();
      expect(consumed).toHaveBeenCalledTimes(1);
      expect(consumed).toHaveBeenCalledWith(51);
      expect(container.querySelector("#contacts-target-error")).toBeNull();
    });

    it("aborts a deferred target with a book notice when its book disappears", async () => {
      let hint: (() => void) | undefined;
      const pageTwo = deferred<ContactView[]>();
      const secondBook = { ...mockContactBook, id: "book-iphone", deviceName: "iPhone" };
      const firstPage = Array.from({ length: 200 }, (_, index) => ({ ...mockContactRiver, id: `contact-${index}`, displayName: `Person ${index}` }));
      const targetContact = { ...mockContactAurora, id: "contact-400", displayName: "Person 400" };
      let books = [mockContactBook, secondBook];
      const consumed = vi.fn();
      vi.mocked(bridge.subscribe).mockImplementation(listener => { hint = listener; return () => undefined; });
      vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
      vi.mocked(bridge.list_contacts).mockImplementation((_book, query, offset = 0) => {
        if (query || offset === 0) return Promise.resolve(firstPage);
        if (offset === 200) return pageTwo.promise;
        return Promise.resolve([targetContact]);
      });
      const { container } = render(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-400", requestId: 52 }} onTargetConsumed={consumed} />);
      await waitFor(() => expect(vi.mocked(bridge.list_contacts).mock.calls.filter(([, query, offset]) => query === "" && offset === 200)).toHaveLength(1));

      books = [secondBook];
      act(() => hint?.());
      await waitFor(() => expect(screen.queryByText("Pixel 8")).not.toBeInTheDocument());
      pageTwo.resolve(firstPage);

      await waitFor(() => expect(consumed).toHaveBeenCalledWith(52));
      expect(container.querySelector("#contacts-target-error")).toHaveTextContent("The requested contact book is no longer available.");
      expect(screen.queryByRole("heading", { name: "Person 400" })).not.toBeInTheDocument();
      expect(consumed).toHaveBeenCalledTimes(1);
    });

    it("clears a target error when the user opens another contact", async () => {
      const { container } = render(<ContactsView target={{ bookId: "missing-book", contactId: "missing", requestId: 54 }} onTargetConsumed={vi.fn()} />);
      await waitFor(() => expect(container.querySelector("#contacts-target-error")).toBeInTheDocument());

      fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));

      expect(container.querySelector("#contacts-target-error")).toBeNull();
    });

    it("consumes a target arriving during a save with a finish-saving notice", async () => {
      const submit = deferred<Awaited<ReturnType<typeof bridge.submit_contact_edit>>>();
      vi.mocked(bridge.submit_contact_edit).mockReturnValue(submit.promise);
      const consumed = vi.fn();
      const { rerender } = render(<ContactsView onTargetConsumed={consumed} />);
      fireEvent.click(await screen.findByLabelText("New contact"));
      fireEvent.change(await screen.findByLabelText("Given name"), { target: { value: "Grace" } });
      fireEvent.click(screen.getByText("Save"));
      await waitFor(() => expect(bridge.submit_contact_edit).toHaveBeenCalledTimes(1));

      rerender(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-river", requestId: 53 }} onTargetConsumed={consumed} />);

      expect(await screen.findByText("Finish saving the current contact before opening the requested contact.")).toBeInTheDocument();
      expect(consumed).toHaveBeenCalledWith(53);
      submit.resolve({ state: "pending", requestId: "r10" });
      expect(await screen.findByText("Sent to Pixel 8. Waiting for the phone to apply it.")).toBeInTheDocument();
    });

    it("focuses a target row whose id contains selector-special characters", async () => {
      const quoted = { ...mockContactAurora, id: 'contact"quoted', displayName: "Quoted Person" };
      Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
      vi.mocked(bridge.list_contacts).mockResolvedValue([quoted, mockContactRiver]);
      render(<ContactsView target={{ bookId: "book-pixel8", contactId: 'contact"quoted', requestId: 55 }} onTargetConsumed={vi.fn()} />);

      const row = await screen.findByRole("button", { name: /Quoted Person/ });
      await waitFor(() => expect(document.activeElement).toBe(row));
    });

    it("resets scan state when a target switches books", async () => {
      const secondBook = { ...mockContactBook, id: "book-iphone", deviceName: "iPhone" };
      vi.mocked(bridge.list_contact_books).mockResolvedValue([mockContactBook, secondBook]);
      vi.mocked(bridge.get_contact_sync_request).mockImplementation(async bookId => bookId === "book-pixel8"
        ? { requestId: "old", bookId, state: "waiting" }
        : null);
      vi.mocked(bridge.list_contacts).mockImplementation(async bookId => bookId === "book-iphone"
        ? [{ ...mockContactRiver, id: "iphone-contact", bookId, displayName: "iPhone Person" }]
        : [mockContactAurora]);
      const { rerender } = render(<ContactsView />);
      expect(await screen.findByText("Waiting for the phone to scan contacts.")).toBeInTheDocument();

      rerender(<ContactsView target={{ bookId: "book-iphone", contactId: "iphone-contact", requestId: 50 }} />);

      expect(await screen.findByRole("heading", { name: "iPhone Person" })).toBeInTheDocument();
      expect(screen.queryByText("Waiting for the phone to scan contacts.")).not.toBeInTheDocument();
    });

  });

  it("persists the saved contact number tone without using a draft phone", async () => {
    const onSelectTone = vi.fn();
    render(<ContactsView toneOverrides={{ "+12025550123": 61 }} onSelectTone={onSelectTone} />);
    fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
    fireEvent.click(screen.getByLabelText("Edit contact"));
    fireEvent.change(screen.getByLabelText("Phone 1 number"), { target: { value: "+12025550999" } });
    fireEvent.click(screen.getByRole("button", { name: "Conversation color: Rose" }));
    fireEvent.click(screen.getByRole("radio", { name: "Automatic" }));
    expect(onSelectTone).toHaveBeenCalledWith("+12025550123", null);
    expect(screen.getByText(/Tied to the saved number/)).toBeVisible();
  });

  describe("book pinning after book refreshes (H10)", () => {
    const restorableBook = { ...mockContactBook, capabilities: { ...mockContactBook.capabilities, canRestore: true } };
    const otherBook: ContactBookView = { ...restorableBook, id: "book-iphone", deviceName: "iPhone" };
    const otherBookContact: ContactView = { ...mockContactRiver, id: "iphone-contact", bookId: "book-iphone", displayName: "iPhone Person" };
    const deletedFromPixel = { id: "deleted-pixel", bookId: "book-pixel8", displayName: "Old Friend", deletedAt: "yesterday" };

    let hint: (() => void) | undefined;
    let books: ContactBookView[];

    beforeEach(() => {
      hint = undefined;
      books = [restorableBook, otherBook];
      vi.mocked(bridge.subscribe).mockImplementation(listener => { hint = listener; return () => undefined; });
      vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
      vi.mocked(bridge.list_contacts).mockImplementation(async bookId => bookId === "book-iphone"
        ? [otherBookContact]
        : [mockContactAurora, mockContactRiver]);
      vi.mocked(bridge.get_contact_sync_request).mockResolvedValue(null);
    });

    const removePixelBook = async (remaining: ContactBookView[]) => {
      books = remaining;
      act(() => hint?.());
      await waitFor(() => expect(screen.queryByRole("button", { name: /Pixel 8/ })).not.toBeInTheDocument());
    };
    const saveButton = () => document.querySelector<HTMLButtonElement>('[data-action="save-edit"]')!;

    it.each(["manual", "prefilled"])("never submits a dirty %s create draft to a fallback book after its book disappears", async mode => {
      render(<ContactsView target={mode === "prefilled" ? { kind: "create", phone: "+18014320853", requestId: 81 } : undefined} />);
      await screen.findByRole("button", { name: /Aurora Chen/ });
      if (mode === "manual") fireEvent.click(screen.getByLabelText("New contact"));
      fireEvent.change(await screen.findByLabelText("Given name"), { target: { value: "Draft for Pixel" } });

      await removePixelBook([otherBook]);

      expect(screen.getByLabelText("Given name")).toHaveValue("Draft for Pixel");
      if (mode === "prefilled") expect(screen.getByLabelText("Phone 1 number")).toHaveValue("+18014320853");
      expect(saveButton()).toBeDisabled();
      expect(screen.getByRole("alert")).toHaveTextContent("This contact book is no longer available. Your draft is still open.");
      fireEvent.click(saveButton());
      await act(async () => { await Promise.resolve(); });
      expect(bridge.submit_contact_edit).not.toHaveBeenCalled();
    });

    it.each(["before refresh starts", "while refresh is pending", "after refresh completes"])(
      "keeps a rejected save visible after a background refresh hint: rejection %s",
      async rejectionOrder => {
        const initialBooks = deferred<ContactBookView[]>();
        const refreshedBooks = deferred<ContactBookView[]>();
        const refreshedPage = deferred<ContactView[]>();
        const submit = deferred<Awaited<ReturnType<typeof bridge.submit_contact_edit>>>();
        vi.mocked(bridge.list_contact_books)
          .mockReturnValueOnce(initialBooks.promise)
          .mockReturnValueOnce(refreshedBooks.promise);
        vi.mocked(bridge.submit_contact_edit).mockReturnValue(submit.promise);
        render(<ContactsView />);

        const create = screen.getByLabelText("New contact");
        expect(create).toBeDisabled();
        await act(async () => initialBooks.resolve(books));
        await waitFor(() => expect(create).toBeEnabled());
        await screen.findByRole("button", { name: /Aurora Chen/ });
        fireEvent.click(create);
        const givenName = screen.getByLabelText("Given name");
        fireEvent.change(givenName, { target: { value: "Draft for Pixel" } });
        fireEvent.click(screen.getByRole("button", { name: "Save" }));
        expect(bridge.submit_contact_edit).toHaveBeenCalledWith(expect.objectContaining({
          kind: "create", targetBookId: "book-pixel8",
        }));
        expect(saveButton()).toBeDisabled();

        const rejectSave = async () => {
          await act(async () => submit.resolve({ state: "rejected", reason: "The phone refused this save." }));
          expect(screen.getByRole("alert")).toHaveTextContent("The phone refused this save.");
        };
        if (rejectionOrder === "before refresh starts") await rejectSave();

        vi.mocked(bridge.list_contacts).mockReturnValueOnce(refreshedPage.promise);
        act(() => hint?.());
        await waitFor(() => expect(bridge.list_contact_books).toHaveBeenCalledTimes(2));
        await waitFor(() => expect(bridge.list_contacts).toHaveBeenCalledTimes(2));
        if (rejectionOrder === "while refresh is pending") await rejectSave();

        await act(async () => {
          refreshedBooks.resolve([otherBook, { ...restorableBook, contactCount: 3 }]);
          refreshedPage.resolve([{ ...mockContactAurora, displayName: "Refreshed Aurora" }, mockContactRiver]);
        });
        expect(screen.getByRole("button", { name: /Pixel 8/ })).toHaveTextContent("3");
        expect(screen.getByRole("button", { name: /Refreshed Aurora/ })).toBeInTheDocument();
        if (rejectionOrder === "after refresh completes") await rejectSave();

        expect(screen.getByLabelText("Given name")).toBe(givenName);
        expect(givenName).toHaveValue("Draft for Pixel");
        expect(screen.getByRole("alert")).toHaveTextContent("The phone refused this save.");
        expect(saveButton()).toBeEnabled();
        fireEvent.click(saveButton());
        await waitFor(() => expect(bridge.submit_contact_edit).toHaveBeenCalledTimes(2));
        expect(vi.mocked(bridge.submit_contact_edit).mock.calls[1][0]).toMatchObject({
          kind: "create", targetBookId: "book-pixel8",
          patches: expect.arrayContaining([{ field: "givenName", value: "Draft for Pixel" }]),
        });
        expect(screen.getByLabelText("Given name")).toHaveValue("Draft for Pixel");
      },
    );

    it("clears a stale clean detail and list when every book disappears", async () => {
      const { container } = render(<ContactsView />);
      fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
      expect(screen.getByRole("heading", { name: "Aurora Chen" })).toBeInTheDocument();

      await removePixelBook([]);

      await waitFor(() => expect(screen.queryByRole("button", { name: /Aurora Chen/ })).not.toBeInTheDocument());
      expect(screen.queryByRole("heading", { name: "Aurora Chen" })).not.toBeInTheDocument();
      expect(container.querySelector("#contacts-detail-panel")).toBeNull();
    });

    it("keeps New contact unavailable while no contact book exists", async () => {
      books = [];
      render(<ContactsView />);
      await waitFor(() => expect(bridge.list_contact_books).toHaveBeenCalled());
      await act(async () => { await Promise.resolve(); });

      const create = screen.getByLabelText("New contact");
      expect(create).toBeDisabled();
      fireEvent.click(create);
      await act(async () => { await Promise.resolve(); });
      expect(screen.queryByRole("form")).not.toBeInTheDocument();
      expect(bridge.submit_contact_edit).not.toHaveBeenCalled();
    });

    it("deletes the open contact against its own book after the current book falls back", async () => {
      vi.mocked(bridge.submit_contact_edit).mockResolvedValue({ state: "pending", requestId: "delete-1" });
      render(<ContactsView />);
      fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
      fireEvent.click(screen.getByLabelText("Edit contact"));
      fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Unsaved" } });

      await removePixelBook([otherBook]);
      fireEvent.click(screen.getByText("Cancel"));
      fireEvent.click(screen.getByRole("button", { name: "Discard" }));
      expect(await screen.findByRole("heading", { name: "Aurora Chen" })).toBeInTheDocument();

      fireEvent.click(screen.getByLabelText("Delete contact"));
      fireEvent.click(screen.getByLabelText("Confirm delete"));

      await waitFor(() => expect(bridge.submit_contact_edit).toHaveBeenCalledOnce());
      expect(vi.mocked(bridge.submit_contact_edit).mock.calls[0][0]).toMatchObject({ kind: "delete", targetBookId: "book-pixel8", contactId: "contact-aurora" });
    });

    it("ignores a delayed restore list for the previous book after selecting another book", async () => {
      const pending = deferred<typeof deletedFromPixel[]>();
      vi.mocked(bridge.list_restorable_contacts).mockReturnValueOnce(pending.promise);
      const { container } = render(<ContactsView />);
      await screen.findByRole("button", { name: /Aurora Chen/ });

      fireEvent.click(screen.getByLabelText("Recently deleted contacts"));
      expect(bridge.list_restorable_contacts).toHaveBeenCalledWith("book-pixel8");
      fireEvent.click(screen.getByRole("button", { name: /iPhone/ }));
      await screen.findByRole("button", { name: /iPhone Person/ });
      await act(async () => pending.resolve([deletedFromPixel]));

      expect(container.querySelector("#contacts-restore-panel")).toBeNull();
      expect(screen.queryByRole("button", { name: "Restore Old Friend" })).not.toBeInTheDocument();
      expect(bridge.restore_contact).not.toHaveBeenCalled();
    });

    it("keeps Recently deleted available beside a wide contact detail", async () => {
      const originalMatchMedia = window.matchMedia;
      window.matchMedia = vi.fn((query: string) => ({
        matches: false,
        media: query,
        onchange: null,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        addListener: vi.fn(),
        removeListener: vi.fn(),
        dispatchEvent: vi.fn(),
      })) as unknown as typeof window.matchMedia;
      try {
        vi.mocked(bridge.list_restorable_contacts).mockResolvedValueOnce([deletedFromPixel]);
        render(<ContactsView />);
        fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
        const recentlyDeleted = screen.getByLabelText("Recently deleted contacts");
        expect(recentlyDeleted).toBeEnabled();
        fireEvent.click(recentlyDeleted);
        expect(await screen.findByRole("dialog", { name: "Recently deleted contacts" })).toBeInTheDocument();
      } finally {
        window.matchMedia = originalMatchMedia;
      }
    });

    it("closes Recently deleted before opening a linked contact in the compact overlay", async () => {
      const originalMatchMedia = window.matchMedia;
      window.matchMedia = vi.fn((query: string) => ({
        matches: query === "(max-width: 560px)",
        media: query,
        onchange: null,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        addListener: vi.fn(),
        removeListener: vi.fn(),
        dispatchEvent: vi.fn(),
      })) as unknown as typeof window.matchMedia;
      try {
        vi.mocked(bridge.list_restorable_contacts).mockResolvedValueOnce([deletedFromPixel]);
        const { container, rerender } = render(<ContactsView />);
        fireEvent.click(await screen.findByLabelText("Recently deleted contacts"));
        await screen.findByRole("dialog", { name: "Recently deleted contacts" });

        rerender(<ContactsView target={{ bookId: "book-pixel8", contactId: "contact-aurora", requestId: 71 }} />);
        await screen.findByRole("region", { name: "Contact detail" });
        expect(container.querySelector("#contacts-restore-panel")).toBeNull();
        expect(container.querySelectorAll("#contacts-error")).toHaveLength(0);
        expect(container.querySelectorAll("#contacts-notice")).toHaveLength(0);
      } finally {
        window.matchMedia = originalMatchMedia;
      }
    });

    it("restores against the listed book and drops its late outcome after a book switch", async () => {
      const outcome = deferred<Awaited<ReturnType<typeof bridge.restore_contact>>>();
      vi.mocked(bridge.list_restorable_contacts).mockResolvedValueOnce([deletedFromPixel]);
      vi.mocked(bridge.restore_contact).mockReturnValueOnce(outcome.promise);
      const { container } = render(<ContactsView />);
      await screen.findByRole("button", { name: /Aurora Chen/ });

      fireEvent.click(screen.getByLabelText("Recently deleted contacts"));
      fireEvent.click(await screen.findByRole("button", { name: "Restore Old Friend" }));
      fireEvent.click(screen.getByRole("button", { name: /iPhone/ }));
      await screen.findByRole("button", { name: /iPhone Person/ });
      await act(async () => outcome.resolve({ state: "rejected", reason: "Pixel restore was rejected." }));

      expect(bridge.restore_contact).toHaveBeenCalledOnce();
      expect(bridge.restore_contact).toHaveBeenCalledWith("book-pixel8", "deleted-pixel");
      expect(bridge.restore_contact).not.toHaveBeenCalledWith("book-iphone", expect.anything());
      expect(container.querySelector("#contacts-error")).toBeNull();
    });

    it("shows a rejected delete outside the editor as an alert", async () => {
      vi.mocked(bridge.submit_contact_edit).mockResolvedValue({ state: "rejected", reason: "The phone refused this delete." });
      render(<ContactsView />);
      fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
      fireEvent.click(screen.getByLabelText("Delete contact"));
      fireEvent.click(screen.getByLabelText("Confirm delete"));

      expect(await screen.findByRole("alert")).toHaveTextContent("The phone refused this delete.");
      expect(screen.queryByRole("form")).not.toBeInTheDocument();
    });

    it("shows a failed restorable-contact list outside the editor as an alert", async () => {
      vi.mocked(bridge.list_restorable_contacts).mockRejectedValueOnce(new Error("Deleted contacts are unavailable."));
      render(<ContactsView />);
      await screen.findByRole("button", { name: /Aurora Chen/ });
      fireEvent.click(screen.getByLabelText("Recently deleted contacts"));

      expect(await screen.findByRole("alert")).toHaveTextContent("Deleted contacts are unavailable.");
    });

    it("keeps a rejected restore error visible inside the recently deleted panel", async () => {
      vi.mocked(bridge.list_restorable_contacts).mockResolvedValueOnce([deletedFromPixel]);
      vi.mocked(bridge.restore_contact).mockResolvedValueOnce({ state: "rejected", reason: "The phone refused this restore." });
      const { container } = render(<ContactsView />);
      await screen.findByRole("button", { name: /Aurora Chen/ });

      fireEvent.click(screen.getByLabelText("Recently deleted contacts"));
      fireEvent.click(await screen.findByRole("button", { name: "Restore Old Friend" }));

      const panel = container.querySelector("#contacts-restore-panel");
      expect(await screen.findByRole("alert")).toHaveTextContent("The phone refused this restore.");
      expect(panel?.querySelector("#contacts-error")).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
      expect(panel?.querySelector("#contacts-error")).toBeNull();
    });
  });
});
