import "@testing-library/jest-dom/vitest";
import { StrictMode } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MarkAllReadAction, TOAST_DISMISS_MS } from "./MarkAllReadAction";

describe("MarkAllReadAction", () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });
  it("uses the full unread count, confirms once, and refreshes after acknowledgement", async () => {
    const markAllRead = vi.fn().mockResolvedValue("2");
    const refresh = vi.fn().mockResolvedValue(undefined);
    render(<MarkAllReadAction unreadCount={2} enabled markAllRead={markAllRead} refresh={refresh} onFocusFallback={vi.fn()} />);

    screen.getByRole("button", { name: "Mark all as read" }).focus();
    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    expect(screen.getByRole("dialog")).toBeVisible();
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" }));

    await waitFor(() => expect(markAllRead).toHaveBeenCalledOnce());
    await waitFor(() => expect(refresh).toHaveBeenCalledOnce());
    expect(screen.getByRole("status")).toBeVisible();
  });

  it("closes confirmation on Escape and outside interaction", () => {
    render(<><button>Elsewhere</button><MarkAllReadAction unreadCount={1} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={vi.fn()} /></>);
    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    fireEvent.pointerDown(screen.getByRole("button", { name: "Elsewhere" }));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("reports a mutation failure with an explicit retry", async () => {
    const markAllRead = vi.fn().mockRejectedValueOnce(new Error("focus lost")).mockResolvedValueOnce("1");
    const refresh = vi.fn().mockResolvedValue(undefined);
    render(<MarkAllReadAction unreadCount={1} enabled markAllRead={markAllRead} refresh={refresh} onFocusFallback={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" }));
    await screen.findByRole("alert");
    expect(refresh).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole("button", { name: "Retry mark all as read" }));
    await waitFor(() => expect(markAllRead).toHaveBeenCalledTimes(2));
  });

  it("keeps acknowledged success visible after the unread count reaches zero and restores owned focus", async () => {
    const focusFallback = vi.fn();
    const { rerender } = render(<MarkAllReadAction unreadCount={1} enabled markAllRead={vi.fn().mockResolvedValue("1")} refresh={vi.fn().mockResolvedValue(undefined)} onFocusFallback={focusFallback} />);
    screen.getByRole("button", { name: "Mark all as read" }).focus();
    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" }));
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Marked"));
    rerender(<MarkAllReadAction unreadCount={0} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={focusFallback} />);
    expect(screen.getByRole("status")).toHaveTextContent("Marked");
    expect(screen.getByRole("button", { name: "Mark all as read" })).toBeDisabled();
    await waitFor(() => expect(focusFallback).toHaveBeenCalled());
  });

  it("consumes zero-unread focus ownership after one fallback", async () => {
    const focusFallback = vi.fn();
    const { rerender } = render(<MarkAllReadAction unreadCount={1} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={focusFallback} />);

    screen.getByRole("button", { name: "Mark all as read" }).focus();
    rerender(<MarkAllReadAction unreadCount={0} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={focusFallback} />);
    await waitFor(() => expect(focusFallback).toHaveBeenCalledOnce());
    rerender(<MarkAllReadAction unreadCount={0} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={focusFallback} />);

    await new Promise(resolve => requestAnimationFrame(resolve));
    expect(focusFallback).toHaveBeenCalledOnce();
  });

  it("treats malformed acknowledgement as refresh-only recovery", async () => {
    const markAllRead = vi.fn().mockResolvedValue("01");
    const refresh = vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce(undefined);
    render(<MarkAllReadAction unreadCount={1} enabled markAllRead={markAllRead} refresh={refresh} onFocusFallback={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" }));
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "Retry refresh" }));
    await waitFor(() => expect(refresh).toHaveBeenCalledTimes(2));
    expect(markAllRead).toHaveBeenCalledOnce();
  });

  it("keeps the action stable, disabled without a dot at zero, and disabled when locked", () => {
    const { rerender } = render(<MarkAllReadAction unreadCount={0} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Mark all as read" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Mark all as read" })).toHaveAttribute("title", "No unread messages");
    expect(screen.queryByText("", { selector: ".titlebar-action-dot" })).toBeNull();
    rerender(<MarkAllReadAction unreadCount={1} enabled={false} markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Mark all as read" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Mark all as read" })).toHaveAttribute("title", "Mark all as read (unavailable while locked or loading)");
  });

  it("shows the unread dot and localized unread title", () => {
    render(<MarkAllReadAction unreadCount={1234} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Mark all as read" })).toHaveAttribute("title", "Mark all as read (1,234 unread)");
    expect(screen.getByRole("button", { name: "Mark all as read" })).toHaveAttribute("aria-describedby", "mark-all-read-count");
    expect(document.getElementById("mark-all-read-count")).toHaveTextContent("1,234 unread messages");
    expect(document.querySelector(".titlebar-action-dot")).toBeInTheDocument();
  });

  it("uses singular acknowledgement copy and portals notices into the supplied list host", async () => {
    const toastHost = document.createElement("div");
    document.body.append(toastHost);
    render(<MarkAllReadAction unreadCount={1} enabled markAllRead={vi.fn().mockResolvedValue("1")} refresh={vi.fn().mockResolvedValue(undefined)} onFocusFallback={vi.fn()} toastHost={toastHost} />);

    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" }));

    const notice = await screen.findByText("Marked 1 message as read.");
    expect(notice.parentElement).toBe(toastHost);
    expect(notice).toHaveAttribute("data-placement", "list");
    toastHost.remove();
  });

  it("places a toast in the window when no list host is supplied", async () => {
    render(<MarkAllReadAction unreadCount={2} enabled markAllRead={vi.fn().mockResolvedValue("2")} refresh={vi.fn().mockResolvedValue(undefined)} onFocusFallback={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" }));

    const notice = await screen.findByText("Marked 2 messages as read.");
    expect(notice.parentElement).toBe(document.body);
    expect(notice).toHaveAttribute("data-placement", "window");
  });

  it("restarts the success dismissal timer after hover and focus leave", async () => {
    vi.useFakeTimers();
    render(<MarkAllReadAction unreadCount={1} enabled markAllRead={vi.fn().mockResolvedValue("1")} refresh={vi.fn().mockResolvedValue(undefined)} onFocusFallback={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" }));
    await act(async () => { await Promise.resolve(); });
    const toast = document.getElementById("thread-list-action-notice")!;
    fireEvent.mouseEnter(toast);
    act(() => { vi.advanceTimersByTime(TOAST_DISMISS_MS); });
    expect(toast).toHaveTextContent("Marked 1 message as read.");
    fireEvent.mouseLeave(toast);
    await act(async () => { await Promise.resolve(); });
    screen.getByRole("button", { name: "Dismiss notification" }).focus();
    await act(async () => { await Promise.resolve(); });
    act(() => { vi.advanceTimersByTime(TOAST_DISMISS_MS); });
    expect(toast).toHaveTextContent("Marked 1 message as read.");
  });

  it("keeps errors until dismissed", async () => {
    render(<MarkAllReadAction unreadCount={1} enabled markAllRead={vi.fn().mockRejectedValue(new Error("offline"))} refresh={vi.fn().mockResolvedValue(undefined)} onFocusFallback={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" }));
    const notice = await screen.findByRole("alert");
    expect(notice).toHaveTextContent("Some messages may have been marked as read.");
    fireEvent.click(screen.getByRole("button", { name: "Dismiss notification" }));
    expect(screen.getByRole("status")).toBeEmptyDOMElement();
  });

  it("restores focus to the action after retrying from a toast when the focus job wins the pending render race", async () => {
    vi.spyOn(window, "requestAnimationFrame").mockImplementation(callback => {
      queueMicrotask(() => callback(performance.now()));
      return 1;
    });
    const markAllRead = vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce("1");
    render(<MarkAllReadAction unreadCount={1} enabled markAllRead={markAllRead} refresh={vi.fn().mockResolvedValue(undefined)} onFocusFallback={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" }));
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "Retry mark all as read" }));
    await screen.findByRole("status");
    await waitFor(() => expect(screen.getByRole("button", { name: "Mark all as read" })).toHaveFocus());
  });

  it("starts the next toast dismissal timer after dismissing a focused toast", async () => {
    vi.useFakeTimers();
    const markAllRead = vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce("1");
    render(<MarkAllReadAction unreadCount={1} enabled markAllRead={markAllRead} refresh={vi.fn().mockResolvedValue(undefined)} onFocusFallback={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" }));
    await act(async () => { await Promise.resolve(); });
    const dismiss = screen.getByRole("button", { name: "Dismiss notification" });
    dismiss.focus();
    fireEvent.click(dismiss);
    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" }));
    await act(async () => { await Promise.resolve(); });
    expect(screen.getByText("Marked 1 message as read.")).toBeInTheDocument();

    act(() => { vi.advanceTimersByTime(TOAST_DISMISS_MS); });
    expect(screen.getByRole("status")).toBeEmptyDOMElement();
  });

  it("completes an acknowledgement under StrictMode", async () => {
    const refresh = vi.fn().mockResolvedValue(undefined);
    render(<StrictMode><MarkAllReadAction unreadCount={1} enabled markAllRead={vi.fn().mockResolvedValue("1")} refresh={refresh} onFocusFallback={vi.fn()} /></StrictMode>);
    fireEvent.click(screen.getByRole("button", { name: "Mark all as read" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" }));
    await waitFor(() => expect(refresh).toHaveBeenCalledOnce());
    expect(screen.getByRole("status")).toHaveTextContent("Marked");
  });
});
