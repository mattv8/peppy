import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MarkAllReadAction } from "./MarkAllReadAction";

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((accept, decline) => {
    resolve = accept;
    reject = decline;
  });
  return { promise, resolve, reject };
};

const confirm = () => {
  fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" }));
};

describe("MarkAllReadAction contract", () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("retries only the authoritative refresh after an acknowledged mutation refresh fails", async () => {
    const markAllRead = vi.fn().mockResolvedValue("2");
    const refresh = vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce(undefined);
    render(<MarkAllReadAction unreadCount={2} enabled markAllRead={markAllRead} refresh={refresh} onFocusFallback={vi.fn()} />);

    confirm();
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "Retry refresh" }));

    await waitFor(() => expect(screen.getByRole("status")).toBeEmptyDOMElement());
    expect(markAllRead).toHaveBeenCalledOnce();
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("uses refresh-only recovery for an invalid acknowledgement without rendering its count", async () => {
    const markAllRead = vi.fn().mockResolvedValue("1.5");
    const refresh = vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce(undefined);
    render(<MarkAllReadAction unreadCount={1} enabled markAllRead={markAllRead} refresh={refresh} onFocusFallback={vi.fn()} />);

    confirm();
    await screen.findByRole("alert");
    expect(screen.queryByText(/1\.5/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Retry refresh" }));

    await waitFor(() => expect(screen.getByRole("status")).toBeEmptyDOMElement());
    expect(markAllRead).toHaveBeenCalledOnce();
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("does not refresh a replacement session when the previous session completes late", async () => {
    const oldAcknowledgement = deferred<string>();
    const oldRefresh = vi.fn().mockResolvedValue(undefined);
    const newRefresh = vi.fn().mockResolvedValue(undefined);
    const { rerender } = render(<MarkAllReadAction key="old" unreadCount={1} enabled markAllRead={() => oldAcknowledgement.promise} refresh={oldRefresh} onFocusFallback={vi.fn()} />);

    confirm();
    rerender(<MarkAllReadAction key="new" unreadCount={1} enabled markAllRead={vi.fn().mockResolvedValue("1")} refresh={newRefresh} onFocusFallback={vi.fn()} />);
    oldAcknowledgement.resolve("1");

    await Promise.resolve();
    expect(oldRefresh).not.toHaveBeenCalled();
    confirm();
    await waitFor(() => expect(newRefresh).toHaveBeenCalledOnce());
  });

  it("does not refresh after unmount when an acknowledgement completes late", async () => {
    const acknowledgement = deferred<string>();
    const refresh = vi.fn().mockResolvedValue(undefined);
    const { unmount } = render(<MarkAllReadAction unreadCount={1} enabled markAllRead={() => acknowledgement.promise} refresh={refresh} onFocusFallback={vi.fn()} />);

    confirm();
    unmount();
    acknowledgement.resolve("1");

    await Promise.resolve();
    expect(refresh).not.toHaveBeenCalled();
  });

  it("does not refresh after unmount when a mutation rejects late", async () => {
    const acknowledgement = deferred<string>();
    const refresh = vi.fn().mockResolvedValue(undefined);
    const { unmount } = render(<MarkAllReadAction unreadCount={1} enabled markAllRead={() => acknowledgement.promise} refresh={refresh} onFocusFallback={vi.fn()} />);

    confirm();
    unmount();
    acknowledgement.reject(new Error("late failure"));

    await Promise.resolve();
    expect(refresh).not.toHaveBeenCalled();
  });

  it("closes an externally invalidated confirmation without taking focus from another control", async () => {
    const focusFallback = vi.fn();
    const { rerender } = render(<><input aria-label="Elsewhere" /><MarkAllReadAction unreadCount={1} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={focusFallback} /></>);

    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    await waitFor(() => expect(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" })).toHaveFocus());
    screen.getByRole("textbox", { name: "Elsewhere" }).focus();
    rerender(<><input aria-label="Elsewhere" /><MarkAllReadAction unreadCount={0} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={focusFallback} /></>);

    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(screen.getByRole("textbox", { name: "Elsewhere" })).toHaveFocus();
    expect(focusFallback).not.toHaveBeenCalled();
  });

  it("does not run a deferred fallback after another control gains focus", () => {
    const animationFrames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      animationFrames.push(callback);
      return animationFrames.length;
    });
    const focusFallback = vi.fn();
    const { rerender } = render(<><input aria-label="Elsewhere" /><MarkAllReadAction unreadCount={1} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={focusFallback} /></>);

    screen.getByRole("button", { name: "Mark all as read" }).focus();
    rerender(<><input aria-label="Elsewhere" /><MarkAllReadAction unreadCount={0} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={focusFallback} /></>);
    screen.getByRole("textbox", { name: "Elsewhere" }).focus();
    act(() => {
      animationFrames.splice(0).forEach(callback => callback(0));
    });

    expect(screen.getByRole("textbox", { name: "Elsewhere" })).toHaveFocus();
    expect(focusFallback).not.toHaveBeenCalled();
  });

  it("uses the fallback when an externally removed action owned focus", async () => {
    const focusFallback = vi.fn();
    const { rerender } = render(<MarkAllReadAction unreadCount={1} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={focusFallback} />);

    screen.getByRole("button", { name: "Mark all as read" }).focus();
    rerender(<MarkAllReadAction unreadCount={0} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={focusFallback} />);

    await waitFor(() => expect(focusFallback).toHaveBeenCalledOnce());
  });

  it("closes an open confirmation when the action becomes locked without invoking the mutation", () => {
    const markAllRead = vi.fn();
    const { rerender } = render(<MarkAllReadAction unreadCount={1} enabled markAllRead={markAllRead} refresh={vi.fn()} onFocusFallback={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    rerender(<MarkAllReadAction unreadCount={1} enabled={false} markAllRead={markAllRead} refresh={vi.fn()} onFocusFallback={vi.fn()} />);

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Mark all as read" })).toBeDisabled();
    expect(markAllRead).not.toHaveBeenCalled();
  });

  it("keeps a newly arrived unread count actionable after a zero acknowledgement", async () => {
    const { rerender } = render(<MarkAllReadAction unreadCount={1} enabled markAllRead={vi.fn().mockResolvedValue("0")} refresh={vi.fn().mockResolvedValue(undefined)} onFocusFallback={vi.fn()} />);

    confirm();
    await screen.findByRole("status");
    rerender(<MarkAllReadAction unreadCount={1} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={vi.fn()} />);

    expect(screen.getByRole("button", { name: "Mark all as read" })).toBeEnabled();
  });

  it("reports a zero acknowledgement as up to date", async () => {
    render(<MarkAllReadAction unreadCount={1} enabled markAllRead={vi.fn().mockResolvedValue("0")} refresh={vi.fn().mockResolvedValue(undefined)} onFocusFallback={vi.fn()} />);

    confirm();

    await waitFor(() => expect(document.getElementById("thread-list-action-notice")).toHaveTextContent(/up to date/i));
  });

  it("restores owned confirmation focus after disabling the confirm button", async () => {
    const acknowledgement = deferred<string>();
    const focusFallback = vi.fn();
    const { rerender } = render(<MarkAllReadAction unreadCount={1} enabled markAllRead={() => acknowledgement.promise} refresh={vi.fn().mockResolvedValue(undefined)} onFocusFallback={focusFallback} />);

    confirm();
    expect(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" })).toBeDisabled();
    acknowledgement.resolve("1");
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Marked"));
    rerender(<MarkAllReadAction unreadCount={0} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={focusFallback} />);

    await waitFor(() => expect(focusFallback).toHaveBeenCalledOnce());
  });
});
