import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { UPDATE_PAUSED_DESCRIPTION } from "@peppy/desktop-ui";
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

it("renders an optional action before the dismiss control and runs it", () => {
  const onAction = vi.fn();
  const onDismiss = vi.fn();
  render(<NotificationActionNotice notice={{ kind: "error", message: "Couldn't dismiss 2 notifications.", action: { label: "Try again", onAction } }} onDismiss={onDismiss} />);
  const action = screen.getByRole("button", { name: "Try again" });
  const dismiss = screen.getByRole("button", { name: "Dismiss notice" });
  expect(action.compareDocumentPosition(dismiss) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  fireEvent.click(action);
  expect(onAction).toHaveBeenCalledOnce();
  expect(onDismiss).not.toHaveBeenCalled();
});

it("disables the action with the paused-writes description while writes are paused", () => {
  const onAction = vi.fn();
  render(<NotificationActionNotice notice={{ kind: "error", message: "Couldn't dismiss 2 notifications.", action: { label: "Try again", onAction, disabled: true } }} onDismiss={vi.fn()} />);
  const action = screen.getByRole("button", { name: "Try again" });
  expect(action).toBeDisabled();
  expect(action).toHaveAttribute("title", UPDATE_PAUSED_DESCRIPTION);
  expect(action).toHaveAttribute("aria-describedby", "update-paused-description");
  fireEvent.click(action);
  expect(onAction).not.toHaveBeenCalled();
});

it("tells the owner when the close control is used, after dismissing", () => {
  const onDismiss = vi.fn();
  const onClosed = vi.fn();
  render(<NotificationActionNotice notice={{ kind: "error", message: "Error" }} onDismiss={onDismiss} onClosed={onClosed} />);
  fireEvent.click(screen.getByRole("button", { name: "Dismiss notice" }));
  expect(onDismiss).toHaveBeenCalledOnce();
  expect(onClosed).toHaveBeenCalledOnce();
});

it("omits the action button when none is supplied", () => {
  render(<NotificationActionNotice notice={{ kind: "error", message: "Error" }} onDismiss={vi.fn()} />);
  expect(screen.queryByRole("button", { name: "Try again" })).toBeNull();
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
