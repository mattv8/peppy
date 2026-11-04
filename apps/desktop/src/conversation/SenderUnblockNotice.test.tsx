import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TOAST_DISMISS_MS } from "./MarkAllReadAction";
import { SenderUnblockNotice } from "./SenderUnblockNotice";

describe("SenderUnblockNotice", () => {
  afterEach(() => { cleanup(); vi.useRealTimers(); });

  it("renders error copy and retries", () => {
    const retry = vi.fn();
    render(<SenderUnblockNotice notice={{ kind: "error", id: "c1", name: "Ada" }} pending={false} onRetry={retry} onDismiss={vi.fn()} />);
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn't unblock Ada.");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(retry).toHaveBeenCalledOnce();
  });

  it("dismisses success after the timeout unless hovered", () => {
    vi.useFakeTimers();
    const dismiss = vi.fn();
    render(<SenderUnblockNotice notice={{ kind: "success", id: "c1", name: "Ada" }} pending={false} onRetry={vi.fn()} onDismiss={dismiss} />);
    const toast = screen.getByRole("status");
    expect(toast).toHaveTextContent("Unblocking Ada on your phone.");
    fireEvent.mouseEnter(toast);
    act(() => { vi.advanceTimersByTime(TOAST_DISMISS_MS); });
    expect(dismiss).not.toHaveBeenCalled();
    fireEvent.mouseLeave(toast);
    act(() => { vi.advanceTimersByTime(TOAST_DISMISS_MS); });
    expect(dismiss).toHaveBeenCalledOnce();
  });
});
