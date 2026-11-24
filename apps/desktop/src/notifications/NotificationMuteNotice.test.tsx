import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { NotificationMuteNotice } from "./NotificationMuteNotice";

afterEach(() => {
  vi.useRealTimers();
  cleanup();
});

const notice = { kind: "success" as const, filter: { sourceDeviceId: "phone", packageName: "chat", appName: "Chat", muted: true }, label: "Chat" };

it("shows a mute confirmation and delegates Undo", () => {
  const onUndo = vi.fn();
  render(<NotificationMuteNotice notice={notice} pending={false} onUndo={onUndo} onRetry={vi.fn()} onDismiss={vi.fn()} />);

  expect(screen.getByRole("status")).toHaveTextContent("Muted Chat.");
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  expect(onUndo).toHaveBeenCalledOnce();
});

it("shows the unmute retry copy for an Undo failure", () => {
  render(<NotificationMuteNotice notice={{ ...notice, kind: "error" }} errorAction="unmute" pending={false} onUndo={vi.fn()} onRetry={vi.fn()} onDismiss={vi.fn()} />);

  expect(screen.getByRole("alert")).toHaveTextContent("Couldn't unmute Chat.");
  expect(screen.getByRole("button", { name: "Retry" })).toBeEnabled();
});

it("pauses its success timeout while hovered", () => {
  vi.useFakeTimers();
  const onDismiss = vi.fn();
  render(<NotificationMuteNotice notice={notice} pending={false} onUndo={vi.fn()} onRetry={vi.fn()} onDismiss={onDismiss} />);
  const toast = screen.getByRole("status");
  fireEvent.mouseEnter(toast);
  act(() => vi.advanceTimersByTime(10_000));
  expect(onDismiss).not.toHaveBeenCalled();
  fireEvent.mouseLeave(toast);
  act(() => vi.advanceTimersByTime(10_000));
  expect(onDismiss).toHaveBeenCalledOnce();
});
