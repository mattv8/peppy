import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ConversationFolderNotice } from "../conversation/ConversationFolderNotice";
import { SenderUnblockNotice } from "../conversation/SenderUnblockNotice";
import { NotificationMuteNotice } from "../notifications/NotificationMuteNotice";
import { useMemo } from "react";

type CaseProps = { kind: "success" | "error"; revision: number; onDismiss(): void; toastHost?: HTMLElement };

const cases = [
  ["folder", ({ kind, revision, onDismiss, toastHost }: CaseProps) => {
    const notice = useMemo(() => ({ kind, conversationId: String(revision), name: "Ada", from: "inbox" as const, to: "archived" as const }), [kind, revision]);
    return <ConversationFolderNotice notice={notice} pending={false} onUndo={() => {}} onRetry={() => {}} onDismiss={onDismiss} toastHost={toastHost} />;
  }],
  ["sender", ({ kind, revision, onDismiss, toastHost }: CaseProps) => {
    const notice = useMemo(() => ({ kind, id: String(revision), name: "Ada" }), [kind, revision]);
    return <SenderUnblockNotice notice={notice} pending={false} onRetry={() => {}} onDismiss={onDismiss} toastHost={toastHost} />;
  }],
  ["mute", ({ kind, revision, onDismiss, toastHost }: CaseProps) => {
    const notice = useMemo(() => ({ kind, filter: { sourceDeviceId: String(revision), packageName: "chat", appName: "Chat", muted: true }, label: "Chat" }), [kind, revision]);
    return <NotificationMuteNotice notice={notice} pending={false} onUndo={() => {}} onRetry={() => {}} onDismiss={onDismiss} toastHost={toastHost} />;
  }],
] as const;

afterEach(() => { cleanup(); vi.useRealTimers(); });

describe.each(cases)("%s notice lifecycle", (_name, Notice) => {
  it("updates the dismissal callback without restarting the timer", () => {
    vi.useFakeTimers();
    const first = vi.fn(); const latest = vi.fn();
    const { rerender } = render(<Notice kind="success" revision={1} onDismiss={first} />);
    act(() => { vi.advanceTimersByTime(2000); });
    rerender(<Notice kind="success" revision={1} onDismiss={latest} />);
    act(() => { vi.advanceTimersByTime(2000); });
    expect(first).not.toHaveBeenCalled();
    expect(latest).toHaveBeenCalledOnce();
  });

  it("restarts on replacement, pauses on hover, and uses the latest callback", () => {
    vi.useFakeTimers();
    const first = vi.fn(); const latest = vi.fn();
    const host = document.createElement("div");
    document.body.appendChild(host);
    const { rerender, unmount } = render(<Notice kind="success" revision={1} onDismiss={first} toastHost={host} />);
    const toast = screen.getByRole("status");
    expect(host.contains(toast)).toBe(true);
    act(() => { vi.advanceTimersByTime(2000); });
    rerender(<Notice kind="success" revision={2} onDismiss={latest} toastHost={host} />);
    act(() => { vi.advanceTimersByTime(2000); });
    expect(latest).not.toHaveBeenCalled();
    fireEvent.mouseEnter(toast);
    act(() => { vi.advanceTimersByTime(4000); });
    expect(latest).not.toHaveBeenCalled();
    fireEvent.mouseLeave(toast);
    act(() => { vi.advanceTimersByTime(3999); });
    expect(latest).not.toHaveBeenCalled();
    act(() => { vi.advanceTimersByTime(1); });
    expect(latest).toHaveBeenCalledOnce();
    expect(first).not.toHaveBeenCalled();
    unmount();
    host.remove();
  });

  it("pauses for focus and restarts after focus leaves", () => {
    vi.useFakeTimers();
    const dismiss = vi.fn();
    render(<Notice kind="success" revision={1} onDismiss={dismiss} />);
    const button = screen.getByRole("button", { name: "Dismiss notification" });
    act(() => { button.focus(); });
    act(() => { vi.advanceTimersByTime(4000); });
    expect(dismiss).not.toHaveBeenCalled();
    act(() => { button.blur(); vi.advanceTimersByTime(16); });
    act(() => { vi.advanceTimersByTime(4000); });
    expect(dismiss).toHaveBeenCalledOnce();
  });

  it("does not dismiss errors or fire after unmount", () => {
    vi.useFakeTimers();
    const dismiss = vi.fn();
    const { rerender, unmount } = render(<Notice kind="error" revision={1} onDismiss={dismiss} />);
    act(() => { vi.advanceTimersByTime(8000); });
    expect(dismiss).not.toHaveBeenCalled();
    rerender(<Notice kind="success" revision={2} onDismiss={dismiss} />);
    unmount();
    act(() => { vi.advanceTimersByTime(4000); });
    expect(dismiss).not.toHaveBeenCalled();
  });
});
