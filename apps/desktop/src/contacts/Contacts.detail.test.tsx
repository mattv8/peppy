import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ContactsView } from "./Contacts";
import { ContactDetail } from "./ContactDetail";
import { bridge, type ContactBookView, type ContactView } from "../bridge/bridge";
import { contactPage, deferred, DeferredCropImage, mockContactAurora, mockContactBook, mockContactRiver, resetContactBridgeMocks, restoreContactDomGlobals } from "./contacts-test-utils";

vi.mock("../bridge/bridge", { spy: true });

const compactMatchMedia = (query: string) => ({
  matches: query === "(max-width: 560px)",
  media: query,
  onchange: null,
  addEventListener: vi.fn(),
  removeEventListener: vi.fn(),
  addListener: vi.fn(),
  removeListener: vi.fn(),
  dispatchEvent: vi.fn(),
});

const adaLovelace = { ...mockContactAurora, id: "contact-ada", displayName: "Ada Lovelace", givenName: "Ada", familyName: "Lovelace" };

function renderDetail(nameFormat: "first-last" | "last-first", contact = adaLovelace) {
  return render(
    <ContactDetail
      contact={contact}
      canWrite={false}
      confirm={false}
      nameFormat={nameFormat}
      onEdit={vi.fn()}
      onDelete={vi.fn()}
      onCancelConfirm={vi.fn()}
      onClose={vi.fn()}
    />,
  );
}

describe("ContactsView detail panel", () => {
  beforeEach(() => {
    resetContactBridgeMocks();
  });

  afterEach(() => {
    restoreContactDomGlobals();
  });

  it("closes the detail from the X and returns focus to the contact row", async () => {
    const { container } = render(<ContactsView />);
    const row = await screen.findByRole("button", { name: /Aurora Chen/ });
    fireEvent.click(row);
    await screen.findByRole("heading", { level: 2, name: "Aurora Chen" });
    expect(row).toHaveAttribute("aria-current", "location");

    fireEvent.click(screen.getByRole("button", { name: "Close contact" }));

    expect(container.querySelector("#contacts-detail-panel")).toBeNull();
    expect(row).not.toHaveAttribute("aria-current");
    await waitFor(() => expect(document.activeElement).toBe(row));
  });

  it("places the X after the h2 and every contact banner after the header", async () => {
    vi.mocked(bridge.list_contacts_page).mockResolvedValue(contactPage([
      { ...mockContactAurora, pendingEditId: "edit-aurora", pendingEditState: "pending", pendingEditSummary: "Phone number update" },
      mockContactRiver,
    ]));
    const { container } = render(<ContactsView />);
    fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));

    const header = container.querySelector("#contacts-detail-header");
    const heading = screen.getByRole("heading", { level: 2, name: "Aurora Chen" });
    const close = screen.getByRole("button", { name: "Close contact" });
    expect(header).not.toBeNull();
    expect(header).toContainElement(heading);
    expect(header).toContainElement(close);
    expect(heading.compareDocumentPosition(close) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

    const banners = container.querySelectorAll("[data-contact-banner]");
    expect(banners).toHaveLength(1);
    banners.forEach(banner => {
      expect(header!.compareDocumentPosition(banner) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    });
  });

  it("hides the X at the compact width and keeps Back as the only closer", async () => {
    const originalMatchMedia = window.matchMedia;
    window.matchMedia = vi.fn(compactMatchMedia) as unknown as typeof window.matchMedia;
    try {
      render(<ContactsView />);
      fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
      await screen.findByRole("button", { name: "Back" });

      expect(screen.queryByRole("button", { name: "Close contact" })).toBeNull();
      expect(screen.getByRole("button", { name: "Back" })).toBeInTheDocument();
    } finally {
      window.matchMedia = originalMatchMedia;
    }
  });

  it("renders the name heading as h2 in the default first-last format", () => {
    renderDetail("first-last");

    expect(screen.getByRole("heading", { level: 2, name: "Ada Lovelace" })).toBeInTheDocument();
  });

  it("renders the name heading as Family, Given in last-first format", () => {
    renderDetail("last-first");

    expect(screen.getByRole("heading", { level: 2, name: "Lovelace, Ada" })).toBeInTheDocument();
  });
});

const picked = { dataUrl: "data:image/png;base64,transparent", naturalWidth: 256, naturalHeight: 256 };
const croppedDataUrl = "data:image/jpeg;base64,cropped";
const storedPhoto = "data:image/png;base64,stored";
const withStoredPhoto: ContactView = { ...mockContactAurora, photoDataUrl: storedPhoto };

function renderPhoto({
  contact = mockContactAurora,
  book = mockContactBook,
  photoLocked = false,
}: { contact?: ContactView; book?: ContactBookView; photoLocked?: boolean } = {}) {
  const onPhotoSubmitted = vi.fn();
  const props = {
    book,
    canWrite: book.capabilities.canWrite,
    confirm: false,
    nameFormat: "first-last" as const,
    photoLocked,
    onEdit: vi.fn(),
    onDelete: vi.fn(),
    onCancelConfirm: vi.fn(),
    onClose: vi.fn(),
    onPhotoSubmitted,
  };
  const utils = render(<ContactDetail {...props} contact={contact} />);
  return {
    ...utils,
    onPhotoSubmitted,
    rerenderContact: (next: ContactView) => utils.rerender(<ContactDetail {...props} contact={next} />),
  };
}

const photoButton = (container: HTMLElement) => container.querySelector<HTMLButtonElement>("#contacts-detail-photo")!;

async function confirmCrop() {
  expect(await screen.findByRole("dialog", { name: "Crop photo to square" })).toBeInTheDocument();
  act(() => DeferredCropImage.instances[0]?.onload?.());
  fireEvent.click(screen.getByRole("button", { name: "Confirm crop" }));
}

describe("ContactDetail photo pencil", () => {
  beforeEach(() => {
    resetContactBridgeMocks();
    DeferredCropImage.instances = [];
    vi.stubGlobal("Image", DeferredCropImage);
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({ fillStyle: "", fillRect: vi.fn(), drawImage: vi.fn() } as unknown as CanvasRenderingContext2D);
    vi.spyOn(HTMLCanvasElement.prototype, "toDataURL").mockReturnValue(croppedDataUrl);
    vi.mocked(bridge.pick_contact_photo).mockResolvedValue(picked);
    vi.mocked(bridge.submit_contact_edit).mockResolvedValue({ state: "pending", requestId: "request-photo" });
  });

  afterEach(() => {
    restoreContactDomGlobals();
    vi.restoreAllMocks();
  });

  it("renders no photo button for read-only, removed, retired or non-photo books", () => {
    const books: ContactBookView[] = [
      { ...mockContactBook, capabilities: { ...mockContactBook.capabilities, canWrite: false } },
      { ...mockContactBook, sourceRevoked: true },
      { ...mockContactBook, state: "retired" },
      { ...mockContactBook, capabilities: { ...mockContactBook.capabilities, supportsPhoto: false } },
    ];
    for (const book of books) {
      const { container, unmount } = renderPhoto({ book });
      expect(photoButton(container)).toBeNull();
      expect(screen.queryByRole("button", { name: /Change photo/ })).toBeNull();
      expect(container.querySelector("#contacts-detail-header")).not.toBeNull();
      unmount();
    }
  });

  it("hides the photo button while the editor is open", async () => {
    const { container } = render(<ContactsView />);
    fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
    expect(photoButton(container)).not.toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Edit contact" }));

    await waitFor(() => expect(container.querySelector("#contacts-detail-header")).toBeNull());
    expect(container.querySelector("#contacts-detail-photo")).toBeNull();
  });

  it("sends one photo-only edit after the crop is confirmed, then reports it", async () => {
    const { container, onPhotoSubmitted } = renderPhoto();

    fireEvent.click(photoButton(container));
    await waitFor(() => expect(bridge.pick_contact_photo).toHaveBeenCalledTimes(1));
    await confirmCrop();

    await waitFor(() => expect(bridge.submit_contact_edit).toHaveBeenCalledTimes(1));
    expect(bridge.submit_contact_edit).toHaveBeenCalledWith({
      targetBookId: "book-pixel8",
      kind: "update",
      contactId: "contact-aurora",
      baseRevision: "1",
      patches: [],
      photo: { kind: "set", croppedDataUrl },
    });
    await waitFor(() => expect(onPhotoSubmitted).toHaveBeenCalledTimes(1));
  });

  it("reloads the contact list after a photo edit from the ContactsView detail", async () => {
    render(<ContactsView />);
    fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
    const listCallsBefore = vi.mocked(bridge.list_contacts_page).mock.calls.length;

    fireEvent.click(screen.getByRole("button", { name: "Change photo for Aurora Chen" }));
    await confirmCrop();

    await waitFor(() => expect(bridge.submit_contact_edit).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(vi.mocked(bridge.list_contacts_page).mock.calls.length).toBeGreaterThan(listCallsBefore));
  });

  it("opens a menu with Choose photo and Remove photo when a photo exists", () => {
    const { container } = renderPhoto({ contact: withStoredPhoto });

    fireEvent.click(photoButton(container));

    expect(photoButton(container)).toHaveAttribute("aria-haspopup", "menu");
    expect(photoButton(container)).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("menu")).toHaveAttribute("id", "contacts-detail-photo-menu");
    expect(screen.getByRole("menuitem", { name: "Choose photo…" })).toHaveAttribute("data-action", "change-photo");
    expect(screen.getByRole("menuitem", { name: "Remove photo" })).toHaveAttribute("data-action", "remove-photo");
    expect(bridge.pick_contact_photo).not.toHaveBeenCalled();
  });

  it("opens the picker from Choose photo and crops the new photo before sending it", async () => {
    const { container } = renderPhoto({ contact: withStoredPhoto });

    fireEvent.click(photoButton(container));
    fireEvent.click(screen.getByRole("menuitem", { name: "Choose photo…" }));
    await confirmCrop();

    await waitFor(() => expect(bridge.submit_contact_edit).toHaveBeenCalledWith(expect.objectContaining({ photo: { kind: "set", croppedDataUrl } })));
  });

  it("sends a photo-only remove edit from Remove photo", async () => {
    const { container, onPhotoSubmitted } = renderPhoto({ contact: withStoredPhoto });

    fireEvent.click(photoButton(container));
    fireEvent.click(screen.getByRole("menuitem", { name: "Remove photo" }));

    await waitFor(() => expect(bridge.submit_contact_edit).toHaveBeenCalledTimes(1));
    expect(bridge.submit_contact_edit).toHaveBeenCalledWith({
      targetBookId: "book-pixel8",
      kind: "update",
      contactId: "contact-aurora",
      baseRevision: "1",
      patches: [],
      photo: { kind: "remove" },
    });
    await waitFor(() => expect(onPhotoSubmitted).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("moves through menu items with arrow keys, closes on Escape with focus back on the button, and closes on an outside click", () => {
    const { container } = renderPhoto({ contact: withStoredPhoto });
    const button = photoButton(container);

    fireEvent.click(button);
    const menu = screen.getByRole("menu");
    expect(screen.getByRole("menuitem", { name: "Choose photo…" })).toHaveFocus();

    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(screen.getByRole("menuitem", { name: "Remove photo" })).toHaveFocus();
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(screen.getByRole("menuitem", { name: "Choose photo…" })).toHaveFocus();

    fireEvent.keyDown(menu, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    expect(button).toHaveFocus();
    expect(button).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(button);
    expect(screen.getByRole("menu")).toBeInTheDocument();
    fireEvent.mouseDown(document.body);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("shows the submitted crop as a pending preview with a dashed ring and a clock badge", async () => {
    const { container, rerenderContact } = renderPhoto();

    fireEvent.click(photoButton(container));
    await confirmCrop();
    await waitFor(() => expect(bridge.submit_contact_edit).toHaveBeenCalledTimes(1));
    rerenderContact({ ...mockContactAurora, pendingEditState: "pending", pendingEditSummary: "Photo change" });

    expect(photoButton(container)).toHaveAttribute("data-photo-state", "pending");
    expect(photoButton(container).querySelector("img")).toHaveAttribute("src", croppedDataUrl);
    expect(photoButton(container).querySelector(".contacts-detail-photo-badge svg")).not.toBeNull();
    expect(container.querySelector("[data-contact-banner]")).toHaveTextContent("Photo change — Waiting for the phone");
  });

  it("shows the stored photo with an error ring after a failed photo change, and Try again opens the picker", async () => {
    const { container, rerenderContact } = renderPhoto({ contact: withStoredPhoto });

    fireEvent.click(photoButton(container));
    fireEvent.click(screen.getByRole("menuitem", { name: "Choose photo…" }));
    await confirmCrop();
    await waitFor(() => expect(bridge.submit_contact_edit).toHaveBeenCalledTimes(1));
    rerenderContact({ ...withStoredPhoto, pendingEditState: "pending" });
    rerenderContact({ ...withStoredPhoto, pendingEditState: "conflict", pendingEditSummary: "Photo change" });

    expect(photoButton(container)).toHaveAttribute("data-photo-state", "error");
    expect(photoButton(container).querySelector("img")).toHaveAttribute("src", storedPhoto);
    const banner = container.querySelector("[data-contact-banner]")!;
    expect(banner).toHaveTextContent("Photo change wasn't applied on the phone.");

    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(bridge.pick_contact_photo).toHaveBeenCalledTimes(2));
  });

  it("shows a thrown send failure below the header and clears it on the next attempt", async () => {
    vi.mocked(bridge.submit_contact_edit).mockRejectedValueOnce(new Error("Relay offline"));
    const { container } = renderPhoto();

    fireEvent.click(photoButton(container));
    await confirmCrop();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveAttribute("id", "contacts-detail-photo-error");
    expect(alert).toHaveTextContent("Couldn't send the photo to Pixel 8. Relay offline");
    const header = container.querySelector("#contacts-detail-header")!;
    expect(header).not.toContainElement(alert);
    expect(header.compareDocumentPosition(alert) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

    fireEvent.click(photoButton(container));
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("disables the photo button with the reason while the last edit is pending", () => {
    const { container } = renderPhoto({ contact: { ...mockContactAurora, pendingEditState: "pending" } });
    const button = photoButton(container);
    const reason = "Waiting for the phone to apply your last change.";

    expect(button).toHaveAttribute("aria-disabled", "true");
    expect(button).toHaveAttribute("title", reason);
    expect(document.getElementById(button.getAttribute("aria-describedby")!)).toHaveTextContent(reason);
    fireEvent.click(button);
    expect(bridge.pick_contact_photo).not.toHaveBeenCalled();
  });

  it("disables the photo button with the unlock reason while sync is locked", () => {
    const { container } = renderPhoto({ photoLocked: true });
    const button = photoButton(container);

    expect(button).toHaveAttribute("aria-disabled", "true");
    expect(button).toHaveAttribute("title", "Unlock sync to change photos.");
    fireEvent.click(button);
    expect(bridge.pick_contact_photo).not.toHaveBeenCalled();
  });
});

describe("ContactDetail photo while an edit is in flight", () => {
  beforeEach(() => {
    resetContactBridgeMocks();
    DeferredCropImage.instances = [];
    vi.stubGlobal("Image", DeferredCropImage);
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({ fillStyle: "", fillRect: vi.fn(), drawImage: vi.fn() } as unknown as CanvasRenderingContext2D);
    vi.spyOn(HTMLCanvasElement.prototype, "toDataURL").mockReturnValue(croppedDataUrl);
    vi.mocked(bridge.pick_contact_photo).mockResolvedValue(picked);
    vi.mocked(bridge.submit_contact_edit).mockResolvedValue({ state: "pending", requestId: "request-photo" });
  });

  afterEach(() => {
    restoreContactDomGlobals();
    vi.restoreAllMocks();
  });

  it("does not start a second pick or submit from the photo button while one is in flight", async () => {
    const submit = deferred<{ state: "pending"; requestId: string }>();
    vi.mocked(bridge.submit_contact_edit).mockReturnValue(submit.promise);
    const { container, onPhotoSubmitted } = renderPhoto();
    fireEvent.click(photoButton(container));
    await waitFor(() => expect(bridge.pick_contact_photo).toHaveBeenCalledTimes(1));
    await confirmCrop();
    await waitFor(() => expect(bridge.submit_contact_edit).toHaveBeenCalledTimes(1));
    expect(photoButton(container)).toHaveAttribute("aria-disabled", "true");
    expect(photoButton(container)).toHaveAttribute("aria-busy", "true");

    fireEvent.click(photoButton(container));

    expect(bridge.pick_contact_photo).toHaveBeenCalledTimes(1);
    expect(bridge.submit_contact_edit).toHaveBeenCalledTimes(1);
    await act(async () => submit.resolve({ state: "pending", requestId: "request-photo" }));
    await waitFor(() => expect(onPhotoSubmitted).toHaveBeenCalledTimes(1));
    expect(photoButton(container)).not.toHaveAttribute("aria-busy");
  });

  it("returns focus to the photo button after Remove photo", async () => {
    const { container } = renderPhoto({ contact: withStoredPhoto });
    fireEvent.click(photoButton(container));

    fireEvent.click(screen.getByRole("menuitem", { name: "Remove photo" }));

    expect(photoButton(container)).toHaveFocus();
    await waitFor(() => expect(bridge.submit_contact_edit).toHaveBeenCalledWith(expect.objectContaining({ photo: { kind: "remove" } })));
  });

  it("disables Close while a save is in flight, like Back", () => {
    render(
      <ContactDetail contact={adaLovelace} canWrite={false} confirm={false} nameFormat="first-last" busy onEdit={vi.fn()} onDelete={vi.fn()} onCancelConfirm={vi.fn()} onClose={vi.fn()} />,
    );

    expect(screen.getByRole("button", { name: "Close contact" })).toBeDisabled();
  });
});
