import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, cancellationDisplayState } from "./App";
import { bridge, fixtureBridge } from "./bridge/bridge";

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => { resolve = next; });
  return { promise, resolve };
};

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
  it("marks the received-by-phone fixture as gateway-persisted without inferring eligibility from its status", async () => {
    const snapshot = await fixtureBridge.load_state("conv-aurora");
    const message = snapshot.conversations.find((conversation) => conversation.id === "conv-aurora")?.messages.find((candidate) => candidate.id === "message-aurora-2");

    expect(message).toMatchObject({ status: "gateway-persisted", sendCancellation: { canCancel: true } });
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

    expect(await screen.findByText(/received by the phone/)).toBeInTheDocument();
    expect(screen.getByText("Cancellation was too late.")).toHaveAttribute("data-message-cancellation-notice", "message-aurora-2");
    cancelSend.mockRestore();
  });

  it("replaces an eligible fixture message with its durable pending state", async () => {
    render(<App />);

    const cancel = await screen.findByRole("button", { name: "Cancel send and remove message" });
    fireEvent.click(cancel);

    expect((await screen.findByText("Cancelling…")).closest("[data-message-cancellation]")).toHaveAttribute("data-message-cancellation", "message-aurora-2");
    expect(screen.queryByText("I'll have it to you shortly.")).not.toBeInTheDocument();
    expect(screen.queryByText("Subject: Fixture subject")).not.toBeInTheDocument();
    expect(document.querySelector('[data-attachment-id="attachment-aurora-1"]')).not.toBeInTheDocument();
  });
});
