import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TOAST_DISMISS_MS } from "./MarkAllReadAction";
import { ConversationFolderNotice, type FolderNotice } from "./ConversationFolderNotice";

const notices: Array<[FolderNotice, string]> = [
  [{ kind: "success", conversationId: "1", name: "Ada", from: "inbox", to: "archived" }, "Archived Ada."],
  [{ kind: "success", conversationId: "1", name: "Ada", from: "inbox", to: "spam" }, "Marked Ada as spam."],
  [{ kind: "success", conversationId: "1", name: "Ada", from: "archived", to: "inbox" }, "Moved Ada to Inbox."],
  [{ kind: "success", conversationId: "1", name: "Ada", from: "spam", to: "inbox" }, "Moved Ada out of Spam."],
];

describe("ConversationFolderNotice", () => {
  afterEach(() => { cleanup(); vi.useRealTimers(); });

  it.each(notices)("renders transition copy", (notice, copy) => {
    const onUndo = vi.fn();
    render(<ConversationFolderNotice notice={notice} pending={false} onUndo={onUndo} onRetry={vi.fn()} onDismiss={vi.fn()} />);
    expect(screen.getByRole("status")).toHaveTextContent(copy);
    expect(screen.getByRole("status")).toHaveAttribute("aria-live", "polite");
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect(onUndo).toHaveBeenCalledOnce();
  });

  it("renders error copy and invokes retry and dismissal callbacks", () => {
    const onRetry = vi.fn(); const onDismiss = vi.fn();
    render(<ConversationFolderNotice notice={{ kind: "error", conversationId: "1", name: "Ada", from: "inbox", to: "archived" }} pending={false} onUndo={vi.fn()} onRetry={onRetry} onDismiss={onDismiss} />);
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn't archive Ada.");
    expect(screen.getByRole("alert")).not.toHaveAttribute("aria-live");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    fireEvent.click(screen.getByRole("button", { name: "Dismiss notification" }));
    expect(onRetry).toHaveBeenCalledOnce(); expect(onDismiss).toHaveBeenCalledOnce();
  });

  it("dismisses success after the timeout unless hovered", () => {
    vi.useFakeTimers();
    const onDismiss = vi.fn();
    render(<ConversationFolderNotice notice={notices[0][0]} pending={false} onUndo={vi.fn()} onRetry={vi.fn()} onDismiss={onDismiss} />);
    const toast = screen.getByRole("status");
    fireEvent.mouseEnter(toast);
    act(() => { vi.advanceTimersByTime(TOAST_DISMISS_MS); });
    expect(onDismiss).not.toHaveBeenCalled();
    fireEvent.mouseLeave(toast);
    act(() => { vi.advanceTimersByTime(TOAST_DISMISS_MS); });
    expect(onDismiss).toHaveBeenCalledOnce();
  });

  it("keeps the original timer when the parent rerenders with a new onDismiss", () => {
    vi.useFakeTimers();
    const firstDismiss = vi.fn(); const latestDismiss = vi.fn();
    const { rerender } = render(<ConversationFolderNotice notice={notices[0][0]} pending={false} onUndo={vi.fn()} onRetry={vi.fn()} onDismiss={firstDismiss} />);
    act(() => { vi.advanceTimersByTime(TOAST_DISMISS_MS / 2); });
    rerender(<ConversationFolderNotice notice={notices[0][0]} pending={false} onUndo={vi.fn()} onRetry={vi.fn()} onDismiss={latestDismiss} />);
    act(() => { vi.advanceTimersByTime(TOAST_DISMISS_MS / 2); });
    expect(latestDismiss).toHaveBeenCalledOnce();
    expect(firstDismiss).not.toHaveBeenCalled();
  });

  it("auto-dismisses a later success notice after the focused previous notice unmounted", () => {
    vi.useFakeTimers();
    const onDismiss = vi.fn();
    const { rerender } = render(<ConversationFolderNotice notice={notices[0][0]} pending={false} onUndo={vi.fn()} onRetry={vi.fn()} onDismiss={onDismiss} />);
    act(() => { screen.getByRole("button", { name: "Undo" }).focus(); });
    rerender(<ConversationFolderNotice notice={undefined} pending={false} onUndo={vi.fn()} onRetry={vi.fn()} onDismiss={onDismiss} />);
    rerender(<ConversationFolderNotice notice={{ ...notices[2][0] }} pending={false} onUndo={vi.fn()} onRetry={vi.fn()} onDismiss={onDismiss} />);
    act(() => { vi.advanceTimersByTime(TOAST_DISMISS_MS); });
    expect(onDismiss).toHaveBeenCalledOnce();
  });
});
