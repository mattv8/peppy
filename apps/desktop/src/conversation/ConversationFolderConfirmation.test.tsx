import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ConversationFolderConfirmation } from "./ConversationFolderConfirmation";

describe("ConversationFolderConfirmation", () => {
  afterEach(() => { cleanup(); vi.restoreAllMocks(); });

  it("uses archive copy, focuses the primary action, and positions below the row", () => {
    const trigger = document.createElement("button");
    const row = document.createElement("div");
    row.dataset.conversationRow = "c1";
    row.append(trigger);
    document.body.append(row);
    vi.spyOn(row, "getBoundingClientRect").mockReturnValue({ bottom: 40, height: 20, left: 100, right: 300, top: 20, width: 200, x: 100, y: 20, toJSON: () => ({}) });
    vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue({ bottom: 40, height: 20, left: 270, right: 300, top: 20, width: 30, x: 270, y: 20, toJSON: () => ({}) });
    render(<ConversationFolderConfirmation kind="archived" name="Ada" trigger={trigger} pending={false} onConfirm={vi.fn()} onCancel={vi.fn()} />);

    const dialog = screen.getByRole("dialog", { name: "Archive the conversation with Ada on all synced devices?" });
    expect(dialog).toHaveAccessibleDescription("It returns to Inbox when a new message arrives.");
    expect(within(dialog).getByRole("button", { name: "Archive" })).toHaveFocus();
    expect(dialog).toHaveStyle({ top: "46px" });
    row.remove();
  });

  it("flips above the trigger, cancels on outside, scroll, focus-out and Escape, and restores eligible trigger focus", () => {
    const trigger = document.createElement("button");
    document.body.append(trigger);
    vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue({ bottom: 790, height: 20, left: 10, right: 40, top: 770, width: 30, x: 10, y: 770, toJSON: () => ({}) });
    vi.spyOn(window, "innerHeight", "get").mockReturnValue(800);
    const onCancel = vi.fn();
    render(<><button>Elsewhere</button><ConversationFolderConfirmation kind="spam" name="Ada" trigger={trigger} pending={false} onConfirm={vi.fn()} onCancel={onCancel} /></>);
    const dialog = screen.getByRole("dialog", { name: "Mark Ada as spam on all synced devices?" });
    expect(dialog.style.top).not.toBe("796px");
    fireEvent.pointerDown(screen.getByRole("button", { name: "Elsewhere" }));
    fireEvent.scroll(document);
    fireEvent.focusOut(dialog, { relatedTarget: screen.getByRole("button", { name: "Elsewhere" }) });
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onCancel).toHaveBeenCalledTimes(4);
    trigger.remove();
  });

  it("cancels once on Shift+Tab from the first action and restores trigger focus without a second cancel", async () => {
    const trigger = document.createElement("button");
    document.body.append(trigger);
    const onCancel = vi.fn();
    render(<ConversationFolderConfirmation kind="archived" name="Ada" trigger={trigger} pending={false} onConfirm={vi.fn()} onCancel={onCancel} />);
    const primary = screen.getByRole("button", { name: "Archive" });

    fireEvent.keyDown(primary, { key: "Tab" });
    expect(onCancel).not.toHaveBeenCalled();
    fireEvent.keyDown(primary, { key: "Tab", shiftKey: true });
    expect(onCancel).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(trigger).toHaveFocus());
    fireEvent.focusOut(screen.getByRole("dialog"), { relatedTarget: trigger });
    expect(onCancel).toHaveBeenCalledTimes(1);
    trigger.remove();
  });

  it("cancels once on Tab from the last action and restores trigger focus", async () => {
    const trigger = document.createElement("button");
    document.body.append(trigger);
    const onCancel = vi.fn();
    render(<ConversationFolderConfirmation kind="spam" name="Ada" trigger={trigger} pending={false} onConfirm={vi.fn()} onCancel={onCancel} />);
    const cancel = screen.getByRole("button", { name: "Cancel" });

    cancel.focus();
    fireEvent.keyDown(cancel, { key: "Tab" });
    expect(onCancel).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(trigger).toHaveFocus());
    expect(onCancel).toHaveBeenCalledTimes(1);
    trigger.remove();
  });

  it("disables both actions while pending", () => {
    const trigger = document.createElement("button");
    document.body.append(trigger);
    render(<ConversationFolderConfirmation kind="spam" name="Ada" trigger={trigger} pending onConfirm={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Mark as spam" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled();
    trigger.remove();
  });
});
