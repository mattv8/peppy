import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, fixtureReactionConversations } from "./bridge/bridge";

let lifecycle: Parameters<typeof bridge.subscribe_lifecycle>[0];

beforeEach(async () => {
  vi.restoreAllMocks();
  const snapshot = structuredClone(await fixtureBridge.load_state());
  snapshot.conversations = [structuredClone(fixtureReactionConversations.find((conversation) => conversation.id === "conv-reaction-states")!)];
  snapshot.activeConversationId = "conv-reaction-states";
  vi.spyOn(bridge, "load_state").mockImplementation(async () => structuredClone(snapshot));
  vi.spyOn(bridge, "load_message_window").mockImplementation(async (input) => ({ conversationId: input.conversationId, messages: snapshot.conversations[0].messages, hasOlder: false, hasNewer: false }));
  vi.spyOn(bridge, "subscribe").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle").mockImplementation((listener) => { lifecycle = listener; return () => undefined; });
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "acknowledge_lifecycle").mockResolvedValue();
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  Object.defineProperty(Element.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
    const top = this.id === "message-list" ? 0 : this.id === "composer-area" ? 700 : 200;
    const bottom = this.id === "message-list" ? 800 : this.id === "composer-area" ? 780 : 260;
    return { top, bottom, left: 0, right: 400, width: 400, height: bottom - top, x: 0, y: top, toJSON() {} };
  });
});

afterEach(() => { cleanup(); vi.useRealTimers(); vi.restoreAllMocks(); });

async function startReaction() {
  render(<App hostKind="browser" />);
  await waitFor(() => expect(document.querySelector('article[data-message-id="reaction-group-dinner"]')).not.toBeNull());
  const article = document.querySelector<HTMLElement>('article[data-message-id="reaction-group-dinner"]')!;
  act(() => article.focus());
  fireEvent.keyDown(article, { key: "Enter" });
  fireEvent.click(document.querySelector('[data-reaction-quick="heart"]')!);
}

describe("reaction lifecycle settlement", () => {
  it.each([true, false])("waits for the reaction before close, accepted: %s", async (accepted) => {
    let resolve!: (value: { accepted: boolean; reason?: "offline" }) => void;
    vi.spyOn(bridge, "react_to_message").mockReturnValue(new Promise((done) => { resolve = done; }));
    await startReaction();
    await act(async () => lifecycle({ id: "close-reaction", action: "close" }));
    expect(bridge.acknowledge_lifecycle).not.toHaveBeenCalled();
    await act(async () => resolve({ accepted, reason: accepted ? undefined : "offline" }));
    await waitFor(() => expect(bridge.acknowledge_lifecycle).toHaveBeenCalledWith("close-reaction", accepted, ...(accepted ? [] : ["send"])));
    if (!accepted) expect(screen.getByText("Your reaction wasn't sent, so the window stayed open. You can close it again now.")).toBeInTheDocument();
  });

  it("does not wait for a reaction on collapse", async () => {
    vi.spyOn(bridge, "react_to_message").mockReturnValue(new Promise(() => {}));
    await startReaction();
    await act(async () => lifecycle({ id: "collapse-reaction", action: "collapse" }));
    await waitFor(() => expect(bridge.acknowledge_lifecycle).toHaveBeenCalledWith("collapse-reaction", true));
  });

  it("blocks close at the shared timeout", async () => {
    vi.spyOn(bridge, "react_to_message").mockReturnValue(new Promise(() => {}));
    await startReaction();
    vi.useFakeTimers({ shouldAdvanceTime: true, toFake: ["setTimeout", "clearTimeout", "Date"] });
    await act(async () => lifecycle({ id: "slow-reaction", action: "close" }));
    await act(async () => { await vi.advanceTimersByTimeAsync(6000); });
    expect(bridge.acknowledge_lifecycle).toHaveBeenCalledWith("slow-reaction", false, "send");
    expect(screen.getByText("Your reaction is still sending. You can close this window again once it finishes.")).toBeInTheDocument();
  });
});
