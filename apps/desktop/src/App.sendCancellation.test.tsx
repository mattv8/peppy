import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Draft } from "@peppy/desktop-ui/bridge-contracts";
import { App, cancellationDisplayState } from "./App";
import { bridge, fixtureBridge } from "./bridge/bridge";
import { CANCELLED_ATTACHMENTS_SKIPPED_NOTICE } from "./drafts/cancelledSendRestore";

afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

const fixtureLoadState = fixtureBridge.load_state;

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => { resolve = next; });
  return { promise, resolve };
};

const mockCancellableAurora = () => vi.spyOn(bridge, "load_state").mockImplementation(async (...args) => {
  const snapshot = await fixtureLoadState(...args);
  return {
    ...snapshot,
    draft: undefined,
    conversations: snapshot.conversations.map(conversation => conversation.id === "conv-aurora"
      ? {
          ...conversation,
          messages: conversation.messages.map(message => message.id === "message-aurora-2"
            ? { ...message, sendCancellation: { ...message.sendCancellation!, state: "none", canCancel: true } }
            : message),
        }
      : conversation),
  };
});

describe("send cancellation", () => {
  it.each(["too-late", "unknown"] as const)("keeps the local %s outcome over an older requested projection", (outcome) => {
    expect(cancellationDisplayState("requested", outcome)).toBe(outcome);
  });

  it("keeps an authoritative requested projection over a later transport failure", () => {
    expect(cancellationDisplayState("requested", "failed")).toBe("requested");
  });

  it("does not let an unmounted request focus a newer pending row", async () => {
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => { frames.push(callback); return frames.length; });
    const first = deferred<{ state: "requested"; canCancel: boolean; requiresGatewayUpdate: boolean }>();
    const second = deferred<{ state: "requested"; canCancel: boolean; requiresGatewayUpdate: boolean }>();
    const cancelSend = vi.spyOn(bridge, "cancel_send").mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const oldView = render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Cancel send and remove message" }));
    oldView.unmount();

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Cancel send and remove message" }));
    frames.length = 0;
    second.resolve({ state: "requested", canCancel: false, requiresGatewayUpdate: false });
    const pending = (await screen.findByText("Cancelling…")).closest<HTMLElement>("[data-message-cancellation]")!;
    await waitFor(() => expect(frames).toHaveLength(1));
    act(() => frames.shift()!(0));
    const titlebar = screen.getByRole("button", { name: "New conversation" });
    titlebar.focus();
    first.resolve({ state: "requested", canCancel: false, requiresGatewayUpdate: false });
    await waitFor(() => expect(frames).toHaveLength(0));
    expect(pending).not.toHaveFocus();
    expect(titlebar).toHaveFocus();
    cancelSend.mockRestore();
  });

  it("does not steal focus from the composer after the cancellation request starts", async () => {
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => { frames.push(callback); return frames.length; });
    const request = deferred<{ state: "requested"; canCancel: boolean; requiresGatewayUpdate: boolean }>();
    const cancelSend = vi.spyOn(bridge, "cancel_send").mockReturnValueOnce(request.promise);
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "Cancel send and remove message" }));
    const composer = document.getElementById("composer-textarea")!;
    composer.focus();
    frames.length = 0;
    request.resolve({ state: "requested", canCancel: false, requiresGatewayUpdate: false });
    await screen.findByText("Cancelling…");
    act(() => frames.shift()!(0));

    expect(composer).toHaveFocus();
    cancelSend.mockRestore();
  });

  it("moves keyboard-initiated cancellation focus to the pending replacement", async () => {
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => { frames.push(callback); return frames.length; });
    const request = deferred<{ state: "requested"; canCancel: boolean; requiresGatewayUpdate: boolean }>();
    const cancelSend = vi.spyOn(bridge, "cancel_send").mockReturnValueOnce(request.promise);
    render(<App />);

    const cancel = await screen.findByRole("button", { name: "Cancel send and remove message" });
    cancel.focus();
    fireEvent.click(cancel);
    frames.length = 0;
    request.resolve({ state: "requested", canCancel: false, requiresGatewayUpdate: false });
    const pending = (await screen.findByText("Cancelling…")).closest<HTMLElement>("[data-message-cancellation]")!;
    await waitFor(() => expect(frames).toHaveLength(1));
    act(() => frames.shift()!(0));

    expect(pending).toHaveFocus();
    cancelSend.mockRestore();
  });
  it("keeps the outgoing body when the durable cancellation request fails", async () => {
    const cancelSend = vi.spyOn(bridge, "cancel_send").mockRejectedValueOnce(new Error("offline"));
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "Cancel send and remove message" }));

    expect(screen.getAllByText(/received by the phone/)).not.toHaveLength(0);
    cancelSend.mockRestore();
  });

  it("restores source content with a sanitized notice when cancellation is too late", async () => {
    const cancelSend = vi.spyOn(bridge, "cancel_send").mockResolvedValueOnce({ state: "too-late", canCancel: false, requiresGatewayUpdate: false });
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "Cancel send and remove message" }));

    const notice = await screen.findByText("Cancellation was too late.");
    expect(notice).toHaveAttribute("data-message-cancellation-notice", "message-aurora-2");
    expect(notice.closest('[data-message-id="message-aurora-2"]')).toHaveTextContent(/received by the phone/);
    cancelSend.mockRestore();
  });

  it("replaces an eligible fixture message with its durable pending state", async () => {
    vi.spyOn(bridge, "cancel_send").mockResolvedValueOnce({ state: "requested", canCancel: false, requiresGatewayUpdate: false });
    render(<App />);

    const cancel = await screen.findByRole("button", { name: "Cancel send and remove message" });
    fireEvent.click(cancel);

    expect((await screen.findByText("Cancelling…")).closest("[data-message-cancellation]")).toHaveAttribute("data-message-cancellation", "message-aurora-2");
    expect(screen.queryByText("I'll have it to you shortly.")).not.toBeInTheDocument();
    expect(screen.queryByText("Subject: Fixture subject")).not.toBeInTheDocument();
    expect(document.querySelector('[data-message-id="message-aurora-2"] [data-attachment-id="attachment-aurora-1"]')).not.toBeInTheDocument();
  });

  it("returns a requested cancellation's text and attachments to the composer", async () => {
    const loadState = mockCancellableAurora();
    const cancelSend = vi.spyOn(bridge, "cancel_send").mockResolvedValueOnce({ state: "requested", canCancel: false, requiresGatewayUpdate: false });
    const saveDraft = vi.spyOn(bridge, "save_draft").mockImplementation(async input => ({ ...input, id: "draft-aurora", revision: "1" }));
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "Cancel send and remove message" }));

    await waitFor(() => expect(document.getElementById("composer-textarea")).toHaveValue("I'll have it to you shortly.\nIt was received by the phone and can still be cancelled."));
    await waitFor(() => expect(saveDraft).toHaveBeenCalledWith(expect.objectContaining({ attachmentIds: ["attachment-aurora-1"] })));
    saveDraft.mockRestore();
    cancelSend.mockRestore();
    loadState.mockRestore();
  });

  it("replaces existing composer text when a cancellation is requested", async () => {
    const loadState = mockCancellableAurora();
    const cancelSend = vi.spyOn(bridge, "cancel_send").mockResolvedValueOnce({ state: "requested", canCancel: false, requiresGatewayUpdate: false });
    const saveDraft = vi.spyOn(bridge, "save_draft").mockImplementation(async input => ({ ...input, id: "draft-aurora", revision: "1" }));
    render(<App />);

    await waitFor(() => expect(document.getElementById("composer-textarea")).not.toBeNull());
    const composer = document.getElementById("composer-textarea")!;
    fireEvent.change(composer, { target: { value: "Existing composer text" } });
    await waitFor(() => expect(composer).toHaveValue("Existing composer text"));
    fireEvent.click(await screen.findByRole("button", { name: "Cancel send and remove message" }));

    await waitFor(() => expect(composer).toHaveValue("I'll have it to you shortly.\nIt was received by the phone and can still be cancelled."));
    saveDraft.mockRestore();
    cancelSend.mockRestore();
    loadState.mockRestore();
  });

  it.each([
    { name: "too late", result: { state: "too-late", canCancel: false, requiresGatewayUpdate: false } },
    { name: "unknown", result: { state: "unknown", canCancel: false, requiresGatewayUpdate: false } },
    { name: "rejected", result: new Error("offline") },
  ] as const)("keeps the composer unchanged when cancellation is $name", async ({ result }) => {
    const loadState = mockCancellableAurora();
    const saveDraft = vi.spyOn(bridge, "save_draft").mockImplementation(async input => ({ ...input, id: "draft-aurora", revision: "1" }));
    const cancelSend = result instanceof Error
      ? vi.spyOn(bridge, "cancel_send").mockRejectedValueOnce(result)
      : vi.spyOn(bridge, "cancel_send").mockResolvedValueOnce(result);
    render(<App />);

    await waitFor(() => expect(document.getElementById("composer-textarea")).not.toBeNull());
    const composer = document.getElementById("composer-textarea")!;
    fireEvent.change(composer, { target: { value: "Keep this draft" } });
    await waitFor(() => expect(composer).toHaveValue("Keep this draft"));
    await act(async () => { fireEvent.click(await screen.findByRole("button", { name: "Cancel send and remove message" })); });

    await waitFor(() => expect(composer).toHaveValue("Keep this draft"));
    saveDraft.mockRestore();
    cancelSend.mockRestore();
    loadState.mockRestore();
  });

  it("keeps the most recently clicked cancellation when replies arrive out of order", async () => {
    const loadState = vi.spyOn(bridge, "load_state").mockImplementation(async (...args) => {
      const snapshot = await fixtureLoadState(...args);
      const conversation = snapshot.conversations.find(candidate => candidate.id === "conv-aurora")!;
      const first = conversation.messages.find(candidate => candidate.id === "message-aurora-2")!;
      const cancellableFirst = { ...first, sendCancellation: { ...first.sendCancellation!, state: "none" as const, canCancel: true } };
      const second = {
        ...cancellableFirst,
        id: "message-aurora-3",
        body: "A newer cancellation should win.",
        attachments: cancellableFirst.attachments.map(attachment => ({ ...attachment, id: "attachment-aurora-2" })),
      };
      return {
        ...snapshot,
        draft: undefined,
        conversations: snapshot.conversations.map(candidate => candidate.id === conversation.id
          ? { ...candidate, messages: [...candidate.messages.filter(message => message.id !== first.id), cancellableFirst, second] }
          : candidate),
      };
    });
    const first = deferred<{ state: "requested"; canCancel: boolean; requiresGatewayUpdate: boolean }>();
    const second = deferred<{ state: "requested"; canCancel: boolean; requiresGatewayUpdate: boolean }>();
    const cancelSend = vi.spyOn(bridge, "cancel_send").mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const saveDraft = vi.spyOn(bridge, "save_draft").mockImplementation(async input => ({ ...input, id: "draft-aurora", revision: "1" }));
    render(<App />);

    const [firstCancel, secondCancel] = await screen.findAllByRole("button", { name: "Cancel send and remove message" });
    fireEvent.click(firstCancel);
    fireEvent.click(secondCancel);
    await act(async () => { second.resolve({ state: "requested", canCancel: false, requiresGatewayUpdate: false }); });
    await waitFor(() => expect(document.getElementById("composer-textarea")).toHaveValue("A newer cancellation should win."));
    await act(async () => { first.resolve({ state: "requested", canCancel: false, requiresGatewayUpdate: false }); });

    await waitFor(() => expect(document.getElementById("composer-textarea")).toHaveValue("A newer cancellation should win."));
    saveDraft.mockRestore();
    cancelSend.mockRestore();
    loadState.mockRestore();
  });

  it("rolls back a requested cancellation when the refreshed message is too late", async () => {
    let tooLate = false;
    let notify: (() => void) | undefined;
    let hostDraft: Draft = {
      id: "draft-aurora",
      conversationId: "conv-aurora",
      text: "Previous draft text",
      recipientIds: [],
      attachmentIds: ["attachment-aurora-1"],
      revision: "1",
    };
    const loadState = vi.spyOn(bridge, "load_state").mockImplementation(async (...args) => {
      const snapshot = await fixtureLoadState(...args);
      const cancellableSnapshot = {
        ...snapshot,
        draft: hostDraft,
        conversations: snapshot.conversations.map(conversation => conversation.id === "conv-aurora"
          ? {
              ...conversation,
              messages: conversation.messages.map(message => message.id === "message-aurora-2"
                ? { ...message, sendCancellation: { ...message.sendCancellation!, state: "none" as const, canCancel: true } }
                : message),
            }
          : conversation),
      };
      if (!tooLate) return cancellableSnapshot;
      return {
        ...cancellableSnapshot,
        conversations: cancellableSnapshot.conversations.map(conversation => conversation.id === "conv-aurora"
          ? {
              ...conversation,
              messages: conversation.messages.map(message => message.id === "message-aurora-2"
                ? { ...message, sendCancellation: { ...message.sendCancellation!, state: "too-late", canCancel: false } }
                : message),
            }
          : conversation),
      };
    });
    const subscribe = vi.spyOn(bridge, "subscribe").mockImplementation(listener => {
      notify = listener;
      return () => undefined;
    });
    const cancelSend = vi.spyOn(bridge, "cancel_send").mockResolvedValueOnce({ state: "requested", canCancel: false, requiresGatewayUpdate: false });
    const saveDraft = vi.spyOn(bridge, "save_draft").mockImplementation(async ({ expectedRevision, ...input }) => {
      hostDraft = { ...input, id: "draft-aurora", revision: String(Number(expectedRevision) + 1) };
      return hostDraft;
    });
    render(<App />);

    await waitFor(() => expect(notify).toBeDefined());
    await waitFor(() => expect(document.getElementById("composer-textarea")).toHaveValue("Previous draft text"));
    fireEvent.click(await screen.findByRole("button", { name: "Cancel send and remove message" }));
    await waitFor(() => expect(document.getElementById("composer-textarea")).toHaveValue("I'll have it to you shortly.\nIt was received by the phone and can still be cancelled."));
    tooLate = true;
    act(() => notify?.());

    await screen.findByText("Cancellation was too late.");
    await waitFor(() => expect(document.getElementById("composer-textarea")).toHaveValue("Previous draft text"));
    await waitFor(() => expect(saveDraft).toHaveBeenLastCalledWith(expect.objectContaining({ text: "Previous draft text", attachmentIds: ["attachment-aurora-1"] })));
    saveDraft.mockRestore();
    cancelSend.mockRestore();
    subscribe.mockRestore();
    loadState.mockRestore();
  });

  it("warns when a requested cancellation has a non-local attachment", async () => {
    let hostDraft: Draft = {
      id: "draft-aurora",
      conversationId: "conv-aurora",
      text: "Existing draft",
      recipientIds: [],
      attachmentIds: [],
      revision: "1",
    };
    const loadState = vi.spyOn(bridge, "load_state").mockImplementation(async (...args) => {
      const snapshot = await fixtureLoadState(...args);
      return {
        ...snapshot,
        draft: hostDraft,
        conversations: snapshot.conversations.map(conversation => conversation.id === "conv-aurora"
          ? {
              ...conversation,
              messages: conversation.messages.map(message => message.id === "message-aurora-2"
                ? {
                    ...message,
                    attachments: [{ ...message.attachments[0], state: "pending" as const }],
                    sendCancellation: { ...message.sendCancellation!, state: "none" as const, canCancel: true },
                  }
                : message),
            }
          : conversation),
      };
    });
    const cancelSend = vi.spyOn(bridge, "cancel_send").mockResolvedValueOnce({ state: "requested", canCancel: false, requiresGatewayUpdate: false });
    const saveDraft = vi.spyOn(bridge, "save_draft").mockImplementation(async ({ expectedRevision, ...input }) => {
      hostDraft = { ...input, id: "draft-aurora", revision: String(Number(expectedRevision) + 1) };
      return hostDraft;
    });
    render(<App />);

    await waitFor(() => expect(document.getElementById("composer-textarea")).toHaveValue("Existing draft"));
    fireEvent.click(await screen.findByRole("button", { name: "Cancel send and remove message" }));

    const notice = (await screen.findByText(CANCELLED_ATTACHMENTS_SKIPPED_NOTICE)).closest("[data-draft-notice]")!;
    expect(notice).toHaveAttribute("data-draft-notice", "attachment-feedback");
    expect(notice).not.toHaveTextContent("Not sent:");
    await waitFor(() => expect(saveDraft).toHaveBeenCalledWith(expect.objectContaining({ attachmentIds: [] })));
    saveDraft.mockRestore();
    cancelSend.mockRestore();
    loadState.mockRestore();
  });
});
