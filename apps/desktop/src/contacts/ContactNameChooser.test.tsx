import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useRef, useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ContactNameChooser } from "./ContactNameChooser";
import type { ContactBookView } from "../bridge/bridge";

const books: ContactBookView[] = [{ id: "book-1234", deviceName: "Phone", state: "active", capabilities: { canWrite: true, canDelete: true, supportsNotes: true, supportsPhoto: true }, contactCount: 1, pendingEditCount: 0 }];
const candidate = { bookId: "book-1234", contactId: "contact-1", displayName: "Ada" };
const makeBridge = () => ({
  list_contact_name_candidates: vi.fn().mockResolvedValue({ candidates: [candidate], pinned: null, moreIndexing: false }),
  set_contact_name_pin: vi.fn().mockResolvedValue(undefined),
  clear_contact_name_pin: vi.fn().mockResolvedValue(undefined),
});

afterEach(cleanup);

describe("ContactNameChooser", () => {
  it("loads candidates, remembers a choice, and resets it", async () => {
    const bridge = makeBridge();
    const refresh = vi.fn();
    render(<ContactNameChooser state="conflict" address="+18015551234" sourceDeviceId="device" books={books} bridge={bridge} onClose={vi.fn()} onRefresh={refresh} onOpenContact={vi.fn()} />);
    expect(screen.getByText("Loading contacts…")).toBeInTheDocument();
    const radio = await screen.findByRole("radio");
    expect(bridge.list_contact_name_candidates).toHaveBeenCalledWith("+18015551234", "device");
    expect(screen.getByText("ID 1234")).toBeInTheDocument();
    fireEvent.click(radio);
    fireEvent.click(screen.getByText("Remember on this computer"));
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Using Ada on this computer."));
    expect(bridge.set_contact_name_pin).toHaveBeenCalledWith("+18015551234", "book-1234", "contact-1", "device");
    expect(screen.getByRole("status")).toHaveTextContent("Using Ada on this computer.");
    fireEvent.click(screen.getByText("Reset choice"));
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Choice cleared. Choose a name again."));
    expect(bridge.clear_contact_name_pin).toHaveBeenCalledWith("+18015551234", "device");
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("preselects a pin and reports save errors", async () => {
    const bridge = makeBridge();
    bridge.list_contact_name_candidates.mockResolvedValue({ candidates: [candidate], pinned: { bookId: candidate.bookId, contactId: candidate.contactId }, moreIndexing: false });
    bridge.set_contact_name_pin.mockRejectedValue(new Error("failed"));
    render(<ContactNameChooser state="pinned" address="+18015551234" books={books} bridge={bridge} onClose={vi.fn()} onRefresh={vi.fn()} onOpenContact={vi.fn()} />);
    expect(await screen.findByRole("radio")).toBeChecked();
    fireEvent.click(screen.getByText("Remember on this computer"));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("Couldn't save this choice. Try again."));
  });

  it("closes on Escape and returns focus to its trigger", async () => {
    const bridge = makeBridge();
    function Harness() {
      const [open, setOpen] = useState(false);
      const trigger = useRef<HTMLButtonElement>(null);
      return <><button ref={trigger} onClick={() => setOpen(true)}>Contact sources</button>{open && <ContactNameChooser state="conflict" address="+18015551234" books={books} bridge={bridge} onClose={() => { setOpen(false); trigger.current?.focus(); }} onRefresh={vi.fn()} onOpenContact={vi.fn()} />}</>;
    }
    render(<Harness />);
    const trigger = screen.getByRole("button", { name: "Contact sources" });
    fireEvent.click(trigger);
    await screen.findByRole("radio");
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });

  it("reports load failure and retries", async () => {
    const bridge = makeBridge();
    bridge.list_contact_name_candidates.mockRejectedValueOnce(new Error("failed"));
    render(<ContactNameChooser state="conflict" address="+18015551234" books={books} bridge={bridge} onClose={vi.fn()} onRefresh={vi.fn()} onOpenContact={vi.fn()} />);
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("Couldn't load contacts. Try again."));
    expect(screen.getByRole("heading")).toHaveFocus();
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("radio")).toHaveFocus();
    expect(bridge.list_contact_name_candidates).toHaveBeenCalledTimes(2);
  });

  it("shows indexing status and retries rather than an empty result", async () => {
    const bridge = makeBridge();
    bridge.list_contact_name_candidates.mockResolvedValueOnce({ candidates: [], pinned: null, moreIndexing: true });
    render(<ContactNameChooser state="consensus" address="+18015551234" books={books} bridge={bridge} onClose={vi.fn()} onRefresh={vi.fn()} onOpenContact={vi.fn()} />);
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Contacts are still being indexed. Try again in a moment."));
    expect(screen.getByRole("heading", { name: "Contact sources" })).toHaveFocus();
    expect(screen.queryByText("No contacts available.")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("radio")).toHaveFocus();
  });

  it("focuses the pinned radio and respects an already handled Escape", async () => {
    const bridge = makeBridge();
    bridge.list_contact_name_candidates.mockResolvedValue({ candidates: [candidate], pinned: { bookId: candidate.bookId, contactId: candidate.contactId }, moreIndexing: false });
    const onClose = vi.fn();
    render(<ContactNameChooser state="pinned" address="+18015551234" books={books} bridge={bridge} onClose={onClose} onRefresh={vi.fn()} onOpenContact={vi.fn()} />);
    const radio = await screen.findByRole("radio");
    expect(radio).toHaveFocus();
    const event = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    event.preventDefault();
    radio.dispatchEvent(event);
    expect(onClose).not.toHaveBeenCalled();
  });

  it("renders 32 candidates while keeping actions available", async () => {
    const bridge = makeBridge();
    bridge.list_contact_name_candidates.mockResolvedValue({ candidates: Array.from({ length: 32 }, (_, index) => ({ ...candidate, contactId: `contact-${index}`, displayName: `Name ${index}` })), pinned: null, moreIndexing: false });
    render(<ContactNameChooser state="conflict" address="+18015551234" books={books} bridge={bridge} onClose={vi.fn()} onRefresh={vi.fn()} onOpenContact={vi.fn()} />);
    expect(await screen.findAllByRole("radio")).toHaveLength(32);
    expect(screen.getByRole("button", { name: "Remember on this computer" })).toBeVisible();
    expect(document.getElementById("contact-source-actions")).toBeVisible();
  });
});
