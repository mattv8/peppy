import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge/bridge";

let snapshot: DesktopSnapshot;

beforeEach(async () => {
  vi.restoreAllMocks();
  snapshot = { ...structuredClone(await fixtureBridge.load_state()), mode: "native" };
  vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId) => ({
    ...structuredClone(snapshot), activeConversationId: conversationId ?? snapshot.activeConversationId,
  }));
  vi.spyOn(bridge, "subscribe").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  vi.spyOn(bridge, "mark_all_read").mockResolvedValue("0");
  vi.spyOn(bridge, "search_local").mockImplementation(fixtureBridge.search_local);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const mirrorFolderChanges = () => vi.spyOn(bridge, "set_conversation_folder").mockImplementation(async (id, folder) => {
  snapshot = { ...snapshot, conversations: snapshot.conversations.map((conversation) => conversation.id === id ? { ...conversation, folder } : conversation) };
});

const withFolder = (id: string, folder: "inbox" | "archived" | "spam", unread?: number) => {
  snapshot = { ...snapshot, conversations: snapshot.conversations.map((conversation) => conversation.id === id ? { ...conversation, folder, ...(unread === undefined ? {} : { unread }) } : conversation) };
};

const rowButton = async (id: string) => {
  await waitFor(() => expect(document.querySelector(`[data-conversation-id="${id}"]`)).toBeInTheDocument());
  return document.querySelector<HTMLButtonElement>(`[data-conversation-id="${id}"]`)!;
};

const folderAction = async (selector: string, id: string) => {
  await waitFor(() => expect(document.querySelector(`[${selector}="${id}"]`)).toBeInTheDocument());
  return document.querySelector<HTMLButtonElement>(`[${selector}="${id}"]`)!;
};

const chooseFolder = (label: RegExp) => {
  fireEvent.click(document.getElementById("conversation-folder-trigger")!);
  fireEvent.click(screen.getByRole("menuitemradio", { name: label }));
};

const folderNotice = () => document.getElementById("conversation-folder-notice")!;

describe("App conversation folders", () => {
  it("confirms archiving, filters the Inbox row, and undoes it", async () => {
    const target = snapshot.conversations[0];
    const changeFolder = vi.spyOn(bridge, "set_conversation_folder").mockImplementation(async (id, folder) => {
      snapshot = { ...snapshot, conversations: snapshot.conversations.map((conversation) => conversation.id === id ? { ...conversation, folder } : conversation) };
    });
    render(<App hostKind="browser" />);

    const archive = await waitFor(() => {
      const button = document.querySelector<HTMLButtonElement>(`[data-archive-conversation-id="${target.id}"]`);
      expect(button).toBeTruthy();
      return button!;
    });
    fireEvent.click(archive);
    const confirmation = await screen.findByRole("dialog", { name: /^Archive the conversation with .* on all synced devices\?$/ });
    expect(confirmation).toHaveTextContent(/Archive the conversation with .+ on all synced devices\?/);
    fireEvent.click(within(confirmation).getByRole("button", { name: "Archive" }));

    await waitFor(() => expect(changeFolder).toHaveBeenCalledWith(target.id, "archived"));
    await waitFor(() => expect(document.querySelector(`[data-conversation-row="${target.id}"]`)).not.toBeInTheDocument());
    expect(document.getElementById("conversation-folder-notice")).toHaveTextContent(/Archived .+\./);
    fireEvent.click(within(document.getElementById("conversation-folder-notice")!).getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(changeFolder).toHaveBeenLastCalledWith(target.id, "inbox"));
  });

  it("cancels archive confirmation with Cancel and Escape", async () => {
    const changeFolder = vi.spyOn(bridge, "set_conversation_folder").mockResolvedValue();
    render(<App hostKind="browser" />);
    const archive = await waitFor(() => {
      const button = document.querySelector<HTMLButtonElement>("[data-archive-conversation-id]");
      expect(button).toBeTruthy();
      return button!;
    });
    fireEvent.click(archive);
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Cancel" }));
    expect(changeFolder).not.toHaveBeenCalled();
    fireEvent.click(archive);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(changeFolder).not.toHaveBeenCalled();
  });

  it("filters folders and moves an archived conversation back to Inbox without confirmation", async () => {
    const target = snapshot.conversations[0];
    snapshot = { ...snapshot, conversations: snapshot.conversations.map((conversation) => conversation.id === target.id ? { ...conversation, folder: "archived" } : conversation) };
    const changeFolder = vi.spyOn(bridge, "set_conversation_folder").mockImplementation(async (id, folder) => {
      snapshot = { ...snapshot, conversations: snapshot.conversations.map((conversation) => conversation.id === id ? { ...conversation, folder } : conversation) };
    });
    render(<App hostKind="browser" />);
    const folderTrigger = await waitFor(() => {
      const button = document.getElementById("conversation-folder-trigger");
      expect(button).toBeTruthy();
      return button!;
    });
    fireEvent.click(folderTrigger);
    fireEvent.click(screen.getByRole("menuitemradio", { name: /Archived/ }));
    await waitFor(() => expect(document.querySelector(`[data-conversation-row="${target.id}"]`)).toBeInTheDocument());
    fireEvent.click(document.querySelector<HTMLButtonElement>(`[data-folder-conversation-id="${target.id}"]`)!);
    await waitFor(() => expect(changeFolder).toHaveBeenCalledWith(target.id, "inbox"));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("confirms marking as spam with the spam copy and reports a toast", async () => {
    const target = snapshot.conversations[0];
    const changeFolder = mirrorFolderChanges();
    render(<App hostKind="browser" />);
    fireEvent.click(await folderAction("data-spam-conversation-id", target.id));
    const confirmation = await screen.findByRole("dialog", { name: /^Mark .* as spam on all synced devices\?$/ });
    expect(confirmation).toHaveTextContent(/Mark .+ as spam on all synced devices\?/);
    expect(confirmation).toHaveTextContent("It moves to Spam and stops desktop notifications. The number isn't blocked on your phone or reported to your carrier.");
    expect(changeFolder).not.toHaveBeenCalled();
    fireEvent.click(within(confirmation).getByRole("button", { name: "Mark as spam" }));
    await waitFor(() => expect(changeFolder).toHaveBeenCalledWith(target.id, "spam"));
    await waitFor(() => expect(folderNotice()).toHaveTextContent(/Marked .+ as spam\./));
    expect(within(folderNotice()).getByRole("button", { name: "Undo" })).toBeInTheDocument();
  });

  it("shows the archive error toast and retries the bridge call", async () => {
    const target = snapshot.conversations[0];
    let attempts = 0;
    const changeFolder = vi.spyOn(bridge, "set_conversation_folder").mockImplementation(async (id, folder) => {
      attempts += 1;
      if (attempts === 1) throw new Error("offline");
      snapshot = { ...snapshot, conversations: snapshot.conversations.map((conversation) => conversation.id === id ? { ...conversation, folder } : conversation) };
    });
    render(<App hostKind="browser" />);
    fireEvent.click(await folderAction("data-archive-conversation-id", target.id));
    fireEvent.click(within(await screen.findByRole("dialog", { name: /^Archive the conversation with .* on all synced devices\?$/ })).getByRole("button", { name: "Archive" }));
    await waitFor(() => expect(folderNotice()).toHaveTextContent(/Couldn't archive .+\./));
    expect(folderNotice()).toHaveAttribute("role", "alert");
    expect(changeFolder).toHaveBeenCalledTimes(1);
    fireEvent.click(within(folderNotice()).getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(changeFolder).toHaveBeenCalledTimes(2));
    expect(changeFolder).toHaveBeenLastCalledWith(target.id, "archived");
    await waitFor(() => expect(folderNotice()).toHaveTextContent(/Archived .+\./));
    await waitFor(() => expect(document.querySelector(`[data-conversation-row="${target.id}"]`)).not.toBeInTheDocument());
  });

  it("hides folder actions while the app is locked or the vault is locked", async () => {
    snapshot = { ...snapshot, encryption: { state: "locked" } };
    render(<App hostKind="browser" />);
    await rowButton(snapshot.conversations[0].id);
    expect(document.querySelectorAll(".conversation-folder-action")).toHaveLength(0);
    expect(document.querySelector("[data-archive-conversation-id]")).toBeNull();
    expect(document.querySelector("[data-spam-conversation-id]")).toBeNull();
  });

  it("hides folder actions while the app lock is engaged", async () => {
    vi.spyOn(bridge, "get_app_lock_status").mockResolvedValue({ enabled: true, locked: true, storageAvailable: true });
    const values = new Map([["peppy.setup.mode", "self-hosted"]]);
    vi.stubGlobal("localStorage", { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key) });
    render(<App hostKind="native" />);
    await rowButton(snapshot.conversations[0].id);
    await waitFor(() => expect(document.querySelectorAll(".conversation-folder-action")).toHaveLength(0));
    expect(document.querySelector("[data-archive-conversation-id]")).toBeNull();
    expect(document.querySelector("[data-spam-conversation-id]")).toBeNull();
  });

  it("clears the selected archived conversation without opening another and focuses the next row", async () => {
    const target = snapshot.conversations[0];
    const next = snapshot.conversations[1];
    const others = snapshot.conversations.slice(1);
    const otherMessageIds = others.flatMap((conversation) => conversation.messages.map((message) => message.id));
    mirrorFolderChanges();
    render(<App hostKind="browser" />);
    await waitFor(() => expect(document.querySelector(`[data-conversation-id="${target.id}"][aria-current="location"]`)).toBeInTheDocument());
    await rowButton(next.id);
    const seenBefore = vi.mocked(bridge.mark_seen).mock.calls.length;
    const loadedBefore = vi.mocked(bridge.load_state).mock.calls.length;

    fireEvent.click(await folderAction("data-archive-conversation-id", target.id));
    fireEvent.click(within(await screen.findByRole("dialog", { name: /^Archive the conversation with .* on all synced devices\?$/ })).getByRole("button", { name: "Archive" }));

    await waitFor(() => expect(document.querySelector(`[data-conversation-row="${target.id}"]`)).not.toBeInTheDocument());
    await waitFor(() => expect(document.querySelector(`[data-conversation-id="${next.id}"]`)).toHaveFocus());
    expect(document.querySelector('[aria-current="location"]')).toBeNull();
    expect(document.querySelector(".conversation-row.selected")).toBeNull();
    expect(vi.mocked(bridge.load_state).mock.calls.slice(loadedBefore).some(([id]) => others.some((conversation) => conversation.id === id))).toBe(false);
    const seenAfter = vi.mocked(bridge.mark_seen).mock.calls.slice(seenBefore).flatMap(([ids]) => ids);
    expect(seenAfter.filter((id) => otherMessageIds.includes(id))).toEqual([]);
  });

  it("shows the empty labels for the archived and spam views", async () => {
    render(<App hostKind="browser" />);
    await rowButton(snapshot.conversations[0].id);
    chooseFolder(/Archived/);
    expect(await screen.findByText("No archived conversations")).toBeInTheDocument();
    chooseFolder(/Spam/);
    expect(await screen.findByText("No spam conversations")).toBeInTheDocument();
  });

  it("counts only inbox unread messages for Mark all read", async () => {
    const [inbox, archived, spam] = snapshot.conversations;
    withFolder(inbox.id, "inbox", 2);
    withFolder(archived.id, "archived", 3);
    withFolder(spam.id, "spam", 5);
    render(<App hostKind="browser" />);
    const action = await screen.findByRole("button", { name: "Mark all as read" });
    expect(action).toBeEnabled();
    expect(document.getElementById("mark-all-read-count")).toHaveTextContent("2 unread messages");
  });

  it("disables Mark all read when only archived and spam conversations are unread", async () => {
    const [inbox, archived, spam] = snapshot.conversations;
    withFolder(inbox.id, "inbox", 0);
    withFolder(archived.id, "archived", 3);
    withFolder(spam.id, "spam", 5);
    render(<App hostKind="browser" />);
    expect(await screen.findByRole("button", { name: "Mark all as read" })).toBeDisabled();
  });

  it("switches to Archived when search opens an archived conversation", async () => {
    const target = snapshot.conversations[1];
    withFolder(target.id, "archived");
    render(<App hostKind="browser" />);
    await rowButton(snapshot.conversations[0].id);
    vi.mocked(bridge.search_local).mockImplementation(async ({ query }) => ({
      query,
      items: [{ kind: "conversation", conversationId: target.id, title: target.name, subtitle: target.preview }],
      indexState: "ready",
      hasMore: { conversation: false, message: false, contact: false },
    }));
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: target.name } });
    await waitFor(() => expect(document.querySelector("[data-search-hit]")).not.toBeNull());
    fireEvent.click(document.querySelector<HTMLElement>("[data-search-hit]")!);
    await waitFor(() => expect(document.getElementById("conversation-folder-trigger")).toHaveTextContent("Archived"));
    expect(await rowButton(target.id)).toHaveAttribute("aria-current", "location");
  });

  it("moves a spam conversation out of Spam without confirmation", async () => {
    const target = snapshot.conversations[1];
    withFolder(target.id, "spam");
    const changeFolder = mirrorFolderChanges();
    render(<App hostKind="browser" />);
    await rowButton(snapshot.conversations[0].id);
    chooseFolder(/Spam/);
    fireEvent.click(await folderAction("data-folder-conversation-id", target.id));
    await waitFor(() => expect(changeFolder).toHaveBeenCalledWith(target.id, "inbox"));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    await waitFor(() => expect(folderNotice()).toHaveTextContent(/Moved .+ out of Spam\./));
  });

  it("ignores a second Not spam click on the next row within the settle window", async () => {
    const [, first, second] = snapshot.conversations;
    withFolder(first.id, "spam");
    withFolder(second.id, "spam");
    const changeFolder = mirrorFolderChanges();
    render(<App hostKind="browser" />);
    await rowButton(snapshot.conversations[0].id);
    chooseFolder(/Spam/);
    fireEvent.click(await folderAction("data-folder-conversation-id", first.id));
    await waitFor(() => expect(changeFolder).toHaveBeenCalledWith(first.id, "inbox"));
    await waitFor(() => expect(document.querySelector(`[data-folder-conversation-id="${second.id}"]`)).toBeEnabled());
    fireEvent.click(document.querySelector<HTMLButtonElement>(`[data-folder-conversation-id="${second.id}"]`)!);
    expect(changeFolder).toHaveBeenCalledTimes(1);
  });
});
