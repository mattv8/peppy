import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge/bridge";

const LAYOUT_KEY = "peppy.layout.v1";

let currentSnapshot: DesktopSnapshot;
let storage: Map<string, string>;

const storedLayout = () => JSON.parse(storage.get(LAYOUT_KEY) ?? "{}") as Record<string, unknown>;
const shell = () => document.getElementById("desktop-shell");
const selectAccent = async (name: string) => {
  fireEvent.click(screen.getByRole("button", { name: /Accent color:/ }));
  fireEvent.click(await screen.findByRole("radio", { name }));
};

beforeEach(async () => {
  vi.restoreAllMocks();
  storage = new Map();
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
  vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId) => ({
    ...structuredClone(currentSnapshot),
    activeConversationId: conversationId ?? currentSnapshot.activeConversationId,
  }));
  vi.spyOn(bridge, "get_app_lock_status").mockResolvedValue({ enabled: false, locked: false, storageAvailable: true, pending: false });
  vi.spyOn(bridge, "subscribe").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
});

afterEach(() => {
  cleanup();
  window.history.replaceState({}, "", "/");
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("App accent color", () => {
  it("places the default accent picker after Theme and preserves layout siblings when selecting Blue", async () => {
    storage.set(LAYOUT_KEY, JSON.stringify({ theme: "dark", composerHeight: 128 }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Settings" }));

    const theme = screen.getByLabelText("Theme");
    const accent = screen.getByRole("button", { name: "Accent color: Default (Coral)" });
    expect(theme.compareDocumentPosition(accent) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.getByLabelText("Accent color", { selector: "button" })).toBe(accent);

    fireEvent.click(accent);
    fireEvent.click(document.getElementById("settings-accent-color-heading")!);
    expect(screen.getByRole("dialog", { name: "Accent color" })).toBeVisible();
    fireEvent.click(document.getElementById("settings-accent-color-panel")!);
    expect(screen.getByRole("dialog", { name: "Accent color" })).toBeVisible();
    fireEvent.click(await screen.findByRole("radio", { name: "Blue" }));

    await waitFor(() => expect(shell()).toHaveAttribute("data-accent", "99"));
    expect(document.documentElement).toHaveAttribute("data-accent", "99");
    expect(document.body).toHaveAttribute("data-accent", "99");
    expect(storedLayout()).toMatchObject({ theme: "dark", composerHeight: 128, accentColor: 99 });
    await waitFor(() => expect(screen.getByRole("button", { name: "Accent color: Blue" })).toHaveFocus());
    expect(screen.queryByRole("radio", { name: "Blue" })).not.toBeInTheDocument();
  });

  it("clears the stored preference for Default (Coral) while the Coral preset remains an explicit default", async () => {
    storage.set(LAYOUT_KEY, JSON.stringify({ accentColor: 99, theme: "light" }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Settings" }));

    await selectAccent("Default (Coral)");
    await waitFor(() => expect(shell()).not.toHaveAttribute("data-accent"));
    expect(document.documentElement).not.toHaveAttribute("data-accent");
    expect(document.body).not.toHaveAttribute("data-accent");
    expect(storedLayout()).toEqual({ theme: "light" });

    await selectAccent("Coral");
    await waitFor(() => expect(shell()).not.toHaveAttribute("data-accent"));
    expect(storedLayout()).toMatchObject({ theme: "light", accentColor: 7 });
  });

  it("applies a stored accent on startup and accepts another window's update", async () => {
    storage.set(LAYOUT_KEY, JSON.stringify({ accentColor: 99 }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Settings" }));

    expect(shell()).toHaveAttribute("data-accent", "99");
    await selectAccent("Violet");
    expect(shell()).toHaveAttribute("data-accent", "47");
    storage.set(LAYOUT_KEY, JSON.stringify({ accentColor: 99 }));
    act(() => {
      window.dispatchEvent(new StorageEvent("storage", { key: LAYOUT_KEY, newValue: JSON.stringify({ accentColor: 99 }) }));
    });
    await waitFor(() => expect(shell()).toHaveAttribute("data-accent", "99"));
  });

  it("applies the accent to composer and head-panel shells", async () => {
    storage.set(LAYOUT_KEY, JSON.stringify({ accentColor: 99 }));
    window.history.replaceState({}, "", "/?window=composer&conversationId=conv-aurora&head=1");
    render(<App />);

    await waitFor(() => expect(document.getElementById("composer-shell")).toHaveAttribute("data-accent", "99"));
    expect(document.getElementById("composer-shell")).toHaveClass("head-panel-shell");
  });

  it("keeps conversation tone picker labels unchanged", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Change conversation color" }));
    expect(screen.getByRole("dialog", { name: "Conversation color" })).toBeVisible();
  });

  it("keeps an unsaved accent in this session and links its persistence error", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Settings" }));
    vi.spyOn(localStorage, "setItem").mockImplementation(() => { throw new Error("unavailable"); });

    await selectAccent("Blue");

    expect(shell()).toHaveAttribute("data-accent", "99");
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn't save the accent color. It will reset when Peppy restarts.");
    expect(screen.getByRole("button", { name: "Accent color: Blue" })).toHaveAttribute("aria-describedby", "settings-accent-color-error");
  });
});
