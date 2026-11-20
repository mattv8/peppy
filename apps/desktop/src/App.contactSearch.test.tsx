import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge/bridge";
import { contactPage, mockContactAurora, mockContactRiver } from "./contacts/contacts-test-utils";

let snapshot: DesktopSnapshot;
let fixtureSnapshot: DesktopSnapshot | undefined;

const CHIP = "Search everything, not just Contacts";

const openContacts = async () => {
  fireEvent.click(await screen.findByRole("button", { name: /^Contacts/ }));
  await screen.findByRole("region", { name: "Contacts browser" });
};

beforeEach(async () => {
  fixtureSnapshot ??= structuredClone(await fixtureBridge.load_state());
  snapshot = structuredClone(fixtureSnapshot);
  vi.spyOn(bridge, "load_state").mockImplementation(async id => ({
    ...structuredClone(snapshot),
    activeConversationId: id ?? snapshot.activeConversationId,
  }));
  vi.spyOn(bridge, "subscribe").mockReturnValue(() => {});
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => {});
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => {});
  Object.defineProperty(Element.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
  vi.spyOn(bridge, "search_local").mockResolvedValue({
    query: "",
    items: [],
    hasMore: { conversation: false, message: false, contact: false },
    indexState: "ready",
  });
  vi.spyOn(bridge, "list_contacts_page").mockImplementation(async (_bookId, { through, query }) => contactPage(
    [mockContactAurora, mockContactRiver].filter(contact => !query || contact.displayName.toLowerCase().includes(query.toLowerCase())),
    through,
  ));
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it("scopes the titlebar search to contacts and filters the list through the server query", async () => {
  render(<App />);
  await openContacts();

  const input = screen.getByRole("searchbox", { name: "Search Peppy" });
  expect(input).toHaveAttribute("placeholder", "Search contacts");
  expect(screen.getByRole("button", { name: CHIP })).toHaveTextContent("Contacts");

  fireEvent.change(input, { target: { value: "river" } });

  await waitFor(() => expect(bridge.list_contacts_page).toHaveBeenLastCalledWith(expect.any(String), expect.objectContaining({ query: "river" })));
  await waitFor(() => expect(document.getElementById("contacts-count")).toHaveTextContent("1 contact matches “river”"));
  expect(screen.getByRole("list", { name: /^Contacts in / })).toHaveTextContent("River Park");
});

it("exits the Contacts chip to global search", async () => {
  render(<App />);
  await openContacts();

  fireEvent.click(screen.getByRole("button", { name: CHIP }));

  expect(screen.queryByRole("button", { name: CHIP })).not.toBeInTheDocument();
  expect(screen.getByRole("combobox", { name: "Search Peppy" })).toHaveAttribute("aria-expanded", "true");
});

it("exits the Contacts chip with Backspace on an empty input", async () => {
  render(<App />);
  await openContacts();

  const input = screen.getByRole("searchbox", { name: "Search Peppy" });
  input.focus();
  fireEvent.keyDown(input, { key: "Backspace" });

  expect(screen.queryByRole("button", { name: CHIP })).not.toBeInTheDocument();
});

it("clears the contact query when leaving Contacts and re-arms the chip on return", async () => {
  render(<App />);
  await openContacts();
  fireEvent.change(screen.getByRole("searchbox", { name: "Search Peppy" }), { target: { value: "river" } });
  await screen.findByText(/1 contact matches/);

  fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
  await openContacts();

  expect(screen.getByRole("button", { name: CHIP })).toBeInTheDocument();
  expect(screen.getByRole("searchbox", { name: "Search Peppy" })).toHaveValue("");
});

it("re-arms the chip on return after it was exited", async () => {
  render(<App />);
  await openContacts();
  fireEvent.click(screen.getByRole("button", { name: CHIP }));

  fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
  await openContacts();

  expect(screen.getByRole("button", { name: CHIP })).toBeInTheDocument();
});

it("scopes Conversations results and re-arms after switching tabs", async () => {
  vi.mocked(bridge.search_local).mockImplementation(async ({ query }) => ({
    query,
    items: [
      { kind: "conversation", conversationId: "c1", title: "Found conversation", subtitle: "Recent" },
      { kind: "contact", bookId: "book", contactId: "contact", title: "Hidden contact", subtitle: "", phones: [], phoneCount: 0 },
    ],
    hasMore: { conversation: false, message: false, contact: true },
    indexState: "ready",
  }));
  render(<App />);
  const chip = await screen.findByRole("button", { name: "Search everything, not just Conversations" });
  const input = screen.getByRole("combobox", { name: "Search Peppy" });
  expect(input).toHaveAttribute("placeholder", "Search conversations and messages");
  fireEvent.change(input, { target: { value: "found" } });
  expect(await screen.findByText("Found conversation")).toBeInTheDocument();
  expect(screen.queryByText("Hidden contact")).not.toBeInTheDocument();
  fireEvent.click(chip);
  expect(await screen.findByText("Hidden contact")).toBeInTheDocument();
  await openContacts();
  fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
  expect(await screen.findByRole("button", { name: "Search everything, not just Conversations" })).toBeInTheDocument();
});

it("keeps the Notifications chip on the Notifications tab, not Contacts", async () => {
  render(<App />);
  fireEvent.click(screen.getByRole("button", { name: /^Notifications/ }));
  await screen.findByRole("region", { name: "Notifications" });

  expect(screen.getByRole("button", { name: "Search everything, not just Notifications" })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: CHIP })).not.toBeInTheDocument();
});
