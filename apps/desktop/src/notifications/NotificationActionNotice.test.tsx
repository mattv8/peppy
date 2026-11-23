import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { TOAST_DISMISS_MS } from "../notices/useNoticeDismissal";
import { NotificationActionNotice } from "./NotificationActionNotice";

afterEach(() => { cleanup(); vi.useRealTimers(); });

it("dismisses info after the timeout", () => {
  vi.useFakeTimers();
  const onDismiss = vi.fn();
  render(<NotificationActionNotice notice={{ kind: "info", message: "Info" }} onDismiss={onDismiss} />);
  expect(screen.getByRole("status")).toHaveTextContent("Info");
  act(() => vi.advanceTimersByTime(TOAST_DISMISS_MS - 1));
  expect(onDismiss).not.toHaveBeenCalled();
  act(() => vi.advanceTimersByTime(1));
  expect(onDismiss).toHaveBeenCalledOnce();
});

it("keeps errors as alerts until dismissed", () => {
  vi.useFakeTimers();
  const onDismiss = vi.fn();
  render(<NotificationActionNotice notice={{ kind: "error", message: "Error" }} onDismiss={onDismiss} />);
  expect(screen.getByRole("alert")).toHaveTextContent("Error");
  act(() => vi.advanceTimersByTime(TOAST_DISMISS_MS * 2));
  expect(onDismiss).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Dismiss notice" }));
  expect(onDismiss).toHaveBeenCalledOnce();
});

it("pauses the info timeout while hovered", () => {
  vi.useFakeTimers();
  const onDismiss = vi.fn();
  render(<NotificationActionNotice notice={{ kind: "info", message: "Info" }} onDismiss={onDismiss} />);
  const toast = screen.getByRole("status");
  fireEvent.mouseEnter(toast);
  act(() => vi.advanceTimersByTime(TOAST_DISMISS_MS * 2));
  expect(onDismiss).not.toHaveBeenCalled();
  fireEvent.mouseLeave(toast);
  act(() => vi.advanceTimersByTime(TOAST_DISMISS_MS));
  expect(onDismiss).toHaveBeenCalledOnce();
});
