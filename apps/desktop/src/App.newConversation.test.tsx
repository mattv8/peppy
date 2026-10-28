import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge } from "./bridge";

beforeEach(async () => {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  });
  const snapshot = structuredClone(await fixtureBridge.load_state());
  vi.spyOn(bridge, "load_state").mockImplementation(async id => ({ ...structuredClone(snapshot), activeConversationId: id ?? snapshot.activeConversationId }));
  vi.spyOn(bridge, "subscribe").mockReturnValue(() => {});
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => {});
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => {});
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
});

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

it("keeps the navbar action visible but disabled until messaging state is ready", () => {
  vi.mocked(bridge.load_state).mockImplementation(() => new Promise(() => {}));
  render(<App />);
  const action = within(document.getElementById("desktop-titlebar")!).getByRole("button", { name: "New conversation" });
  expect(action).toBeVisible();
  expect(action).toBeDisabled();
});

it("creates a conversation only after the contact navigation guard approves", async () => {
  render(<App />);
  await screen.findByRole("textbox", { name: "Message" });
  fireEvent.click(screen.getByRole("button", { name: /^Contacts/ }));
  fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
  fireEvent.click(screen.getByLabelText("Edit contact"));
  fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Unsaved" } });
  const compose = within(document.getElementById("desktop-titlebar")!).getByRole("button", { name: "New conversation" });
  fireEvent.click(compose);
  fireEvent.click(await screen.findByRole("button", { name: "Keep editing" }));
  expect(screen.getByLabelText("Given name")).toHaveValue("Unsaved");
  expect(document.querySelector('[data-conversation-id="conv-aurora"]')).toHaveAttribute("aria-current", "location");
  fireEvent.click(compose);
  fireEvent.click(await screen.findByRole("button", { name: "Discard" }));
  await waitFor(() => expect(document.getElementById("conversation-pane")).toHaveAttribute("data-view", "conversations"));
  expect(document.getElementById("draft-recipients")).toBeInTheDocument();
});
