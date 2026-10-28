import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { conversationAvatar, incomingToneSeed, paletteIndex, seedKey } from "@peppy/desktop-ui";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot, type MessageView } from "./bridge";

let currentSnapshot: DesktopSnapshot;
let stateHint: (() => void) | undefined;

const toneVariable = (seed: string) => `var(--peppy-avatar-${paletteIndex(seed)}, var(--peppy-accent))`;

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
};

function message(id: string, senderAddress?: string): MessageView {
  return {
    id,
    revision: id,
    sender: "other",
    senderAddress,
    body: id,
    timestamp: "1788434460000",
    attachments: [],
  };
}

beforeEach(async () => {
  vi.restoreAllMocks();
  const storage = new Map<string, string>();
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, value),
      removeItem: (key: string) => storage.delete(key),
      clear: () => storage.clear(),
    },
  });
  currentSnapshot = structuredClone(await fixtureBridge.load_state());
  currentSnapshot.encryption.profileFingerprint = "tone-profile";
  stateHint = undefined;
  vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId) => ({
    ...structuredClone(currentSnapshot),
    activeConversationId: conversationId ?? currentSnapshot.activeConversationId,
  }));
  vi.spyOn(bridge, "get_app_lock_status").mockResolvedValue({ enabled: false, locked: false, storageAvailable: true });
  vi.spyOn(bridge, "subscribe").mockImplementation((listener) => {
    stateHint = listener;
    return () => undefined;
  });
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("App conversation tones", () => {
  it("saves a header preset, applies the refreshed snapshot, and resets it to Automatic", async () => {
    const seed = seedKey("+12025550123");
    const saveTone = vi.spyOn(bridge, "set_conversation_tone").mockImplementation(async (savedSeed, index) => {
      currentSnapshot.toneOverrides = index === null
        ? {}
        : { [savedSeed]: index };
    });

    render(<App />);

    const trigger = await screen.findByRole("button", { name: "Change conversation color" });
    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("radio", { name: "Blue" }));

    await waitFor(() => expect(saveTone).toHaveBeenCalledWith(seed, 99));
    await waitFor(() => expect(document.querySelector("#thread-contact-avatar .contact-avatar")).toHaveAttribute("data-avatar-color", "99"));

    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("radio", { name: "Automatic" }));

    await waitFor(() => expect(saveTone).toHaveBeenLastCalledWith(seed, null));
    await waitFor(() => expect(document.querySelector("#thread-contact-avatar .contact-avatar")).toHaveAttribute("data-avatar-color", String(paletteIndex(seed))));
    expect(bridge.load_state).toHaveBeenCalledWith(undefined, undefined, { metadataOnly: true });
  });

  it("uses the one-to-one avatar seed and per-sender group seeds for incoming bubbles", async () => {
    const oneToOne = currentSnapshot.conversations.find((conversation) => conversation.id === "conv-aurora")!;
    oneToOne.messages = [message("one-to-one", "+12025550123")];
    render(<App />);

    await screen.findByText("one-to-one");
    const oneToOneBubble = document.querySelector<HTMLElement>('[data-message-row="one-to-one"] .message-bubble')!;
    expect(oneToOneBubble.style.getPropertyValue("--peppy-tone")).toBe(toneVariable(seedKey("+12025550123")));

    cleanup();
    const group = currentSnapshot.conversations.find((conversation) => conversation.id === "conv-group-numbers")!;
    group.messages = [message("sender-a", "+18015551234"), message("unknown-sender")];
    currentSnapshot.activeConversationId = group.id;
    render(<App />);

    await screen.findByText("sender-a");
    const groupSeed = conversationAvatar({ id: group.id, name: group.name, participants: group.participants }).seed;
    const senderBubble = document.querySelector<HTMLElement>('[data-message-row="sender-a"] .message-bubble')!;
    const fallbackBubble = document.querySelector<HTMLElement>('[data-message-row="unknown-sender"] .message-bubble')!;
    expect(senderBubble.style.getPropertyValue("--peppy-tone")).toBe(toneVariable(incomingToneSeed(groupSeed, "+18015551234")));
    expect(fallbackBubble.style.getPropertyValue("--peppy-tone")).toBe(toneVariable(groupSeed));
  });

  it("keeps an active contact editor and its unsaved draft when a color save refreshes metadata", async () => {
    const contact = {
      id: "contact-aurora",
      bookId: "book-pixel8",
      revision: "1",
      displayName: "Aurora Chen",
      givenName: "Aurora",
      familyName: "Chen",
      phones: [{ label: "mobile", number: "+12025550123" }],
      emails: [],
      addresses: [],
    };
    currentSnapshot.activeConversationId = "conv-river";
    vi.spyOn(bridge, "list_contact_books").mockResolvedValue([{
      id: "book-pixel8",
      deviceName: "Pixel 8",
      state: "active",
      capabilities: { canWrite: true, canDelete: true, supportsNotes: false, supportsPhoto: false },
      contactCount: 1,
      pendingEditCount: 0,
    }]);
    vi.spyOn(bridge, "list_contacts").mockResolvedValue([contact]);
    const saveTone = vi.spyOn(bridge, "set_conversation_tone").mockImplementation(async (seed, index) => {
      currentSnapshot.toneOverrides = index === null ? {} : { [seed]: index };
    });

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Contacts" }));
    fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
    fireEvent.click(screen.getByLabelText("Edit contact"));
    const givenName = screen.getByLabelText("Given name");
    fireEvent.change(givenName, { target: { value: "Unsaved Aurora" } });
    fireEvent.click(screen.getByRole("button", { name: /Conversation color: Automatic/ }));
    fireEvent.click(screen.getByRole("radio", { name: "Blue" }));

    await waitFor(() => expect(saveTone).toHaveBeenCalledWith(seedKey("+12025550123"), 99));
    await waitFor(() => expect(givenName).toHaveValue("Unsaved Aurora"));
    expect(screen.getByLabelText("Given name")).toHaveValue("Unsaved Aurora");
    expect(bridge.load_state).toHaveBeenLastCalledWith(undefined, undefined, { metadataOnly: true });
    expect(currentSnapshot.activeConversationId).toBe("conv-river");
  });

  it("drops a delayed color result after the vault session changes", async () => {
    const pendingSave = deferred<void>();
    const seed = seedKey("+12025550123");
    const saveTone = vi.spyOn(bridge, "set_conversation_tone").mockReturnValue(pendingSave.promise);
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "Change conversation color" }));
    fireEvent.click(screen.getByRole("radio", { name: "Blue" }));
    await waitFor(() => expect(saveTone).toHaveBeenCalledWith(seed, 99));

    currentSnapshot = { ...currentSnapshot, encryption: { ...currentSnapshot.encryption, profileFingerprint: "other-profile" }, toneOverrides: {} };
    await act(async () => stateHint?.());
    await waitFor(() => expect(document.querySelector("#thread-contact-avatar .contact-avatar")).toHaveAttribute("data-avatar-color", String(paletteIndex(seed))));
    await act(async () => pendingSave.resolve());

    await waitFor(() => expect(document.querySelector("#thread-contact-avatar .contact-avatar")).toHaveAttribute("data-avatar-color", String(paletteIndex(seed))));
  });

  it("prevents duplicate pending saves and lets a failed color save be retried", async () => {
    const firstSave = deferred<void>();
    const seed = seedKey("+12025550123");
    const saveTone = vi.spyOn(bridge, "set_conversation_tone")
      .mockReturnValueOnce(firstSave.promise)
      .mockResolvedValueOnce();
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "Change conversation color" }));
    const blue = screen.getByRole("radio", { name: "Blue" });
    fireEvent.click(blue);
    fireEvent.click(blue);
    expect(saveTone).toHaveBeenCalledTimes(1);
    await act(async () => firstSave.reject(new Error("offline")));
    await screen.findByText("Couldn't save the color. Try again.");

    fireEvent.click(blue);
    await waitFor(() => expect(saveTone).toHaveBeenNthCalledWith(2, seed, 99));
  });
});
