import "@testing-library/jest-dom/vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { bridge, type ContactBookView } from "../bridge/bridge";
import { ContactsView } from "./Contacts";
import { ContactsActionNotice } from "./ContactsActionNotice";
import type { ContactActionNotice } from "./contact-action-notice";
import { mockContactBook, resetContactBridgeMocks, restoreContactDomGlobals } from "./contacts-test-utils";

vi.mock("../bridge/bridge", { spy: true });

const removedBook: ContactBookView = { ...mockContactBook, id: "book-iphone", deviceName: "iPhone", state: "retired", sourceRevoked: true, contactCount: 803 };
const readyReadiness = { state: "ready" as const, contacts_ready: true, server_active: true, backfill_unreadable: 0 };
const backfillReadiness = { ...readyReadiness, state: "backfill_pending" as const, server_active: true };

describe("ContactsView status slot priority", () => {
  beforeEach(() => resetContactBridgeMocks());
  afterEach(() => restoreContactDomGlobals());

  it("shows a load error before the removed-book strip", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    vi.mocked(bridge.list_contacts).mockRejectedValue(new Error("boom"));
    render(<ContactsView />);
    await waitFor(() => expect(document.querySelector("#contacts-status")).toHaveAttribute("data-status-kind", "error"));
    expect(document.querySelector("#contacts-error")).toBeInTheDocument();
    expect(document.getElementById("contacts-book-removed")).not.toBeInTheDocument();
  });

  it("shows the removed-book strip before the sync status", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    render(<ContactsView sync={{ repairRequired: true }} />);
    await waitFor(() => expect(document.querySelector("#contacts-status")).toHaveAttribute("data-status-kind", "removed"));
    expect(document.getElementById("contacts-book-removed")).toBeInTheDocument();
    expect(document.getElementById("contacts-sync-status")).not.toBeInTheDocument();
  });

  it("hides the unavailable line for a removed book", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([removedBook, mockContactBook]);
    render(<ContactsView />);
    await waitFor(() => expect(document.querySelector("#contacts-status")).toHaveAttribute("data-status-kind", "removed"));
    expect(document.getElementById("contacts-sync-unavailable")).not.toBeInTheDocument();
  });

  it("shows the sync status before the readiness status", async () => {
    const { container } = render(<ContactsView sync={{ repairRequired: true, readiness: backfillReadiness }} />);
    await waitFor(() => expect(container.querySelector("#contacts-status")).toHaveAttribute("data-status-kind", "sync"));
    expect(container.querySelector("#contacts-readiness-status")).not.toBeInTheDocument();
  });

  it("shows the readiness status before the scan status", async () => {
    vi.mocked(bridge.get_contact_sync_request).mockResolvedValue({ requestId: "scan", bookId: "book-pixel8", state: "waiting" });
    const { container } = render(<ContactsView sync={{ repairRequired: false, readiness: backfillReadiness }} />);
    await waitFor(() => expect(container.querySelector("#contacts-status")).toHaveAttribute("data-status-kind", "readiness"));
    expect(container.querySelector("#contacts-scan-status")).not.toBeInTheDocument();
  });

  it("shows the scan status when nothing higher is present", async () => {
    vi.mocked(bridge.get_contact_sync_request).mockResolvedValue({ requestId: "scan", bookId: "book-pixel8", state: "waiting" });
    const { container } = render(<ContactsView sync={{ repairRequired: false, readiness: readyReadiness }} />);
    await waitFor(() => expect(container.querySelector("#contacts-status")).toHaveAttribute("data-status-kind", "scan"));
    expect(container.querySelector("#contacts-scan-status")).toHaveTextContent("Waiting for the phone to scan contacts.");
  });

  it("shows the unavailable line alone when no other strip applies", async () => {
    const { container } = render(<ContactsView sync={{ repairRequired: false, readiness: readyReadiness }} />);
    await waitFor(() => expect(container.querySelector("#contacts-status")).toHaveAttribute("data-status-kind", "unavailable"));
    expect(container.querySelector("#contacts-sync-unavailable")).toHaveTextContent("This phone does not support remote contact scans.");
  });

  it("points the sync button at the unavailable line only while that line is the active strip", async () => {
    const { container } = render(<ContactsView sync={{ repairRequired: false, readiness: readyReadiness }} />);
    await waitFor(() => expect(container.querySelector("#contacts-status")).toHaveAttribute("data-status-kind", "unavailable"));
    expect(container.querySelector("#contacts-sync-now")).toHaveAttribute("aria-describedby", "contacts-sync-unavailable");
    expect(container.querySelector("#contacts-sync-unavailable")).toBeInTheDocument();
  });

  it("leaves the sync button undescribed while a higher strip hides the unavailable line", async () => {
    vi.mocked(bridge.get_contact_sync_request).mockResolvedValue({ requestId: "scan", bookId: "book-pixel8", state: "waiting" });
    const { container } = render(<ContactsView sync={{ repairRequired: false, readiness: readyReadiness }} />);
    await waitFor(() => expect(container.querySelector("#contacts-status")).toHaveAttribute("data-status-kind", "scan"));
    expect(container.querySelector("#contacts-sync-unavailable")).not.toBeInTheDocument();
    expect(container.querySelector("#contacts-sync-now")).not.toHaveAttribute("aria-describedby");
  });

  it("keeps the completed-scan wording on the sync strip while a repair is pending", async () => {
    vi.mocked(bridge.get_contact_sync_request).mockResolvedValue({ requestId: "scan", bookId: "book-pixel8", state: "completed" });
    const { container } = render(<ContactsView sync={{ repairRequired: true, projection: { state: "rebuilding" } }} />);
    await waitFor(() => expect(container.querySelector("#contacts-status")).toHaveAttribute("data-status-kind", "sync"));
    await waitFor(() => expect(container.querySelector("#contacts-sync-status")).toHaveTextContent("The phone finished its scan. Contacts are still being repaired locally."));
    expect(container.querySelector("#contacts-scan-status")).not.toBeInTheDocument();
  });

  it("renders no status slot when nothing persistent applies", async () => {
    vi.mocked(bridge.list_contact_books).mockResolvedValue([{ ...mockContactBook, capabilities: { ...mockContactBook.capabilities, canSync: true } }]);
    const { container } = render(<ContactsView sync={{ repairRequired: false, readiness: readyReadiness }} />);
    await screen.findByLabelText("New contact");
    expect(container.querySelector("#contacts-status")).toBeNull();
  });
});

describe("sync status icons", () => {
  beforeEach(() => resetContactBridgeMocks());
  afterEach(() => restoreContactDomGlobals());

  it("spins the icon while rebuilding and uses a clock or warning icon for the rest", async () => {
    const { container, rerender } = render(<ContactsView sync={{ repairRequired: false, projection: { state: "rebuilding" } }} />);
    let status = container.querySelector("#contacts-sync-status")!;
    expect(status).toHaveAttribute("data-sync-state", "rebuilding");
    expect(status.querySelector("svg")).toHaveClass("contacts-sync-icon");
    expect(status.querySelector("svg")).toHaveAttribute("aria-hidden", "true");

    rerender(<ContactsView sync={{ repairRequired: true, projection: { state: "failed" } }} />);
    status = container.querySelector("#contacts-sync-status")!;
    expect(status).toHaveAttribute("data-sync-state", "repairWaiting");
    expect(status.querySelector("svg")).toHaveClass("lucide-clock");
    expect(status.querySelector("svg")).not.toHaveClass("contacts-sync-icon");

    rerender(<ContactsView sync={{ repairRequired: true }} />);
    status = container.querySelector("#contacts-sync-status")!;
    expect(status).toHaveAttribute("data-sync-state", "needsRepair");
    expect(status.querySelector("svg")).toHaveClass("lucide-clock");

    rerender(<ContactsView sync={{ repairRequired: false, projection: { state: "failed" } }} />);
    status = container.querySelector("#contacts-sync-status")!;
    expect(status).toHaveAttribute("data-sync-state", "failed");
    expect(status.querySelector("svg")).toHaveClass("lucide-triangle-alert");
  });
});

describe("ContactsActionNotice", () => {
  const success: ContactActionNotice = { kind: "success", message: "Names and recipient search no longer use Pixel 8." };
  const failure: ContactActionNotice = { kind: "error", message: "Couldn't save this change. Try again.", retry: vi.fn() };

  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    restoreContactDomGlobals();
  });

  function renderNotice(notice?: ContactActionNotice) {
    const onDismiss = vi.fn();
    render(<ContactsActionNotice notice={notice} onDismiss={onDismiss} />);
    const toast = () => document.getElementById("contacts-action-notice")!;
    return { onDismiss, toast };
  }

  it("stays mounted and empty when there is no notice", () => {
    const { toast } = renderNotice();
    expect(toast()).toBeInTheDocument();
    expect(toast()).toBeEmptyDOMElement();
  });

  it("announces a success politely and dismisses it after 4 seconds", () => {
    const { onDismiss, toast } = renderNotice(success);
    expect(toast()).toHaveAttribute("role", "status");
    expect(toast()).toHaveAttribute("aria-live", "polite");
    expect(toast()).toHaveAttribute("data-placement", "list");
    expect(toast()).toHaveTextContent("Names and recipient search no longer use Pixel 8.");
    act(() => vi.advanceTimersByTime(3999));
    expect(onDismiss).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(1));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("pauses a success dismissal while hovered and restarts it on leave", () => {
    const { onDismiss, toast } = renderNotice(success);
    fireEvent.mouseEnter(toast());
    act(() => vi.advanceTimersByTime(10000));
    expect(onDismiss).not.toHaveBeenCalled();
    fireEvent.mouseLeave(toast());
    act(() => vi.advanceTimersByTime(3999));
    expect(onDismiss).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(1));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("pauses a success dismissal while focus is inside the toast", () => {
    const { onDismiss } = renderNotice(success);
    const dismiss = screen.getByRole("button", { name: "Dismiss notification" });
    act(() => dismiss.focus());
    act(() => vi.advanceTimersByTime(10000));
    expect(onDismiss).not.toHaveBeenCalled();
  });

  it("keeps an error until it is dismissed, with role alert and no timer", () => {
    const { onDismiss, toast } = renderNotice(failure);
    expect(toast()).toHaveAttribute("role", "alert");
    expect(toast()).not.toHaveAttribute("aria-live");
    act(() => vi.advanceTimersByTime(60000));
    expect(onDismiss).not.toHaveBeenCalled();
  });

  it("clears the error first, then calls retry from Try again", () => {
    const calls: string[] = [];
    const retry = vi.fn(() => calls.push("retry"));
    const onDismiss = vi.fn(() => calls.push("dismiss"));
    render(<ContactsActionNotice notice={{ ...failure, retry }} onDismiss={onDismiss} />);
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(calls).toEqual(["dismiss", "retry"]);
  });

  it("hides Try again for a success notice", () => {
    renderNotice(success);
    expect(screen.queryByRole("button", { name: "Try again" })).not.toBeInTheDocument();
  });

  it("dismisses from the X button", () => {
    const { onDismiss } = renderNotice(failure);
    fireEvent.click(screen.getByRole("button", { name: "Dismiss notification" }));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });
});

describe("ContactsView action notice placement", () => {
  beforeEach(() => resetContactBridgeMocks());
  afterEach(() => restoreContactDomGlobals());

  it("shows a removal result in the toast inside the center pane", async () => {
    let books = [removedBook, mockContactBook];
    vi.mocked(bridge.list_contact_books).mockImplementation(async () => books);
    vi.mocked(bridge.forget_contact_book).mockImplementation(async id => {
      books = books.filter(item => item.id !== id);
    });
    render(<ContactsView />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove from this computer" }));
    fireEvent.click(screen.getByRole("dialog").querySelector<HTMLButtonElement>(".destructive-button")!);
    const toast = await waitFor(() => {
      const element = document.getElementById("contacts-action-notice");
      expect(element).toHaveTextContent("Removed iPhone from this computer.");
      return element!;
    });
    expect(toast.closest("#contacts-center-pane")).not.toBeNull();
    expect(toast).toHaveAttribute("role", "status");
  });
});
