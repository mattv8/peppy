import "@testing-library/jest-dom/vitest";
import { StrictMode } from "react";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MarkAllReadAction } from "./MarkAllReadAction";

describe("MarkAllReadAction", () => {
  afterEach(cleanup);
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
    expect(screen.queryByRole("button", { name: "Mark all as read" })).toBeNull();
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

  it("keeps zero absent and disables the action when locked", () => {
    const { rerender } = render(<MarkAllReadAction unreadCount={0} enabled markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={vi.fn()} />);
    expect(screen.queryByRole("button", { name: "Mark all as read" })).toBeNull();
    rerender(<MarkAllReadAction unreadCount={1} enabled={false} markAllRead={vi.fn()} refresh={vi.fn()} onFocusFallback={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Mark all as read" })).toBeDisabled();
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
