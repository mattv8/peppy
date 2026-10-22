import "@testing-library/jest-dom/vitest";
import { StrictMode, type ComponentProps } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Modal } from "./Modal";

const originalShowModal = HTMLDialogElement.prototype.showModal;
const originalClose = HTMLDialogElement.prototype.close;

afterEach(() => {
  cleanup();
  HTMLDialogElement.prototype.showModal = originalShowModal;
  HTMLDialogElement.prototype.close = originalClose;
  vi.restoreAllMocks();
});

function mockNativeDialog() {
  const showModal = vi.fn(function (this: HTMLDialogElement) { this.open = true; });
  const close = vi.fn(function (this: HTMLDialogElement) { this.open = false; });
  HTMLDialogElement.prototype.showModal = showModal;
  HTMLDialogElement.prototype.close = close;
  return { showModal, close };
}

function renderModal(props: Partial<ComponentProps<typeof Modal>> = {}) {
  const onDismiss = vi.fn();
  return {
    onDismiss,
    ...render(<Modal id="test-modal" title="Test modal" onDismiss={onDismiss} {...props}><button type="button">First</button><button type="button">Last</button></Modal>),
  };
}

describe("Modal", () => {
  it("calls showModal once and focuses the title when native dialogs are available", () => {
    const { showModal } = mockNativeDialog();
    renderModal();
    expect(showModal).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("heading", { name: "Test modal" })).toHaveFocus();
  });

  it("falls back to the open attribute and dismisses on document Escape", () => {
    const { onDismiss } = renderModal();
    expect(screen.getByRole("dialog")).toHaveAttribute("open");
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("prevents native cancel and dismisses", () => {
    const { onDismiss } = renderModal();
    const event = new Event("cancel", { cancelable: true });
    screen.getByRole("dialog").dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("blocks dismissal while busy and restores title focus when focus leaves", () => {
    const { onDismiss, rerender } = renderModal();
    const elsewhere = document.createElement("button");
    document.body.append(elsewhere);
    elsewhere.focus();
    rerender(<Modal id="test-modal" title="Test modal" onDismiss={onDismiss} busy><button type="button">First</button></Modal>);
    fireEvent.keyDown(document, { key: "Escape" });
    fireEvent(screen.getByRole("dialog"), new Event("cancel", { cancelable: true }));
    expect(onDismiss).not.toHaveBeenCalled();
    expect(screen.getByRole("heading", { name: "Test modal" })).toHaveFocus();
    elsewhere.remove();
  });

  it("wraps Tab and Shift+Tab focus", () => {
    renderModal();
    const title = screen.getByRole("heading", { name: "Test modal" });
    const first = screen.getByRole("button", { name: "First" });
    const last = screen.getByRole("button", { name: "Last" });
    last.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(title).toHaveFocus();
    title.focus();
    fireEvent.keyDown(document, { key: "Tab", shiftKey: true });
    expect(last).toHaveFocus();
    first.focus();
    fireEvent.keyDown(document, { key: "Tab", shiftKey: true });
    expect(first).toHaveFocus();
    expect(title).not.toHaveFocus();
  });

  it("opens exactly once under StrictMode and closes natively on unmount", () => {
    const { showModal, close } = mockNativeDialog();
    const { unmount } = render(<StrictMode><Modal id="strict-modal" title="Strict" onDismiss={() => {}} /></StrictMode>);
    expect(screen.getByRole("dialog")).toHaveAttribute("open");
    expect(showModal.mock.calls.length - close.mock.calls.length).toBe(1);
    unmount();
    expect(showModal.mock.calls.length - close.mock.calls.length).toBe(0);
  });

  it("leaves native Escape to the cancel event", () => {
    mockNativeDialog();
    const { onDismiss } = renderModal();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onDismiss).not.toHaveBeenCalled();
    fireEvent(screen.getByRole("dialog"), new Event("cancel", { cancelable: true }));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("reopens a force-closed dialog while busy and dismisses it otherwise", () => {
    const { showModal } = mockNativeDialog();
    const { onDismiss, rerender } = renderModal({ busy: true });
    const dialog = screen.getByRole("dialog") as HTMLDialogElement;
    dialog.open = false;
    fireEvent(dialog, new Event("close"));
    expect(showModal).toHaveBeenCalledTimes(2);
    expect(onDismiss).not.toHaveBeenCalled();
    rerender(<Modal id="test-modal" title="Test modal" onDismiss={onDismiss} />);
    dialog.open = false;
    fireEvent(dialog, new Event("close"));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("refocuses the title when the focused control is disabled by busy", () => {
    const { onDismiss, rerender } = renderModal();
    const first = screen.getByRole("button", { name: "First" });
    first.focus();
    rerender(<Modal id="test-modal" title="Test modal" onDismiss={onDismiss} busy><button type="button" disabled>First</button><button type="button">Last</button></Modal>);
    expect(screen.getByRole("heading", { name: "Test modal" })).toHaveFocus();
  });

  it("adds the caller's class to the panel", () => {
    renderModal({ className: "wide-panel" });
    expect(document.querySelector("[data-modal-panel]")).toHaveClass("peppy-modal-panel", "wide-panel");
  });

  it("returns focus to the opener or fallback after unmount", () => {
    const fallback = { current: document.createElement("button") };
    document.body.append(fallback.current);
    const opener = document.createElement("button");
    document.body.append(opener);
    opener.focus();
    const { unmount } = renderModal({ focusFallback: fallback });
    unmount();
    expect(opener).toHaveFocus();
    const removedOpener = document.createElement("button");
    document.body.append(removedOpener);
    removedOpener.focus();
    const second = renderModal({ focusFallback: fallback });
    removedOpener.remove();
    second.unmount();
    expect(fallback.current).toHaveFocus();
    fallback.current.remove();
  });

  it("returns focus to an explicit opener when nothing was focused at open", () => {
    const trigger = document.createElement("button");
    document.body.append(trigger);
    (document.activeElement as HTMLElement | null)?.blur();
    const { unmount } = renderModal({ opener: { current: trigger } });
    unmount();
    expect(trigger).toHaveFocus();
    trigger.remove();
  });

  it("names the dialog and renders actions in its footer", () => {
    render(<Modal id="named-modal" title="Named modal" onDismiss={() => {}} actions={<button type="button">Confirm</button>} />);
    expect(screen.getByRole("dialog", { name: "Named modal" })).toHaveAttribute("aria-labelledby", "named-modal-title");
    expect(document.querySelector("[data-modal-actions]")).toContainElement(screen.getByRole("button", { name: "Confirm" }));
  });
});
